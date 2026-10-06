//! The Internet Message Format date-time grammar — one owner.
//!
//! `Day, DD Mon YYYY HH:MM:SS <zone>` is a single grammar wearing three RFC
//! numbers, and the codebase emits it on three different wires:
//!
//! * **RFC 5322 § 3.3** — a mail `Date:` header (also RFC 2822/822, which is
//!   what an RSS `pubDate` is);
//! * **RFC 7231 § 7.1.1.1** — the HTTP `Date:` header, whose `IMF-fixdate`
//!   production is *defined* as the RFC 5322 form with a fixed `GMT` zone;
//! * the same string again inside TLSRPT report MIME and DSN bounces.
//!
//! Before this module there were **four byte-identical `+0000` formatters and a
//! fifth for `GMT`**, in four crates, and the drift duplication predicts had
//! already arrived: the weekday derivation was spelled **four different ways**,
//! and one of them was wrong. `web_content::render::rfc822_date` computed
//! `unix_secs.div_euclid(86400) as u64` and then indexed `% 7`, so every
//! pre-1970 timestamp got a weekday off by an arbitrary amount — the `as u64`
//! turns `-1` into `2^64 - 1`, whose residue mod 7 is 1 where the answer is 6.
//! Nothing caught it because no copy had a pre-epoch test, and each copy was
//! individually plausible.
//!
//! The month and weekday tables here are **wire tokens, not display text.**
//! They are ASCII constants fixed by the RFCs and must never be localized —
//! contrast `ui/events.md` § Where logic lives, which routes *human-facing*
//! month/weekday names through the i18n catalog. A localized `Date:` header is
//! a malformed header, so these two vocabularies are deliberately separate and
//! must stay that way.
//!
//! Civil-date arithmetic is not re-derived here either: it belongs to
//! [`crate::caltime::civil_from_days`], and a copy of it is merge-gated by a
//! dedicated dev-fleet date-wire-grammar checker.

use crate::caltime::civil_from_days;

/// RFC 5322 month abbreviations, `[0]` = January. Wire tokens — never localized.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// RFC 5322 weekday abbreviations, `[0]` = Sunday. Wire tokens — never localized.
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

/// The wire abbreviation for a 1-based month, or `None` outside `1..=12`.
pub fn month_abbrev(month: u32) -> Option<&'static str> {
    MONTHS.get(month.checked_sub(1)? as usize).copied()
}

/// The 1-based month for a wire abbreviation, or `None` if it is not one of the
/// twelve. Case-sensitive on purpose: RFC 5322 fixes the capitalisation, and a
/// parser that accepts `JAN` widens the input surface of whatever decision it
/// feeds (here, HTTP-signature freshness) to buy compatibility with a sender
/// that does not exist.
pub fn month_from_abbrev(abbrev: &str) -> Option<u32> {
    MONTHS
        .iter()
        .position(|m| *m == abbrev)
        .map(|i| i as u32 + 1)
}

/// The wire weekday abbreviation for a count of days since the Unix epoch.
///
/// 1970-01-01 was a Thursday, index 4 with Sunday = 0. `rem_euclid` rather than
/// `%` so pre-epoch day counts land on a non-negative index — the bug three of
/// the four pre-unification copies avoided and the fourth did not.
pub fn weekday_abbrev(days_since_epoch: i64) -> &'static str {
    WEEKDAYS[(days_since_epoch.rem_euclid(7) + 4).rem_euclid(7) as usize]
}

/// Split epoch seconds into the fields the grammar prints.
fn fields(unix_secs: i64) -> (&'static str, u32, &'static str, i32, i64, i64, i64) {
    let days = unix_secs.div_euclid(86_400);
    let sod = unix_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    (
        weekday_abbrev(days),
        day,
        MONTHS[(month - 1) as usize],
        year,
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60,
    )
}

/// Format epoch seconds as an RFC 5322 § 3.3 date-time in UTC, e.g.
/// `Sun, 25 May 2026 12:30:00 +0000`.
///
/// This is also the RFC 2822/822 form an RSS `pubDate` carries and the value a
/// DSN or TLSRPT `Date:` header takes.
pub fn format_rfc5322_date(unix_secs: i64) -> String {
    let (wday, d, mon, y, hh, mm, ss) = fields(unix_secs);
    format!("{wday}, {d:02} {mon} {y:04} {hh:02}:{mm:02}:{ss:02} +0000")
}

/// Format epoch seconds as an RFC 7231 § 7.1.1.1 `IMF-fixdate`, e.g.
/// `Thu, 01 Jan 1970 00:00:00 GMT`.
///
/// Identical to [`format_rfc5322_date`] but for the zone token, which RFC 7231
/// fixes to the literal `GMT`. Both directions of the ActivityPub HTTP
/// signature sign a `Date` header and Mastodon parses it with Ruby's strict
/// `Time.httpdate`, so a second formatter drifting by one character is a
/// rejected signature — which is exactly why there is only one.
pub fn format_http_date(unix_secs: i64) -> String {
    let (wday, d, mon, y, hh, mm, ss) = fields(unix_secs);
    format!("{wday}, {d:02} {mon} {y:04} {hh:02}:{mm:02}:{ss:02} GMT")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_thursday_in_both_forms() {
        assert_eq!(format_http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(format_rfc5322_date(0), "Thu, 01 Jan 1970 00:00:00 +0000");
    }

    #[test]
    fn the_two_forms_differ_only_in_the_zone_token() {
        for secs in [0, 1_700_000_000, 951_782_400, -1, i32::MAX as i64] {
            let http = format_http_date(secs);
            let mail = format_rfc5322_date(secs);
            assert_eq!(
                http.trim_end_matches("GMT"),
                mail.trim_end_matches("+0000"),
                "the grammar diverged at {secs}"
            );
        }
    }

    /// The defect that motivated the unification. `web_content::render`'s copy
    /// cast the epoch-day count to `u64` before taking `% 7`, so every pre-1970
    /// instant rendered an arbitrary weekday. 1969-12-31 00:00:00 UTC was a
    /// **Wednesday**.
    #[test]
    fn a_pre_epoch_instant_gets_the_right_weekday() {
        assert_eq!(
            format_rfc5322_date(-86_400),
            "Wed, 31 Dec 1969 00:00:00 +0000"
        );
        // The shape the broken copy produced: `as u64` maps -1 day to
        // 2^64-1, whose residue mod 7 is 1, not the correct 6.
        assert_eq!((-1i64).rem_euclid(7), 6);
        assert_eq!(((-1i64) as u64) % 7, 1);
    }

    #[test]
    fn leap_day_2024() {
        // 2024-02-29 12:34:56 UTC = 1709210096.
        assert_eq!(
            format_http_date(1_709_210_096),
            "Thu, 29 Feb 2024 12:34:56 GMT"
        );
    }

    #[test]
    fn every_field_is_zero_padded_to_fixed_width() {
        // RFC 7231 calls IMF-fixdate fixed-length; Ruby's Time.httpdate agrees.
        for secs in [0, 1, 3_600, 1_700_000_000, -86_400] {
            assert_eq!(format_http_date(secs).len(), 29, "at {secs}");
            assert_eq!(format_rfc5322_date(secs).len(), 31, "at {secs}");
        }
    }

    #[test]
    fn month_abbrev_round_trips_and_refuses_out_of_range() {
        for m in 1..=12u32 {
            let a = month_abbrev(m).expect("in range");
            assert_eq!(month_from_abbrev(a), Some(m));
        }
        assert_eq!(month_abbrev(0), None);
        assert_eq!(month_abbrev(13), None);
        assert_eq!(month_from_abbrev("JAN"), None);
        assert_eq!(month_from_abbrev("Smarch"), None);
    }

    #[test]
    fn the_weekday_advances_by_one_per_day_without_wrapping_wrong() {
        // Seven consecutive days must name seven distinct weekdays, and day 7
        // must come back round — across the epoch boundary, where the broken
        // copy failed.
        for start in [-10i64, -1, 0, 1, 20_000] {
            let names: Vec<_> = (start..start + 7).map(weekday_abbrev).collect();
            let mut sorted = names.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), 7, "duplicate weekday in {names:?}");
            assert_eq!(weekday_abbrev(start + 7), names[0]);
        }
    }
}
