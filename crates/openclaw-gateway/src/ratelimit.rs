use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

/// Simple per-user rate limiter using a sliding window.
pub struct RateLimiter {
    /// Max messages per window
    max_messages: usize,
    /// Window duration in seconds
    window_secs: u64,
    /// Per-user timestamps of recent messages
    state: Mutex<HashMap<i64, Vec<Instant>>>,
}

impl RateLimiter {
    pub fn new(max_messages: usize, window_secs: u64) -> Self {
        Self {
            max_messages,
            window_secs,
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Check if a user is allowed to send a message.
    /// Returns Ok(()) if allowed, Err(seconds_until_next) if rate limited.
    pub fn check(&self, user_id: i64) -> Result<(), u64> {
        let mut state = self.state.lock().unwrap();
        let now = Instant::now();
        let window = std::time::Duration::from_secs(self.window_secs);

        let timestamps = state.entry(user_id).or_insert_with(Vec::new);

        // Remove expired entries
        timestamps.retain(|t| now.duration_since(*t) < window);

        if timestamps.len() >= self.max_messages {
            // Calculate when the oldest entry expires
            let oldest = timestamps.first().unwrap();
            let wait = window
                .checked_sub(now.duration_since(*oldest))
                .unwrap_or_default();
            Err(wait.as_secs() + 1)
        } else {
            timestamps.push(now);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allows_under_limit() {
        let limiter = RateLimiter::new(3, 60);
        assert!(limiter.check(1).is_ok());
        assert!(limiter.check(1).is_ok());
        assert!(limiter.check(1).is_ok());
    }

    #[test]
    fn test_blocks_over_limit() {
        let limiter = RateLimiter::new(2, 60);
        assert!(limiter.check(1).is_ok());
        assert!(limiter.check(1).is_ok());
        assert!(limiter.check(1).is_err());
    }

    #[test]
    fn test_separate_users() {
        let limiter = RateLimiter::new(1, 60);
        assert!(limiter.check(1).is_ok());
        assert!(limiter.check(2).is_ok());
        assert!(limiter.check(1).is_err());
        assert!(limiter.check(2).is_err());
    }
}
