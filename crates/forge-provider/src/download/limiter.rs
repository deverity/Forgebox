//! Token bucket shared by every transfer of the manager (optional bandwidth limit).
//!
//! The critical section is a few arithmetic operations and is never held across an
//! `.await`: [`RateLimiter::reserve`] returns how long the caller must sleep.

use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug)]
struct Bucket {
    /// Bytes per second; `None` means unlimited.
    rate: Option<u64>,
    /// Available tokens (bytes). Negative means debt: the caller already waited for it.
    tokens: f64,
    last: Instant,
}

/// Bandwidth limiter. Burst capacity is one second of the configured rate.
#[derive(Debug)]
pub(crate) struct RateLimiter {
    bucket: Mutex<Bucket>,
}

impl RateLimiter {
    pub(crate) fn new(rate: Option<u64>) -> Self {
        Self {
            bucket: Mutex::new(Bucket {
                rate: rate.filter(|r| *r > 0),
                tokens: rate.unwrap_or(0) as f64,
                last: Instant::now(),
            }),
        }
    }

    /// Changes the limit at runtime (`None` or `0` removes it).
    pub(crate) fn set_rate(&self, rate: Option<u64>) {
        let mut bucket = self.lock();
        bucket.rate = rate.filter(|r| *r > 0);
        bucket.tokens = bucket.rate.unwrap_or(0) as f64;
        bucket.last = Instant::now();
    }

    pub(crate) fn rate(&self) -> Option<u64> {
        self.lock().rate
    }

    /// Takes `bytes` from the bucket and returns how long to wait before using them.
    pub(crate) fn reserve(&self, bytes: usize) -> Duration {
        self.reserve_at(bytes, Instant::now())
    }

    fn reserve_at(&self, bytes: usize, now: Instant) -> Duration {
        let mut bucket = self.lock();
        let Some(rate) = bucket.rate else {
            return Duration::ZERO;
        };
        let rate = rate as f64;
        let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
        bucket.last = now;
        bucket.tokens = (bucket.tokens + elapsed * rate).min(rate);
        bucket.tokens -= bytes as f64;
        if bucket.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-bucket.tokens / rate)
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Bucket> {
        // A poisoned bucket only holds plain numbers: keep going with them.
        self.bucket.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_never_waits() {
        let limiter = RateLimiter::new(None);
        assert_eq!(limiter.reserve(10_000_000), Duration::ZERO);
        assert_eq!(limiter.rate(), None);
    }

    #[test]
    fn burst_is_free_then_debt_is_paid_at_the_rate() {
        let limiter = RateLimiter::new(Some(1000));
        let t0 = Instant::now();
        // One second of burst is available.
        assert_eq!(limiter.reserve_at(1000, t0), Duration::ZERO);
        // 500 more bytes at 1000 B/s: half a second.
        let wait = limiter.reserve_at(500, t0);
        assert!((wait.as_secs_f64() - 0.5).abs() < 1e-6, "{wait:?}");
        // After 1 s the debt of 500 is repaid and 500 tokens are back.
        let wait = limiter.reserve_at(500, t0 + Duration::from_secs(1));
        assert_eq!(wait, Duration::ZERO);
    }

    #[test]
    fn tokens_do_not_accumulate_beyond_one_second() {
        let limiter = RateLimiter::new(Some(1000));
        let t0 = Instant::now();
        let wait = limiter.reserve_at(2000, t0 + Duration::from_secs(60));
        assert!((wait.as_secs_f64() - 1.0).abs() < 1e-6, "{wait:?}");
    }

    #[test]
    fn rate_can_change_at_runtime() {
        let limiter = RateLimiter::new(None);
        limiter.set_rate(Some(100));
        assert_eq!(limiter.rate(), Some(100));
        assert!(limiter.reserve(1000) > Duration::ZERO);
        limiter.set_rate(Some(0));
        assert_eq!(limiter.rate(), None);
        assert_eq!(limiter.reserve(1000), Duration::ZERO);
    }
}
