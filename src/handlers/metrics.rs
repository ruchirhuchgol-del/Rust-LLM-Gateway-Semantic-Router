//! `/metrics` endpoint - exposes the Prometheus text-format snapshot.
//!
//! The recorder is installed once at startup (see `telemetry::metrics::init`).
//! This handler simply renders the current snapshot synchronously - it's O(1) and
//! safe to scrape at 15-second intervals.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;

use crate::state::AppState;

pub async fn prometheus_metrics(State(state): State<AppState>) -> impl IntoResponse {
    let body = state.metrics_handle.render();
    let mut headers = HeaderMap::new();
    headers.insert(
        "content-type",
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    (StatusCode::OK, headers, body)
}
