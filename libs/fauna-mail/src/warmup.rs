//! Pure fresh-IP outbound warm-up schedule.
//!
//! Implements the *ramp table* of `docs/goal/behavior/mail-deliverability.md`
//! § Fresh-IP warm-up → § The ramp: the per-deployment outbound-volume cap as a
//! function of the day since the deployment's first outbound mail. A freshly
//! provisioned VPS has an outbound IP with no reputation; major receivers
//! throttle bursts on unknown IPs, so the deployment ramps volume over ~30 days
//! before sending at full volume.
//!
//! This is the *schedule only* — pure std, no clock, no DB, no I/O — so it is
//! WASM-safe and unit-testable in isolation. The day-counter / `mails_sent_today`
//! state, the submission-time enforcement decision, and the queue-tomorrow
//! deferral all live nest-side (`bins/fauna-nest/src/db/mail_warmup.rs` +
//! `submit_outbound`), bound to nest's clock + the `outbound_mail_queue`. Keeping
//! the cap curve here (priority #2) lets a future client "warmup status" preview
//! or the Go MTA bridge reuse the identical day→max interpretation.

/// The ramp anchors (`mail-deliverability.md` § The ramp), strictly increasing
/// in both day and cap. Days between two anchors are linearly interpolated
/// (round-to-nearest); from `30` on, the cap is unlimited (see [`max_for_day`]).
///
/// Day 7→14 reproduces the doc's "8–13 ramp 3000→10000" (day 8 = 4000 … day 13 =
/// 9000); day 14→29 reproduces "15–29 ramp 10000→50000".
const WARMUP_ANCHORS: &[(u32, u64)] = &[
    (1, 50),
    (2, 100),
    (3, 200),
    (4, 400),
    (5, 750),
    (6, 1500),
    (7, 3000),
    (14, 10_000),
    (29, 50_000),
];

/// The first day on which the warm-up cap is lifted entirely (`mail-
/// deliverability.md` § The ramp → "30+ = unlimited").
pub const WARMUP_UNLIMITED_DAY: u32 = 30;

/// Maximum outbound mails allowed on `day` (1-based: the day of the first
/// outbound mail is day 1). `None` means **unlimited** — from
/// [`WARMUP_UNLIMITED_DAY`] on, the warm-up imposes no cap (only the per-actor
/// rate caps in `mail-policy-config.md` § Submission policy still apply).
///
/// Exact at each anchor in [`WARMUP_ANCHORS`]; piecewise-linear (round-to-
/// nearest) between anchors. `day == 0` is treated as day 1 (defensive — the
/// caller derives `current_day ≥ 1` from `first_outbound_at`).
pub fn max_for_day(day: u32) -> Option<u64> {
    let day = day.max(1);
    if day >= WARMUP_UNLIMITED_DAY {
        return None;
    }
    // day ∈ [1, 29] here, so it is bracketed by the anchor table.
    let mut prev = WARMUP_ANCHORS[0];
    for &(anchor_day, anchor_val) in WARMUP_ANCHORS {
        if day == anchor_day {
            return Some(anchor_val);
        }
        if day < anchor_day {
            let (prev_day, prev_val) = prev;
            let span_days = (anchor_day - prev_day) as u64;
            let span_val = anchor_val - prev_val; // anchors strictly increasing
            let into = (day - prev_day) as u64;
            // Round-to-nearest: + span_days/2 before the integer divide.
            return Some(prev_val + (span_val * into + span_days / 2) / span_days);
        }
        prev = (anchor_day, anchor_val);
    }
    // Unreachable: day ≤ 29 is always < the last anchor's day or equal to it.
    Some(50_000)
}

/// The next 00:00-UTC boundary strictly after `now_secs` (Unix seconds) — the
/// `next_attempt_at` an over-cap submission is deferred to (`mail-
/// deliverability.md` § Enforcement at submission time → "queued for tomorrow").
/// Also the daily-reset boundary for `mails_sent_today`.
pub fn next_utc_midnight(now_secs: i64) -> i64 {
    const DAY: i64 = 86_400;
    (now_secs.div_euclid(DAY) + 1) * DAY
}

/// The 1-based warm-up day for `now_secs`, given the deployment's
/// `first_outbound_at` (Unix seconds). Day boundaries are 00:00 UTC (epoch-day
/// aligned), so this is `(now_day − first_day) + 1`, floored at 1 — it never
/// runs backwards while `now ≥ first_outbound_at` (`mail-deliverability.md`
/// § Don't do these → "Don't let the warm-up ramp go backwards").
pub fn current_day_for(first_outbound_at: i64, now_secs: i64) -> u32 {
    const DAY: i64 = 86_400;
    let delta_days = now_secs.div_euclid(DAY) - first_outbound_at.div_euclid(DAY);
    (delta_days.max(0) + 1) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_are_exact() {
        assert_eq!(max_for_day(1), Some(50));
        assert_eq!(max_for_day(2), Some(100));
        assert_eq!(max_for_day(3), Some(200));
        assert_eq!(max_for_day(4), Some(400));
        assert_eq!(max_for_day(5), Some(750));
        assert_eq!(max_for_day(6), Some(1500));
        assert_eq!(max_for_day(7), Some(3000));
        assert_eq!(max_for_day(14), Some(10_000));
        assert_eq!(max_for_day(29), Some(50_000));
    }

    #[test]
    fn day_zero_clamps_to_day_one() {
        assert_eq!(max_for_day(0), Some(50));
    }

    #[test]
    fn ramp_8_to_13_is_3000_to_10000_smooth() {
        // (7,3000)→(14,10000): slope 1000/day.
        assert_eq!(max_for_day(8), Some(4000));
        assert_eq!(max_for_day(9), Some(5000));
        assert_eq!(max_for_day(10), Some(6000));
        assert_eq!(max_for_day(11), Some(7000));
        assert_eq!(max_for_day(12), Some(8000));
        assert_eq!(max_for_day(13), Some(9000));
    }

    #[test]
    fn ramp_15_to_28_is_10000_to_50000_smooth() {
        // (14,10000)→(29,50000): slope 40000/15 ≈ 2666.67/day, round-to-nearest.
        assert_eq!(max_for_day(15), Some(12_667));
        assert_eq!(max_for_day(28), Some(47_333));
        // Monotonic non-decreasing across the whole ramp.
        let mut last = 0;
        for d in 1..WARMUP_UNLIMITED_DAY {
            let v = max_for_day(d).unwrap();
            assert!(v >= last, "day {d} = {v} < prev {last}");
            last = v;
        }
    }

    #[test]
    fn day_30_and_beyond_is_unlimited() {
        assert_eq!(max_for_day(30), None);
        assert_eq!(max_for_day(31), None);
        assert_eq!(max_for_day(1000), None);
    }

    #[test]
    fn next_utc_midnight_rounds_up_to_the_next_day() {
        const DAY: i64 = 86_400;
        // Mid-day → next midnight.
        assert_eq!(next_utc_midnight(DAY + 1), 2 * DAY);
        // Exactly on a boundary → the *following* boundary (strictly after).
        assert_eq!(next_utc_midnight(DAY), 2 * DAY);
        assert_eq!(next_utc_midnight(0), DAY);
    }

    #[test]
    fn current_day_counts_from_first_outbound() {
        const DAY: i64 = 86_400;
        let first = 100 * DAY + 12 * 3600; // day 100, noon
        assert_eq!(current_day_for(first, first), 1); // same day → day 1
        assert_eq!(current_day_for(first, first + 3600), 1); // later same UTC day
        assert_eq!(current_day_for(first, 101 * DAY), 2); // next UTC midnight → day 2
        assert_eq!(current_day_for(first, 129 * DAY), 30); // unlimited threshold
        // Never runs backwards: a `now` before first_outbound clamps to day 1.
        assert_eq!(current_day_for(first, first - DAY), 1);
    }
}
