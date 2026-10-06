//! Retry schedule with ±10% jitter and 4 h delay-warning.
//!
//! Implements `docs/goal/behavior/smtp-server.md` § Retry schedule. The
//! decision is split across two return cases:
//!
//! * `NextAction::Retry { delay, should_warn_delay }` — schedule the next
//!   attempt after `delay` (already jittered). `should_warn_delay` is
//!   true exactly once per row: when wall-clock-elapsed crosses
//!   `delay_warning_at` and `already_warned` is false.
//! * `NextAction::GiveUp` — retry budget exhausted; the bridge promotes
//!   the row to `permfail` and runs the bounce path (backscatter →
//!   NDR-rate-limit → DSN → enqueue).
//!
//! The default policy mirrors the spec defaults verbatim
//! (`docs/goal/behavior/mail-policy-config.md` § Outbound delivery).
//! Admin overrides flow through `OutboundPolicy::retry_schedule_seconds`
//! / `permanent_failure_timeout_hours` / `delay_warning_at_hours` in the
//! `fauna.bridges.fetch_config` reply; this module is the consumer, not
//! the wire surface.

use std::time::Duration;

use rand::Rng;

/// Spec retry policy: nine delays (attempts 2-10), 5 d total give-up,
/// 4 h delay-warning, ±10% jitter.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Delays before attempts 2..N+1 (one entry per future attempt).
    pub schedule: Vec<Duration>,
    /// ±N percent applied uniformly to each scheduled delay.
    pub jitter_pct: u8,
    /// Total wall-clock budget. Crossing this triggers `GiveUp` regardless
    /// of remaining schedule slots.
    pub permanent_failure_after: Duration,
    /// Emit a delay warning when wall-clock-elapsed first crosses this
    /// boundary (and `already_warned` is false).
    pub delay_warning_at: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            schedule: vec![
                Duration::from_secs(300),    // attempt 2 ← 5 min
                Duration::from_secs(900),    // attempt 3 ← 15 min
                Duration::from_secs(3600),   // attempt 4 ← 1 h
                Duration::from_secs(14_400), // attempt 5 ← 4 h
                Duration::from_secs(43_200), // attempt 6 ← 12 h
                Duration::from_secs(86_400), // attempt 7 ← 1 d
                Duration::from_secs(86_400), // attempt 8 ← 1 d
                Duration::from_secs(86_400), // attempt 9 ← 1 d
                Duration::from_secs(86_400), // attempt 10 ← 1 d
            ],
            jitter_pct: 10,
            permanent_failure_after: Duration::from_secs(5 * 86_400),
            delay_warning_at: Duration::from_secs(4 * 3_600),
        }
    }
}

/// Result of asking the schedule for the next move on a row that just
/// finished an attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum NextAction {
    Retry {
        delay: Duration,
        should_warn_delay: bool,
    },
    GiveUp,
}

/// Inputs: `completed_attempts` = count of attempts that have already
/// finished (0 = pre-first-try, just enqueued); `wall_clock_elapsed` =
/// elapsed since the row's `created_at`; `already_warned` = the row's
/// `delay_warned_at IS NOT NULL`.
pub trait RetrySchedule: Send + Sync + 'static {
    fn next(
        &self,
        completed_attempts: u32,
        wall_clock_elapsed: Duration,
        already_warned: bool,
    ) -> NextAction;
}

impl RetrySchedule for RetryPolicy {
    fn next(
        &self,
        completed_attempts: u32,
        wall_clock_elapsed: Duration,
        already_warned: bool,
    ) -> NextAction {
        if wall_clock_elapsed >= self.permanent_failure_after {
            return NextAction::GiveUp;
        }
        let idx = completed_attempts.saturating_sub(1) as usize;
        if idx >= self.schedule.len() {
            return NextAction::GiveUp;
        }
        let base = self.schedule[idx];
        let delay = jitter(base, self.jitter_pct);
        let should_warn_delay = !already_warned && wall_clock_elapsed >= self.delay_warning_at;
        NextAction::Retry {
            delay,
            should_warn_delay,
        }
    }
}

fn jitter(base: Duration, pct: u8) -> Duration {
    if pct == 0 {
        return base;
    }
    let span = base.as_secs_f64() * (pct as f64 / 100.0);
    // gen_range is exclusive on the upper bound; the test thresholds at
    // ±10% are lenient (they accept ≤ 110% / ≥ 90%) so the open interval
    // is fine.
    let offset = rand::thread_rng().gen_range(-span..span);
    Duration::from_secs_f64((base.as_secs_f64() + offset).max(0.0))
}
