//! Axum route handlers.
//!
//! Mounted in `lib.rs::build_router`:
//!   * `POST /v1/chat/completions` - OpenAI-compatible chat (stream + non-stream)
//!   * `POST /v1/embeddings`       - OpenAI-compatible embeddings
//!   * `GET  /healthz`              - liveness probe
//!   * `GET  /metrics`              - Prometheus scrape endpoint

pub mod chat;
pub mod embeddings;
pub mod health;
pub mod metrics;
