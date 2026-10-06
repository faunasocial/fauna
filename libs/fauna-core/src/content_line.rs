//! The DAV **content-line** grammar primitives — one home for the quoting rule
//! iCalendar and vCard share.
//!
//! RFC 5545 § 3.1 (iCalendar) and RFC 6350 § 3.3 (vCard) define the *same*
//! content line:
//!
//! ```text
//! NAME;PARAM=value;PARAM="quoted, value":the property value
//! ```
//!
//! and the same escape hatch for it: a parameter value that contains a `:`,
//! `;` or `,` **must** be wrapped in DQUOTE, and inside those quotes those
//! characters are ordinary text. So every scan over the head of a content line
//! has to track quoting, and getting it wrong does not fail loudly — it
//! silently cuts the line in the wrong place and hands the caller a truncated
//! value.
//!
//! That rule used to be written twice, once in [`crate::ical`] and once inside
//! `fauna-client-carddav`'s vCard parser, which is how a calendar parameter and
//! a contact parameter could come to disagree about the same syntax. It lives
//! here instead: pure `&str` work, no dependencies, WASM-safe, so the vCard side
//! keeps the "no native-only slice" property its crate docs claim.
//!
//! **Not in scope, deliberately:** `ical::extract_tzid` still splits its
//! parameter list on a bare `;`. A quote-aware split would be more correct in
//! principle, but a TZID is an IANA tz identifier (`Europe/Oslo`) or a Windows
//! zone name, and neither can contain a `;` — so the naive split cannot
//! misparse a real value, and swapping it would be churn dressed as a fix. If a
//! *different* parameter ever needs splitting, use [`split_unquoted`].

/// The index of the first `:` that is not inside a DQUOTE-quoted parameter
/// value — the boundary between a content line's `NAME;PARAM=…` head and its
/// value.
///
/// Returns `None` for a line with no unquoted colon, which is a malformed
/// content line rather than an empty one: callers treat it as "not a property".
#[must_use]
pub fn find_value_colon(line: &str) -> Option<usize> {
    let mut in_quotes = false;
    for (i, ch) in line.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ':' if !in_quotes => return Some(i),
            _ => {}
        }
    }
    None
}

/// Split `s` on `sep`, ignoring separators inside a DQUOTE-quoted span.
///
/// The DQUOTEs are **kept** in the output — the caller decides whether to strip
/// them, because the two uses differ: a param *value* list wants them gone, a
/// re-serialized param wants them intact. No backslash escaping: the content
/// line's head has none (RFC 6350 § 3.3), only the DQUOTE span.
///
/// Always returns at least one element, so `split_unquoted("", ',')` is
/// `[""]` — the same shape `str::split` gives, so a caller can swap one for the
/// other without a new empty-input branch.
/// Undo RFC 5545 §3.1 / RFC 6350 §3.2 line folding: a line beginning with a
/// single SPACE or TAB is a continuation of the prior line, and that one
/// whitespace byte is the fold marker, not content — it is stripped, not kept.
///
/// Was written twice, once in [`crate::ical`] and once in
/// `fauna-client-carddav`'s vCard parser: both formats fold physical lines the
/// same way, inherited from the RFC 5322 message-header grammar. Pure `&str`
/// work, no dependencies, WASM-safe.
#[must_use]
pub fn unfold_lines(text: &str) -> String {
    // Normalize line endings to LF first
    let text = text.replace("\r\n", "\n");
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\n' {
            // Check if next char is a space or tab (continuation)
            if let Some(&next) = chars.peek()
                && (next == ' ' || next == '\t')
            {
                // Skip the newline and the whitespace — continuation
                chars.next();
                continue;
            }
        }
        result.push(ch);
    }
    result
}

#[must_use]
pub fn split_unquoted(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    for c in s.chars() {
        if c == '"' {
            in_quote = !in_quote;
            cur.push(c);
        } else if c == sep && !in_quote {
            out.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the module: the separator inside DQUOTEs is text.
    /// Drop the quote tracking and a naive `find(':')` answers **20** on the
    /// first line below — the colon inside `"mailto:a@b"` — so the parser would
    /// read the property value as `a@b":value` and the param as a torn
    /// fragment. Neither half is empty, so nothing downstream would notice.
    #[test]
    fn find_value_colon_skips_colons_inside_quotes() {
        // The closing DQUOTE sits at 24, so the real boundary is 25.
        assert_eq!(
            find_value_colon("ATTENDEE;URI=\"mailto:a@b\":value"),
            Some(25)
        );
        assert_eq!(find_value_colon("SUMMARY:plain"), Some(7));
    }

    #[test]
    fn find_value_colon_is_none_without_an_unquoted_colon() {
        assert_eq!(find_value_colon("NAME;PARAM=\"a:b\""), None);
        assert_eq!(find_value_colon(""), None);
    }

    #[test]
    fn split_unquoted_keeps_quoted_separators_together() {
        assert_eq!(split_unquoted("a,b,c", ','), vec!["a", "b", "c"]);
        assert_eq!(
            split_unquoted("\"x,y\",z", ','),
            vec!["\"x,y\"".to_string(), "z".to_string()],
            "a quoted comma is data, and the DQUOTEs survive for the caller to strip"
        );
        assert_eq!(
            split_unquoted("TYPE=\"work;home\";PREF=1", ';'),
            vec!["TYPE=\"work;home\"".to_string(), "PREF=1".to_string()],
        );
    }

    /// Pins the `str::split`-shaped contract the doc comment promises, so a
    /// caller swapping one for the other needs no empty-input branch.
    #[test]
    fn split_unquoted_always_yields_at_least_one_element() {
        assert_eq!(split_unquoted("", ','), vec!["".to_string()]);
        assert_eq!(
            split_unquoted(",", ','),
            vec!["".to_string(), "".to_string()]
        );
    }

    /// An unbalanced DQUOTE is malformed input, and the rule that matters is
    /// that it cannot panic or lose the tail — the scan simply treats the rest
    /// of the line as quoted.
    #[test]
    fn unbalanced_quote_does_not_panic_or_drop_the_tail() {
        assert_eq!(find_value_colon("NAME;P=\"open:still"), None);
        assert_eq!(
            split_unquoted("a,\"open,tail", ','),
            vec!["a".to_string(), "\"open,tail".to_string()]
        );
    }

    /// One leading whitespace byte on a continuation line is the fold marker
    /// and is stripped; a second leading space is content and survives as the
    /// join — the exact case that once diverged between the iCalendar and
    /// vCard copies of this function before it was shared.
    #[test]
    fn unfold_lines_strips_exactly_one_fold_marker_byte() {
        assert_eq!(unfold_lines("NOTE:hello\n  world\n"), "NOTE:hello world\n");
        assert_eq!(
            unfold_lines("NOTE:hello\n\tworld\n"),
            "NOTE:helloworld\n",
            "a single fold-marker byte carries no content of its own — \
             nothing survives as a separator when it's the only whitespace present"
        );
        assert_eq!(
            unfold_lines("NOTE:one\r\n  two\r\n"),
            "NOTE:one two\n",
            "CRLF-folded input normalizes to LF and unfolds the same way"
        );
    }
}
