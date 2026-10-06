//! Text normalization shared by every parser: the Facebook byte-wise
//! mojibake repair (`archive-import.md` § Parser contract rule 6), whitespace
//! normalization, and the `name_key` matching key.

use std::borrow::Cow;

/// Facebook's JSON writer escapes each UTF-8 byte as its own `\u00XX`, so
/// after JSON decoding `ř` (UTF-8 `C5 99`) is the two chars U+00C5 U+0099.
/// Repair: when every char fits in a byte and at least one is ≥ U+0080,
/// re-encode as Latin-1 bytes and decode as UTF-8; anything that does not
/// decode was never mojibake and is returned untouched. Undoes exactly ONE
/// level of escaping — the one pass Facebook applies; a second application
/// is a no-op whenever the repaired text holds any char above U+00FF, but a
/// doubly-escaped input is, by design, repaired one level per call (a
/// fixed-point loop would mis-repair genuinely mojibake-shaped text).
pub fn repair_facebook_mojibake(s: &str) -> Cow<'_, str> {
    let mut saw_high = false;
    for c in s.chars() {
        let cp = c as u32;
        if cp > 0xFF {
            return Cow::Borrowed(s);
        }
        if cp >= 0x80 {
            saw_high = true;
        }
    }
    if !saw_high {
        return Cow::Borrowed(s);
    }
    let bytes: Vec<u8> = s.chars().map(|c| c as u32 as u8).collect();
    match String::from_utf8(bytes) {
        Ok(repaired) => Cow::Owned(repaired),
        Err(_) => Cow::Borrowed(s),
    }
}

/// Trims and collapses every run of whitespace to one space.
pub fn normalize_text(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The matching key for a display name: normalized, then Unicode-lowercased.
/// (No NFC normalization: that needs a crate this crate does not carry; the
/// platforms emit composed forms in practice.)
pub fn name_key(display_name: &str) -> String {
    normalize_text(display_name).to_lowercase()
}

/// What every parsed string field goes through: repair, then normalize.
pub fn clean(s: &str) -> String {
    normalize_text(&repair_facebook_mojibake(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repairs_two_byte_sequence_to_r_caron() {
        // ř = UTF-8 C5 99, exported as "Å".
        assert_eq!(repair_facebook_mojibake("\u{c5}\u{99}"), "ř");
        assert_eq!(
            repair_facebook_mojibake(
                "Dobr\u{c3}\u{bd} den, p\u{c5}\u{99}\u{c3}\u{a1}tel\u{c3}\u{a9}"
            ),
            "Dobrý den, přátelé"
        );
    }

    #[test]
    fn repairs_three_byte_sequence_to_heart() {
        // ❤ = UTF-8 E2 9D A4.
        assert_eq!(repair_facebook_mojibake("\u{e2}\u{9d}\u{a4}"), "❤");
    }

    #[test]
    fn repairs_four_byte_sequence_to_emoji() {
        // 😀 = UTF-8 F0 9F 98 80.
        assert_eq!(repair_facebook_mojibake("\u{f0}\u{9f}\u{98}\u{80}"), "😀");
    }

    #[test]
    fn leaves_ascii_alone_without_allocating() {
        assert!(matches!(
            repair_facebook_mojibake("plain ascii"),
            Cow::Borrowed(_)
        ));
        assert!(matches!(repair_facebook_mojibake(""), Cow::Borrowed(s) if s.is_empty()));
    }

    #[test]
    fn leaves_real_unicode_alone() {
        // A char above U+00FF cannot be a byte escape: not mojibake.
        assert_eq!(repair_facebook_mojibake("přátelé ❤"), "přátelé ❤");
        assert!(matches!(
            repair_facebook_mojibake("přátelé ❤"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn leaves_invalid_utf8_alone() {
        // A lone é (E9) is not a valid UTF-8 lead byte sequence: untouched.
        assert_eq!(repair_facebook_mojibake("caf\u{e9}"), "caf\u{e9}");
    }

    #[test]
    fn repair_is_a_no_op_on_repaired_text_with_chars_above_u00ff() {
        let once = repair_facebook_mojibake("\u{c5}\u{99}").into_owned();
        assert_eq!(repair_facebook_mojibake(&once), once);
    }

    #[test]
    fn repair_undoes_exactly_one_level_of_escaping() {
        // Doubly-escaped U+0080: one call undoes one level, never more.
        let once = repair_facebook_mojibake("\u{c3}\u{82}\u{c2}\u{80}").into_owned();
        assert_eq!(once, "\u{c2}\u{80}");
        assert_eq!(repair_facebook_mojibake(&once), "\u{80}");
    }

    #[test]
    fn normalize_collapses_whitespace() {
        assert_eq!(normalize_text("  a \t b\n\nc  "), "a b c");
        assert_eq!(normalize_text(""), "");
    }

    #[test]
    fn name_key_lowercases_after_normalizing() {
        assert_eq!(name_key("  Friend   One "), "friend one");
        assert_eq!(name_key("ŘEHOŘ"), "řehoř");
    }

    #[test]
    fn clean_repairs_then_normalizes() {
        assert_eq!(
            clean("  p\u{c5}\u{99}\u{c3}\u{a1}tel\u{c3}\u{a9}   \n"),
            "přátelé"
        );
    }
}
