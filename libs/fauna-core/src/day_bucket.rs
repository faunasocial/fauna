//! The local-day bucket — the one rule for keying coarse per-day usage counters.
//!
//! Two planes account usage in day buckets and must agree exactly on where a
//! day starts, or the same instant lands in different buckets depending on
//! which plane asked: family safety's screen time and Guardian Notify
//! (`docs/goal/behavior/family-safety.md` § Screen time — the day-bucket rule,
//! ratified 2026-07-16) and the controversial-class feature gate
//! (`docs/goal/architecture/dynamic-features.md` § Usage accounting). The rule
//! is therefore stated once, here, rather than re-derived per plane.
//!
//! **The rule.** The nest stamps the day from **its own clock** plus the
//! client's *clamped* reported UTC offset: `(now_epoch_secs + offset·60) /
//! 86_400`. The wire never carries a day — client input is limited to the
//! offset, and the clamp is what keeps the bucket key space bounded, so for any
//! real instant at most two adjacent buckets are reachable and a hostile offset
//! is under-reporting-equivalent rather than a way to mint fresh quota.
//!
//! Pure arithmetic, no system clock: the caller passes `now_epoch_secs`, which
//! keeps this WASM-safe and lets a test drive it without a fake clock.

/// The minimum real UTC offset, in minutes (UTC-12).
pub const UTC_OFFSET_MIN_MINUTES: i32 = -720;
/// The maximum real UTC offset, in minutes (UTC+14).
pub const UTC_OFFSET_MAX_MINUTES: i32 = 840;

/// Clamp a client-reported UTC offset into the range real offsets occupy.
///
/// `0` (UTC) is the degrade for an older client that reports no offset, and is
/// how pre-offset rows were stamped.
pub fn clamp_utc_offset(offset_minutes: i32) -> i32 {
    offset_minutes.clamp(UTC_OFFSET_MIN_MINUTES, UTC_OFFSET_MAX_MINUTES)
}

/// The account's current local-day bucket, from the nest's clock and the
/// client's clamped offset.
pub fn local_day_bucket(now_epoch_secs: i64, utc_offset_minutes: i32) -> i64 {
    now_epoch_secs
        .saturating_add(i64::from(clamp_utc_offset(utc_offset_minutes)) * 60)
        .div_euclid(86_400)
}

/// The first bucket of a trailing window of `window_days` ending at (and
/// including) `today`.
///
/// Trailing, not calendar: a calendar boundary is a reset anyone bounded by a
/// quota can simply wait for, and it would make a bound's real strictness
/// depend on the day of the month (`dynamic-features.md` § The quota grammar).
/// A window of 1 day is `today` itself.
pub fn window_start_bucket(today: i64, window_days: u32) -> i64 {
    today - i64::from(window_days.max(1) - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_clamp_to_the_real_range() {
        assert_eq!(clamp_utc_offset(0), 0);
        assert_eq!(clamp_utc_offset(-720), -720);
        assert_eq!(clamp_utc_offset(840), 840);
        assert_eq!(clamp_utc_offset(-10_000), -720);
        assert_eq!(clamp_utc_offset(10_000), 840);
    }

    #[test]
    fn the_bucket_is_the_nests_clock_shifted_by_the_clamped_offset() {
        // 1970-01-02T00:00:00Z is exactly bucket 1 at UTC.
        assert_eq!(local_day_bucket(86_400, 0), 1);
        // One second earlier is still bucket 0…
        assert_eq!(local_day_bucket(86_399, 0), 0);
        // …but an hour ahead of UTC has already rolled over.
        assert_eq!(local_day_bucket(86_399, 60), 1);
    }

    /// A hostile offset must be able to reach at most one adjacent bucket, which
    /// is what makes it under-reporting-equivalent rather than a quota mint.
    #[test]
    fn a_hostile_offset_reaches_at_most_one_adjacent_bucket() {
        let now = 1_700_000_000;
        let honest = local_day_bucket(now, 0);
        for offset in [-100_000, -721, -720, 839, 840, 100_000] {
            let hostile = local_day_bucket(now, offset);
            assert!(
                (hostile - honest).abs() <= 1,
                "offset {offset} moved the bucket by more than one day"
            );
        }
    }

    /// Pre-epoch instants must floor, not truncate toward zero — an integer
    /// division would put the whole of 1969-12-31 in bucket 0 alongside
    /// 1970-01-01.
    #[test]
    fn pre_epoch_instants_floor_rather_than_truncate() {
        assert_eq!(local_day_bucket(-1, 0), -1);
        assert_eq!(local_day_bucket(-86_400, 0), -1);
        assert_eq!(local_day_bucket(-86_401, 0), -2);
    }

    #[test]
    fn a_trailing_window_includes_today() {
        assert_eq!(window_start_bucket(100, 1), 100);
        assert_eq!(window_start_bucket(100, 7), 94);
        assert_eq!(window_start_bucket(100, 30), 71);
        // A zero-day window is meaningless; treat it as today alone rather than
        // producing a window that starts after it ends.
        assert_eq!(window_start_bucket(100, 0), 100);
    }
}
