//! Per-API-key token-bucket rate limiter.
//!
//! Implements FR-2.1..2.3 from the PRD:
//!   * Tiered limits per API key (RPM + TPM buckets).
//!   * Non-blocking token bucket using fine-grained `std::sync::Mutex` - the critical
//!     section is O(1) arithmetic and is never held across an await boundary, so
//!     std (sync) mutex is preferred over `tokio::sync::Mutex` for lower contention.
//!   * Returns HTTP 429 + `Retry-After` header when limits are exceeded (see `AppError`).
//!
//! ## Algorithm
//!
//! Tokens are replenished continuously based on wall-clock elapsed time:
//! ```text
//! new_tokens = min(capacity, current_tokens + elapsed_seconds * refill_rate_per_sec)
//! ```
//! A request consumes 1 RPM token. Token counts (TPM) are deducted after the upstream
//! response is received - this allows a soft-cap on TPM without pre-counting.

use std::sync::Mutex;
use std::time::Instant;

/// One token bucket. Stores tokens as `f64` for sub-second refill granularity.
/// Capacity and refill rate are fixed at construction time.
pub struct TokenBucket {
    capacity: f64,
    refill_per_sec: f64,
    inner: Mutex<BucketState>,
}

struct BucketState {
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    /// Build a new bucket pre-filled with `capacity` tokens.
    /// `refill_per_sec` is the steady-state refill rate.
    pub fn new(capacity: f64, refill_per_sec: f64) -> Self {
        Self {
            capacity: capacity.max(0.0),
            refill_per_sec: refill_per_sec.max(0.0),
            inner: Mutex::new(BucketState {
                tokens: capacity,
                last_refill: Instant::now(),
            }),
        }
    }

    /// Refill the bucket based on elapsed wall-clock time, returning current token count.
    fn refill_locked(state: &mut BucketState, capacity: f64, refill_per_sec: f64) {
        let now = Instant::now();
        let elapsed = now.duration_since(state.last_refill).as_secs_f64();
        // Clamp elapsed to a sensible upper bound (e.g., a long-paused process).
        let elapsed = elapsed.min(3600.0);
        if elapsed > 0.0 {
            state.tokens = (state.tokens + elapsed * refill_per_sec).min(capacity);
            state.last_refill = now;
        }
    }

    /// Try to consume `cost` tokens. Returns `Ok(())` if there was enough capacity, or
    /// `Err(retry_after_seconds)` describing how long the client should wait before retry.
    pub fn try_consume(&self, cost: f64) -> Result<(), u64> {
        let mut state = self.inner.lock().expect("bucket mutex poisoned");
        Self::refill_locked(&mut state, self.capacity, self.refill_per_sec);

        if state.tokens >= cost {
            state.tokens -= cost;
            Ok(())
        } else {
            let deficit = cost - state.tokens;
            let retry_after = if self.refill_per_sec > 0.0 {
                (deficit / self.refill_per_sec).ceil() as u64
            } else {
                u64::MAX
            };
            Err(retry_after.max(1))
        }
    }

    /// Refund tokens (e.g., when an upstream call fails before any tokens were consumed
    /// on the LLM side). Always clamps to capacity.
    pub fn refund(&self, n: f64) {
        let mut state = self.inner.lock().expect("bucket mutex poisoned");
        Self::refill_locked(&mut state, self.capacity, self.refill_per_sec);
        state.tokens = (state.tokens + n).min(self.capacity);
    }

    /// Inspect current token count without consuming. Useful for metrics/debugging.
    pub fn available_tokens(&self) -> f64 {
        let mut state = self.inner.lock().expect("bucket mutex poisoned");
        Self::refill_locked(&mut state, self.capacity, self.refill_per_sec);
        state.tokens
    }
}

/// Holds the per-API-key rate limit buckets. Stored in `AppState::rate_limiter` DashMap.
#[derive(Clone)]
pub struct RateLimitBuckets {
    pub rpm: std::sync::Arc<TokenBucket>,
    pub tpm: std::sync::Arc<TokenBucket>,
}

impl RateLimitBuckets {
    /// Construct fresh RPM and TPM buckets from `RateLimitConfig`.
    pub fn from_config(rpm: u32, tpm: u32) -> Self {
        Self {
            rpm: std::sync::Arc::new(TokenBucket::new(rpm as f64, rpm as f64 / 60.0)),
            tpm: std::sync::Arc::new(TokenBucket::new(tpm as f64, tpm as f64 / 60.0)),
        }
    }

    /// Try to reserve TPM tokens before dispatch.
    pub fn try_reserve_tpm(&self, estimated_cost: f64) -> Result<ReservationHandle, u64> {
        self.tpm.try_consume(estimated_cost)?;
        Ok(ReservationHandle {
            bucket: self.tpm.clone(),
            reserved: estimated_cost,
            reconciled: false,
        })
    }
}

/// A handle representing an active token reservation.
/// If dropped before being explicitly reconciled, it refunds the reserved amount
/// back to the bucket (useful for error paths that abort before using the LLM).
pub struct ReservationHandle {
    bucket: std::sync::Arc<TokenBucket>,
    reserved: f64,
    reconciled: bool,
}

impl ReservationHandle {
    /// Completes the request, charging the actual cost. Refunds any over-reservation
    /// or tries to consume any under-reservation.
    pub fn reconcile(mut self, actual_cost: f64) {
        self.reconciled = true;
        if actual_cost < self.reserved {
            self.bucket.refund(self.reserved - actual_cost);
        } else if actual_cost > self.reserved {
            // Best effort consumption of the deficit. If this fails, they will just
            // hit the rate limit on the next request.
            let _ = self.bucket.try_consume(actual_cost - self.reserved);
        }
    }
}

impl Drop for ReservationHandle {
    fn drop(&mut self) {
        if !self.reconciled {
            self.bucket.refund(self.reserved);
        }
    }
}

// ---- Axum middleware ---------------------------------------------------------

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::error::AppError;
use crate::state::{AppState, ClientIdentity};
use crate::telemetry::metrics as m;

/// Per-API-key rate-limit middleware.
///
/// Pulls the validated API key (set by `auth_middleware`) out of request extensions,
/// looks up (or lazily creates) its RPM/TPM bucket pair, and tries to charge 1 RPM token.
/// On exhaustion returns `AppError::RateLimited` with a `Retry-After` hint.
pub async fn rate_limit_middleware(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, AppError> {
    // Read ClientIdentity from extensions (set by auth_middleware). If absent, fail closed.
    let identity = request
        .extensions()
        .get::<ClientIdentity>()
        .ok_or(AppError::Unauthorized)?;

    let cfg = &state.config.rate_limit;
    // entry().or_insert_with(...) holds a write lock on the DashMap shard until the
    // returned RefMut is dropped. We clone the Arc<TokenBucket> out so the lock is
    // released before we do the bucket operation - minimizes contention.
    let buckets: RateLimitBuckets = state
        .rate_limiter
        .entry(identity.client_id.clone())
        .or_insert_with(|| RateLimitBuckets::from_config(cfg.default_rpm, cfg.default_tpm))
        .clone();

    match buckets.rpm.try_consume(1.0) {
        Ok(()) => Ok(next.run(request).await),
        Err(retry_after) => {
            m::record_error("rate_limit", "rpm_exceeded");
            Err(AppError::RateLimited { retry_after })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn consumes_until_empty() {
        let bucket = TokenBucket::new(3.0, 0.0); // no refill - deterministic
        assert!(bucket.try_consume(1.0).is_ok());
        assert!(bucket.try_consume(1.0).is_ok());
        assert!(bucket.try_consume(1.0).is_ok());
        let err = bucket.try_consume(1.0).unwrap_err();
        assert!(err >= 1);
    }

    #[test]
    fn refills_over_time() {
        // Start with full capacity (10 tokens), drain them, then verify refill.
        let bucket = TokenBucket::new(10.0, 100.0); // cap=10, refill=100/sec
        for _ in 0..10 {
            assert!(bucket.try_consume(1.0).is_ok());
        }
        assert!(
            bucket.available_tokens() < 0.5,
            "expected ~0 tokens after draining, got {}",
            bucket.available_tokens()
        );
        thread::sleep(Duration::from_millis(50)); // ~5 tokens should refill
        let available = bucket.available_tokens();
        assert!(
            (4.0..=6.0).contains(&available),
            "expected ~5 tokens after 50ms refill, got {}",
            available
        );
    }

    #[test]
    fn refund_clamps_to_capacity() {
        let bucket = TokenBucket::new(5.0, 0.0);
        bucket.refund(100.0);
        assert!((bucket.available_tokens() - 5.0).abs() < 1e-9);
    }

    #[test]
    fn retry_after_is_at_least_1_second() {
        let bucket = TokenBucket::new(0.0, 1.0);
        let err = bucket.try_consume(1.0).unwrap_err();
        assert_eq!(err, 1);
    }
}
