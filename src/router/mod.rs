//! Provider routing & failover logic.
//!
//! - [`circuit_breaker::CircuitBreaker`] - per-provider state machine (Closed / Open / Half-Open)
//!   that prevents the gateway from hammering a degraded upstream.
//! - [`selector::ProviderSelector`] - returns the ordered list of providers to try for a given
//!   request, skipping any whose breaker is currently `Open`.

pub mod circuit_breaker;
pub mod selector;
