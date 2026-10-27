//! Global bandwidth limit shared by all transfers (token bucket).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use tokio::time::Instant;

/// Token bucket in bytes per second; 0 means unlimited. Tokens may go negative so a large
/// request (a whole multipart part) passes at once and later requests wait it off: the average
/// rate stays at the limit.
pub struct RateLimiter {
    limit: AtomicU64,
    state: Mutex<(f64, Instant)>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(0)
    }
}

impl RateLimiter {
    /// Limiter with `bytes_per_sec` (0 = unlimited).
    pub fn new(bytes_per_sec: u64) -> Self {
        Self {
            limit: AtomicU64::new(bytes_per_sec),
            state: Mutex::new((0.0, Instant::now())),
        }
    }

    /// Current limit in bytes per second (0 = unlimited).
    pub fn limit(&self) -> u64 {
        self.limit.load(Ordering::Relaxed)
    }

    /// Changes the limit; applies to the next request.
    pub fn set_limit(&self, bytes_per_sec: u64) {
        self.limit.store(bytes_per_sec, Ordering::Relaxed);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        *state = (0.0, Instant::now());
    }

    /// Waits until `bytes` may be sent.
    pub async fn acquire(&self, bytes: u64) {
        while let Some(wait) = self.reserve(bytes) {
            tokio::time::sleep(wait).await;
        }
    }

    /// Takes `bytes` from the bucket when it is not in debt (`None`); otherwise returns how
    /// long to wait before asking again.
    pub(crate) fn reserve(&self, bytes: u64) -> Option<Duration> {
        let limit = self.limit();
        if limit == 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let rate = limit as f64;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        let elapsed = now.duration_since(state.1).as_secs_f64();
        // At most one second of burst.
        state.0 = (state.0 + elapsed * rate).min(rate);
        state.1 = now;
        if state.0 >= 0.0 {
            #[allow(clippy::cast_precision_loss)]
            {
                state.0 -= bytes as f64;
            }
            None
        } else {
            Some(Duration::from_secs_f64(-state.0 / rate))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn averages_to_the_limit() {
        let limiter = RateLimiter::new(1000);
        let start = Instant::now();
        for _ in 0..10 {
            limiter.acquire(500).await;
        }
        // 5000 bytes at 1000 B/s: about 4 s after the free first second of burst.
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(3900), "{elapsed:?}");
        assert!(elapsed <= Duration::from_millis(5100), "{elapsed:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn unlimited_never_waits() {
        let limiter = RateLimiter::new(0);
        let start = Instant::now();
        limiter.acquire(10_000_000).await;
        assert_eq!(start.elapsed(), Duration::ZERO);
        limiter.set_limit(10);
        assert_eq!(limiter.limit(), 10);
    }
}
