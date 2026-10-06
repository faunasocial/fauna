//! `INTERNALDATE` → epoch seconds.
//!
//! RFC 3501 §9 `date-time`: `"17-Jul-1996 02:44:25 -0700"` — a fixed grammar
//! with a space-padded or two-digit day, a three-letter English month, and a
//! mandatory four-digit zone offset.
//!
//! Hand-parsed rather than delegated to `chrono` on purpose. The workspace
//! pins `chrono` with its `clock` feature, which reaches for
//! `std::time::SystemTime` — fine natively, a runtime panic on
//! `wasm32-unknown-unknown`. We need *parsing*, never *now*, so this module
//! avoids anything clock-touching and the whole import client remains
//! WASM-safe — the civil-calendar math below delegates to
//! [`fauna_core::caltime::days_from_civil`] and
//! [`fauna_core::caltime::days_in_month`], themselves pure arithmetic with no
//! platform glue (`caltime.rs`'s own module doc). (`ImportMessageItem.
//! timestamp` is `i64` epoch seconds, so pre-1970 source mail — it exists —
//! must stay representable and negative.)

use super::ImapClientError;

/// Parse an IMAP `date-time` into epoch seconds (UTC).
///
/// Tolerates surrounding whitespace and the enclosing `DQUOTE`s, so it accepts
/// both the raw wire token and the unquoted value a parser hands back.
/// Month matching is ASCII-case-insensitive: the RFC fixes the case, but
/// real-world servers do not.
pub fn parse_internal_date(s: &str) -> Result<i64, ImapClientError> {
    let t = s.trim().trim_matches('"').trim();
    let bad = || ImapClientError::Protocol(format!("bad INTERNALDATE {s:?}"));

    // `dd-Mon-yyyy hh:mm:ss +zzzz` — split on the two spaces, then on '-'
    // inside the date. A space-padded day (" 7-Feb-1994") is why we trim first
    // and split_whitespace rather than slicing at fixed offsets.
    let mut parts = t.split(' ').filter(|p| !p.is_empty());
    let date = parts.next().ok_or_else(bad)?;
    let time = parts.next().ok_or_else(bad)?;
    let zone = parts.next().ok_or_else(bad)?;
    if parts.next().is_some() {
        return Err(bad());
    }

    let mut d = date.split('-');
    let (day, mon, year) = (
        d.next().ok_or_else(bad)?,
        d.next().ok_or_else(bad)?,
        d.next().ok_or_else(bad)?,
    );
    if d.next().is_some() {
        return Err(bad());
    }

    let day: i64 = day.parse().map_err(|_| bad())?;
    let year: i64 = year.parse().map_err(|_| bad())?;
    let month = month_number(mon).ok_or_else(bad)?;
    // RFC 3501 §9 `date-year = 4DIGIT`. Bounding it is not cosmetic: every other
    // field is range-checked, but an unbounded year overflows `days_from_civil`
    // (a source past ~10^11 wraps i64 in debug-panic / release-garbage), so a
    // hostile or broken source server could crash the import or store a garbage
    // timestamp. Reject it like a bad day or hour.
    if !(1..=31).contains(&day) || !(1..=9999).contains(&year) {
        return Err(bad());
    }
    if day > fauna_core::caltime::days_in_month(year as i32, month as u32) as i64 {
        return Err(bad());
    }

    let mut hms = time.split(':');
    let (h, mi, sec) = (
        hms.next().ok_or_else(bad)?,
        hms.next().ok_or_else(bad)?,
        hms.next().ok_or_else(bad)?,
    );
    if hms.next().is_some() {
        return Err(bad());
    }
    let h: i64 = h.parse().map_err(|_| bad())?;
    let mi: i64 = mi.parse().map_err(|_| bad())?;
    // Leap second: RFC 3501 permits `60`; clamp rather than reject.
    let sec: i64 = sec.parse().map_err(|_| bad())?;
    if h > 23 || mi > 59 || sec > 60 {
        return Err(bad());
    }

    let zone_secs = parse_zone(zone).ok_or_else(bad)?;

    let days = days_from_civil(year, month, day);
    Ok(days * 86_400 + h * 3_600 + mi * 60 + sec.min(59) - zone_secs)
}

/// `+HHMM` / `-HHMM` → offset in seconds east of UTC.
fn parse_zone(z: &str) -> Option<i64> {
    let b = z.as_bytes();
    if b.len() != 5 {
        return None;
    }
    let sign = match b[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    if !b[1..].iter().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hh: i64 = z[1..3].parse().ok()?;
    let mm: i64 = z[3..5].parse().ok()?;
    if hh > 23 || mm > 59 {
        return None;
    }
    Some(sign * (hh * 3_600 + mm * 60))
}

fn month_number(m: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    if m.len() != 3 {
        return None;
    }
    let lower = m.to_ascii_lowercase();
    MONTHS
        .iter()
        .position(|x| *x == lower)
        .map(|i| i as i64 + 1)
}

/// Days since 1970-01-01 for a proleptic-Gregorian y/m/d. The caller bounds
/// `year` to `1..=9999` and `month`/`day` to their valid ranges before
/// calling, so the narrowing casts here are lossless.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    fauna_core::caltime::days_from_civil(y as i32, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ground truth from `date -u -d '<iso>' +%s`.
    #[test]
    fn rfc3501_examples_match_coreutils() {
        assert_eq!(
            parse_internal_date("17-Jul-1996 02:44:25 -0700").unwrap(),
            837_596_665
        );
        // Space-padded single-digit day — the `date-day-fixed` alternative.
        assert_eq!(
            parse_internal_date(" 7-Feb-1994 22:43:04 -0800").unwrap(),
            760_689_784
        );
        assert_eq!(
            parse_internal_date("10-Jul-2026 12:00:00 +0200").unwrap(),
            1_783_677_600
        );
    }

    #[test]
    fn epoch_and_pre_epoch_round_trip() {
        assert_eq!(
            parse_internal_date("01-Jan-1970 00:00:00 +0000").unwrap(),
            0
        );
        // Pre-1970 source mail must stay representable and negative — the wire
        // field is `i64`, not `u64`.
        assert_eq!(
            parse_internal_date("31-Dec-1969 23:59:59 +0000").unwrap(),
            -1
        );
    }

    #[test]
    fn leap_day_is_accepted_and_correct() {
        assert_eq!(
            parse_internal_date("29-Feb-2000 12:00:00 +0000").unwrap(),
            951_825_600
        );
        // 1900 is not a leap year (the %100 rule), so 29-Feb-1900 does not exist.
        assert!(parse_internal_date("29-Feb-1900 00:00:00 +0000").is_err());
        assert!(parse_internal_date("29-Feb-2001 00:00:00 +0000").is_err());
    }

    #[test]
    fn quotes_and_case_are_tolerated() {
        let quoted = parse_internal_date("\"17-Jul-1996 02:44:25 -0700\"").unwrap();
        let bare = parse_internal_date("17-Jul-1996 02:44:25 -0700").unwrap();
        assert_eq!(quoted, bare);
        assert_eq!(
            parse_internal_date("17-JUL-1996 02:44:25 -0700").unwrap(),
            bare
        );
    }

    #[test]
    fn zone_offset_is_applied_in_the_right_direction() {
        // A positive zone is *east* of UTC, so the same wall clock is an
        // earlier instant. Sign errors here silently shift every imported
        // message's date, so pin the direction.
        let east = parse_internal_date("01-Jan-2000 12:00:00 +0100").unwrap();
        let utc = parse_internal_date("01-Jan-2000 12:00:00 +0000").unwrap();
        let west = parse_internal_date("01-Jan-2000 12:00:00 -0100").unwrap();
        assert_eq!(utc - east, 3_600);
        assert_eq!(west - utc, 3_600);
    }

    #[test]
    fn leap_second_clamps_rather_than_rejects() {
        // RFC 3501 permits `60`; POSIX epoch has no leap seconds, so clamp.
        assert_eq!(
            parse_internal_date("31-Dec-1998 23:59:60 +0000").unwrap(),
            parse_internal_date("31-Dec-1998 23:59:59 +0000").unwrap()
        );
    }

    #[test]
    fn malformed_inputs_are_rejected_not_guessed() {
        for bad in [
            "",
            "17-Jul-1996 02:44:25",        // no zone
            "17-Jul-1996 02:44:25 -07:00", // colon in zone
            "17-Jul-1996 02:44:25 GMT",    // named zone
            "17-Juk-1996 02:44:25 -0700",  // bogus month
            "17-Jul-1996 24:00:00 +0000",  // hour out of range
            "17-Jul-1996 02:60:00 +0000",  // minute out of range
            "32-Jul-1996 02:44:25 +0000",  // day out of range
            "31-Apr-1996 02:44:25 +0000",  // April has 30 days
            "17-Jul-1996 02:44 +0000",     // no seconds
            "17-Jul-1996 02:44:25 +0000 xtra",
            // A source-supplied year past the RFC 3501 `date-year = 4DIGIT`
            // grammar. Every other field is range-checked; an unbounded year
            // overflows `days_from_civil`'s i64 arithmetic (debug panic /
            // release wrap to a garbage epoch), so it must be rejected like a
            // bad day or hour, not multiplied.
            "01-Jan-1000000000000 00:00:00 +0000",
            "01-Jan-99999999999999999999 00:00:00 +0000", // also i64::parse overflow
        ] {
            assert!(
                parse_internal_date(bad).is_err(),
                "should have rejected {bad:?}"
            );
        }
    }
}
