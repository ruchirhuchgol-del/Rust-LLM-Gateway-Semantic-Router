//! SSE streaming pipeline with idle timeout enforcement.
//!
//! ## Architecture
//!
//! ```text
//! Upstream (vLLM/OpenAI/etc.)
//!      │ HTTP chunks
//!      ▼
//! reqwest::bytes_stream()
//!      │
//!      ▼
//! tokio::time::timeout(idle_timeout, next_chunk)   ← enforced per-chunk
//!      │
//!      ▼
//! SseParser::push()  →  complete events
//!      │
//!      ├─ TTFT: first event with choices[].delta.content
//!      ├─ Usage: trailing event with usage{} → on_usage callback (exactly once)
//!      └─ Pass all bytes through unchanged to client
//!      │
//!      ▼
//! axum::Body::from_stream()
//! ```
//!
//! ## Stream idle timeout (P0 #4)
//!
//! Each network chunk read is wrapped in `tokio::time::timeout(idle_timeout, ...)`.
//! A long-running stream is valid as long as bytes keep arriving. But if no bytes
//! arrive within `stream_idle_timeout_seconds`, the stream is terminated and the
//! client receives an error. This prevents a stalled upstream from holding a
//! client connection indefinitely.
//!
//! ## Client cancellation
//!
//! When the client drops the connection, Axum drops the response body, which drops
//! the stream. The `reqwest::bytes_stream()` future is cancelled, which cancels
//! the upstream HTTP connection. This is inherent to Axum + tokio's cancellation
//! semantics — no special code is needed.
//!
//! ## Circuit breaker on stream failure
//!
//! Stream errors (network errors mid-stream or idle timeouts) are reported to the
//! circuit breaker via the `on_stream_error` callback. This ensures that a provider
//! that routinely drops streams mid-response will eventually be circuit-broken.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use tracing::{debug, warn};

use crate::proxy::sse_parser::SseParser;
use crate::telemetry::metrics as m;

/// Wrap an upstream `reqwest::Response::bytes_stream()` into an Axum `Response`
/// that streams SSE chunks back to the client without buffering.
///
/// ## Callbacks
///
/// * `on_usage(prompt_tokens, completion_tokens)` — called exactly once when the
///   trailing usage SSE event is received. Caller reconciles TPM reservation.
/// * `on_stream_error()` — called when the stream encounters a network error or
///   idle timeout. Caller records circuit breaker failure.
///
/// ## TTFT semantics
///
/// TTFT is recorded on the first SSE event containing `choices[].delta.content`
/// (a meaningful model-output token), NOT on the first TCP byte.
pub fn proxy_stream_passthrough<S, FUsage, FError>(
    upstream: S,
    provider_name: String,
    stream_idle_timeout_seconds: u64,
    on_usage: FUsage,
    on_stream_error: FError,
) -> Response
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
    FUsage: FnOnce(u64, u64) + Send + 'static,
    FError: FnOnce() + Send + 'static,
{
    let start = Instant::now();
    let ttft_recorded = Arc::new(AtomicBool::new(false));
    let usage_charged = Arc::new(AtomicBool::new(false));
    let provider_name = Arc::new(provider_name);
    let idle_timeout = Duration::from_secs(stream_idle_timeout_seconds.max(1));

    // These are FnOnce — wrap in Option+Arc<Mutex> so they can be moved into
    // the FnMut closure and called exactly once.
    let on_usage = Arc::new(std::sync::Mutex::new(Some(on_usage)));
    let on_error = Arc::new(std::sync::Mutex::new(Some(on_stream_error)));

    let mut parser = SseParser::new();

    // We wrap the stream in a custom async_stream that enforces idle timeout.
    let ttft_recorded_clone = ttft_recorded.clone();
    let usage_charged_clone = usage_charged.clone();
    let provider_clone = provider_name.clone();
    let on_usage_clone = on_usage.clone();
    let on_error_clone = on_error.clone();

    let body_stream = async_stream::stream! {
        tokio::pin!(upstream);

        loop {
            // Enforce stream idle timeout: if no chunk arrives within the timeout,
            // terminate the stream. A long-running stream is valid if bytes keep flowing.
            let chunk_result = match tokio::time::timeout(idle_timeout, upstream.next()).await {
                Ok(Some(result)) => result,
                Ok(None) => {
                    // Stream ended normally
                    break;
                }
                Err(_elapsed) => {
                    // Idle timeout — upstream stalled
                    warn!(
                        provider = %provider_clone,
                        timeout_seconds = stream_idle_timeout_seconds,
                        "stream idle timeout exceeded, terminating"
                    );
                    // Report to circuit breaker
                    if let Ok(mut guard) = on_error_clone.lock() {
                        if let Some(cb) = guard.take() {
                            cb();
                        }
                    }
                    yield Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "stream idle timeout exceeded",
                    ));
                    break;
                }
            };

            match chunk_result {
                Ok(bytes) => {
                    // Parse SSE events from the raw bytes
                    let events = parser.push(&bytes);
                    for event in &events {
                        // TTFT: record on first event with actual content delta
                        if !ttft_recorded_clone.load(Ordering::Relaxed)
                            && SseParser::is_content_event(event)
                            && !ttft_recorded_clone.swap(true, Ordering::Relaxed)
                        {
                            let ttft = start.elapsed();
                            let provider: &str = &provider_clone;
                            debug!(
                                provider = provider,
                                ttft_ms = ttft.as_secs_f64() * 1000.0,
                                "first content token"
                            );
                            m::record_ttft(provider, ttft);
                        }

                        // Usage: extract exactly once from trailing usage event
                        if !usage_charged_clone.load(Ordering::Relaxed) {
                            if let Some((prompt, completion)) = SseParser::extract_usage(event) {
                                if !usage_charged_clone.swap(true, Ordering::Relaxed) {
                                    if let Ok(mut guard) = on_usage_clone.lock() {
                                        if let Some(cb) = guard.take() {
                                            cb(prompt, completion);
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Always pass the raw bytes through unchanged to the client.
                    yield Ok::<_, std::io::Error>(bytes);
                }
                Err(e) => {
                    warn!(
                        provider = %provider_clone,
                        error = %e,
                        "stream error from upstream"
                    );
                    // Report to circuit breaker
                    if let Ok(mut guard) = on_error_clone.lock() {
                        if let Some(cb) = guard.take() {
                            cb();
                        }
                    }
                    yield Err(std::io::Error::other(e.to_string()));
                    break;
                }
            }
        }
    };

    let body = Body::from_stream(body_stream);
    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        "content-type",
        HeaderValue::from_static("text/event-stream"),
    );
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-cache"));
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    // NOTE: Connection: keep-alive is NOT set — it is invalid for HTTP/2
    // and unnecessary for HTTP/1.1 (where it is the default).
    response
}
