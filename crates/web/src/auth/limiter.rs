//! A token bucket bounding the outbound work unauthenticated requests can cause (OIDC
//! discovery and code exchanges with the identity provider).
//!
//! It is global, not per client: behind the reverse proxy every request comes from the same
//! address, and `X-Forwarded-For` isn't trusted. Draining it delays new logins only while the
//! flood lasts; signed-in sessions are unaffected, and in-flight logins hold no server state
//! (see `login_state`), so there is nothing to fill up.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A token bucket.
#[derive(Debug)]
pub struct RateLimiter {
    burst: f64,
    per_token: Duration,
    state: Mutex<Bucket>,
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    refilled: Instant,
}

impl RateLimiter {
    /// Allows `burst` operations at once, refilling one every `per_token`.
    pub fn new(burst: u32, per_token: Duration) -> Self {
        Self {
            burst: f64::from(burst),
            per_token,
            state: Mutex::new(Bucket {
                tokens: f64::from(burst),
                refilled: Instant::now(),
            }),
        }
    }

    /// Takes a token if one is available.
    pub fn try_acquire(&self) -> bool {
        self.try_acquire_at(Instant::now())
    }

    fn try_acquire_at(&self, now: Instant) -> bool {
        let mut bucket = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let elapsed = now.saturating_duration_since(bucket.refilled);
        bucket.tokens =
            (bucket.tokens + elapsed.as_secs_f64() / self.per_token.as_secs_f64()).min(self.burst);
        bucket.refilled = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bursts_then_refills() {
        let limiter = RateLimiter::new(3, Duration::from_secs(1));
        let start = Instant::now();
        assert!((0..3).all(|_| limiter.try_acquire_at(start)));
        assert!(!limiter.try_acquire_at(start));
        assert!(!limiter.try_acquire_at(start + Duration::from_millis(500)));
        assert!(limiter.try_acquire_at(start + Duration::from_millis(1100)));
        // Never more than the burst, however long it idles.
        let later = start + Duration::from_secs(3600);
        assert!((0..3).all(|_| limiter.try_acquire_at(later)));
        assert!(!limiter.try_acquire_at(later));
    }
}
