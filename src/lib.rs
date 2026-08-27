//! Rust LLM Gateway — library crate.
//!
//! Provides the core gateway functionality: Axum router with middleware stack
//! (tracing → auth → rate limiting), route handlers for chat/embeddings/health/metrics,
//! provider failover with circuit breaking, exact-match caching, and Prometheus metrics.
//!
//! The binary crate (`main.rs`) calls [`run`] to start the server.

#![forbid(unsafe_code)]

pub mod cache;
pub mod config;
pub mod error;
pub mod handlers;
pub mod middleware;
pub mod proxy;
pub mod router;
pub mod state;
pub mod telemetry;
pub mod types;

use axum::middleware as axum_mw;
use axum::Router;
use tower_http::limit::RequestBodyLimitLayer;

use crate::config::AppConfig;
use crate::state::AppState;
use crate::telemetry::metrics as tel_metrics;

/// Build the full Axum router with all routes and middleware.
///
/// Middleware order (outer → inner):
///   1. `RequestBodyLimitLayer` — rejects oversized bodies (P0 #3)
///   2. `trace_middleware`      — request ID, latency span
///   3. `auth_middleware`       — bearer token validation (API routes only)
///   4. `rate_limit_middleware` — per-key RPM bucket (API routes only)
fn build_router(state: AppState) -> Router {
    // Routes that require auth + rate limiting.
    let api_routes = Router::new()
        .route(
            "/v1/chat/completions",
            axum::routing::post(handlers::chat::chat_completions),
        )
        .route(
            "/v1/embeddings",
            axum::routing::post(handlers::embeddings::embeddings),
        )
        // Innermost middleware first: rate_limit, then auth wrapping it.
        .layer(axum_mw::from_fn_with_state(
            state.clone(),
            middleware::rate_limit::rate_limit_middleware,
        ))
        .layer(axum_mw::from_fn_with_state(
            state.clone(),
            middleware::auth::auth_middleware,
        ));

    // Public routes — no auth or rate limiting.
    let public_routes = Router::new()
        .route("/healthz", axum::routing::get(handlers::health::healthz))
        .route(
            "/metrics",
            axum::routing::get(handlers::metrics::prometheus_metrics),
        );

    Router::new()
        .merge(api_routes)
        .merge(public_routes)
        // trace wraps all routes (outer to auth/rate_limit).
        .layer(axum_mw::from_fn(middleware::trace::trace_middleware))
        // P0 #3: Enforce max_body_bytes. Outermost layer — rejects oversized
        // bodies before any middleware or handler reads them.
        .layer(RequestBodyLimitLayer::new(
            state.config.server.max_body_bytes,
        ))
        .with_state(state)
}

/// Build a router suitable for integration testing (no TCP binding).
pub fn build_test_router(state: AppState) -> Router {
    build_router(state)
}

/// Run the gateway: bind to the configured host:port, serve requests,
/// and drain gracefully on SIGINT / SIGTERM (P0 #4).
pub async fn run(config: AppConfig) -> Result<(), Box<dyn std::error::Error>> {
    let metrics_handle = tel_metrics::init();
    let state = AppState::new(config.clone(), metrics_handle).await;

    let app = build_router(state);

    let addr = format!("{}:{}", config.server.host, config.server.port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(addr = %addr, "gateway listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("gateway shut down gracefully");
    Ok(())
}

/// Wait for a shutdown signal (Ctrl+C on all platforms, SIGTERM on Unix).
/// Used by `axum::serve(...).with_graceful_shutdown(...)` so in-flight requests
/// (including active SSE streams) drain instead of getting hard-killed.
async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("received Ctrl+C, initiating graceful shutdown");
        },
        _ = terminate => {
            tracing::info!("received SIGTERM, initiating graceful shutdown");
        },
    }
}
