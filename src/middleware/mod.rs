//! Tower / Axum middleware layers and request-extension extractors.
//!
//! Order of layers (outer -> inner):
//!   1. `trace`    - injects request ID, starts latency timer, logs span.
//!   2. `auth`     - validates Bearer API key against configured allowlist.
//!   3. `rate_limit` - per-API-key token bucket; returns 429 with `Retry-After`.
//!
//! Layers are applied with `Router::layer(...)`. Axum executes them in reverse order
//! of insertion, so to achieve the order above we push them in reverse.

pub mod auth;
pub mod rate_limit;
pub mod trace;
