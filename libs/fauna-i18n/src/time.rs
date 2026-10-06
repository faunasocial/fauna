//! Month + weekday **name** lookups, shared by the two Rust apps.
//!
//! These map a calendar index onto the generated catalog constants in
//! [`crate::strings::time`] — nothing more. They live here, beside the table
//! they read, because both Rust apps had byte-for-byte identical copies of all
//! four (tui in `crate::format`, linux in `views/events/time_utils.rs`) over the
//! *same* constants, which is priority #4's resolve-don't-match case.
//!
//! **Why the name tables move but the label formatters do not** (2026-08-19):
//! this crate is a dependency-free leaf that much of the workspace links
//! (`fauna-protocol`, `fauna-client`, `fauna-conversations`, `fauna-anon-client`,
//! `fauna-client-features`, plus both Rust apps). These four functions read only constants already in this crate, so
//! they cost it nothing. The three calendar *label* formatters that call them
//! (`format_month_label` / `format_week_label` / `format_day_label`) need
//! `fauna_core::caltime`'s Gregorian math, and giving this leaf a `fauna-core`
//! dependency to host them would invert the build graph for every crate above —
//! so those stay app-side, now over one shared name source.
//!
//! `docs/goal/ui/events.md` § Where logic lives owns the rule these serve:
//! the names come from the i18n catalog, never from a platform date library's
//! own formatting (which renders a language from inside the binary, where no
//! catalog can localize it).

use crate::strings::{common, time as t};

/// Full month name for `1..=12` — "January". Out-of-range answers the catalog's
/// `common.unknown` rather than panicking: a bad month reaching a label is a
/// display bug, not a reason to take the app down.
pub fn month_name(month: u32) -> &'static str {
    match month {
        1 => t::MONTH_FULL_JAN,
        2 => t::MONTH_FULL_FEB,
        3 => t::MONTH_FULL_MAR,
        4 => t::MONTH_FULL_APR,
        5 => t::MONTH_FULL_MAY,
        6 => t::MONTH_FULL_JUN,
        7 => t::MONTH_FULL_JUL,
        8 => t::MONTH_FULL_AUG,
        9 => t::MONTH_FULL_SEP,
        10 => t::MONTH_FULL_OCT,
        11 => t::MONTH_FULL_NOV,
        12 => t::MONTH_FULL_DEC,
        _ => common::UNKNOWN,
    }
}

/// Short month name for `1..=12` — "Jan".
///
/// The out-of-range arm is `"???"`, not `common.unknown`, deliberately: this
/// form is used inside dense fixed-width labels ("Jul 13 – 19, 2026") where a
/// full word would blow the line out. Both apps already agreed on that split
/// between the full and short tables; it is preserved exactly.
pub fn month_name_short(month: u32) -> &'static str {
    match month {
        1 => t::MONTH_JAN,
        2 => t::MONTH_FEB,
        3 => t::MONTH_MAR,
        4 => t::MONTH_APR,
        5 => t::MONTH_MAY,
        6 => t::MONTH_JUN,
        7 => t::MONTH_JUL,
        8 => t::MONTH_AUG,
        9 => t::MONTH_SEP,
        10 => t::MONTH_OCT,
        11 => t::MONTH_NOV,
        12 => t::MONTH_DEC,
        _ => "???",
    }
}

/// Full weekday name on `caltime`'s convention (`0 = Mon … 6 = Sun`) —
/// "Monday".
///
/// The argument is a [`fauna_core::caltime::day_of_week`] answer (that crate is
/// not linked here — see the module docs). Feeding it a Sunday-first index
/// instead silently shifts every name by one day, which is why both weekday
/// helpers state the convention rather than implying it.
pub fn weekday_name(day: u32) -> &'static str {
    match day {
        0 => t::WEEKDAY_FULL_MON,
        1 => t::WEEKDAY_FULL_TUE,
        2 => t::WEEKDAY_FULL_WED,
        3 => t::WEEKDAY_FULL_THU,
        4 => t::WEEKDAY_FULL_FRI,
        5 => t::WEEKDAY_FULL_SAT,
        6 => t::WEEKDAY_FULL_SUN,
        _ => common::UNKNOWN,
    }
}

/// Short weekday name on `caltime`'s convention (`0 = Mon … 6 = Sun`) — "Mon".
/// Same `"???"` out-of-range reasoning as [`month_name_short`].
pub fn weekday_short(day: u32) -> &'static str {
    match day {
        0 => t::WEEKDAY_MON,
        1 => t::WEEKDAY_TUE,
        2 => t::WEEKDAY_WED,
        3 => t::WEEKDAY_THU,
        4 => t::WEEKDAY_FRI,
        5 => t::WEEKDAY_SAT,
        6 => t::WEEKDAY_SUN,
        _ => "???",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every in-range index resolves to a distinct, non-empty catalog string.
    ///
    /// The failure this guards is a copy/paste slip in a 12- or 7-arm match —
    /// two indices pointing at the same constant, or one pointing at the
    /// fallback. Asserting *distinctness* catches that where a spot-check of
    /// one or two arms does not, and it stays true under translation, unlike
    /// pinning the English text.
    #[test]
    fn every_index_resolves_to_a_distinct_name() {
        for (label, names) in [
            ("month_name", (1..=12).map(month_name).collect::<Vec<_>>()),
            (
                "month_name_short",
                (1..=12).map(month_name_short).collect::<Vec<_>>(),
            ),
            (
                "weekday_name",
                (0..=6).map(weekday_name).collect::<Vec<_>>(),
            ),
            (
                "weekday_short",
                (0..=6).map(weekday_short).collect::<Vec<_>>(),
            ),
        ] {
            for (i, n) in names.iter().enumerate() {
                assert!(!n.is_empty(), "{label}[{i}] is empty");
                assert_ne!(*n, common::UNKNOWN, "{label}[{i}] fell through to unknown");
                assert_ne!(*n, "???", "{label}[{i}] fell through to ???");
            }
            let mut sorted = names.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(
                sorted.len(),
                names.len(),
                "{label} has duplicate entries: {names:?}"
            );
        }
    }

    /// Out-of-range indices answer the documented fallback rather than
    /// panicking — including `0` for months and `7` for weekdays, the two
    /// off-by-one values a caller mixing up the conventions actually produces.
    #[test]
    fn out_of_range_answers_the_documented_fallback() {
        for m in [0, 13, u32::MAX] {
            assert_eq!(month_name(m), common::UNKNOWN);
            assert_eq!(month_name_short(m), "???");
        }
        for d in [7, u32::MAX] {
            assert_eq!(weekday_name(d), common::UNKNOWN);
            assert_eq!(weekday_short(d), "???");
        }
    }

    /// The short tables are not wired to the full ones — `MONTH_JAN` and
    /// `MONTH_FULL_JAN` are one underscore apart, so a swapped or self-pointing
    /// table reads perfectly plausibly at a glance.
    ///
    /// Deliberately **not** a per-entry `assert_ne!`: "May" is its own
    /// abbreviation, so an entry-wise inequality is simply false (it failed on
    /// exactly that when first written). What does hold, and survives
    /// translation, is that the full form is never *shorter* than the short one
    /// and that the two tables are not wholesale identical.
    #[test]
    fn the_short_tables_are_not_the_full_ones() {
        let mut months_differ = 0;
        for m in 1..=12 {
            assert!(
                month_name(m).len() >= month_name_short(m).len(),
                "month {m}: full {:?} is shorter than short {:?} — tables swapped?",
                month_name(m),
                month_name_short(m)
            );
            months_differ += usize::from(month_name(m) != month_name_short(m));
        }
        assert!(
            months_differ >= 11,
            "only {months_differ}/12 months differ between the full and short \
             tables — one of them is wired to the other's constants"
        );

        let mut days_differ = 0;
        for d in 0..=6 {
            assert!(
                weekday_name(d).len() >= weekday_short(d).len(),
                "weekday {d}: full {:?} is shorter than short {:?} — tables swapped?",
                weekday_name(d),
                weekday_short(d)
            );
            days_differ += usize::from(weekday_name(d) != weekday_short(d));
        }
        assert_eq!(
            days_differ, 7,
            "every weekday abbreviates, unlike May — a match here means one \
             table is wired to the other's constants"
        );
    }
}
