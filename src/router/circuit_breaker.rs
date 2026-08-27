//! Circuit breaker state machine - one instance per upstream provider.
//!
//! Implements the classic three-state breaker described in the architecture spec:
//!
//! ```text
//!            Successes > Threshold
//!        ┌────────────────────────────┐
//!        ▼                            │
//!  ┌──────────┐  Errors >= Limit  ┌──────────┐
//!  │  CLOSED  │ ────────────────> │   OPEN   │ ── (Fail fast, skip provider)
//!  └──────────┘                   └──────────┘
//!        ▲                              │
//!        │ Probe Success                │ Cooldown Elapsed
//!        │                              ▼
//!        └─────────────────────── ┌───────────┐
//!                                 │ HALF-OPEN │ ── (Send limited probe)
//!                                 └───────────┘
//! ```
//!
//! * **Closed** - normal traffic flow. Consecutive failures increment a counter;
//!   reaching `failure_threshold` trips the breaker OPEN.
//! * **Open** - requests bypass this provider immediately and fall back to the next.
//!   After `cooldown` elapses, transition to HALF-OPEN.
//! * **Half-Open** - a single (or `half_open_max_calls`) probe request is allowed.
//!   On success, state transitions to CLOSED. On failure, returns to OPEN.
//!
//! ## Concurrency notes
//!
//! We use a single `std::sync::Mutex<BreakerInner>` because every operation is O(1)
//! and we never hold the lock across an await. This keeps contention extremely low
//! and matches the spec's "fine-grained atomic primitives" requirement.
//!
//! ## P0 #1 fix: probe-budget leak
//!
//! `is_eligible()` provides a **pure-read** check used by `ProviderSelector::select_ordered()`
//! to build the candidate list without consuming probe slots. `allow_request()` is the
//! **slot-consuming** version called right before actually issuing a request.

use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

/// All mutable breaker state under a single lock (P2 #10).
struct BreakerInner {
    state: BreakerState,
    failure_count: u32,
    last_failure: Option<Instant>,
    half_open_calls: u32,
}

pub struct CircuitBreaker {
    inner: Mutex<BreakerInner>,
    failure_threshold: u32,
    cooldown: Duration,
    half_open_max_calls: u32,
}

impl CircuitBreaker {
    pub fn new(failure_threshold: u32, cooldown: Duration, half_open_max_calls: u32) -> Self {
        Self {
            inner: Mutex::new(BreakerInner {
                state: BreakerState::Closed,
                failure_count: 0,
                last_failure: None,
                half_open_calls: 0,
            }),
            failure_threshold,
            cooldown,
            half_open_max_calls: half_open_max_calls.max(1),
        }
    }

    /// **Pure-read eligibility check** — does NOT consume a probe slot or mutate state.
    ///
    /// Used by `ProviderSelector::select_ordered()` to build the candidate list
    /// without side effects. This fixes P0 #1: previously `allow_request()` was
    /// called during list-building, which consumed probe slots for providers that
    /// were never actually contacted because an earlier provider succeeded.
    pub fn is_eligible(&self) -> bool {
        let inner = self.inner.lock().expect("breaker mutex poisoned");
        match inner.state {
            BreakerState::Closed => true,
            BreakerState::Open => {
                if let Some(t) = inner.last_failure {
                    t.elapsed() >= self.cooldown
                } else {
                    false
                }
            }
            BreakerState::HalfOpen => inner.half_open_calls < self.half_open_max_calls,
        }
    }

    /// Returns `true` if the caller is permitted to send a request through this provider.
    /// **This mutates state**: transitions OPEN → HALF-OPEN and consumes probe slots.
    ///
    /// Call this right before actually issuing a request, NOT during candidate
    /// list-building (use `is_eligible()` for that).
    pub fn allow_request(&self) -> bool {
        let mut inner = self.inner.lock().expect("breaker mutex poisoned");
        match inner.state {
            BreakerState::Closed => true,
            BreakerState::Open => {
                if let Some(t) = inner.last_failure {
                    if t.elapsed() >= self.cooldown {
                        inner.state = BreakerState::HalfOpen;
                        inner.half_open_calls = 0;
                        return true;
                    }
                }
                false
            }
            BreakerState::HalfOpen => {
                if inner.half_open_calls < self.half_open_max_calls {
                    inner.half_open_calls += 1;
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Record a successful request. Resets the failure counter and, if HALF-OPEN,
    /// transitions back to CLOSED.
    pub fn record_success(&self) {
        let mut inner = self.inner.lock().expect("breaker mutex poisoned");
        inner.failure_count = 0;
        if inner.state == BreakerState::HalfOpen {
            inner.state = BreakerState::Closed;
            inner.half_open_calls = 0;
        }
    }

    /// Record a failed request (5xx, 429, timeout, connection error).
    /// Increments the failure counter; trips OPEN when threshold reached or when
    /// already HALF-OPEN.
    pub fn record_failure(&self) {
        let mut inner = self.inner.lock().expect("breaker mutex poisoned");
        inner.last_failure = Some(Instant::now());
        inner.failure_count += 1;

        if inner.state == BreakerState::HalfOpen {
            // Probe failed - go back to OPEN immediately.
            inner.state = BreakerState::Open;
            inner.half_open_calls = 0;
            return;
        }

        if inner.failure_count >= self.failure_threshold {
            inner.state = BreakerState::Open;
        }
    }

    /// Inspect the breaker's current state without side effects.
    pub fn current_state(&self) -> BreakerState {
        self.inner.lock().expect("breaker mutex poisoned").state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_breaker() -> CircuitBreaker {
        CircuitBreaker::new(3, Duration::from_millis(50), 1)
    }

    #[test]
    fn closed_state_allows_all_requests() {
        let b = fresh_breaker();
        for _ in 0..5 {
            assert!(b.allow_request());
        }
    }

    #[test]
    fn trips_open_after_threshold_failures() {
        let b = fresh_breaker();
        for _ in 0..3 {
            b.record_failure();
        }
        assert_eq!(b.current_state(), BreakerState::Open);
        assert!(!b.allow_request());
    }

    #[test]
    fn success_resets_failure_count() {
        let b = fresh_breaker();
        b.record_failure();
        b.record_failure();
        b.record_success();
        // 2 failures before, 1 success resets, 1 more failure shouldn't trip
        b.record_failure();
        assert_eq!(b.current_state(), BreakerState::Closed);
    }

    #[test]
    fn half_open_after_cooldown_then_closes_on_success() {
        let b = fresh_breaker();
        for _ in 0..3 {
            b.record_failure();
        }
        assert_eq!(b.current_state(), BreakerState::Open);

        std::thread::sleep(Duration::from_millis(60));
        assert!(b.allow_request()); // transitions to HalfOpen
        assert_eq!(b.current_state(), BreakerState::HalfOpen);

        b.record_success();
        assert_eq!(b.current_state(), BreakerState::Closed);
    }

    #[test]
    fn half_open_reopens_on_failure() {
        let b = fresh_breaker();
        for _ in 0..3 {
            b.record_failure();
        }
        std::thread::sleep(Duration::from_millis(60));
        assert!(b.allow_request()); // -> HalfOpen
        b.record_failure();
        assert_eq!(b.current_state(), BreakerState::Open);
    }

    /// Regression test for P0 #1: `select_ordered()` was calling `allow_request()`
    /// on all providers just to build the candidate list, which consumed probe
    /// slots for HalfOpen breakers even when the provider was never actually
    /// contacted. With `half_open_max_calls = 1`, this permanently blocked recovery.
    ///
    /// The fix separates `is_eligible()` (pure read, no side effects) from
    /// `allow_request()` (slot-consuming, called only before actual requests).
    #[test]
    fn probe_budget_not_consumed_by_eligibility_check() {
        let b = CircuitBreaker::new(3, Duration::from_millis(50), 1);

        // Trip the breaker
        for _ in 0..3 {
            b.record_failure();
        }
        assert_eq!(b.current_state(), BreakerState::Open);

        // Wait for cooldown
        std::thread::sleep(Duration::from_millis(60));

        // Multiple is_eligible() calls must NOT consume the probe slot.
        // Before the fix, these would have been allow_request() calls that
        // burned the single probe slot.
        assert!(b.is_eligible());
        assert!(b.is_eligible());
        assert!(b.is_eligible());

        // The breaker must still allow a real request (probe slot intact).
        assert!(b.allow_request());
        assert_eq!(b.current_state(), BreakerState::HalfOpen);
    }

    #[test]
    fn is_eligible_reflects_current_state_without_mutation() {
        let b = fresh_breaker();

        // Closed: always eligible
        assert!(b.is_eligible());

        // Trip open
        for _ in 0..3 {
            b.record_failure();
        }
        // Open, cooldown not elapsed: not eligible
        assert!(!b.is_eligible());

        // Wait for cooldown
        std::thread::sleep(Duration::from_millis(60));
        // Open, cooldown elapsed: eligible (but state is still Open)
        assert!(b.is_eligible());
        assert_eq!(b.current_state(), BreakerState::Open); // no mutation!
    }
}
