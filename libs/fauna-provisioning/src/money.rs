//! Shared decimal-money-string parsing for the registrar/VPS provider APIs.

/// Parse a decimal-money string ("8.57", "11", "0.00") into integer cents.
/// The whole/dollars part is required; the fractional part (if present) is
/// truncated or zero-padded to exactly 2 digits ("5" → 50, "573" → 57).
/// Returns `None` on unparseable input.
///
/// The shared core behind every registrar/VPS provider's own typed wrapper —
/// callers with a distinct failure contract (a `Result` with a formatted
/// error, a zero-means-"not-applicable" filter) apply that on top rather than
/// this function growing provider-specific parameters.
pub(crate) fn parse_decimal_to_cents(s: &str) -> Option<u64> {
    let s = s.trim();
    let (whole_s, frac_s) = match s.split_once('.') {
        Some((w, f)) => (w, f),
        None => (s, "0"),
    };
    let whole: u64 = whole_s.parse().ok()?;
    let frac: u64 = match frac_s.len() {
        0 => 0,
        1 => format!("{frac_s}0").parse().ok()?,
        _ => frac_s.get(..2)?.parse().ok()?,
    };
    Some(whole * 100 + frac)
}

/// Render integer cents as the decimal amount (`973` → `"9.73"`) — the inverse
/// of [`parse_decimal_to_cents`], for a price named in an error message.
pub(crate) fn format_cents(cents: u64) -> String {
    format!("{}.{:02}", cents / 100, cents % 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_cents_as_a_decimal_amount() {
        assert_eq!(format_cents(973), "9.73");
        assert_eq!(format_cents(1100), "11.00");
        assert_eq!(format_cents(5), "0.05");
    }

    #[test]
    fn parses_whole_and_fractional_cents() {
        assert_eq!(parse_decimal_to_cents("8.57"), Some(857));
        assert_eq!(parse_decimal_to_cents("11.00"), Some(1100));
        assert_eq!(parse_decimal_to_cents("11"), Some(1100));
    }

    #[test]
    fn pads_a_single_fractional_digit() {
        assert_eq!(parse_decimal_to_cents("4.5"), Some(450));
    }

    #[test]
    fn truncates_more_than_two_fractional_digits() {
        assert_eq!(parse_decimal_to_cents("9.735"), Some(973));
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(parse_decimal_to_cents(" 8.57 "), Some(857));
    }

    #[test]
    fn rejects_unparseable_input() {
        assert_eq!(parse_decimal_to_cents("free"), None);
        assert_eq!(parse_decimal_to_cents(""), None);
    }

    #[test]
    fn zero_parses_to_zero_cents() {
        // The "0 means not applicable" filter is a caller-side concern
        // (namecheap.rs), not this shared core's.
        assert_eq!(parse_decimal_to_cents("0.00"), Some(0));
    }
}
