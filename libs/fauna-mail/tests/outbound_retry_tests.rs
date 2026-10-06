//! Retry schedule, ±10% jitter, and 4 h delay-warning behaviour.
//!
//! Implements docs/goal/behavior/smtp-server.md § Retry schedule. Tests
//! live in the `tests/` directory so they exercise the crate API exactly
//! as downstream consumers (the bridge crate, the future Go MTA via
//! UniFFI) will use it.

#![cfg(feature = "outbound")]

use std::time::Duration;

use fauna_mail::outbound::retry::{NextAction, RetryPolicy, RetrySchedule};

const SCHEDULE_BASE: &[(u32, Duration)] = &[
    (1, Duration::from_secs(300)),   // attempt 2 ← 5 min
    (2, Duration::from_secs(900)),   // attempt 3 ← 15 min
    (3, Duration::from_secs(3600)),  // attempt 4 ← 1 h
    (4, Duration::from_secs(14400)), // attempt 5 ← 4 h
    (5, Duration::from_secs(43200)), // attempt 6 ← 12 h
    (6, Duration::from_secs(86400)), // attempt 7 ← 1 d
    (7, Duration::from_secs(86400)), // attempt 8 ← 1 d
    (8, Duration::from_secs(86400)), // attempt 9 ← 1 d
    (9, Duration::from_secs(86400)), // attempt 10 ← 1 d
];

#[test]
fn each_attempt_lands_in_its_jitter_band() {
    let pol = RetryPolicy::default();
    for &(completed_attempts, base) in SCHEDULE_BASE {
        let mut min_obs = base;
        let mut max_obs = Duration::ZERO;
        for _ in 0..2_000 {
            let action = pol.next(completed_attempts, Duration::ZERO, false);
            let NextAction::Retry { delay, .. } = action else {
                panic!("expected Retry, got GiveUp for completed_attempts={completed_attempts}");
            };
            if delay < min_obs {
                min_obs = delay;
            }
            if delay > max_obs {
                max_obs = delay;
            }
        }
        let low = base.mul_f64(0.90);
        let high = base.mul_f64(1.10);
        assert!(
            min_obs >= low,
            "completed_attempts={completed_attempts}: min observed {min_obs:?} below 90% of base {base:?}"
        );
        assert!(
            max_obs <= high,
            "completed_attempts={completed_attempts}: max observed {max_obs:?} above 110% of base {base:?}"
        );
    }
}

#[test]
fn delay_warning_fires_after_4_hours_and_only_once() {
    let pol = RetryPolicy::default();

    // Just under 4h elapsed — no warning.
    let action = pol.next(2, Duration::from_secs(4 * 3600 - 1), false);
    let NextAction::Retry {
        should_warn_delay, ..
    } = action
    else {
        panic!("expected Retry");
    };
    assert!(!should_warn_delay);

    // Just over 4h elapsed, not yet warned — warn now.
    let action = pol.next(2, Duration::from_secs(4 * 3600 + 1), false);
    let NextAction::Retry {
        should_warn_delay, ..
    } = action
    else {
        panic!("expected Retry");
    };
    assert!(should_warn_delay);

    // 4h elapsed but already warned — no second warning.
    let action = pol.next(2, Duration::from_secs(4 * 3600 + 1), true);
    let NextAction::Retry {
        should_warn_delay, ..
    } = action
    else {
        panic!("expected Retry");
    };
    assert!(!should_warn_delay);
}

#[test]
fn give_up_after_5_day_wall_clock() {
    let pol = RetryPolicy::default();
    let elapsed = Duration::from_secs(5 * 86_400 + 1);
    assert!(matches!(pol.next(3, elapsed, false), NextAction::GiveUp));
}

#[test]
fn give_up_when_schedule_exhausted_even_below_5_days() {
    let pol = RetryPolicy::default();
    // 10 completed attempts means attempt 11 is the final attempt per spec,
    // and we have no slot beyond index 9. GiveUp.
    let elapsed = Duration::from_secs(4 * 86_400);
    assert!(matches!(pol.next(10, elapsed, false), NextAction::GiveUp));
}

#[test]
fn custom_policy_overrides_schedule_and_warning() {
    let pol = RetryPolicy {
        schedule: vec![Duration::from_secs(60), Duration::from_secs(120)],
        jitter_pct: 0,
        permanent_failure_after: Duration::from_secs(180),
        delay_warning_at: Duration::from_secs(30),
    };
    let action = pol.next(1, Duration::from_secs(0), false);
    assert!(matches!(
        action,
        NextAction::Retry {
            delay,
            should_warn_delay: false,
        } if delay == Duration::from_secs(60)
    ));
    let action = pol.next(1, Duration::from_secs(45), false);
    assert!(matches!(
        action,
        NextAction::Retry {
            should_warn_delay: true,
            ..
        }
    ));
    let action = pol.next(2, Duration::from_secs(200), false);
    assert!(matches!(action, NextAction::GiveUp));
}
