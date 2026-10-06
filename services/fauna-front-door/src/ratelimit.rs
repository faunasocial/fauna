//! Per-peer-IP token bucket for the proxy vhost. Same shape as the nest's
//! anonymous-surface throttling: key on the connection's peer address,
//! refill continuously, answer allow/deny per request.
//!
//! The clock is a parameter so tests are latency-independent (e2e
//! convention 14 — never assert on wall-clock timing).

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;

/// Burst capacity per peer — sized for a provider wizard's call bursts.
pub const PROXY_RATE_CAPACITY: f64 = 30.0;
/// Sustained requests/second per peer.
pub const PROXY_RATE_REFILL_PER_SEC: f64 = 5.0;
/// Bucket-map bound; when exceeded, full (idle) buckets are pruned.
const MAX_TRACKED_BUCKETS: usize = 10_000;

struct Bucket {
    tokens: f64,
    last: Instant,
}

pub struct IpRateLimiter {
    capacity: f64,
    refill_per_sec: f64,
    buckets: Mutex<HashMap<IpAddr, Bucket>>,
}

impl IpRateLimiter {
    pub fn new(capacity: f64, refill_per_sec: f64) -> Self {
        Self {
            capacity,
            refill_per_sec,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Admit or reject one request from `ip` at `now`. Production passes
    /// `Instant::now()`; tests drive a fake clock.
    pub fn allow(&self, ip: IpAddr, now: Instant) -> bool {
        let mut buckets = self.buckets.lock().unwrap();
        if buckets.len() > MAX_TRACKED_BUCKETS {
            let capacity = self.capacity;
            let refill = self.refill_per_sec;
            buckets.retain(|_, b| {
                let refilled = (b.tokens
                    + now.saturating_duration_since(b.last).as_secs_f64() * refill)
                    .min(capacity);
                refilled < capacity // keep only buckets with spent budget
            });
        }
        let bucket = buckets.entry(ip).or_insert(Bucket {
            tokens: self.capacity,
            last: now,
        });
        let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        bucket.last = now;
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
    use std::time::Duration;

    fn ip(last: u8) -> IpAddr {
        IpAddr::V4(std::net::Ipv4Addr::new(203, 0, 113, last))
    }

    #[test]
    fn burst_is_capped_and_refills_with_time() {
        let limiter = IpRateLimiter::new(3.0, 1.0);
        let t0 = Instant::now();
        assert!(limiter.allow(ip(1), t0));
        assert!(limiter.allow(ip(1), t0));
        assert!(limiter.allow(ip(1), t0));
        // Budget spent at the same instant — rejected.
        assert!(!limiter.allow(ip(1), t0));
        // Two fake seconds later two tokens are back.
        let t2 = t0 + Duration::from_secs(2);
        assert!(limiter.allow(ip(1), t2));
        assert!(limiter.allow(ip(1), t2));
        assert!(!limiter.allow(ip(1), t2));
    }

    #[test]
    fn peers_have_independent_buckets() {
        let limiter = IpRateLimiter::new(1.0, 1.0);
        let t0 = Instant::now();
        assert!(limiter.allow(ip(1), t0));
        assert!(!limiter.allow(ip(1), t0));
        // A different peer is unaffected.
        assert!(limiter.allow(ip(2), t0));
    }

    #[test]
    fn refill_never_exceeds_capacity() {
        let limiter = IpRateLimiter::new(2.0, 1.0);
        let t0 = Instant::now();
        assert!(limiter.allow(ip(1), t0));
        // A long idle stretch refills to capacity, not beyond.
        let later = t0 + Duration::from_secs(3600);
        assert!(limiter.allow(ip(1), later));
        assert!(limiter.allow(ip(1), later));
        assert!(!limiter.allow(ip(1), later));
    }
}
