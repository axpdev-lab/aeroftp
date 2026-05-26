//! KE-A3: Transactions-per-second cap (rclone `--tpslimit`).
//!
//! Token-bucket rate limiter applied to every HTTP request that goes
//! through [`super::http_retry::send_with_retry`]. The bucket holds at
//! most `burst` tokens; each acquired permit costs one token; the
//! bucket refills at `tps` tokens per second up to the burst cap.
//! When the bucket is empty `acquire()` sleeps for the time needed to
//! generate one token, then proceeds.
//!
//! The limiter lives behind a [`std::sync::OnceLock`] so the CLI can
//! install it once at startup and every provider uses it implicitly.
//! When the CLI omits `--tpslimit` the lock stays empty and
//! [`maybe_acquire`] is a no-op: zero overhead on the default path.
//!
//! Layering above AIMD: AIMD is the reactive throttle (halves window on
//! 429/503 with Retry-After hints). `--tpslimit` is the proactive cap
//! (never issue more than N req/s). The two compose: AIMD may shrink
//! the effective parallelism well below the tpslimit cap, but never
//! above it. This is the rclone semantics.

use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Token-bucket state, hidden behind [`TpsLimiter::state`].
#[derive(Debug)]
struct TpsState {
    tokens: f64,
    last_refill: Instant,
}

/// KE-A3: A per-process transactions-per-second limiter.
#[derive(Debug)]
pub struct TpsLimiter {
    tps: f64,
    burst: f64,
    state: Mutex<TpsState>,
}

impl TpsLimiter {
    /// Build a limiter for `tps` transactions/sec with the given `burst`
    /// capacity. `burst` is clamped to at least 1 token to avoid an
    /// always-stalled bucket; `tps` is clamped to at least
    /// 0.001 (1 token per 1000 seconds) for the same reason.
    pub fn new(tps: f64, burst: f64) -> Self {
        let tps = tps.max(0.001);
        let burst = burst.max(1.0);
        Self {
            tps,
            burst,
            state: Mutex::new(TpsState {
                tokens: burst,
                last_refill: Instant::now(),
            }),
        }
    }

    /// Pure-fn helper: compute the new token count given the elapsed
    /// time since the last refill. Hoisted out of [`acquire`] so the
    /// math is unit-testable without touching `Instant` / `tokio::time`.
    /// Returns `tokens` clamped to the closed interval `[0.0, burst]`.
    fn refill_tokens(prev: f64, tps: f64, burst: f64, elapsed: Duration) -> f64 {
        let added = elapsed.as_secs_f64() * tps;
        (prev + added).min(burst).max(0.0)
    }

    /// Acquire one permit. Sleeps when the bucket is empty.
    ///
    /// The implementation is cooperative: the mutex is released across
    /// the sleep boundary so other tasks can re-enter and consume their
    /// own permits while the caller waits. Without that release, two
    /// concurrent callers would serialise on the mutex even though the
    /// limiter is supposed to gate by tokens, not by lock turn.
    pub async fn acquire(&self) {
        loop {
            let wait = {
                let mut state = self.state.lock().await;
                let now = Instant::now();
                let elapsed = now.duration_since(state.last_refill);
                state.tokens = Self::refill_tokens(state.tokens, self.tps, self.burst, elapsed);
                state.last_refill = now;
                if state.tokens >= 1.0 {
                    state.tokens -= 1.0;
                    return;
                }
                // Time to wait until we cross the 1-token threshold.
                let deficit = 1.0 - state.tokens;
                let secs = deficit / self.tps;
                Duration::from_secs_f64(secs.max(0.001))
            };
            tokio::time::sleep(wait).await;
        }
    }
}

/// Global limiter handle; populated by [`init`] from the CLI dispatcher.
static GLOBAL: OnceLock<TpsLimiter> = OnceLock::new();

/// Install a global limiter. Idempotent: the first caller wins; later
/// calls are silently ignored. The CLI dispatcher calls this once,
/// before the command handler runs.
pub fn init(tps: f64, burst: f64) {
    let _ = GLOBAL.set(TpsLimiter::new(tps, burst));
}

/// Acquire a permit from the global limiter, or return immediately if
/// no limiter has been installed (default / GUI path).
pub async fn maybe_acquire() {
    if let Some(limiter) = GLOBAL.get() {
        limiter.acquire().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refill_tokens_caps_at_burst() {
        // Starting at 5 tokens, 1 second elapsed at 10 tps would add 10
        // tokens but burst caps at 8.
        let new = TpsLimiter::refill_tokens(5.0, 10.0, 8.0, Duration::from_secs(1));
        assert!((new - 8.0).abs() < 1e-9);
    }

    #[test]
    fn refill_tokens_partial_second_adds_partial_token() {
        // 250 ms at 4 tps = 1 token.
        let new = TpsLimiter::refill_tokens(0.0, 4.0, 10.0, Duration::from_millis(250));
        assert!((new - 1.0).abs() < 1e-9);
    }

    #[test]
    fn refill_tokens_zero_elapsed_returns_input() {
        let new = TpsLimiter::refill_tokens(3.5, 10.0, 10.0, Duration::ZERO);
        assert!((new - 3.5).abs() < 1e-9);
    }

    #[test]
    fn refill_tokens_never_returns_negative() {
        // Even with absurd inputs the formula clamps at zero.
        let new = TpsLimiter::refill_tokens(-5.0, 10.0, 10.0, Duration::ZERO);
        assert!((new - 0.0).abs() < 1e-9);
    }

    #[test]
    fn refill_tokens_caps_when_already_at_burst() {
        let new = TpsLimiter::refill_tokens(10.0, 5.0, 10.0, Duration::from_secs(10));
        assert!((new - 10.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn acquire_returns_immediately_when_tokens_available() {
        let limiter = TpsLimiter::new(10.0, 5.0);
        let started = Instant::now();
        for _ in 0..5 {
            limiter.acquire().await;
        }
        // 5 tokens are pre-loaded so all 5 acquires should complete
        // without sleeping: the bucket starts at full burst capacity.
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn maybe_acquire_is_noop_when_uninstalled() {
        // GLOBAL is not set in this test binary because no init() runs
        // in the test path. Just verify the no-op branch does not
        // panic.
        maybe_acquire().await;
    }

    #[test]
    fn new_clamps_pathological_inputs() {
        let l = TpsLimiter::new(0.0, 0.0);
        assert!(l.tps >= 0.001);
        assert!(l.burst >= 1.0);
        let l2 = TpsLimiter::new(-5.0, -1.0);
        assert!(l2.tps >= 0.001);
        assert!(l2.burst >= 1.0);
    }
}
