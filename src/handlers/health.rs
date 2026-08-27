//! `/healthz` liveness probe.
//!
//! Returns 200 OK if the process is alive and the request router can accept new
//! requests. Does NOT probe upstream providers (that's a readiness concern, handled
//! separately if needed).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde_json::json;

use crate::state::AppState;

pub async fn healthz(State(state): State<AppState>) -> impl IntoResponse {
    let body = json!({
        "status": "ok",
        "providers_configured": state.config.providers.len(),
        "circuit_breakers_tracked": state.circuit_breakers.len(),
    });
    (StatusCode::OK, Json(body))
}
