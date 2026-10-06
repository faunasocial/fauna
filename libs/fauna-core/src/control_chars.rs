//! Control-character stripping — the project's **one** implementation.
//!
//! Two planes call this, and both render text into **terminals**, where a
//! control character is not data but an instruction:
//!
//! 1. **The log plane** — both `fauna_log` rings strip at their storage
//!    boundary, and the nest's sidecar-plane admission (`log_plane::admit`)
//!    calls it ahead of its byte cap so the cap counts real content
//!    (`observability.md` § Ring-ingest sanitization). Log writes are
//!    user-influenceable: the proven case is a non-admin's raw URL logged on
//!    the nest's media-proxy rejection path.
//! 2. **The tui render funnel** — `apps/fauna-tui/src/ui.rs` strips every
//!    painted line, because remote-authored content (a post body, a
//!    remote-chosen display name, third-party OpenGraph `og:title` /
//!    `og:description`) reaches the cell grid there (`tui.md` § Rendering).
//!
//! A probe established that ratatui's cell grid preserves ESC/BEL **verbatim**, so an unstripped escape is a
//! terminal injection into the reader's screen and a bare newline forges row
//! structure.
//!
//! **Why this lives in `fauna-core` rather than in either caller.** It was
//! `fauna_log`'s private helper while logs were the only consumer. The render
//! funnel is a *content* boundary, not a log one, and making it depend on the
//! log crate to sanitize a post body would be backwards — but so would a second
//! copy of the filter, since the security property is exactly "there is one
//! strip and every boundary calls it". `fauna-core` is the shared, wasm-safe
//! floor both planes already depend on, so it is the one home that keeps the
//! single-implementation property. `fauna_log` re-exports it for its callers.

use std::borrow::Cow;

/// Remove every control character (`char::is_control` — C0 including `\n`/`\t`,
/// ESC and BEL, DEL, and the C1 range, whose `0x9B` is a one-byte CSI on
/// terminals that honour it) from `s`.
///
/// Borrowed when already clean, so the cost on a hot path (per log event, per
/// painted line) is one scan and no allocation.
///
/// ⚠ **`\n` is stripped too, so callers that treat newlines as *structure* must
/// split on them FIRST.** The tui funnel does exactly that: `ui.rs` splits an
/// element body into one `Line` per text row before stripping, because a `Line`
/// built from a string containing `\n` collapses every row onto one terminal row
/// (the bug that once painted the identity-export QR as an unscannable strip).
/// After that split a surviving `\n` could only forge row structure, which is
/// the same reason the log plane strips it.
pub fn strip_control_chars(s: &str) -> Cow<'_, str> {
    if s.contains(|c: char| c.is_control()) {
        Cow::Owned(s.chars().filter(|c| !c.is_control()).collect())
    } else {
        Cow::Borrowed(s)
    }
}

/// Is `c` a Unicode format or line-separator character that renders no glyph of
/// its own but steers the text around it — the bidi embeddings, overrides and
/// isolates (U+202A–U+202E, U+2066–U+2069), the directional marks (U+200E/F,
/// U+061C), the zero-width and joiner family (U+200B–U+200D, U+2060–U+2064,
/// U+FEFF), the soft hyphen, and the line/paragraph separators U+2028/U+2029?
/// `char::is_control` (category Cc) misses every one of them.
fn is_steering_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
    )
}

/// Reduce text a stranger chose to **plain single-line text** of at most
/// `max_chars` characters, for a surface that paints it outside the app's own
/// rendering (an OS toast body — `behavior/notifications.md` § Localized body →
/// the knock toast).
///
/// Newlines, tabs and the Unicode line/paragraph separators become one space
/// (a stranger must not forge extra lines under a trusted title); every other
/// control character ([`strip_control_chars`]) and every bidi/format character
/// is dropped; runs of whitespace collapse and the ends are trimmed; and the
/// result is cut on a **character** boundary, with `…` marking a cut.
pub fn sanitize_plain_line(s: &str, max_chars: usize) -> String {
    let spaced: String = s
        .chars()
        .map(|c| match c {
            '\n' | '\r' | '\t' | '\u{85}' | '\u{2028}' | '\u{2029}' => ' ',
            c => c,
        })
        .collect();
    let stripped = strip_control_chars(&spaced);
    let cleaned: String = stripped
        .chars()
        .filter(|c| !is_steering_format_char(*c))
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let mut cut: String = collapsed
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_line_drops_markup_steering_and_forged_lines() {
        // A bidi override, a newline faking a second line, ESC, a zero-width
        // joiner and U+2028; the `<a>` markup is text, left for the shell's
        // own escape.
        let s = "\u{202E}hi\n\nAlice (your contact):\x1b[2J\u{200D} ok\u{2028}now";
        let out = sanitize_plain_line(s, 200);
        assert_eq!(out, "hi Alice (your contact):[2J ok now");
        assert!(
            !out.chars()
                .any(|c| c.is_control() || is_steering_format_char(c))
        );
    }

    #[test]
    fn plain_line_caps_on_a_char_boundary() {
        let out = sanitize_plain_line(&"é😀".repeat(5_000), 10);
        assert_eq!(out.chars().count(), 10);
        assert!(out.ends_with('…'));
        // Under the cap: untouched.
        assert_eq!(sanitize_plain_line("short", 10), "short");
    }

    #[test]
    fn clean_text_is_borrowed_not_reallocated() {
        assert!(matches!(
            strip_control_chars("plain text"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn strips_the_escape_classes_a_terminal_acts_on() {
        // CSI (ESC [), OSC (ESC ] … BEL), a one-byte C1 CSI, DEL, newline, tab.
        let payload = "a\x1b[2Jb\x1b]0;title\x07c\u{9b}1md\x7fe\nf\tg";
        // The escapes' *introducers* die; their printable remainder is left as
        // inert text, which is the point — the terminal never acts on it.
        assert_eq!(strip_control_chars(payload), "a[2Jb]0;titlec1mdefg");
    }

    /// The property the callers actually depend on, stated directly: nothing
    /// `char::is_control` survives. Asserting the exact output string above is
    /// brittle to how the escapes' *printable* remainder reads; this is the
    /// invariant.
    #[test]
    fn no_control_character_survives() {
        let payload = "a\x1b[2Jb\x1b]0;title\x07c\u{9b}1md\x7fe\nf\tg";
        let stripped = strip_control_chars(payload);
        assert!(!stripped.chars().any(char::is_control));
        // The printable content is kept — a strip that dropped everything would
        // satisfy the assertion above vacuously.
        assert!(stripped.contains('a') && stripped.contains('g'));
    }
}
