//! Linux calendar/events platform glue.
//!
//! The pure, portable date/time math (Gregorian arithmetic, ISO/epoch parsing,
//! overlap layout) lives in `fauna_core::caltime` and is shared with the other
//! apps (priority #2). It is re-exported here so existing `time_utils::*`
//! call sites keep resolving unchanged.
//!
//! What stays linux-side: the system "now"/locale helpers (unsafe libc calls —
//! each platform supplies its own clock + locale) and the localized month/
//! weekday *name* formatting (per-app i18n; `events.md` § Where logic lives
//! keeps display strings client-side).

pub use fauna_core::caltime::{
    EventPlacement, add_days, day_column_layout, day_of_week, days_in_month, find_overlaps,
    is_all_day, locale_week_start, month_grid, next_month, normalize_event_datetime_input,
    parse_date, parse_time, prev_month, week_start_date,
};

// ── Grid geometry ────────────────────────────────────────────────────────────

/// Height per half-hour slot in pixels — shared by `day_grid` and `week_grid`
/// so the two time grids can never paint at different scales.
pub const HALF_HOUR_PX: f64 = 30.0;

// ── System time ─────────────────────────────────────────────────────────────

/// Get today's date as (year, month, day) in system local time.
pub fn today() -> (i32, u32, u32) {
    unsafe {
        let mut t: libc::time_t = 0;
        libc::time(&mut t);
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        (tm.tm_year + 1900, (tm.tm_mon + 1) as u32, tm.tm_mday as u32)
    }
}

/// Is this date today (system local time)?
#[allow(dead_code)]
pub fn is_today(year: i32, month: u32, day: u32) -> bool {
    let (ty, tm, td) = today();
    ty == year && tm == month && td == day
}

/// Get current local time as (hour, minute).
pub fn now_hm() -> (u32, u32) {
    unsafe {
        let mut t: libc::time_t = 0;
        libc::time(&mut t);
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        (tm.tm_hour as u32, tm.tm_min as u32)
    }
}

// ── Formatting ────────────────────────────────────────────────────────────────

/// Format label for the header bar: "March 2026".
pub fn format_month_label(year: i32, month: u32) -> String {
    format!("{} {}", month_name(month), year)
}

/// Format label for the header bar: "Mar 16 – 22, 2026".
///
/// Takes `week_start` rather than probing it: this label hardcoded Monday while
/// `week_grid`/`month_grid` beside it already read `locale_week_start()`, so on
/// any Sunday- or Saturday-start locale the header named a different week than
/// the grid underneath it painted. The param makes that disagreement
/// unrepresentable — the caller probes once and hands the same answer to both.
pub fn format_week_label(year: i32, month: u32, day: u32, week_start: u32) -> String {
    let start = week_start_date(year, month, day, week_start);
    let end = add_days(start.0, start.1, start.2, 6);

    if start.0 == end.0 {
        // Same year.
        if start.1 == end.1 {
            // Same month.
            format!(
                "{} {} \u{2013} {}, {}",
                month_name_short(start.1),
                start.2,
                end.2,
                start.0
            )
        } else {
            format!(
                "{} {} \u{2013} {} {}, {}",
                month_name_short(start.1),
                start.2,
                month_name_short(end.1),
                end.2,
                start.0
            )
        }
    } else {
        format!(
            "{} {}, {} \u{2013} {} {}, {}",
            month_name_short(start.1),
            start.2,
            start.0,
            month_name_short(end.1),
            end.2,
            end.0
        )
    }
}

/// Format label for the header bar: "Wednesday, Mar 18, 2026".
pub fn format_day_label(year: i32, month: u32, day: u32) -> String {
    let dow = day_of_week(year, month, day);
    format!(
        "{}, {} {}, {}",
        weekday_name(dow),
        month_name_short(month),
        day,
        year
    )
}

// ── Name tables ──────────────────────────────────────────────────────────────
//
// Localized from the i18n catalog (docs/goal/ui/events.md § Where logic lives —
// these are catalog strings, never a platform date library's own name
// formatting, and never `fauna_core`).
//
// The four lookups moved to `fauna_i18n::time` 2026-08-19: tui's `crate::format`
// carried a byte-for-byte identical set over the same constants, so they were
// one function duplicated across the two Rust apps rather than per-app glue.
// Re-exported here so this module's `time_utils::{month_name, …}` call-site path
// — and the formatters above, which are its only consumers besides the views —
// are unchanged.
pub use fauna_i18n::time::{month_name, month_name_short, weekday_name, weekday_short};

#[cfg(test)]
mod tests {
    use super::*;

    /// The week range label names the same week the grid beside it paints.
    ///
    /// This was live: `week_grid.rs`/`month_grid.rs` read `locale_week_start()`
    /// while this label hardcoded `week_start_date(.., 0)`, so on any Sunday- or
    /// Saturday-start locale the header said one week and the columns below it
    /// showed another. The label is now a pure function of the same
    /// `week_start` its grid gets, so the two cannot drift.
    #[test]
    fn the_week_label_names_the_week_the_grid_paints() {
        // 2026-07-15 is a Wednesday.
        for (ws, expected) in [
            (0, "Jul 13 \u{2013} 19, 2026"),
            (6, "Jul 12 \u{2013} 18, 2026"),
            (5, "Jul 11 \u{2013} 17, 2026"),
        ] {
            assert_eq!(format_week_label(2026, 7, 15, ws), expected);
            // The grid's own snap, which the label above must agree with.
            let (_, _, first) = week_start_date(2026, 7, 15, ws);
            assert!(
                expected.contains(&format!("{first}")),
                "week_start {ws}: label {expected:?} must name grid day {first}"
            );
        }
    }

    /// The shared probe's convention is caltime's (`0 = Mon … 6 = Sun`) — the
    /// same one every `week_start` param in this module is defined on. A
    /// mismatch here silently shifts every grid by a day.
    #[test]
    fn the_locale_probe_speaks_caltimes_convention() {
        assert_eq!(fauna_core::caltime::week_start_for_locale("en_US.UTF-8"), 6);
        assert_eq!(fauna_core::caltime::week_start_for_locale("en_GB.UTF-8"), 0);
        assert!(locale_week_start() <= 6);
    }
}
