//! IMAP `FETCH BODY[<section>]` extraction (RFC 9051 §6.4.5) from decrypted
//! RFC 5322 bytes.
//!
//! The MDA bridge hands the decrypted message plaintext plus a parsed
//! [`BodySectionSpec`] (lifted from the go-imap fork's
//! `imap.FetchItemBodySection`) and gets back the exact octets to write into
//! the `BODY[…]` FETCH response literal. Reuses the same `mail-parser` parse
//! the rest of `fauna-mail` uses (priority #2: MIME logic is shared Rust, not
//! Go-side) and slices the **raw** message bytes verbatim — preserving the
//! original octets (folding, DKIM-relevant header bytes, line endings) rather
//! than re-serialising.
//!
//! **Top-level specifiers (Slice 1)**, each optionally sliced by a trailing
//! `<offset.size>` partial substring applied last:
//!
//! - `[]` — the whole message (byte-identical to today's `BODY[]`).
//! - `HEADER` — the header block incl. the terminating blank line.
//! - `TEXT` — everything after the header block (the body).
//! - `HEADER.FIELDS (f1 f2 …)` — only the named headers, **in message order**
//!   (matching the go-imap reference `imapserver.ExtractBodySection` and
//!   Dovecot — *not* request order), + the blank-line terminator.
//! - `HEADER.FIELDS.NOT (f1 f2 …)` — every header except the named ones.
//!
//! **Numbered MIME parts (Slice 2)** — `BODY[N]` (the part's contents),
//! `BODY[N.MIME]` (its MIME header), and for `message/rfc822` parts
//! `BODY[N.HEADER]` / `BODY[N.TEXT]` / `BODY[N.HEADER.FIELDS …]` (the
//! encapsulated message's header/body), recursing into nested `multipart/<subtype>`
//! (`BODY[N.M]`) and encapsulated messages. Part addressing follows RFC 9051
//! §6.4.5: a leading `1` on a non-multipart message refers to the message
//! itself; an out-of-range part returns the empty string (NIL), not an error.
//!
//! **`BINARY[…]` / `BINARY.SIZE[…]` (RFC 3516 / RFC 9051 §6.4.5)** —
//! [`fetch_binary_section`] / [`fetch_binary_size`] return a numbered part's
//! contents with its `Content-Transfer-Encoding` *removed* (base64 / quoted-
//! printable decoded) but — unlike `mail-parser`'s `PartType::Text` — with
//! **no charset conversion** (RFC 3516 decodes the CTE only). `BINARY[]`
//! (empty part) returns the whole message (header + decoded body, like
//! `BODY[]`); a `multipart` / `message/rfc822` part has no leaf CTE and is
//! returned verbatim. A `Content-Transfer-Encoding` this server can't decode
//! yields [`ParseError::UnknownCte`] (→ tagged `NO [UNKNOWN-CTE]`).
//!
//! Still unsupported (returns [`ParseError::Unsupported`]): the **top-level**
//! `MIME` specifier (only meaningful with a numbered part).
//!
//! Byte-level reference: `bins/fauna-bridges/third_party/go-imap/imapserver/message.go`
//! `ExtractBodySection` / `ExtractBinarySection` / `findMessagePart`.

use std::collections::HashSet;

use mail_parser::decoders::base64::base64_decode;
use mail_parser::decoders::quoted_printable::quoted_printable_decode;
use mail_parser::{Message, MessageParser, MessagePart, MimeHeaders, PartType};
use serde::{Deserialize, Serialize};

use crate::parser::ParseError;

/// A parsed IMAP `BODY[<section>]` request, mirroring the relevant fields of
/// the go-imap fork's `imap.FetchItemBodySection`. The bridge fills this from
/// the value its IMAP command parser already produced.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BodySectionSpec {
    /// The part specifier, upper-cased: `""` (whole / none / a numbered part's
    /// contents), `"HEADER"`, `"TEXT"`, or `"MIME"`. `"MIME"` is valid only with
    /// a non-empty [`Self::part`] (a top-level `MIME` is rejected, as it only
    /// has meaning for a numbered part).
    pub specifier: String,
    /// Numbered MIME part path (e.g. `[1, 2]` for part `1.2`). Empty = the
    /// top-level section. A non-empty path addresses one MIME part, recursing
    /// into `multipart/<subtype>` and encapsulated `message/rfc822` per RFC 9051
    /// §6.4.5.
    pub part: Vec<u32>,
    /// `HEADER.FIELDS (f1 f2 …)` field names. Non-empty ⇒ keep only the named
    /// headers (case-insensitive match), in message order.
    pub header_fields: Vec<String>,
    /// `HEADER.FIELDS.NOT (f1 f2 …)` field names. Non-empty ⇒ keep every
    /// header except the named ones, in message order.
    pub header_fields_not: Vec<String>,
    /// `<offset.size>` partial substring, applied to the extracted section
    /// last. `None` = the whole section.
    pub partial: Option<BodySectionPartial>,
}

/// The RFC 9051 §6.4.5 `<partial>` origin-octet / length pair.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BodySectionPartial {
    /// Origin octet, 0-based.
    pub offset: u64,
    /// Maximum number of octets to return starting at `offset`.
    pub size: u64,
}

/// A parsed IMAP `BINARY[<section-binary>]` request (RFC 3516 / RFC 9051
/// §6.4.5), mirroring the go-imap fork's `imap.FetchItemBinarySection`. A
/// binary section addresses a numbered MIME part (or the whole message when
/// `part` is empty) and returns its `Content-Transfer-Encoding`-decoded
/// contents — there is no `HEADER` / `TEXT` / `MIME` sub-specifier (those are
/// `BODY[…]` forms).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BinarySectionSpec {
    /// Numbered MIME part path (e.g. `[1, 2]` for part `1.2`). Empty = the
    /// whole message (header + decoded body, like `BODY[]`).
    pub part: Vec<u32>,
    /// `<offset.size>` partial substring, applied to the decoded section
    /// last. `None` = the whole section. (`BINARY.SIZE` carries no partial.)
    pub partial: Option<BodySectionPartial>,
}

/// Extract the requested `BODY[<section>]` octets from raw RFC 5322 bytes.
///
/// Returns the verbatim section bytes (with the `<partial>` substring applied
/// if present), `ParseError::Malformed` if the message can't be parsed, or
/// `ParseError::Unsupported` for the top-level `MIME` specifier (which only has
/// meaning for a numbered part) or an unknown specifier.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn fetch_body_section(raw: &[u8], spec: BodySectionSpec) -> Result<Vec<u8>, ParseError> {
    let specifier = spec.specifier.to_ascii_uppercase();
    let section = if spec.part.is_empty() {
        // Top-level section (no part path).
        match specifier.as_str() {
            // BODY[] — the whole message, byte-identical to the existing path.
            "" => raw.to_vec(),
            "HEADER" => {
                let msg = parse(raw)?;
                extract_header_of(
                    raw,
                    &msg.parts[0],
                    &spec.header_fields,
                    &spec.header_fields_not,
                )
            }
            "TEXT" => {
                let msg = parse(raw)?;
                extract_text_of(raw, &msg.parts[0])
            }
            // Top-level MIME has no meaning (it refers to a part's MIME header);
            // anything else is unknown.
            _ => return Err(ParseError::Unsupported),
        }
    } else {
        // Numbered-part addressing recurses into the MIME tree.
        extract_numbered_part(
            raw,
            &spec.part,
            &specifier,
            &spec.header_fields,
            &spec.header_fields_not,
        )?
    };
    Ok(apply_partial(section, spec.partial))
}

/// Extract the requested `BINARY[<section-binary>]` octets from raw RFC 5322
/// bytes (RFC 3516 / RFC 9051 §6.4.5).
///
/// Returns the addressed part's contents with its `Content-Transfer-Encoding`
/// decoded (base64 / quoted-printable) — **no charset conversion**, unlike a
/// `BODY[…]` text fetch through `mail-parser`'s `PartType::Text`. An empty
/// `spec.part` returns the whole message (its header block + decoded body, like
/// `BODY[]`); a `multipart` / `message/rfc822` part has no leaf CTE and so is
/// returned verbatim. The `<partial>` substring, if any, is applied last.
///
/// # Errors
///
/// - [`ParseError::Malformed`] — the message can't be parsed, or a declared
///   base64 / quoted-printable body fails to decode.
/// - [`ParseError::UnknownCte`] — the part declares a `Content-Transfer-
///   Encoding` this server can't decode (anything other than `7bit` / `8bit` /
///   `binary` / `quoted-printable` / `base64`); the MDA maps it to a tagged
///   `NO [UNKNOWN-CTE]` rather than returning undecoded bytes.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn fetch_binary_section(raw: &[u8], spec: BinarySectionSpec) -> Result<Vec<u8>, ParseError> {
    let section = extract_binary(raw, &spec.part)?;
    Ok(apply_partial(section, spec.partial))
}

/// `BINARY.SIZE[<section-binary>]` (RFC 3516 / RFC 9051 §6.4.5): the octet
/// count of the CTE-decoded section [`fetch_binary_section`] would return for
/// the same `part` (no `<partial>` — `BINARY.SIZE` carries none).
///
/// Errors are the same as [`fetch_binary_section`].
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn fetch_binary_size(raw: &[u8], part: Vec<u32>) -> Result<u32, ParseError> {
    let section = extract_binary(raw, &part)?;
    Ok(section.len() as u32)
}

/// Shared core of [`fetch_binary_section`] / [`fetch_binary_size`]: locate the
/// addressed part (mirroring the go-imap reference `ExtractBinarySection` /
/// `findMessagePart`) and return its CTE-decoded contents. The whole-message
/// (empty-part) case includes the header block, matching the reference
/// (`writeHeader = len(item.Part) == 0`); a numbered part returns body only.
fn extract_binary(raw: &[u8], path: &[u32]) -> Result<Vec<u8>, ParseError> {
    let msg = parse(raw)?;
    let Some(part) = locate_part(&msg, path) else {
        // RFC 9051 §6.4.5: a structurally-valid request for a part that does
        // not exist returns the empty string (matching the BODY[N] path).
        return Ok(Vec::new());
    };
    let body = slice(raw, part.offset_body, part.offset_end);
    let mut decoded = cte_decode(&body, part.content_transfer_encoding())?;
    if path.is_empty() {
        // BINARY[] — whole message: header block + decoded body (the reference
        // writes the header only for the empty-part case).
        let mut out = slice(raw, part.offset_header, part.offset_body);
        out.append(&mut decoded);
        return Ok(out);
    }
    Ok(decoded)
}

/// Decode a part body by its `Content-Transfer-Encoding` (RFC 2045 §6.1),
/// **without** charset conversion (RFC 3516 decodes the CTE only). `7bit` /
/// `8bit` / `binary` / absent are identity; `base64` / `quoted-printable` use
/// `mail-parser`'s own decoders (whitespace- and soft-break-tolerant); any
/// other token is [`ParseError::UnknownCte`].
fn cte_decode(body: &[u8], cte: Option<&str>) -> Result<Vec<u8>, ParseError> {
    match cte.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        None | Some("") | Some("7bit") | Some("8bit") | Some("binary") => Ok(body.to_vec()),
        Some("quoted-printable") => quoted_printable_decode(body).ok_or(ParseError::Malformed),
        Some("base64") => base64_decode(body).ok_or(ParseError::Malformed),
        Some(_) => Err(ParseError::UnknownCte),
    }
}

/// `BODY[N…]` — addresses one numbered MIME part (RFC 9051 §6.4.5 part
/// numbering), recursing into `multipart/<subtype>` and encapsulated `message/rfc822`.
///
/// Navigates `mail-parser`'s parsed tree to the addressed part, then extracts
/// the requested specifier verbatim from `raw` (all offsets are absolute into
/// the original buffer, including those of parts inside a nested
/// `message/rfc822`). A structurally-valid request for a part that does not
/// exist returns the empty string (RFC 9051 §6.4.5 NIL — matching the go-imap
/// reference `imapserver.ExtractBodySection` and Dovecot), not an error.
fn extract_numbered_part(
    raw: &[u8],
    path: &[u32],
    specifier: &str,
    keep_fields: &[String],
    drop_fields: &[String],
) -> Result<Vec<u8>, ParseError> {
    let msg = parse(raw)?;
    let Some(part) = locate_part(&msg, path) else {
        return Ok(Vec::new());
    };
    Ok(match specifier {
        // BODY[N] — the part's contents: a leaf's decoded-transfer body bytes
        // verbatim, a multipart's raw body (boundaries + children), or — for a
        // message/rfc822 part — the *entire* encapsulated message.
        "" => slice(raw, part.offset_body, part.offset_end),
        // BODY[N.MIME] — the part's own MIME header block (incl. blank line).
        "MIME" => slice(raw, part.offset_header, part.offset_body),
        // BODY[N.HEADER] / .HEADER.FIELDS[.NOT] — open a message/rfc822 part and
        // return its (encapsulated) header; for a non-message part, the part's
        // own header (the reference's openMessagePart passthrough).
        "HEADER" => {
            let hpart = opened_inner(part).unwrap_or(part);
            extract_header_of(raw, hpart, keep_fields, drop_fields)
        }
        // BODY[N.TEXT] — the encapsulated message's body (or the part's own body
        // for a non-message part, mirroring the reference).
        "TEXT" => {
            let tpart = opened_inner(part).unwrap_or(part);
            extract_text_of(raw, tpart)
        }
        _ => return Err(ParseError::Unsupported),
    })
}

/// Walk the IMAP 1-based part path to the addressed `MessagePart`, or `None`
/// when the path addresses a part that does not exist.
///
/// Mirrors the go-imap reference's `findMessagePart`: a leading `1` on a
/// non-multipart message refers to the message itself (RFC 9051 §6.4.5), and an
/// encapsulated `message/rfc822` is opened before its sub-parts are addressed.
fn locate_part<'m, 'r>(msg: &'m Message<'r>, path: &[u32]) -> Option<&'m MessagePart<'r>> {
    let mut cur_msg: &'m Message<'r> = msg;
    let mut cur: &'m MessagePart<'r> = cur_msg.parts.first()?;

    // Non-multipart root: BODY[1] refers to the message itself; strip it.
    let mut rest = path;
    if !is_multipart(cur) && rest.first() == Some(&1) {
        rest = &rest[1..];
    }

    for &part_num in rest {
        // Open an encapsulated message/rfc822 (selected on the previous step)
        // before addressing its sub-parts.
        if let PartType::Message(inner) = &cur.body {
            cur_msg = inner;
            cur = cur_msg.parts.first()?;
            // The inner message's own non-multipart root: BODY[…1] is itself.
            if !is_multipart(cur) {
                if part_num != 1 {
                    return None;
                }
                continue;
            }
        }

        match &cur.body {
            PartType::Multipart(child_ids) => {
                let idx = *child_ids.get((part_num as usize).checked_sub(1)?)?;
                cur = cur_msg.parts.get(idx as usize)?;
            }
            // A non-multipart, non-message leaf: only part 1 (the part itself)
            // is addressable; any other number does not exist.
            _ => {
                if part_num != 1 {
                    return None;
                }
            }
        }
    }
    Some(cur)
}

/// Whether a part is a `multipart/<subtype>` container (its children are addressable by
/// number).
fn is_multipart(part: &MessagePart<'_>) -> bool {
    matches!(part.body, PartType::Multipart(_))
}

/// For a `message/rfc822` part, the root part of its encapsulated message
/// (whose header/body `.HEADER` / `.TEXT` then address); `None` otherwise.
fn opened_inner<'m, 'r>(part: &'m MessagePart<'r>) -> Option<&'m MessagePart<'r>> {
    match &part.body {
        PartType::Message(inner) => inner.parts.first(),
        _ => None,
    }
}

/// Clamp `[start, end)` to `raw` and return the verbatim slice.
fn slice(raw: &[u8], start: u32, end: u32) -> Vec<u8> {
    let s = (start as usize).min(raw.len());
    let e = (end as usize).min(raw.len());
    raw.get(s..e).unwrap_or(&[]).to_vec()
}

/// Parse `raw`, returning a `Malformed` error on failure. Offsets on the
/// returned message index into `raw` (we hand the whole buffer to the parser),
/// so callers slice `raw` directly — including offsets on the parts of a nested
/// `message/rfc822`, which are likewise absolute into `raw`.
fn parse(raw: &[u8]) -> Result<mail_parser::Message<'_>, ParseError> {
    MessageParser::default()
        .parse(raw)
        .ok_or(ParseError::Malformed)
}

/// `HEADER` / `HEADER.FIELDS (…)` / `HEADER.FIELDS.NOT (…)` of one MIME part
/// (the whole message at the top level, or a numbered/encapsulated part).
///
/// With no field list, returns the whole header block including the blank-line
/// terminator (`raw[offset_header..offset_body]`). With a field list, emits the
/// selected header lines verbatim in **message order**, then the blank-line
/// terminator — mirroring the go-imap reference, which iterates the part's own
/// header order and deletes non-matching fields.
fn extract_header_of(
    raw: &[u8],
    part: &MessagePart<'_>,
    keep_fields: &[String],
    drop_fields: &[String],
) -> Vec<u8> {
    if keep_fields.is_empty() && drop_fields.is_empty() {
        return slice(raw, part.offset_header, part.offset_body);
    }

    // Apply the keep-set first (HEADER.FIELDS), then the drop-set
    // (HEADER.FIELDS.NOT) — matching the reference's two-step filter. The
    // fork's parser only ever populates one of the two, but applying both is
    // robust and order-independent.
    let keep: Option<HashSet<String>> = if keep_fields.is_empty() {
        None
    } else {
        Some(keep_fields.iter().map(|f| f.to_ascii_lowercase()).collect())
    };
    let drop: HashSet<String> = drop_fields.iter().map(|f| f.to_ascii_lowercase()).collect();

    let mut out = Vec::new();
    for h in &part.headers {
        let name = h.name.as_str().to_ascii_lowercase();
        if let Some(keep) = &keep
            && !keep.contains(&name)
        {
            continue;
        }
        if drop.contains(&name) {
            continue;
        }
        let s = h.offset_field as usize;
        let e = (h.offset_end as usize).min(raw.len());
        if let Some(slice) = raw.get(s..e) {
            out.extend_from_slice(slice);
        }
    }
    // The header block always ends with the empty line (RFC 9051 §6.4.5 /
    // RFC 5322 §2.2). offset_end on the last kept field stops at its own CRLF,
    // so the separator is ours to add.
    out.extend_from_slice(b"\r\n");
    out
}

/// `TEXT` of one MIME part — everything after its header block's blank line
/// (`raw[offset_body..offset_end]`; for the top-level part `offset_end` is the
/// end of `raw`).
fn extract_text_of(raw: &[u8], part: &MessagePart<'_>) -> Vec<u8> {
    slice(raw, part.offset_body, part.offset_end)
}

/// Apply the RFC 9051 §6.4.5 `<partial>` substring. An origin past the end of
/// the section yields the empty string; a length running past the end clamps to
/// what's available (mirroring the reference's `extractPartial`).
fn apply_partial(bytes: Vec<u8>, partial: Option<BodySectionPartial>) -> Vec<u8> {
    let Some(p) = partial else {
        return bytes;
    };
    let off = match usize::try_from(p.offset) {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    if off > bytes.len() {
        return Vec::new();
    }
    let size = usize::try_from(p.size).unwrap_or(usize::MAX);
    let end = off.saturating_add(size).min(bytes.len());
    bytes[off..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Headers deliberately NOT in alphabetical order so the message-order
    // assertion below is meaningful (request order would differ).
    const MSG: &[u8] = b"From: alice@example.com\r\n\
To: bob@example.com\r\n\
Subject: hello world\r\n\
Date: Mon, 14 May 2026 12:00:00 +0000\r\n\
Message-ID: <abc@example.com>\r\n\
\r\n\
line one\r\n\
line two\r\n";

    fn header_block() -> &'static [u8] {
        b"From: alice@example.com\r\n\
To: bob@example.com\r\n\
Subject: hello world\r\n\
Date: Mon, 14 May 2026 12:00:00 +0000\r\n\
Message-ID: <abc@example.com>\r\n\
\r\n"
    }

    fn body() -> &'static [u8] {
        b"line one\r\nline two\r\n"
    }

    fn spec(specifier: &str) -> BodySectionSpec {
        BodySectionSpec {
            specifier: specifier.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn whole_message_is_verbatim() {
        let got = fetch_body_section(MSG, spec("")).unwrap();
        assert_eq!(got, MSG, "BODY[] must return the raw message verbatim");
    }

    #[test]
    fn header_block_includes_blank_line() {
        let got = fetch_body_section(MSG, spec("HEADER")).unwrap();
        assert_eq!(got, header_block());
        assert!(
            got.ends_with(b"\r\n\r\n"),
            "header block ends with the blank line"
        );
    }

    #[test]
    fn text_is_body_after_blank_line() {
        let got = fetch_body_section(MSG, spec("TEXT")).unwrap();
        assert_eq!(got, body());
    }

    #[test]
    fn header_block_plus_text_reconstructs_message() {
        let mut combined = fetch_body_section(MSG, spec("HEADER")).unwrap();
        combined.extend_from_slice(&fetch_body_section(MSG, spec("TEXT")).unwrap());
        assert_eq!(combined, MSG, "HEADER ++ TEXT == whole message");
    }

    #[test]
    fn header_fields_selects_named_in_message_order() {
        // Request order [Message-ID, Subject] differs from message order
        // [Subject, …, Message-ID]; output must follow MESSAGE order.
        let mut s = spec("HEADER");
        s.header_fields = vec!["Message-ID".to_string(), "Subject".to_string()];
        let got = fetch_body_section(MSG, s).unwrap();
        assert_eq!(
            got,
            b"Subject: hello world\r\nMessage-ID: <abc@example.com>\r\n\r\n"
        );
    }

    #[test]
    fn header_fields_is_case_insensitive() {
        let mut s = spec("HEADER");
        s.header_fields = vec!["subject".to_string()];
        let got = fetch_body_section(MSG, s).unwrap();
        assert_eq!(got, b"Subject: hello world\r\n\r\n");
    }

    #[test]
    fn header_fields_missing_field_is_skipped() {
        let mut s = spec("HEADER");
        s.header_fields = vec!["Subject".to_string(), "X-Not-Present".to_string()];
        let got = fetch_body_section(MSG, s).unwrap();
        // Only the present field survives; still terminated by a blank line.
        assert_eq!(got, b"Subject: hello world\r\n\r\n");
    }

    #[test]
    fn header_fields_empty_keep_set_yields_only_blank_line() {
        // An empty HEADER.FIELDS () keeps no headers — just the terminator.
        // (header_fields non-empty conceptually, but here we model the
        // none-matched case via a name that matches nothing.)
        let mut s = spec("HEADER");
        s.header_fields = vec!["X-Absent".to_string()];
        let got = fetch_body_section(MSG, s).unwrap();
        assert_eq!(got, b"\r\n");
    }

    #[test]
    fn header_fields_not_excludes_named() {
        let mut s = spec("HEADER");
        s.header_fields_not = vec!["Received".to_string(), "Date".to_string()];
        let got = fetch_body_section(MSG, s).unwrap();
        // Everything except Date, in message order, + blank line.
        assert_eq!(
            got,
            b"From: alice@example.com\r\n\
To: bob@example.com\r\n\
Subject: hello world\r\n\
Message-ID: <abc@example.com>\r\n\r\n"
        );
    }

    #[test]
    fn partial_returns_substring() {
        let mut s = spec("");
        s.partial = Some(BodySectionPartial { offset: 0, size: 4 });
        let got = fetch_body_section(MSG, s).unwrap();
        assert_eq!(got, b"From");
    }

    #[test]
    fn partial_clamps_length_past_eof() {
        let mut s = spec("TEXT");
        s.partial = Some(BodySectionPartial {
            offset: 9,
            size: 10_000,
        });
        let got = fetch_body_section(MSG, s).unwrap();
        // body() is "line one\r\nline two\r\n"; from offset 9 to end.
        assert_eq!(got, &body()[9..]);
    }

    #[test]
    fn partial_origin_past_eof_is_empty() {
        let mut s = spec("");
        s.partial = Some(BodySectionPartial {
            offset: 1_000_000,
            size: 10,
        });
        let got = fetch_body_section(MSG, s).unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn header_only_message_no_body() {
        // No blank line, no body: TEXT is empty, HEADER is the whole input.
        let raw: &[u8] = b"Subject: just a header\r\nFrom: x@y.z\r\n";
        let text = fetch_body_section(raw, spec("TEXT")).unwrap();
        assert!(text.is_empty(), "no body ⇒ TEXT empty, got {text:?}");
        // A header-only message with a closing blank line round-trips HEADER.
        let raw2: &[u8] = b"Subject: just a header\r\n\r\n";
        let hdr = fetch_body_section(raw2, spec("HEADER")).unwrap();
        assert_eq!(hdr, raw2);
    }

    #[test]
    fn folded_header_value_preserved_verbatim() {
        // A folded (continuation-line) header must be returned with its fold
        // intact when selected by HEADER.FIELDS.
        let raw: &[u8] = b"Subject: a very long\r\n subject that folds\r\n\
From: x@y.z\r\n\r\nbody\r\n";
        let mut s = spec("HEADER");
        s.header_fields = vec!["Subject".to_string()];
        let got = fetch_body_section(raw, s).unwrap();
        assert_eq!(got, b"Subject: a very long\r\n subject that folds\r\n\r\n");
    }

    #[test]
    fn top_level_mime_is_unsupported() {
        assert!(matches!(
            fetch_body_section(MSG, spec("MIME")),
            Err(ParseError::Unsupported)
        ));
    }

    // ---- Slice 2: numbered MIME parts (RFC 9051 §6.4.5 part numbering) ----

    // multipart/mixed with two leaf parts.
    const MP: &[u8] = b"From: a@x\r\n\
Content-Type: multipart/mixed; boundary=BB\r\n\
\r\n\
--BB\r\n\
Content-Type: text/plain\r\n\
\r\n\
first body\r\n\
--BB\r\n\
Content-Type: text/html\r\n\
\r\n\
<p>second</p>\r\n\
--BB--\r\n";

    // multipart/mixed > [ text/plain, multipart/alternative > [plain, html] ].
    const NESTED: &[u8] = b"From: a@x\r\n\
Content-Type: multipart/mixed; boundary=OUT\r\n\
\r\n\
--OUT\r\n\
Content-Type: text/plain\r\n\
\r\n\
top text\r\n\
--OUT\r\n\
Content-Type: multipart/alternative; boundary=IN\r\n\
\r\n\
--IN\r\n\
Content-Type: text/plain\r\n\
\r\n\
alt plain\r\n\
--IN\r\n\
Content-Type: text/html\r\n\
\r\n\
<p>alt html</p>\r\n\
--IN--\r\n\
--OUT--\r\n";

    // multipart/mixed > [ text/plain cover, message/rfc822 attachment ].
    const RFC822: &[u8] = b"From: a@x\r\n\
Content-Type: multipart/mixed; boundary=BB\r\n\
\r\n\
--BB\r\n\
Content-Type: text/plain\r\n\
\r\n\
cover\r\n\
--BB\r\n\
Content-Type: message/rfc822\r\n\
\r\n\
Subject: inner subject\r\n\
From: inner@x\r\n\
\r\n\
inner body line\r\n\
--BB--\r\n";

    fn part_spec(specifier: &str, part: &[u32]) -> BodySectionSpec {
        BodySectionSpec {
            specifier: specifier.to_string(),
            part: part.to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn numbered_part_leaf_body_only() {
        // BODY[1] / BODY[2] = the part's *content* (body), without its MIME header.
        assert_eq!(
            fetch_body_section(MP, part_spec("", &[1])).unwrap(),
            b"first body"
        );
        assert_eq!(
            fetch_body_section(MP, part_spec("", &[2])).unwrap(),
            b"<p>second</p>"
        );
    }

    #[test]
    fn numbered_part_mime_header() {
        // BODY[N.MIME] = the part's own MIME header block (incl. blank line).
        assert_eq!(
            fetch_body_section(MP, part_spec("MIME", &[1])).unwrap(),
            b"Content-Type: text/plain\r\n\r\n"
        );
        assert_eq!(
            fetch_body_section(MP, part_spec("MIME", &[2])).unwrap(),
            b"Content-Type: text/html\r\n\r\n"
        );
    }

    #[test]
    fn numbered_part_recurses_into_nested_multipart() {
        // BODY[1] is the leaf; BODY[2] is the whole inner multipart body
        // (boundaries + children verbatim); BODY[2.1] / BODY[2.2] address its
        // children.
        assert_eq!(
            fetch_body_section(NESTED, part_spec("", &[1])).unwrap(),
            b"top text"
        );
        assert_eq!(
            fetch_body_section(NESTED, part_spec("", &[2, 1])).unwrap(),
            b"alt plain"
        );
        assert_eq!(
            fetch_body_section(NESTED, part_spec("", &[2, 2])).unwrap(),
            b"<p>alt html</p>"
        );
        // BODY[2] = the inner multipart's raw body (everything between the
        // parent's boundaries, verbatim). The CRLF before the parent's `--OUT`
        // delimiter belongs to that boundary, so the part body ends at the
        // inner close-delimiter `--IN--` with no trailing CRLF.
        let inner_mp = fetch_body_section(NESTED, part_spec("", &[2])).unwrap();
        assert!(inner_mp.starts_with(b"--IN\r\nContent-Type: text/plain"));
        assert!(inner_mp.ends_with(b"--IN--"));
    }

    #[test]
    fn numbered_nested_part_mime_header() {
        assert_eq!(
            fetch_body_section(NESTED, part_spec("MIME", &[2, 1])).unwrap(),
            b"Content-Type: text/plain\r\n\r\n"
        );
    }

    #[test]
    fn message_rfc822_part_body_is_whole_encapsulated_message() {
        // BODY[2] on a message/rfc822 part = the *entire* encapsulated message
        // (its header + body), per the go-imap reference. The CRLF before the
        // outer `--BB--` delimiter is the boundary's, so the encapsulated body
        // ends at "inner body line" without a trailing CRLF.
        assert_eq!(
            fetch_body_section(RFC822, part_spec("", &[2])).unwrap(),
            b"Subject: inner subject\r\nFrom: inner@x\r\n\r\ninner body line"
        );
    }

    #[test]
    fn message_rfc822_part_mime_header() {
        // BODY[2.MIME] = the message/rfc822 part's *own* MIME header, not the
        // encapsulated message's header.
        assert_eq!(
            fetch_body_section(RFC822, part_spec("MIME", &[2])).unwrap(),
            b"Content-Type: message/rfc822\r\n\r\n"
        );
    }

    #[test]
    fn message_rfc822_part_header_opens_inner_message() {
        // BODY[2.HEADER] opens the encapsulated message and returns its header.
        assert_eq!(
            fetch_body_section(RFC822, part_spec("HEADER", &[2])).unwrap(),
            b"Subject: inner subject\r\nFrom: inner@x\r\n\r\n"
        );
    }

    #[test]
    fn message_rfc822_part_text_opens_inner_message() {
        // BODY[2.TEXT] = the encapsulated message's body (trailing CRLF belongs
        // to the outer boundary, as above).
        assert_eq!(
            fetch_body_section(RFC822, part_spec("TEXT", &[2])).unwrap(),
            b"inner body line"
        );
    }

    #[test]
    fn message_rfc822_part_header_fields_filters_inner_header() {
        // BODY[2.HEADER.FIELDS (Subject)] selects from the *inner* header.
        let mut s = part_spec("HEADER", &[2]);
        s.header_fields = vec!["Subject".to_string()];
        assert_eq!(
            fetch_body_section(RFC822, s).unwrap(),
            b"Subject: inner subject\r\n\r\n"
        );
    }

    #[test]
    fn non_multipart_body_part_1_is_the_message_body() {
        // RFC 9051 §6.4.5: for a non-multipart message, BODY[1] refers to the
        // message itself — equivalently its TEXT (the body after the header).
        assert_eq!(
            fetch_body_section(MSG, part_spec("", &[1])).unwrap(),
            fetch_body_section(MSG, spec("TEXT")).unwrap()
        );
    }

    #[test]
    fn non_message_part_header_returns_its_mime_header() {
        // BODY[1.HEADER] on a non-message leaf returns that part's own header
        // (the go-imap reference's openMessagePart passthrough — Dovecot parity).
        assert_eq!(
            fetch_body_section(MP, part_spec("HEADER", &[1])).unwrap(),
            b"Content-Type: text/plain\r\n\r\n"
        );
    }

    #[test]
    fn out_of_range_part_is_empty() {
        // A structurally-valid request for a part that doesn't exist returns
        // the empty string (RFC 9051 §6.4.5 NIL; reference + Dovecot parity),
        // not an error.
        assert!(
            fetch_body_section(MP, part_spec("", &[5]))
                .unwrap()
                .is_empty()
        );
        // Descending past a leaf (BODY[1.2] where part 1 is a leaf) is likewise
        // empty.
        assert!(
            fetch_body_section(MP, part_spec("", &[1, 2]))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn numbered_part_partial_applies_last() {
        // BODY[1]<0.5> = the first 5 octets of part 1's body.
        let mut s = part_spec("", &[1]);
        s.partial = Some(BodySectionPartial { offset: 0, size: 5 });
        assert_eq!(fetch_body_section(MP, s).unwrap(), b"first");
    }

    // ---- BINARY[…] / BINARY.SIZE[…] (RFC 3516 / RFC 9051 §6.4.5) ----

    // multipart/mixed > [ text/plain 7bit, octet-stream base64, text/plain QP ].
    // Part 2 base64 "Zm9vYmFy" decodes to "foobar"; part 3 QP "caf=C3=A9"
    // decodes to the UTF-8 bytes of "café" (b"caf\xc3\xa9"). Neither is charset-
    // converted — BINARY removes the CTE only.
    const BIN: &[u8] = b"From: a@x\r\n\
Content-Type: multipart/mixed; boundary=BB\r\n\
\r\n\
--BB\r\n\
Content-Type: text/plain\r\n\
\r\n\
plain text\r\n\
--BB\r\n\
Content-Type: application/octet-stream\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
Zm9vYmFy\r\n\
--BB\r\n\
Content-Type: text/plain\r\n\
Content-Transfer-Encoding: quoted-printable\r\n\
\r\n\
caf=C3=A9\r\n\
--BB--\r\n";

    // A single-part base64 message (no multipart) for the whole-message
    // BINARY[] case.
    const BIN_SIMPLE: &[u8] = b"Subject: s\r\n\
Content-Type: application/octet-stream\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
Zm9vYmFy\r\n";

    fn bin_spec(part: &[u32]) -> BinarySectionSpec {
        BinarySectionSpec {
            part: part.to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn binary_leaf_base64_decodes_cte_only() {
        // BINARY[2] = base64-decoded part contents, no charset conversion.
        assert_eq!(
            fetch_binary_section(BIN, bin_spec(&[2])).unwrap(),
            b"foobar"
        );
    }

    #[test]
    fn binary_leaf_quoted_printable_decodes() {
        // BINARY[3] = QP-decoded bytes (UTF-8 of "café"), NOT re-encoded text.
        assert_eq!(
            fetch_binary_section(BIN, bin_spec(&[3])).unwrap(),
            b"caf\xc3\xa9"
        );
    }

    #[test]
    fn binary_leaf_7bit_is_verbatim() {
        // BINARY[1] = a 7bit (no CTE) part is returned byte-identical.
        assert_eq!(
            fetch_binary_section(BIN, bin_spec(&[1])).unwrap(),
            b"plain text"
        );
    }

    #[test]
    fn binary_size_is_decoded_octet_count() {
        // SIZE is the *decoded* length: base64 8-char "Zm9vYmFy" → 6;
        // QP "caf=C3=A9" → 5; 7bit "plain text" → 10.
        assert_eq!(fetch_binary_size(BIN, vec![2]).unwrap(), 6);
        assert_eq!(fetch_binary_size(BIN, vec![3]).unwrap(), 5);
        assert_eq!(fetch_binary_size(BIN, vec![1]).unwrap(), 10);
    }

    #[test]
    fn binary_whole_message_includes_header_and_decodes_root() {
        // BINARY[] = the header block + the CTE-decoded root body (the
        // reference writes the header only for the empty-part case).
        let got = fetch_binary_section(BIN_SIMPLE, bin_spec(&[])).unwrap();
        assert_eq!(
            got,
            b"Subject: s\r\n\
Content-Type: application/octet-stream\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
foobar"
        );
    }

    #[test]
    fn binary_unknown_cte_is_error() {
        // A Content-Transfer-Encoding we can't decode → UnknownCte (→ tagged
        // NO [UNKNOWN-CTE]), never undecoded bytes.
        let raw: &[u8] = b"Content-Type: application/octet-stream\r\n\
Content-Transfer-Encoding: x-uuencode\r\n\
\r\n\
begin 644 x\r\n";
        assert!(matches!(
            fetch_binary_section(raw, bin_spec(&[1])),
            Err(ParseError::UnknownCte)
        ));
        // SIZE on the same part surfaces the same error (not a 0 count).
        assert!(matches!(
            fetch_binary_size(raw, vec![1]),
            Err(ParseError::UnknownCte)
        ));
    }

    #[test]
    fn binary_partial_applies_to_decoded_bytes() {
        // BINARY[2]<0.3> = the first 3 octets of the *decoded* "foobar".
        let mut s = bin_spec(&[2]);
        s.partial = Some(BodySectionPartial { offset: 0, size: 3 });
        assert_eq!(fetch_binary_section(BIN, s).unwrap(), b"foo");
    }

    #[test]
    fn binary_out_of_range_part_is_empty() {
        assert!(
            fetch_binary_section(BIN, bin_spec(&[9]))
                .unwrap()
                .is_empty()
        );
        assert_eq!(fetch_binary_size(BIN, vec![9]).unwrap(), 0);
    }

    #[test]
    fn binary_multipart_part_returned_verbatim() {
        // A BINARY fetch of a nested multipart part has no leaf CTE → its raw
        // body (boundaries + children) is returned verbatim, like BODY[N].
        assert_eq!(
            fetch_binary_section(NESTED, bin_spec(&[2])).unwrap(),
            fetch_body_section(NESTED, part_spec("", &[2])).unwrap(),
        );
    }
}
