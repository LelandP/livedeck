//! Client-side rate limiting driven by the Helix `Ratelimit-*` headers.
//!
//! Helix uses a token bucket per client ID (or per token). Every response carries
//! `Ratelimit-Limit` (bucket size), `Ratelimit-Remaining` (points left) and `Ratelimit-Reset`
//! (Unix seconds when the bucket is full again). The limiter remembers the last values, waits
//! before a request when the bucket is empty, and tells the client how long to back off after a
//! 429 or a 5xx.

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::HeaderMap;

/// Retry and backoff limits. Injectable so tests do not sleep for real.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Retries after a 429 or 5xx, on top of the first attempt.
    pub max_retries: u32,
    /// First backoff when no reset time is known. Doubles on every retry.
    pub base_backoff: Duration,
    /// Upper bound for any single wait, including waits for the bucket to refill.
    pub max_wait: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_backoff: Duration::from_millis(500),
            max_wait: Duration::from_secs(10),
        }
    }
}

/// The last bucket state Helix reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bucket {
    pub limit: u32,
    pub remaining: u32,
    /// Unix seconds when the bucket refills.
    pub reset: u64,
}

impl Bucket {
    /// Reads the `Ratelimit-*` headers. `None` if any is missing or malformed.
    pub fn from_headers(headers: &HeaderMap) -> Option<Self> {
        let num = |name: &str| headers.get(name)?.to_str().ok()?.trim().parse::<u64>().ok();
        Some(Self {
            limit: u32::try_from(num("ratelimit-limit")?).ok()?,
            remaining: u32::try_from(num("ratelimit-remaining")?).ok()?,
            reset: num("ratelimit-reset")?,
        })
    }

    /// Time until the bucket refills, measured from `now` (Unix seconds).
    fn until_reset(&self, now: u64) -> Duration {
        Duration::from_secs(self.reset.saturating_sub(now))
    }
}

#[derive(Debug, Default)]
pub struct RateLimiter {
    policy: RetryPolicy,
    bucket: Mutex<Option<Bucket>>,
}

impl RateLimiter {
    pub fn new(policy: RetryPolicy) -> Self {
        Self {
            policy,
            bucket: Mutex::new(None),
        }
    }

    pub fn policy(&self) -> RetryPolicy {
        self.policy
    }

    /// The last reported bucket, if any.
    pub fn bucket(&self) -> Option<Bucket> {
        *self.bucket.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records the headers of any response, success or not.
    pub fn observe(&self, headers: &HeaderMap) {
        if let Some(b) = Bucket::from_headers(headers) {
            if b.remaining < b.limit / 10 {
                tracing::debug!(?b, "Helix rate-limit bucket running low");
            }
            *self.bucket.lock().unwrap_or_else(|e| e.into_inner()) = Some(b);
        }
    }

    /// How long to wait before the next request. Zero unless the bucket is known to be empty.
    pub fn wait_before_request(&self) -> Duration {
        match self.bucket() {
            Some(b) if b.remaining == 0 => b.until_reset(unix_now()).min(self.policy.max_wait),
            _ => Duration::ZERO,
        }
    }

    /// How long to back off before retry number `attempt` (0-based) after a 429 or 5xx.
    /// A 429 with a known reset time waits for the reset. Otherwise exponential backoff.
    pub fn backoff(&self, attempt: u32, rate_limited: bool) -> Duration {
        let exp = self
            .policy
            .base_backoff
            .saturating_mul(2u32.saturating_pow(attempt));
        let wait = match self.bucket() {
            Some(b) if rate_limited => b.until_reset(unix_now()).max(self.policy.base_backoff),
            _ => exp,
        };
        wait.min(self.policy.max_wait)
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn headers(limit: u64, remaining: u64, reset: u64) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("Ratelimit-Limit", HeaderValue::from(limit));
        h.insert("Ratelimit-Remaining", HeaderValue::from(remaining));
        h.insert("Ratelimit-Reset", HeaderValue::from(reset));
        h
    }

    fn policy() -> RetryPolicy {
        RetryPolicy {
            max_retries: 3,
            base_backoff: Duration::from_millis(100),
            max_wait: Duration::from_secs(5),
        }
    }

    #[test]
    fn parses_headers() {
        let b = Bucket::from_headers(&headers(800, 799, 1_700_000_000)).unwrap();
        let want = Bucket {
            limit: 800,
            remaining: 799,
            reset: 1_700_000_000,
        };
        assert_eq!(b, want);
        assert_eq!(Bucket::from_headers(&HeaderMap::new()), None);
    }

    #[test]
    fn waits_only_when_the_bucket_is_empty() {
        let rl = RateLimiter::new(policy());
        assert_eq!(rl.wait_before_request(), Duration::ZERO);

        rl.observe(&headers(800, 5, unix_now() + 60));
        assert_eq!(rl.wait_before_request(), Duration::ZERO);

        rl.observe(&headers(800, 0, unix_now() + 60));
        assert_eq!(rl.wait_before_request(), Duration::from_secs(5), "capped");

        rl.observe(&headers(800, 0, unix_now().saturating_sub(10)));
        assert_eq!(rl.wait_before_request(), Duration::ZERO, "reset passed");
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        let rl = RateLimiter::new(policy());
        assert_eq!(rl.backoff(0, false), Duration::from_millis(100));
        assert_eq!(rl.backoff(1, false), Duration::from_millis(200));
        assert_eq!(rl.backoff(3, false), Duration::from_millis(800));
        assert_eq!(rl.backoff(20, false), Duration::from_secs(5));
    }

    #[test]
    fn rate_limited_backoff_uses_reset() {
        let rl = RateLimiter::new(policy());
        rl.observe(&headers(800, 0, unix_now() + 2));
        let wait = rl.backoff(0, true);
        assert!(wait >= Duration::from_secs(1) && wait <= Duration::from_secs(2));
        // A reset already in the past still waits the base backoff.
        rl.observe(&headers(800, 0, unix_now().saturating_sub(1)));
        assert_eq!(rl.backoff(0, true), Duration::from_millis(100));
    }
}
