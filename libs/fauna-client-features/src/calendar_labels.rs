//! The three calendar header labels — month, week range, day — shared by the
//! two Rust apps (tui, linux).
//!
//! They were byte-for-byte twins in `apps/fauna-tui/src/events/grids.rs` and
//! `apps/fauna-linux/src/views/events/time_utils.rs`; `fauna_i18n::time` hosts
//! the name tables but cannot host these (they need `fauna_core::caltime`'s
//! Gregorian math, and `fauna-i18n` is a dependency-free leaf). This crate
//! already depends on both, so the lift needs no crate-graph change.
//!
//! Names come from the i18n catalog, never a platform date library
//! (`docs/goal/ui/events.md` § Where logic lives).
//!
//! Every `week_start` follows caltime's convention (`0 = Mon … 6 = Sun`). The
//! week label takes it as a parameter, never probing the locale itself, so it
//! cannot name a different week than the grid beside it paints.

use fauna_core::caltime::{add_days, day_of_week, week_start_date};
use fauna_i18n::time::{month_name, month_name_short, weekday_name};

/// "July 2026".
pub fn format_month_label(year: i32, month: u32) -> String {
    format!("{} {}", month_name(month), year)
}

/// "Jul 13 – 19, 2026" — the week range label, in one of three shapes (same
/// month, same year across months, across years), joined by an en-dash.
pub fn format_week_label(year: i32, month: u32, day: u32, week_start: u32) -> String {
    let (sy, sm, sd) = week_start_date(year, month, day, week_start);
    let (ey, em, ed) = add_days(sy, sm, sd, 6);
    if sy == ey && sm == em {
        format!("{} {sd} \u{2013} {ed}, {sy}", month_name_short(sm))
    } else if sy == ey {
        format!(
            "{} {sd} \u{2013} {} {ed}, {sy}",
            month_name_short(sm),
            month_name_short(em)
        )
    } else {
        format!(
            "{} {sd}, {sy} \u{2013} {} {ed}, {ey}",
            month_name_short(sm),
            month_name_short(em)
        )
    }
}

/// "Wednesday, Jul 15, 2026".
pub fn format_day_label(year: i32, month: u32, day: u32) -> String {
    format!(
        "{}, {} {day}, {year}",
        weekday_name(day_of_week(year, month, day)),
        month_name_short(month)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_labels_format() {
        assert_eq!(format_month_label(2026, 7), "July 2026");
        assert_eq!(
            format_week_label(2026, 7, 15, 0),
            "Jul 13 \u{2013} 19, 2026"
        );
        assert_eq!(format_day_label(2026, 7, 15), "Wednesday, Jul 15, 2026");
    }

    /// The week label moves with `week_start` and names the same first day the
    /// grid's own `week_start_date` snap gives (2026-07-15 is a Wednesday).
    #[test]
    fn the_week_label_names_the_week_the_grid_paints() {
        for (ws, expected) in [
            (0, "Jul 13 \u{2013} 19, 2026"),
            (6, "Jul 12 \u{2013} 18, 2026"),
            (5, "Jul 11 \u{2013} 17, 2026"),
        ] {
            assert_eq!(format_week_label(2026, 7, 15, ws), expected);
            let (_, _, first) = week_start_date(2026, 7, 15, ws);
            assert!(expected.contains(&format!("{first}")));
        }
    }

    #[test]
    fn the_week_label_spans_months_and_years() {
        // Week of Mon 2026-06-29 crosses a month; Mon 2025-12-29 crosses a year.
        assert_eq!(
            format_week_label(2026, 7, 1, 0),
            "Jun 29 \u{2013} Jul 5, 2026"
        );
        assert_eq!(
            format_week_label(2026, 1, 1, 0),
            "Dec 29, 2025 \u{2013} Jan 4, 2026"
        );
    }
}
