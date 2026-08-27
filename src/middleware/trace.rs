//! Request tracing middleware.
//!
//! Generates a `x-request-id` (UUID v4) for every incoming request, attaches it to
//! both the `tracing` span and the outgoing response header, and records end-to-end
//! request latency into the Prometheus histogram.

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use std::time::Instant;
use uuid::Uuid;

/// Request-extension carrying the correlation ID assigned to this request.
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

/// Wall-clock start of the current request - used by handlers and middleware
/// to compute end-to-end latency without re-reading `Instant::now()`.
#[derive(Clone, Copy, Debug)]
pub struct RequestStart(pub Instant);

pub async fn trace_middleware(req: Request, next: Next) -> Response {
    let request_id = Uuid::new_v4().to_string();
    let start = Instant::now();

    let method = req.method().clone();
    let uri = req.uri().clone();

    let span = tracing::info_span!(
        "request",
        request_id = %request_id,
        method = %method,
        path = %uri.path(),
    );
    let _enter = span.enter();

    // Stash the request id + start time for downstream handlers.
    let mut req = req;
    req.extensions_mut().insert(RequestId(request_id.clone()));
    req.extensions_mut().insert(RequestStart(start));

    let mut response = next.run(req).await;

    let elapsed = start.elapsed();
    let elapsed_ms = elapsed.as_secs_f64() * 1000.0;

    if let Ok(val) = request_id.parse() {
        response.headers_mut().insert("x-request-id", val);
    }
    if let Ok(val) = format!("{:.2}", elapsed_ms).parse() {
        response.headers_mut().insert("x-response-time-ms", val);
    }

    tracing::info!(
        status = response.status().as_u16(),
        elapsed_ms = elapsed_ms,
        "request completed"
    );

    crate::telemetry::metrics::record_request_latency(elapsed);
    response
}
