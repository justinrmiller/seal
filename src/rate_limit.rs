use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::AppError;

const RATE_LIMIT_WINDOW_SECS: f64 = 60.0;
const RATE_LIMIT_MAX: usize = 20;

#[derive(Default)]
pub struct RateLimiter {
    store: Mutex<HashMap<IpAddr, Vec<f64>>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mirrors `check_rate_limit` in app/main.py: 20 requests per 60s window
    /// keyed by direct peer IP. Prunes stale entries, errors with 429 if the
    /// window is full, otherwise appends a fresh timestamp.
    pub fn check(&self, ip: IpAddr) -> Result<(), AppError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let mut store = self.store.lock().expect("rate limit store poisoned");
        let entry = store.entry(ip).or_default();
        entry.retain(|t| now - t < RATE_LIMIT_WINDOW_SECS);
        if entry.len() >= RATE_LIMIT_MAX {
            return Err(AppError::TooManyRequests);
        }
        entry.push(now);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn clear(&self) {
        self.store
            .lock()
            .expect("rate limit store poisoned")
            .clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, last))
    }

    #[test]
    fn allows_up_to_the_limit_then_rejects() {
        let rl = RateLimiter::new();
        let addr = ip(1);
        for i in 0..RATE_LIMIT_MAX {
            assert!(rl.check(addr).is_ok(), "request {i} should be allowed");
        }
        let err = rl.check(addr).expect_err("over the limit");
        assert!(matches!(err, AppError::TooManyRequests));
    }

    #[test]
    fn limits_are_tracked_per_ip() {
        let rl = RateLimiter::new();
        // Exhaust one IP entirely.
        for _ in 0..RATE_LIMIT_MAX {
            rl.check(ip(1)).unwrap();
        }
        assert!(matches!(
            rl.check(ip(1)).unwrap_err(),
            AppError::TooManyRequests
        ));
        // A different IP is unaffected.
        assert!(rl.check(ip(2)).is_ok());
    }

    #[test]
    fn clear_resets_the_window() {
        let rl = RateLimiter::new();
        let addr = ip(1);
        for _ in 0..RATE_LIMIT_MAX {
            rl.check(addr).unwrap();
        }
        assert!(rl.check(addr).is_err());
        rl.clear();
        // After clearing, the full budget is available again.
        assert!(rl.check(addr).is_ok());
    }
}
