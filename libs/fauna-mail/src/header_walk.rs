//! Continuation-aware RFC 5322 header-section walking — the one shared
//! primitive behind every consumer that rewrites or reads a header section by
//! field: [`crate::received_header::strip_fauna_headers`] (the inbound
//! `X-Fauna-*` forgery strip), [`crate::outbound::received_strip`] (strip
//! `Received:` before relay), [`crate::lists::stamp`] (strip the client
//! `List-*` set, prepend the nest's authoritative one),
//! `crate::aliases::read_spam_threshold_stamp` (read the delivery-time
//! spam-threshold stamp back off a message), and the mailbox export's
//! transit strip, mbox flag-header strip and `first_header_value`. Pure std,
//! no deps. Gated to its consumers so it never compiles dead.
//!
//! **Line framing agrees with the readers downstream.** A line ends at LF,
//! with or without a preceding CR; a bare CR inside a line is content; a line
//! starting with SP or HTAB continues the field before it; the header section
//! ends at the first empty line. That is how the parser (`parse_rfc5322`)
//! reads a message and how `crate::from_field` counts one, so a walker framing
//! on CRLF alone would see a bare-LF header section as ONE field named by its
//! first header — and a strip keyed on field names would pass every header
//! after it to the parser untouched. On CRLF input the framing is the
//! RFC 5322 §2.2 one, so a conformant message walks exactly as it always did.
//!
//! Was duplicated inside `received_strip`; lifted here when the list-mode
//! submission path needed the identical walk (priority #2 — one walk, no
//! drift). The `lists` feature must stay WASM-safe (no `outbound` stack), so
//! the primitive lives here un-coupled to either feature's heavier deps.

/// Byte offset one past the LF ending the line that starts at `start`, or
/// `raw.len()` when the line runs to the end of the input.
fn line_end(raw: &[u8], start: usize) -> usize {
    raw[start..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(raw.len(), |p| start + p + 1)
}

/// Returns the byte offset of the start of the body (one past the empty line
/// that ends the header section), or `None` if `raw` holds no empty line —
/// the whole input is then header section.
pub(crate) fn find_body_offset(raw: &[u8]) -> Option<usize> {
    let mut start = 0;
    while start < raw.len() {
        let end = line_end(raw, start);
        if raw[end - 1] != b'\n' {
            return None; // Last line, unterminated — no separator.
        }
        let line = &raw[start..end - 1];
        if line.strip_suffix(b"\r").unwrap_or(line).is_empty() {
            return Some(end);
        }
        start = end;
    }
    None
}

/// Parses one logical header from the start of `slice`. Returns
/// `(end_offset, field_name)` where `end_offset` is the byte position
/// immediately after the header's last line ending (so subsequent calls can
/// resume from there) and `field_name` is the bytes before the colon on its
/// first line, surrounding ASCII whitespace trimmed (the parser reads `From :`
/// and a CR-led `\rFrom:` as `From` too). A first line without a colon has an
/// empty name. Never returns an `end_offset` of 0 for a non-empty `slice`.
///
/// Continuation lines (starting with SP or HTAB) are part of the same logical
/// header. End is the first line ending that's NOT followed by SP/HTAB.
pub(crate) fn parse_header(slice: &[u8]) -> (usize, &[u8]) {
    let first_line = &slice[..line_end(slice, 0)];
    let name = first_line
        .iter()
        .position(|&b| b == b':')
        .map_or(&[][..], |colon| first_line[..colon].trim_ascii());

    let mut end = first_line.len();
    while end < slice.len() && matches!(slice[end], b' ' | b'\t') {
        end = line_end(slice, end);
    }
    (end, name)
}

/// Filters `raw`'s header section, dropping every header for which
/// `should_drop` returns `true`; the body (after the empty line ending the
/// header section, or the whole input if there is none) is copied through
/// unchanged, and every kept header keeps its own line endings. Fail-safe:
/// if the walker can't make progress on a malformed header, the remainder is
/// copied verbatim rather than looping or silently dropping data. Shared by
/// [`crate::received_header::strip_fauna_headers`] (drops sender-forged
/// `X-Fauna-*` stamps), [`crate::outbound::received_strip::strip_received_headers`]
/// (drops `Received:`) and the mailbox export's transit and mbox flag-header
/// strips — same walk, different predicate. Gated to exactly those
/// consumers' features: the module also compiles under `lists` and
/// `aliases` (for `find_body_offset`/`parse_header`), where this function
/// would be dead code.
#[cfg(any(
    feature = "mail-export",
    feature = "outbound",
    feature = "received-header"
))]
pub(crate) fn strip_headers_where(
    raw: &[u8],
    mut should_drop: impl FnMut(&[u8]) -> bool,
) -> Vec<u8> {
    let body_offset = find_body_offset(raw).unwrap_or(raw.len());
    let header_section = &raw[..body_offset];
    let body_section = &raw[body_offset..];

    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < header_section.len() {
        let (header_end, name) = parse_header(&header_section[i..]);
        let absolute_end = i + header_end;
        let header_bytes = &header_section[i..absolute_end];

        if !should_drop(name) {
            out.extend_from_slice(header_bytes);
        }

        if header_end == 0 {
            // Walker stalled — copy the remainder verbatim (fail-safe).
            out.extend_from_slice(&header_section[i..]);
            break;
        }
        i = absolute_end;
    }

    out.extend_from_slice(body_section);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_offset_finds_separator() {
        assert_eq!(find_body_offset(b"A: b\r\n\r\nbody"), Some(8));
        assert_eq!(find_body_offset(b"A: b\r\nno-sep"), None);
    }

    #[test]
    fn parse_header_single_line() {
        let (end, name) = parse_header(b"From: a@x\r\nNext: y\r\n");
        assert_eq!(name, b"From");
        assert_eq!(end, "From: a@x\r\n".len()); // past the first CRLF (11)
    }

    #[test]
    fn parse_header_with_continuation() {
        let (end, name) = parse_header(b"X: one\r\n two\r\nNext: y\r\n");
        assert_eq!(name, b"X");
        // The continuation (" two\r\n") travels with the header.
        assert_eq!(end, "X: one\r\n two\r\n".len());
    }

    #[test]
    fn a_line_ends_at_lf_with_or_without_cr() {
        assert_eq!(find_body_offset(b"A: b\n\nbody"), Some(6));
        assert_eq!(find_body_offset(b"A: b\r\nC: d\n\r\nbody"), Some(13));
        assert_eq!(find_body_offset(b"A: b\r\n\nbody"), Some(7));
        assert_eq!(find_body_offset(b"\nbody"), Some(1));
        assert_eq!(find_body_offset(b"A: b\nno-sep"), None);
        assert_eq!(parse_header(b"From: a@x\nNext: y\n"), (10, &b"From"[..]));
        assert_eq!(parse_header(b"X: one\n\ttwo\r\nNext: y\n"), (13, &b"X"[..]));
    }

    #[test]
    fn a_bare_cr_inside_a_line_is_content() {
        let (end, name) = parse_header(b"Subject: s\rX-Fauna-A: b\r\nNext: y\r\n");
        assert_eq!(name, b"Subject");
        assert_eq!(end, "Subject: s\rX-Fauna-A: b\r\n".len());
    }

    #[test]
    fn the_name_is_the_first_lines_trimmed_field_name() {
        assert_eq!(parse_header(b"From : a\r\n").1, b"From");
        assert_eq!(parse_header(b"\rFrom: a\r\n").1, b"From");
        // A colon-less first line names nothing; the colon on the next line
        // belongs to the next header.
        assert_eq!(parse_header(b"garbage\r\nFrom: a\r\n"), (9, &b""[..]));
    }

    /// The CRLF-only walk this module shipped before it learned bare LF, kept
    /// as the reference a conformant message must still walk identically to:
    /// the goldens, the outbound relay strip and the list stamp all hold
    /// CRLF bytes and must see no change.
    mod crlf_reference {
        pub(super) fn find_body_offset(raw: &[u8]) -> Option<usize> {
            raw.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
        }

        pub(super) fn parse_header_end(slice: &[u8]) -> usize {
            if !slice.contains(&b':') {
                return slice.len();
            }
            let mut pos = 0;
            while pos < slice.len() {
                let Some(p) = slice[pos..].windows(2).position(|w| w == b"\r\n") else {
                    return slice.len();
                };
                let next = pos + p + 2;
                if next >= slice.len() || !matches!(slice[next], b' ' | b'\t') {
                    return next;
                }
                pos = next;
            }
            slice.len()
        }
    }

    #[test]
    fn a_conformant_crlf_message_walks_exactly_as_the_crlf_only_walker_did() {
        let corpus: &[&[u8]] = &[
            b"From: a@x\r\nTo: b@y\r\nSubject: hi\r\n\r\nbody\r\n",
            b"Received: from a\r\n\tby b\r\n  id c\r\nFrom: a@x\r\n\r\nX: body\r\n\r\nmore",
            b"X-Fauna-Scan: 1\r\nX-Fauna-Forwarded-By: t\r\n\r\n",
            b"A: b\r\nC: d\r\n",
            b"A: b\r\nC: d",
            b"Subject: s\rt\r\nFrom: a\r\n\r\nbody",
            b"",
        ];
        for raw in corpus {
            let offset = find_body_offset(raw);
            assert_eq!(offset, crlf_reference::find_body_offset(raw), "{raw:?}");
            let section = &raw[..offset.unwrap_or(raw.len())];
            let mut i = 0;
            while i < section.len() {
                let (end, _) = parse_header(&section[i..]);
                assert_eq!(
                    end,
                    crlf_reference::parse_header_end(&section[i..]),
                    "{raw:?} @{i}"
                );
                i += end;
            }
        }
    }
}
