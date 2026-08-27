//! Provider selection with failover.
//!
//! [`ProviderSelector::select_ordered`] returns the list of providers to try for a
//! given request, in ascending `priority` order, skipping any whose circuit breaker
//! is currently OPEN (and has not yet elapsed its cooldown).
//!
//! ## P0 #1 fix
//!
//! This module now calls `breaker.is_eligible()` (pure read, no mutation) instead
//! of `breaker.allow_request()` (which consumed probe slots as a side effect).
//! The slot-consuming `allow_request()` is deferred to the actual request site
//! in `chat.rs` / `embeddings.rs`.

use std::sync::Arc;

use crate::config::ProviderConfig;
use crate::router::circuit_breaker::CircuitBreaker;
use crate::state::AppState;

/// A provider plus its (per-process) circuit breaker. The selector returns a
/// `Vec<SelectedProvider>` ordered by ascending `priority`.
#[derive(Clone)]
pub struct SelectedProvider {
    pub config: ProviderConfig,
    pub breaker: Arc<CircuitBreaker>,
}

pub struct ProviderSelector {
    state: AppState,
}

impl ProviderSelector {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// Returns providers in failover order (lowest `priority` first), skipping any
    /// whose breaker is OPEN and cooldown has not yet elapsed.
    ///
    /// Uses `is_eligible()` — a pure-read check — so that building this list does
    /// not consume HalfOpen probe slots (P0 #1 fix).
    pub fn select_ordered(&self) -> Vec<SelectedProvider> {
        self.state
            .sorted_providers
            .iter()
            .filter_map(|p| {
                let breaker = self.state.breaker_for(&p.name);
                if breaker.is_eligible() {
                    Some(SelectedProvider {
                        config: p.clone(),
                        breaker,
                    })
                } else {
                    tracing::warn!(provider = %p.name, "circuit breaker open, skipping");
                    None
                }
            })
            .collect()
    }
}
