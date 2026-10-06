//! § Throttling: at most 4 concurrent FETCH, at most 100 messages per minute,
//! **per source server**.
//!
//! > "The throttle is per-source-server, not per-mailbox; a Gmail import opens
//! > one source IMAP session and pipelines its FETCH within the per-server
//! > cap."
//!
//! One [`FetchThrottle`] therefore lives for the whole import, spanning every
//! mailbox, and it lives *here* — in shared Rust — not in each platform shell,
//! so all seven apps rate-limit a provider identically (priority #1/#2).
//!
//! The type is **pure**: it never reads a clock and never sleeps. The caller
//! supplies `now_ms` and performs the sleep the decision asks for. That keeps
//! the ratified numbers unit-testable in virtual time and keeps
//! `Instant::now()` — a `wasm32-unknown-unknown` panic — out of shared code.

use std::collections::VecDeque;

/// The rolling window § Throttling's "per minute" is measured over.
const WINDOW_MS: u64 = 60_000;

/// What the caller must do before issuing the next FETCH.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleDecision {
    /// Under both caps — issue the FETCH now, then call
    /// [`FetchThrottle::on_start`].
    Go,
    /// The concurrency cap is reached. Read a response and call
    /// [`FetchThrottle::on_complete`] before polling again. **Not** a sleep:
    /// waiting on the wire is what makes progress here.
    AwaitInFlight,
    /// The rate cap is reached. Sleep this many milliseconds (always ≥ 1),
    /// then poll again. Sleeping exactly this long retires the oldest start
    /// from the window, so the next poll cannot loop.
    SleepMs(u64),
}

/// Enforces § Throttling's two caps against one source server.
#[derive(Debug, Clone)]
pub struct FetchThrottle {
    max_concurrent: u32,
    max_per_minute: u32,
    in_flight: u32,
    /// Start timestamps of the FETCHes inside the current window, oldest
    /// first. Pruned on every [`Self::poll`], so it is bounded by
    /// `max_per_minute` — we never start a FETCH that would overfill it.
    starts: VecDeque<u64>,
}

impl FetchThrottle {
    /// The ratified caps: [`crate::imap_client::MAX_CONCURRENT_FETCH`] and
    /// [`crate::imap_client::MAX_FETCH_PER_MINUTE`].
    pub fn ratified() -> Self {
        Self::new(
            crate::imap_client::MAX_CONCURRENT_FETCH,
            crate::imap_client::MAX_FETCH_PER_MINUTE,
        )
    }

    /// Both caps must be non-zero: a zero concurrency cap can never be
    /// released (nothing can be in flight to reap), and a zero rate cap can
    /// never elapse.
    pub fn new(max_concurrent: u32, max_per_minute: u32) -> Self {
        debug_assert!(max_concurrent > 0, "a zero concurrency cap deadlocks");
        debug_assert!(max_per_minute > 0, "a zero rate cap never elapses");
        Self {
            max_concurrent: max_concurrent.max(1),
            max_per_minute: max_per_minute.max(1),
            in_flight: 0,
            starts: VecDeque::new(),
        }
    }

    /// Number of FETCHes issued but not yet completed.
    pub fn in_flight(&self) -> u32 {
        self.in_flight
    }

    /// May the caller issue another FETCH at `now_ms`?
    ///
    /// Checks concurrency before rate on purpose: when both caps are saturated,
    /// reaping an in-flight response is strictly better than sleeping — it
    /// makes progress *and* the completed message is what the caller wanted.
    pub fn poll(&mut self, now_ms: u64) -> ThrottleDecision {
        self.prune(now_ms);

        if self.in_flight >= self.max_concurrent {
            return ThrottleDecision::AwaitInFlight;
        }
        if self.starts.len() as u32 >= self.max_per_minute {
            // `prune` guarantees `oldest + WINDOW_MS > now_ms`, so this is ≥ 1.
            let oldest = *self.starts.front().expect("non-empty: len >= cap >= 1");
            return ThrottleDecision::SleepMs(oldest + WINDOW_MS - now_ms);
        }
        ThrottleDecision::Go
    }

    /// Record that a FETCH was issued at `now_ms`. Call only after
    /// [`Self::poll`] returned [`ThrottleDecision::Go`].
    pub fn on_start(&mut self, now_ms: u64) {
        self.in_flight += 1;
        self.starts.push_back(now_ms);
    }

    /// Record that an in-flight FETCH finished (successfully or not). The rate
    /// window is unaffected — it counts *starts*, so a burst of fast FETCHes
    /// cannot outrun the per-minute cap.
    pub fn on_complete(&mut self) {
        self.in_flight = self.in_flight.saturating_sub(1);
    }

    /// Drop starts that have aged out of the window.
    fn prune(&mut self, now_ms: u64) {
        while let Some(&oldest) = self.starts.front() {
            if oldest + WINDOW_MS <= now_ms {
                self.starts.pop_front();
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratified_caps_match_the_goal_doc() {
        let t = FetchThrottle::ratified();
        assert_eq!(t.max_concurrent, 4, "§ Throttling: 4 concurrent FETCH");
        assert_eq!(t.max_per_minute, 100, "§ Throttling: 100 messages/minute");
    }

    #[test]
    fn concurrency_cap_blocks_the_fifth_fetch() {
        let mut t = FetchThrottle::ratified();
        for i in 0..4 {
            assert_eq!(t.poll(0), ThrottleDecision::Go, "fetch {i} should go");
            t.on_start(0);
        }
        assert_eq!(t.poll(0), ThrottleDecision::AwaitInFlight);
        assert_eq!(t.in_flight(), 4);

        // Reaping one frees exactly one slot.
        t.on_complete();
        assert_eq!(t.poll(0), ThrottleDecision::Go);
        t.on_start(0);
        assert_eq!(t.poll(0), ThrottleDecision::AwaitInFlight);
    }

    #[test]
    fn concurrency_is_checked_before_rate() {
        // Both caps saturated => AwaitInFlight, because reaping progresses and
        // sleeping does not.
        let mut t = FetchThrottle::new(2, 2);
        t.on_start(0);
        t.on_start(0);
        assert_eq!(t.poll(0), ThrottleDecision::AwaitInFlight);
    }

    #[test]
    fn rate_cap_blocks_the_hundred_and_first_message_in_the_window() {
        let mut t = FetchThrottle::ratified();
        // 100 starts that all complete immediately: concurrency never binds,
        // so only the rate cap can stop us.
        for i in 0..100 {
            assert_eq!(t.poll(i), ThrottleDecision::Go, "message {i}");
            t.on_start(i);
            t.on_complete();
        }
        assert_eq!(t.in_flight(), 0, "nothing in flight; only the rate binds");

        // The oldest start was at t=0, so we may go again at t=60_000.
        match t.poll(100) {
            ThrottleDecision::SleepMs(ms) => assert_eq!(ms, 60_000 - 100),
            other => panic!("expected SleepMs, got {other:?}"),
        }
    }

    #[test]
    fn sleeping_exactly_as_told_unblocks_on_the_next_poll() {
        // Pins the invariant that makes the driver loop terminate: the
        // returned duration is *sufficient*, never one millisecond short.
        let mut t = FetchThrottle::new(4, 3);
        for i in 0..3 {
            assert_eq!(t.poll(i * 10), ThrottleDecision::Go);
            t.on_start(i * 10);
            t.on_complete();
        }
        let now = 30;
        let ThrottleDecision::SleepMs(ms) = t.poll(now) else {
            panic!("expected the rate cap to bind");
        };
        assert_eq!(t.poll(now + ms), ThrottleDecision::Go, "sleep was short");
    }

    #[test]
    fn the_window_rolls_rather_than_resetting() {
        // A fixed-bucket limiter would let 2*cap through at a bucket boundary.
        // A rolling window must not.
        let mut t = FetchThrottle::new(4, 2);
        t.poll(0);
        t.on_start(0);
        t.on_complete();
        t.poll(59_000);
        t.on_start(59_000);
        t.on_complete();

        // At t=59_500 both starts are still inside the 60s window.
        assert!(matches!(t.poll(59_500), ThrottleDecision::SleepMs(_)));

        // At t=60_000 the first start (t=0) has aged out — exactly one slot.
        assert_eq!(t.poll(60_000), ThrottleDecision::Go);
        t.on_start(60_000);
        t.on_complete();
        assert!(matches!(t.poll(60_000), ThrottleDecision::SleepMs(_)));
    }

    #[test]
    fn the_window_never_grows_past_the_rate_cap() {
        // Memory bound: we never start a FETCH that would overfill the window,
        // so `starts` cannot grow without limit over a long import.
        let mut t = FetchThrottle::new(4, 10);
        for i in 0..10_000u64 {
            if let ThrottleDecision::Go = t.poll(i) {
                t.on_start(i);
                t.on_complete();
            }
            assert!(t.starts.len() <= 10, "window grew to {}", t.starts.len());
        }
    }

    #[test]
    fn completion_underflow_is_saturating() {
        // A transport error can complete a FETCH the caller already reaped.
        let mut t = FetchThrottle::ratified();
        t.on_complete();
        t.on_complete();
        assert_eq!(t.in_flight(), 0);
        assert_eq!(t.poll(0), ThrottleDecision::Go);
    }
}
