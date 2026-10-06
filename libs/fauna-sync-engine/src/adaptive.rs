//! Adaptive concurrency control using AIMD (Additive Increase, Multiplicative Decrease).
//!
//! Starts at `min` concurrent transfers, increases by 1 after every
//! `RAMP_THRESHOLD` consecutive successes, and halves on any error
//! (down to `min`).  Wraps a tokio `Semaphore` whose capacity is
//! adjusted dynamically.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Number of consecutive successes before incrementing concurrency by 1.
const RAMP_THRESHOLD: u32 = 10;

/// AIMD-based adaptive concurrency controller.
pub struct AdaptiveConcurrency {
    semaphore: Arc<Semaphore>,
    current: AtomicU32,
    min: u32,
    max: u32,
    consecutive_ok: AtomicU32,
    /// When true, adaptive logic is disabled — fixed concurrency.
    fixed: bool,
}

impl AdaptiveConcurrency {
    /// Create an adaptive controller that starts at `min` and can grow to `max`.
    pub fn new(min: u32, max: u32) -> Self {
        let min = min.max(1);
        let max = max.max(min);
        Self {
            semaphore: Arc::new(Semaphore::new(min as usize)),
            current: AtomicU32::new(min),
            min,
            max,
            consecutive_ok: AtomicU32::new(0),
            fixed: false,
        }
    }

    /// Create a fixed-capacity controller (adaptive logic disabled).
    pub fn fixed(capacity: u32) -> Self {
        let capacity = capacity.max(1);
        Self {
            semaphore: Arc::new(Semaphore::new(capacity as usize)),
            current: AtomicU32::new(capacity),
            min: capacity,
            max: capacity,
            consecutive_ok: AtomicU32::new(0),
            fixed: true,
        }
    }

    /// Current concurrency level.
    pub fn current(&self) -> u32 {
        self.current.load(Ordering::Relaxed)
    }

    /// Acquire a transfer permit. Waits if all slots are in use.
    pub async fn acquire(&self) -> OwnedSemaphorePermit {
        self.semaphore
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore closed unexpectedly")
    }

    /// Record a successful transfer. After `RAMP_THRESHOLD` consecutive
    /// successes, adds one permit (additive increase).
    ///
    /// Note: the threshold check uses relaxed atomics and is best-effort —
    /// concurrent calls may occasionally double-increment.  This is acceptable
    /// because the semaphore is the real concurrency bound; `current` is only
    /// a heuristic tracking value.
    pub fn record_success(&self) {
        if self.fixed {
            return;
        }
        let prev = self.consecutive_ok.fetch_add(1, Ordering::Relaxed);
        if prev + 1 >= RAMP_THRESHOLD {
            self.consecutive_ok.store(0, Ordering::Relaxed);
            let cur = self.current.load(Ordering::Relaxed);
            if cur < self.max {
                self.current.store(cur + 1, Ordering::Relaxed);
                self.semaphore.add_permits(1);
                tracing::debug!(new_concurrency = cur + 1, "adaptive: increased concurrency");
            }
        }
    }

    /// Record a transfer error. Halves concurrency (multiplicative decrease)
    /// down to `min`. Excess permits are not forcibly revoked — they drain
    /// naturally as in-flight transfers complete.
    pub fn record_error(&self) {
        if self.fixed {
            return;
        }
        self.consecutive_ok.store(0, Ordering::Relaxed);
        let cur = self.current.load(Ordering::Relaxed);
        let new = (cur / 2).max(self.min);
        if new < cur {
            // We can't remove permits from a tokio Semaphore directly.
            // Instead, acquire and forget (cur - new) permits so they are
            // consumed.  Use try_acquire to avoid blocking — if permits are
            // already in flight, the effective concurrency will drain naturally.
            let to_remove = (cur - new) as usize;
            for _ in 0..to_remove {
                if let Ok(permit) = self.semaphore.clone().try_acquire_owned() {
                    permit.forget();
                }
            }
            self.current.store(new, Ordering::Relaxed);
            tracing::debug!(
                new_concurrency = new,
                "adaptive: decreased concurrency on error"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn adaptive_starts_at_min() {
        let ac = AdaptiveConcurrency::new(2, 16);
        assert_eq!(ac.current(), 2);
    }

    #[tokio::test]
    async fn adaptive_ramps_up_on_success() {
        let ac = AdaptiveConcurrency::new(2, 16);
        // 10 successes should bump from 2 to 3
        for _ in 0..10 {
            ac.record_success();
        }
        assert_eq!(ac.current(), 3);
    }

    #[tokio::test]
    async fn adaptive_halves_on_error() {
        let ac = AdaptiveConcurrency::new(2, 16);
        // Ramp up to 4
        for _ in 0..20 {
            ac.record_success();
        }
        assert_eq!(ac.current(), 4);

        // Error should halve to 2 (the min)
        ac.record_error();
        assert_eq!(ac.current(), 2);
    }

    #[tokio::test]
    async fn adaptive_acquire_respects_capacity() {
        let ac = Arc::new(AdaptiveConcurrency::new(2, 16));
        // Should be able to acquire 2 permits (the min/starting value)
        let p1 = ac.acquire().await;
        let _p2 = ac.acquire().await;
        // A third acquire should not complete immediately
        let ac2 = ac.clone();
        let handle = tokio::spawn(async move { ac2.acquire().await });
        // Give it a moment — should NOT complete
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!handle.is_finished());
        // Drop a permit to unblock
        drop(p1);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(handle.is_finished());
    }

    #[tokio::test]
    async fn adaptive_disabled_uses_fixed() {
        let ac = AdaptiveConcurrency::fixed(8);
        assert_eq!(ac.current(), 8);
        // Success/error have no effect in fixed mode
        for _ in 0..20 {
            ac.record_success();
        }
        assert_eq!(ac.current(), 8);
        ac.record_error();
        assert_eq!(ac.current(), 8);
    }
}
