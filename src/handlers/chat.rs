//! `/v1/chat/completions` handler.
//!
//! Implements the core gateway flow:
//!   1. Parse the request body (model, stream flag, temperature, prompt payload).
//!   2. Acquire TPM reservation (admission control — request is rejected if over budget).
//!   3. Build the exact-cache key (only for `temperature == 0`, non-streaming).
//!   4. If cache hit -> return cached response immediately, refund full reservation.
//!   5. Select providers via `ProviderSelector` (with circuit breaker awareness).
//!   6. Try each provider in priority order; on retryable failure (5xx, 429, timeout)
//!      record breaker failure and try the next provider.
//!   7. On success:
//!        - Non-streaming: read body, populate exact cache, record token usage,
//!          reconcile reservation, return JSON.
//!        - Streaming: pipe upstream `bytes_stream()` into Axum response via
//!          `proxy_stream_passthrough` (records TTFT on first meaningful content chunk,
//!          extracts trailing usage for TPM accounting, enforces stream idle timeout).

use std::time::Instant;

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::response::Response;
use axum::Json;
use bytes::Bytes;
use serde_json::Value;
use tracing::{info, warn};

use crate::types::ChatCompletionRequest;

use crate::cache::exact::ExactCache;
use crate::error::AppError;
use crate::middleware::rate_limit::RateLimitBuckets;
use crate::proxy::client as proxy_client;
use crate::proxy::stream::proxy_stream_passthrough;
use crate::router::selector::ProviderSelector;
use crate::state::{AppState, ClientIdentity};
use crate::telemetry::metrics as m;

// ---------------------------------------------------------------------------
// Provider failure classification
// ---------------------------------------------------------------------------
// 2xx complete        = success  → record_success()
// 400/422             = client error, no breaker trip  → record_success() (provider is healthy)
// 401/403             = provider/config error          → record_failure() + fail fast (not retryable)
// 408                 = timeout (transient)            → record_failure() + failover
// 429                 = overload/rate-limit            → record_failure() + failover
// 5xx                 = provider failure               → record_failure() + failover
// timeout (reqwest)   = provider failure               → record_failure() + failover
// connection error    = provider failure               → record_failure() + failover
// stream failure      = provider failure               → record_failure() (via stream callback)
// ---------------------------------------------------------------------------

/// Returns `true` if the upstream HTTP status should trigger failover.
fn is_retryable_upstream_status(status: reqwest::StatusCode) -> bool {
    status.is_server_error()
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
}

/// Classify an upstream status as a permanent client error (no failover).
/// 401/403 are config errors but still fail fast — retrying another provider
/// with the same malformed request is pointless.
fn is_non_retryable_client_error(status: reqwest::StatusCode) -> bool {
    status.is_client_error()
        && status != reqwest::StatusCode::TOO_MANY_REQUESTS
        && status != reqwest::StatusCode::REQUEST_TIMEOUT
}

/// Look up (or lazily create) rate-limit buckets for a client.
fn get_rate_limit_buckets(state: &AppState, client_id: &str) -> RateLimitBuckets {
    let cfg = &state.config.rate_limit;
    state
        .rate_limiter
        .entry(client_id.to_string())
        .or_insert_with(|| RateLimitBuckets::from_config(cfg.default_rpm, cfg.default_tpm))
        .clone()
}

pub async fn chat_completions(
    State(state): State<AppState>,
    identity: ClientIdentity,
    // P1 #5: Extract as Result so JSON parse errors route through AppError envelope
    // instead of Axum's default rejection response.
    payload: Result<Json<ChatCompletionRequest>, JsonRejection>,
) -> Result<Response, AppError> {
    let Json(mut payload) = payload.map_err(AppError::from)?;
    let request_start = Instant::now();

    // NOTE: RPM rate limiting is enforced by `rate_limit_middleware` BEFORE this
    // handler is invoked. We handle TPM admission below.

    // ---- 1. Extract request metadata --------------------------------------
    let model = payload.model.clone();
    let is_stream = payload.stream;
    let temperature = payload.temperature;

    // P0 #2: Inject stream_options for usage accounting on streaming responses.
    if is_stream && payload.stream_options.is_none() {
        payload.stream_options = Some(crate::types::StreamOptions {
            include_usage: true,
        });
    }

    // ---- 2. TPM admission control -----------------------------------------
    // Estimate token cost BEFORE dispatch. The reservation is deducted immediately
    // from the TPM bucket. After the response, we reconcile with actual usage.
    // If the bucket is empty, the request is rejected with 429.
    let estimated_tokens =
        payload.max_tokens.unwrap_or(256) as f64 + (payload.messages.len() as f64 * 20.0); // rough heuristic
    let buckets = get_rate_limit_buckets(&state, &identity.client_id);
    let mut reservation = match buckets.try_reserve_tpm(estimated_tokens) {
        Ok(handle) => Some(handle),
        Err(retry_after) => {
            m::record_error("rate_limit", "tpm_exceeded");
            return Err(AppError::RateLimited { retry_after });
        }
    };

    // ---- 3. Build exact cache key (only for deterministic requests) --------
    // We hash the canonical JSON form of the entire payload. Different temperature
    // values produce different keys (temperature > 0 -> None, skip cache).
    let prompt_payload = serde_json::to_string(&payload)
        .map_err(|e| AppError::internal(format!("failed to serialize payload: {}", e)))?;

    let cache_key = if !is_stream {
        ExactCache::compute_key(&model, &prompt_payload, temperature)
    } else {
        None
    };

    // ---- 4. Exact cache lookup --------------------------------------------
    if let Some(ref key) = cache_key {
        if let Some(cached) = state.exact_cache.get(key).await {
            // Refund the full TPM reservation — no upstream tokens consumed.
            if let Some(r) = reservation.take() {
                r.reconcile(0.0);
            }
            m::record_cache_hit("exact");
            info!(model = %model, "exact cache hit");
            m::record_request_latency(request_start.elapsed());
            m::record_request(&model, "cache_hit");
            let resp = axum::response::Response::builder()
                .status(axum::http::StatusCode::OK)
                .header("content-type", "application/json")
                .header("x-cache", "HIT")
                .body(axum::body::Body::from(cached))
                .map_err(|e| AppError::internal(e.to_string()))?;
            return Ok(resp);
        }
        m::record_cache_miss("exact");
    }

    // ---- 5. Provider selection --------------------------------------------
    let selector = ProviderSelector::new(state.clone());
    let candidates = selector.select_ordered();
    if candidates.is_empty() {
        m::record_error("all", "no_available_providers");
        // reservation is dropped here -> refunded via Drop impl
        return Err(AppError::upstream(
            "no available providers (all circuit breakers open)",
        ));
    }

    // Serialize the request body once — reused across provider failover attempts.
    let body_bytes: Bytes = serde_json::to_vec(&payload)
        .map_err(|e| AppError::internal(format!("failed to serialize payload: {}", e)))?
        .into();

    // ---- 6. Failover loop --------------------------------------------------
    let mut last_error: Option<String> = None;
    for candidate in candidates.iter() {
        let provider = &candidate.config;
        let breaker = &candidate.breaker;

        // P0 #1: Consume a probe slot right before actually sending the request.
        if !breaker.allow_request() {
            tracing::debug!(provider = %provider.name, "breaker denied request at send time, skipping");
            continue;
        }

        let upstream_start = Instant::now();

        let req =
            proxy_client::build_chat_request(&state.http_client, provider, body_bytes.clone());

        let upstream_response = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                warn!(provider = %provider.name, error = %e, "upstream request failed");
                breaker.record_failure();
                m::record_error(&provider.name, "connect_error");
                last_error = Some(format!("{}: connection error", provider.name));
                continue;
            }
        };

        let status = upstream_response.status();

        // Retryable: 5xx, 429, 408 — record failure, try next provider
        if is_retryable_upstream_status(status) {
            warn!(provider = %provider.name, status = %status.as_u16(), "retryable upstream error, failing over");
            breaker.record_failure();
            m::record_error(&provider.name, "retryable_error");
            last_error = Some(format!("{} returned {}", provider.name, status));
            continue;
        }

        // Non-retryable client error: 400, 401, 403, 422, etc — fail fast
        if is_non_retryable_client_error(status) {
            // Provider is healthy, the request was just bad. Don't trip breaker.
            breaker.record_success();
            m::record_error(&provider.name, "client_error");
            // reservation will be refunded via Drop since we return Err
            return Err(AppError::bad_request(format!(
                "upstream rejected request with {}",
                status
            )));
        }

        // At this point: success (2xx).
        breaker.record_success();
        m::record_request(&provider.name, "success");
        m::record_upstream_latency(&provider.name, upstream_start.elapsed());

        // ---- 7a. Streaming response: pipe bytes_stream through -------------
        if is_stream {
            let stream = upstream_response.bytes_stream();
            let provider_name_for_usage = provider.name.clone();
            let breaker_for_stream = candidate.breaker.clone();
            // Move reservation into the streaming callback so it gets reconciled
            // when the usage event arrives, or refunded on drop if the stream fails.
            let stream_reservation = reservation.take();
            let stream_idle_secs = provider.stream_idle_timeout_seconds;

            let response = proxy_stream_passthrough(
                stream,
                provider.name.clone(),
                stream_idle_secs,
                // on_usage: called exactly once when the trailing usage SSE event arrives
                move |prompt_tokens, completion_tokens| {
                    m::record_tokens(&provider_name_for_usage, "prompt", prompt_tokens);
                    m::record_tokens(&provider_name_for_usage, "completion", completion_tokens);
                    let total = prompt_tokens + completion_tokens;
                    // Explicitly reconcile the reservation with actual usage
                    if let Some(r) = stream_reservation {
                        r.reconcile(total as f64);
                    }
                },
                // on_error: called when the stream encounters a network error
                move || {
                    breaker_for_stream.record_failure();
                },
            );
            m::record_request_latency(request_start.elapsed());
            return Ok(response);
        }

        // ---- 7b. Non-streaming: read body, cache, count tokens ------------
        let resp_body_bytes = match upstream_response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                warn!(provider = %provider.name, error = %e, "failed to read upstream body");
                breaker.record_failure();
                m::record_error(&provider.name, "body_read_error");
                last_error = Some(format!("{}: body read error", provider.name));
                continue;
            }
        };

        let body_str = String::from_utf8_lossy(&resp_body_bytes).to_string();

        // Cache the response.
        if let Some(ref key) = cache_key {
            state
                .exact_cache
                .insert(key.clone(), body_str.clone())
                .await;
        }

        // Best-effort token usage accounting + TPM reconciliation.
        if let Ok(usage_val) = serde_json::from_str::<Value>(&body_str) {
            if let Some(usage) = usage_val.get("usage") {
                let prompt_tokens = usage
                    .get("prompt_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let completion_tokens = usage
                    .get("completion_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                m::record_tokens(&provider.name, "prompt", prompt_tokens);
                m::record_tokens(&provider.name, "completion", completion_tokens);

                let total = prompt_tokens + completion_tokens;
                if let Some(r) = reservation.take() {
                    r.reconcile(total as f64);
                }
            }
        }

        // If usage wasn't in the response, reservation will be refunded on drop.
        m::record_request_latency(request_start.elapsed());

        let response = axum::response::Response::builder()
            .status(axum::http::StatusCode::OK)
            .header("content-type", "application/json")
            .header("x-cache", "MISS")
            .header("x-provider", &provider.name)
            .body(axum::body::Body::from(body_str))
            .map_err(|e| AppError::internal(e.to_string()))?;
        return Ok(response);
    }

    // All providers failed. Reservation is refunded via Drop.
    m::record_error("all", "all_providers_failed");
    Err(AppError::upstream(
        last_error.unwrap_or_else(|| "all providers failed".into()),
    ))
}
