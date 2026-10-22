//! Login rate limiting (spec section 15). In-memory sliding window per
//! client identity (IP or email); resets on process restart, which is
//! acceptable for a single-container deployment.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Maximum failed attempts within the window.
const MAX_ATTEMPTS: u32 = 5;
/// Window length for counting failures.
const WINDOW: Duration = Duration::from_secs(15 * 60);
/// Lockout applied after exceeding the limit.
const LOCKOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Default)]
pub struct RateLimiter {
    failures: Mutex<HashMap<String, Vec<Instant>>>,
    locked_until: Mutex<HashMap<String, Instant>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the identity is currently locked out.
    pub fn is_locked(&self, identity: &str) -> bool {
        let mut locked = self.locked_until.lock().expect("rate limiter poisoned");
        if let Some(&until) = locked.get(identity) {
            if Instant::now() < until {
                return true;
            }
            locked.remove(identity);
        }
        false
    }

    /// Record a failed attempt. Returns true when the identity just became
    /// locked out.
    pub fn record_failure(&self, identity: &str) -> bool {
        let now = Instant::now();
        let mut failures = self.failures.lock().expect("rate limiter poisoned");
        let attempts = failures.entry(identity.to_string()).or_default();
        attempts.retain(|t| now.duration_since(*t) < WINDOW);
        attempts.push(now);
        if attempts.len() as u32 >= MAX_ATTEMPTS {
            drop(failures);
            self.locked_until
                .lock()
                .expect("rate limiter poisoned")
                .insert(identity.to_string(), now + LOCKOUT);
            // Clear the failure history after locking.
            self.failures
                .lock()
                .expect("rate limiter poisoned")
                .remove(identity);
            return true;
        }
        false
    }

    /// Clear failures after a successful login.
    pub fn clear(&self, identity: &str) {
        self.failures
            .lock()
            .expect("rate limiter poisoned")
            .remove(identity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_after_max_failures() {
        let limiter = RateLimiter::new();
        assert!(!limiter.is_locked("user@example.com"));
        for _ in 0..MAX_ATTEMPTS {
            limiter.record_failure("user@example.com");
        }
        assert!(limiter.is_locked("user@example.com"));
        // Other identities are unaffected.
        assert!(!limiter.is_locked("other@example.com"));
    }

    #[test]
    fn clear_resets_failures() {
        let limiter = RateLimiter::new();
        for _ in 0..(MAX_ATTEMPTS - 1) {
            limiter.record_failure("user@example.com");
        }
        limiter.clear("user@example.com");
        // One more failure is not enough to lock.
        assert!(!limiter.record_failure("user@example.com"));
        assert!(!limiter.is_locked("user@example.com"));
    }
}