use std::{
    collections::HashMap,
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant},
};

/// Per-IP fixed-window rate limiter.
pub struct RateLimiter {
    inner: Mutex<HashMap<IpAddr, (Instant, u32)>>,
    max: u32,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max: u32, window: Duration) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            max,
            window,
        }
    }

    pub fn allow(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut map = self.inner.lock().unwrap();
        // Keep the map from growing unbounded over the window's lifetime.
        if map.len() > 4096 {
            map.retain(|_, (start, _)| now.duration_since(*start) <= self.window);
        }
        let entry = map.entry(ip).or_insert((now, 0));
        if now.duration_since(entry.0) > self.window {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= self.max
    }
}
