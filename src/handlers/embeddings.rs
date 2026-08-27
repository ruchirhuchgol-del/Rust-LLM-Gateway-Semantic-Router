//! `/v1/embeddings` handler.
//!
//! Pass-through to the first available provider with circuit-breaker-aware failover.
//! Loops over every candidate in priority order, identical to `chat_completions`.
//! No caching — embedding outputs are non-deterministic across providers.
//!
//! ## TPM admission
//!
//! Token cost is estimated from the input text length (chars / 4 heuristic).
//! A TPM reservation is acquired before dispatch and reconciled with actual
//! usage from the response body.

use std::time::Instant;

use axum::extract::rejection::JsonRejection;
use axum::extract::State;
use axum::response::Response;
use axum::Json;
use bytes::Bytes;
use tracing::warn;

use crate::types::{EmbeddingInput, EmbeddingRequest};

use crate::error::AppError;
use crate::middleware::rate_limit::RateLimitBuckets;
use crate::proxy::client as proxy_client;
use crate::router::selector::ProviderSelector;
use crate::state::{AppState, ClientIdentity};
use crate::telemetry::metrics as m;

/// Look up (or lazily create) rate-limit buckets for a client.
fn get_rate_limit_buckets(state: &AppState, client_id: &str) -> RateLimitBuckets {
    let cfg = &state.config.rate_limit;
    state
        .rate_limiter
        .entry(client_id.to_string())
        .or_insert_with(|| RateLimitBuckets::from_config(cfg.default_rpm, cfg.default_tpm))
        .clone()
}

/// Estimate token count from embedding input using chars/4 heuristic.
fn estimate_embedding_tokens(input: &EmbeddingInput) -> f64 {
    let chars = match input {
        EmbeddingInput::String(s) => s.len(),
        EmbeddingInput::StringArray(arr) => arr.iter().map(|s| s.len()).sum(),
        EmbeddingInput::TokenArray(arr) => arr.len() * 4, // tokens already, estimate chars
        EmbeddingInput::TokenMatrix(mat) => mat.iter().map(|row| row.len() * 4).sum(),
    };
    // Rough heuristic: 4 chars ≈ 1 token. Minimum 1 token.
    ((chars as f64) / 4.0).max(1.0)
}

/// Retryable upstream status for embeddings (same policy as chat).
fn is_retryable_upstream_status(status: reqwest::StatusCode) -> bool {
    status.is_server_error()
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status == reqwest::StatusCode::REQUEST_TIMEOUT
}

fn is_non_retryable_client_error(status: reqwest::StatusCode) -> bool {
    status.is_client_error()
        && status != reqwest::StatusCode::TOO_MANY_REQUESTS
        && status != reqwest::StatusCode::REQUEST_TIMEOUT
}

pub async fn embeddings(
    State(state): State<AppState>,
    identity: ClientIdentity,
    payload: Result<Json<EmbeddingRequest>, JsonRejection>,
) -> Result<Response, AppError> {
    let Json(payload) = payload.map_err(AppError::from)?;
    let request_start = Instant::now();

    // ---- 1. TPM admission control -----------------------------------------
    let estimated_tokens = estimate_embedding_tokens(&payload.input);
    let buckets = get_rate_limit_buckets(&state, &identity.client_id);
    let mut reservation = match buckets.try_reserve_tpm(estimated_tokens) {
        Ok(handle) => Some(handle),
        Err(retry_after) => {
            m::record_error("rate_limit", "tpm_exceeded");
            return Err(AppError::RateLimited { retry_after });
        }
    };

    // ---- 2. Serialize request body ----------------------------------------
    let body_bytes: Bytes = serde_json::to_vec(&payload)
        .map_err(|e| AppError::internal(format!("failed to serialize payload: {}", e)))?
        .into();

    // ---- 3. Provider selection --------------------------------------------
    let selector = ProviderSelector::new(state.clone());
    let candidates = selector.select_ordered();
    if candidates.is_empty() {
        m::record_error("all", "no_available_providers");
        return Err(AppError::upstream("no available providers"));
    }

    // ---- 4. Failover loop -------------------------------------------------
    let mut last_error: Option<String> = None;
    for candidate in candidates.iter() {
        let provider = &candidate.config;
        let breaker = &candidate.breaker;

        if !breaker.allow_request() {
            tracing::debug!(provider = %provider.name, "breaker denied request at send time, skipping");
            continue;
        }

        let upstream_start = Instant::now();

        let req = proxy_client::build_embeddings_request(
            &state.http_client,
            provider,
            body_bytes.clone(),
        );

        match req.send().await {
            Ok(response) => {
                let status = response.status();

                // Retryable errors: failover
                if is_retryable_upstream_status(status) {
                    warn!(provider = %provider.name, status = %status.as_u16(), "retryable embeddings upstream error");
                    breaker.record_failure();
                    m::record_error(&provider.name, "retryable_error");
                    last_error = Some(format!("{} returned {}", provider.name, status));
                    continue;
                }

                // Non-retryable client errors: fail fast
                if is_non_retryable_client_error(status) {
                    breaker.record_success(); // provider is healthy
                    m::record_error(&provider.name, "client_error");
                    return Err(AppError::bad_request(format!(
                        "upstream rejected request with {}",
                        status
                    )));
                }

                // Success path
                if !status.is_success() {
                    // Unexpected status (1xx, 3xx) — treat as provider failure
                    breaker.record_failure();
                    m::record_error(&provider.name, "unexpected_status");
                    last_error = Some(format!("{} returned {}", provider.name, status));
                    continue;
                }

                breaker.record_success();
                m::record_request(&provider.name, "success");
                m::record_upstream_latency(&provider.name, upstream_start.elapsed());

                let body = response.bytes().await.map_err(|_e| {
                    breaker.record_failure();
                    m::record_error(&provider.name, "body_read_error");
                    AppError::upstream(format!("{}: body read error", provider.name))
                })?;

                // Best-effort token usage accounting + TPM reconciliation
                if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&body) {
                    if let Some(usage) = val.get("usage") {
                        let total = usage
                            .get("total_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        m::record_tokens(&provider.name, "embedding", total);
                        if let Some(r) = reservation.take() {
                            r.reconcile(total as f64);
                        }
                    }
                }

                m::record_request_latency(request_start.elapsed());

                let resp = axum::response::Response::builder()
                    .status(axum::http::StatusCode::OK)
                    .header("content-type", "application/json")
                    .header("x-provider", &provider.name)
                    .body(axum::body::Body::from(body))
                    .map_err(|e| AppError::internal(e.to_string()))?;
                return Ok(resp);
            }
            Err(e) => {
                warn!(provider = %provider.name, error = %e, "embeddings upstream request failed");
                breaker.record_failure();
                m::record_error(&provider.name, "connect_error");
                last_error = Some(format!("{}: connection error", provider.name));
            }
        }
    }

    // All providers failed. Reservation refunded via Drop.
    m::record_error("all", "all_providers_failed");
    Err(AppError::upstream(
        last_error.unwrap_or_else(|| "all providers failed".into()),
    ))
}
