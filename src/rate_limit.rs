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
