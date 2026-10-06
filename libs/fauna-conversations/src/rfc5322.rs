//! Minimal RFC 5322 message assembly + parsing for the SMTP rail.
//!
//! **Outbound** (`build_message`): `SmtpBackend::send` builds the on-the-wire
//! message here (shared Rust, per `docs/goal/ui/conversations.md` § Where logic
//! lives — "Per-rail wire format encode/decode" is shared) and hands the bytes
//! to the platform's [`crate::backend::OutboundMailSink`], which performs the
//! `fauna.email.send` WS-RPC call. The nest's handler
//! (`bins/fauna-nest/src/email_handlers.rs` `send_handler`) parses
//! `From`/`Subject`/`Message-ID` out of these bytes with `mail-parser` and
//! relays the raw bytes to delivery untouched, so the header contract is: a
//! well-formed `From: <handle>@<domain>` (the local part must match the caller's
//! handle, or the nest rejects same-domain sends), `To`, `Subject`, `Date`,
//! `Message-ID`. The body is **`multipart/alternative`** (`docs/goal/behavior/html-mail.md`):
//! the compose body is markdown, emitted as a `text/plain` part (the markdown
//! source) **and** a `text/html` part (markdown→HTML via the shared, sanitized
//! [`fauna_core::markdown::markdown_to_html`]). Plain-text MUAs and non-Fauna
//! recipients still get a readable copy; the nest relays multipart transparently.
//!
//! **Inbound** (`parse_message`): the conversations receive feed
//! (`docs/goal/behavior/smtp-server.md` § Inbound client receive) decrypts a
//! sealed inbox record to its original RFC 5322 bytes client-side, then parses
//! the fields a `RailInboundMessage` needs here. `Message-ID`/`In-Reply-To` and
//! the body-boundary split are a minimal ASCII-header parser (folding-unfold +
//! blank-line split; they carry no encoded-words to decode); `From`/`To`/
//! `Subject` are decoded by `mail_parser` instead — see [`ParsedMessage`] for
//! detail.
//!
//! Kept dependency-free of `chrono` / `time` and `wasm32`-safe: the date is
//! formatted with a self-contained civil-from-days routine rather than a clock
//! crate. `mail_parser` is not a second dependency to weigh here — this crate
//! already carries it for the body/attachment path ([`crate::html_markdown`]),
//! and it is wasm32-safe.

/// One outbound attachment, inlined as a `multipart/mixed` part by
/// [`build_message`]. Borrowed so the SMTP backend can pass slices of its
/// resolved attachment bytes without copying (`docs/goal/ui/conversations.md`
/// § Attachments — SMTP attachments ride inline in the MIME, not as nest blobs).
pub struct MimeAttachment<'a> {
    pub filename: &'a str,
    pub mime_type: &'a str,
    pub bytes: &'a [u8],
}

/// Mint the `Message-ID:` for one outbound message:
/// `<{hex(96 random bits ‖ 32-bit provenance tag)}@{domain}>` — the
/// self-describing Fauna mint, [`fauna_mail::msgid`] (one implementation for
/// both submission paths; the Go server stamps the same over UniFFI for an
/// MUA that omitted one, RFC 6409 § 8.3).
///
/// **The randomness is load-bearing twice over, so this is not a timestamp.**
///
/// 1. *Uniqueness.* RFC 5322 § 3.6.4 requires a globally unique id, and Fauna
///    leans on it: [`fauna_mail::dedup_key`]'s primary form is
///    `msgid:v1:<normalized>`, so two distinct messages sharing an id make one of
///    them invisible to a later mailbox import. A clock-plus-counter id collides
///    across devices and app launches by construction — two of a user's clients
///    sending in the same second both mint `<secs.0@domain>`.
/// 2. *Unguessability.* The guardian mail gate delivers a `MAIL FROM:<>`
///    delivery-status report to a supervised child only when it names the
///    Message-ID of a message the child actually sent (`family-safety.md`
///    § The mail gate). A guessable id would let a stranger forge a report about
///    mail they never received and land it in the child's INBOX, unseen by the
///    guardian. 96 random bits close that.
///
/// The tag is what lets the gate's seed *verify* "a Fauna path minted this"
/// instead of pattern-matching a shape an MD5/`uuid4().hex` lookalike also
/// wears — `fauna_mail::msgid` owns the construction and the keyless-tag
/// rationale. The RNG stays here (this crate builds for wasm, where the
/// caller owns the getrandom backend); the mint itself is the shared pure fn.
pub fn new_message_id(domain: &str) -> String {
    let mut random = [0u8; fauna_mail::msgid::MSGID_RANDOM_LEN];
    getrandom::fill(&mut random).expect("getrandom failed");
    format!("<{}@{}>", fauna_mail::msgid::mint_local(&random), domain)
}

/// Assemble an RFC 5322 message from a **markdown** compose body
/// (`docs/goal/behavior/html-mail.md`), with optional file attachments.
///
/// `from` is the sender's canonical `<handle>@<domain>` address; `to` are the
/// recipient addresses (rendered into the `To:` header, comma-joined). An
/// empty `subject` (or `None`) omits the `Subject:` header. `date_unix_secs`
/// is formatted into an RFC 5322 `Date:` in UTC (`+0000`). `in_reply_to`, when
/// `Some`, adds an `In-Reply-To:` header for threaded replies.
///
/// `body` is markdown: it is emitted both as a `text/plain` part (the markdown
/// source verbatim — readable for plain-text MUAs and non-Fauna recipients) and
/// as a `text/html` part rendered by the shared, already-sanitized
/// [`fauna_core::markdown::markdown_to_html`]. No new markdown→HTML dependency:
/// the renderer the web app already emits is reused (priority #2/#4).
///
/// **Attachments.** With none, the message is a flat `multipart/alternative`
/// (body only) — unchanged from before attachment support. With one or more, the
/// body alternative is nested inside a `multipart/mixed`, followed by one
/// base64-encoded `Content-Disposition: attachment` part per attachment — the
/// standard email shape every MUA renders, so a non-Fauna recipient gets the
/// real files inline.
// One positional arg per RFC 5322 field is clearer at the (few) call sites than a
// builder/struct would be; the attachments slice pushed it from 7 to 8.
#[allow(clippy::too_many_arguments)]
pub fn build_message(
    from: &str,
    to: &[String],
    subject: Option<&str>,
    body: &str,
    message_id: &str,
    in_reply_to: Option<&str>,
    date_unix_secs: i64,
    attachments: &[MimeAttachment<'_>],
) -> Vec<u8> {
    let alt_boundary = multipart_boundary(message_id);
    let mut m = String::new();
    m.push_str(&format!("From: {from}\r\n"));
    m.push_str(&format!("To: {}\r\n", to.join(", ")));
    if let Some(s) = subject.filter(|s| !s.is_empty()) {
        m.push_str(&format!("Subject: {s}\r\n"));
    }
    m.push_str(&format!(
        "Date: {}\r\n",
        format_rfc5322_date(date_unix_secs)
    ));
    m.push_str(&format!("Message-ID: {message_id}\r\n"));
    if let Some(irt) = in_reply_to {
        m.push_str(&format!("In-Reply-To: {irt}\r\n"));
    }
    m.push_str("MIME-Version: 1.0\r\n");

    if attachments.is_empty() {
        // No attachments: a flat multipart/alternative carrying just the body.
        m.push_str(&format!(
            "Content-Type: multipart/alternative; boundary=\"{alt_boundary}\"\r\n\r\n"
        ));
        push_alternative_body(&mut m, &alt_boundary, body);
        m.push_str(&format!("--{alt_boundary}--\r\n"));
    } else {
        // With attachments: a multipart/mixed wrapping the alternative body part
        // + one base64 attachment part each. A distinct outer boundary so the
        // nested alternative's parts can't collide with the mixed delimiters.
        let mixed_boundary = format!("{alt_boundary}_mix");
        m.push_str(&format!(
            "Content-Type: multipart/mixed; boundary=\"{mixed_boundary}\"\r\n\r\n"
        ));
        // Body part — a nested multipart/alternative.
        m.push_str(&format!("--{mixed_boundary}\r\n"));
        m.push_str(&format!(
            "Content-Type: multipart/alternative; boundary=\"{alt_boundary}\"\r\n\r\n"
        ));
        push_alternative_body(&mut m, &alt_boundary, body);
        m.push_str(&format!("--{alt_boundary}--\r\n"));
        // Attachment parts.
        for att in attachments {
            m.push_str(&format!("--{mixed_boundary}\r\n"));
            m.push_str(&format!("Content-Type: {}\r\n", att.mime_type));
            m.push_str("Content-Transfer-Encoding: base64\r\n");
            m.push_str(&format!(
                "Content-Disposition: attachment; filename=\"{}\"\r\n\r\n",
                sanitize_filename(att.filename)
            ));
            m.push_str(&fauna_core::mime_wrap::base64_wrap_76(att.bytes));
        }
        m.push_str(&format!("--{mixed_boundary}--\r\n"));
    }
    m.into_bytes()
}

/// Emit the two `multipart/alternative` body parts (text/plain markdown source
/// and text/html rendered) between `--boundary` delimiters. The caller writes
/// the closing `--boundary--`.
fn push_alternative_body(m: &mut String, boundary: &str, body: &str) {
    // text/plain alternative — the markdown source itself.
    m.push_str(&format!("--{boundary}\r\n"));
    m.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    m.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
    m.push_str(&to_crlf(body));
    m.push_str("\r\n");

    // text/html alternative — markdown→HTML via the shared sanitized renderer.
    m.push_str(&format!("--{boundary}\r\n"));
    m.push_str("Content-Type: text/html; charset=utf-8\r\n");
    m.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
    m.push_str(&to_crlf(&html_document(body)));
    m.push_str("\r\n");
}

/// Strip the characters that would break a quoted `filename="…"` parameter
/// (double-quote, CR, LF). RFC 2231 extended encoding for non-ASCII is deferred;
/// most filenames are ASCII and a stripped quote is a safe lossy fallback.
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .filter(|c| *c != '"' && *c != '\r' && *c != '\n')
        .collect()
}

/// Normalize line endings to CRLF (RFC 5322 requires CRLF; the compose body and
/// generated HTML use `\n`).
fn to_crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// Wrap the shared markdown→HTML output in a minimal HTML document for the
/// `text/html` alternative.
fn html_document(markdown: &str) -> String {
    let inner = fauna_core::markdown::markdown_to_html(markdown);
    format!(
        "<!DOCTYPE html>\n<html>\n<head><meta charset=\"utf-8\"></head>\n<body>\n{inner}\n</body>\n</html>"
    )
}

/// A MIME boundary unique to this message, derived from its `Message-ID` so it is
/// stable and collision-free without a RNG (this crate is WASM-safe and
/// dependency-light). Non-alphanumeric chars are stripped so the boundary is a
/// valid token that the body cannot contain verbatim.
fn multipart_boundary(message_id: &str) -> String {
    let seed: String = message_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    format!("----=_Fauna_{seed}")
}

/// Format a unix timestamp (seconds) as an RFC 5322 `Date:` value in UTC, e.g.
/// `Sun, 25 May 2026 12:30:00 +0000`. Kept as this module's name for the
/// grammar; the grammar itself belongs to [`fauna_core::imf_date`], which the
/// mail, RSS, TLSRPT and HTTP-signature emitters all share (it is dependency-
/// free, so this crate stays WASM-safe).
pub fn format_rfc5322_date(unix_secs: i64) -> String {
    fauna_core::imf_date::format_rfc5322_date(unix_secs)
}

// ── Inbound parsing ──────────────────────────────────────────────────

/// Fields extracted from an inbound RFC 5322 message — exactly what the SMTP
/// rail maps onto a `RailInboundMessage`.
///
/// **Two decoders, one per concern.** `Message-ID` and `In-Reply-To` come from
/// a minimal hand-rolled header parser: headers are unfolded (continuation
/// lines joined) and matched case-insensitively; the body is everything after
/// the first blank line, decoded as UTF-8 (lossy) with line endings normalized
/// to `\n` and trailing newlines trimmed. It does **not** decode MIME
/// multipart parts or `Content-Transfer-Encoding` (base64/quoted-printable) —
/// the body is the raw post-headers text; richer MIME handling for the body
/// the SMTP backend actually renders lives in [`crate::html_markdown`].
///
/// `From`, `To` and `Subject` instead go through `mail_parser`, which decodes
/// RFC 2047 encoded-words and respects RFC 5322 quoted display names (so
/// `"Doe, John" <j@x>` doesn't split on the display name's comma). Only the
/// bare address is kept for `from`/`to` — a decoded display name has nowhere
/// to go, so it's discarded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedMessage {
    /// The `From:` address (inside `<…>` if present, else the trimmed value).
    pub from: Option<String>,
    /// The `To:` addresses (comma-split; each reduced to its `<…>` address).
    pub to: Vec<String>,
    pub subject: Option<String>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub body: String,
}

/// Parse an inbound RFC 5322 message into [`ParsedMessage`]. Lossy on non-UTF-8
/// input (headers are ASCII; the body is decoded with replacement). Never
/// fails — a malformed message yields best-effort fields (the caller decides
/// whether an empty `from`/`to` is usable).
pub fn parse_message(raw: &[u8]) -> ParsedMessage {
    let text = String::from_utf8_lossy(raw);
    let (header_block, body) = split_headers_body(&text);

    let mut parsed = ParsedMessage {
        body,
        ..Default::default()
    };
    for (name, value) in unfold_headers(header_block) {
        match name.to_ascii_lowercase().as_str() {
            "message-id" => parsed.message_id = non_empty(value.trim()),
            "in-reply-to" => parsed.in_reply_to = non_empty(value.trim()),
            _ => {}
        }
    }

    // `From`/`To`/`Subject` are re-parsed by `mail_parser` for RFC 2047 +
    // quoted-display-name handling (see `ParsedMessage`'s doc). Re-terminate
    // the header block with a blank line so `parse_headers` sees a clean
    // header/body boundary regardless of the source's own line endings (the
    // `lf_only_separator_and_no_body` case has none at all).
    let mut header_only = header_block.as_bytes().to_vec();
    header_only.extend_from_slice(b"\r\n\r\n");
    if let Some(msg) = mail_parser::MessageParser::default().parse_headers(&header_only) {
        parsed.from = first_address(msg.from());
        parsed.to = flat_addresses(msg.to());
        parsed.subject = msg.subject().and_then(|s| non_empty(s.trim()));
    }
    parsed
}

/// Reduce a `mail_parser` address field to its bare `mailbox@host` strings,
/// dropping any display name — [`ParsedMessage::from`]/[`ParsedMessage::to`]
/// only ever store the address. Walks both list and group syntax
/// (`Team: a@x.test, b@y.test;`).
fn flat_addresses(addr: Option<&mail_parser::Address<'_>>) -> Vec<String> {
    let Some(addr) = addr else {
        return Vec::new();
    };
    let entries: Vec<&mail_parser::Addr<'_>> = match addr {
        mail_parser::Address::List(list) => list.iter().collect(),
        mail_parser::Address::Group(groups) => {
            groups.iter().flat_map(|g| g.addresses.iter()).collect()
        }
    };
    entries
        .into_iter()
        .filter_map(|entry| entry.address.as_deref())
        .filter(|a| !a.is_empty())
        .map(str::to_string)
        .collect()
}

fn first_address(addr: Option<&mail_parser::Address<'_>>) -> Option<String> {
    flat_addresses(addr).into_iter().next()
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty()).then(|| s.to_string())
}

/// Split a message into (header block, normalized body) at the first blank
/// line (CRLF or LF). With no blank line the whole input is headers.
fn split_headers_body(text: &str) -> (&str, String) {
    let (headers, body_raw) = if let Some(i) = text.find("\r\n\r\n") {
        (&text[..i], &text[i + 4..])
    } else if let Some(i) = text.find("\n\n") {
        (&text[..i], &text[i + 2..])
    } else {
        (text, "")
    };
    let body = body_raw
        .replace("\r\n", "\n")
        .trim_end_matches('\n')
        .to_string();
    (headers, body)
}

/// Unfold a header block into `(name, value)` pairs. A line starting with space
/// or tab continues the previous header's value (the fold is replaced by a
/// single space, per RFC 5322 §2.2.3).
fn unfold_headers(block: &str) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in block.replace("\r\n", "\n").split('\n') {
        if line.is_empty() {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some((_, value)) = headers.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minted_message_id_is_unique_and_unguessable() {
        // Both properties the guardian mail gate and the dedup key lean on. The
        // predecessor — `<{unix_secs}.{process_local_seq}@domain>` — had
        // neither: two of a user's devices sending in the same second both
        // minted `<secs.0@domain>` (a dedup-key collision hides one message from
        // a later import), and a clock is guessable, so a stranger could forge a
        // delivery-status report naming an id the child never showed them.
        let a = new_message_id("fauna.test");
        let b = new_message_id("fauna.test");
        assert_ne!(a, b, "two mints must never collide");

        let token = a
            .strip_prefix('<')
            .and_then(|s| s.strip_suffix("@fauna.test>"))
            .expect("shape is <token@domain>");
        assert_eq!(token.len(), 32, "128 bits, hex-encoded");
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
        // A timestamp-derived id would share a long prefix across mints; random
        // ones share essentially nothing.
        let b_token = b.trim_start_matches('<');
        assert_ne!(
            &token[..8],
            &b_token[..8],
            "the leading bits must not be a clock"
        );
    }

    #[test]
    fn epoch_formats_as_thursday() {
        assert_eq!(format_rfc5322_date(0), "Thu, 01 Jan 1970 00:00:00 +0000");
    }

    #[test]
    fn known_timestamp_formats_correctly() {
        // 1_700_000_000 == 2023-11-14T22:13:20Z (a Tuesday).
        assert_eq!(
            format_rfc5322_date(1_700_000_000),
            "Tue, 14 Nov 2023 22:13:20 +0000"
        );
    }

    #[test]
    fn builds_well_formed_multipart_message() {
        let raw = build_message(
            "alice@localhost",
            &["bob@external.test".to_string()],
            Some("Hello there"),
            "Line one\nLine two",
            "<abc.1@localhost>",
            None,
            1_700_000_000,
            &[],
        );
        let text = String::from_utf8(raw).unwrap();
        assert!(text.starts_with("From: alice@localhost\r\n"), "{text:?}");
        assert!(text.contains("To: bob@external.test\r\n"));
        assert!(text.contains("Subject: Hello there\r\n"));
        assert!(text.contains("Message-ID: <abc.1@localhost>\r\n"));
        assert!(text.contains("Date: Tue, 14 Nov 2023 22:13:20 +0000\r\n"));
        // Boundary derived from the Message-ID (alphanumerics only).
        assert!(
            text.contains(
                "Content-Type: multipart/alternative; boundary=\"----=_Fauna_abc1localhost\"\r\n"
            ),
            "{text:?}"
        );
        // text/plain alternative carries the markdown source verbatim (CRLF).
        assert!(text.contains("Content-Type: text/plain; charset=utf-8\r\n"));
        assert!(
            text.contains("\r\n\r\nLine one\r\nLine two\r\n"),
            "{text:?}"
        );
        // text/html alternative carries the rendered HTML (paragraph joined).
        assert!(text.contains("Content-Type: text/html; charset=utf-8\r\n"));
        assert!(text.contains("<p>Line one Line two</p>"), "{text:?}");
        // On-wire part delimiters are `--` + boundary; closing adds a trailing `--`.
        assert!(
            text.contains("\r\n------=_Fauna_abc1localhost\r\n"),
            "{text:?}"
        );
        assert!(
            text.ends_with("\r\n------=_Fauna_abc1localhost--\r\n"),
            "{text:?}"
        );
    }

    #[test]
    fn html_alternative_renders_markdown_formatting() {
        let raw = build_message(
            "a@x.test",
            &["b@y.test".to_string()],
            Some("Fmt"),
            "# Title\n\n**bold** and a [link](https://e.test)",
            "<id@x.test>",
            None,
            0,
            &[],
        );
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("<h1>Title</h1>"), "{text:?}");
        assert!(text.contains("<strong>bold</strong>"), "{text:?}");
        assert!(
            text.contains("<a href=\"https://e.test\""),
            "sanitized link in html part: {text:?}"
        );
    }

    #[test]
    fn multiple_recipients_comma_joined() {
        let raw = build_message(
            "a@x.test",
            &["b@y.test".to_string(), "c@z.test".to_string()],
            None,
            "hi",
            "<id@x.test>",
            None,
            0,
            &[],
        );
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("To: b@y.test, c@z.test\r\n"));
        // No subject header when subject is None.
        assert!(!text.contains("Subject:"));
    }

    #[test]
    fn in_reply_to_header_emitted_when_present() {
        let raw = build_message(
            "a@x.test",
            &["b@y.test".to_string()],
            Some("Re: hi"),
            "reply",
            "<id2@x.test>",
            Some("<parent@y.test>"),
            0,
            &[],
        );
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("In-Reply-To: <parent@y.test>\r\n"));
    }

    #[test]
    fn header_parser_round_trips_built_message() {
        // `parse_message` is the header parser (production uses it only for
        // From/To/Subject/Message-ID/In-Reply-To); the body now lives in MIME
        // parts, so its `.body` is no longer the markdown source.
        let raw = build_message(
            "alice@localhost",
            &["bob@external.test".to_string()],
            Some("Hello there"),
            "Line one\nLine two",
            "<abc.1@localhost>",
            Some("<parent@y.test>"),
            1_700_000_000,
            &[],
        );
        let p = parse_message(&raw);
        assert_eq!(p.from.as_deref(), Some("alice@localhost"));
        assert_eq!(p.to, vec!["bob@external.test".to_string()]);
        assert_eq!(p.subject.as_deref(), Some("Hello there"));
        assert_eq!(p.message_id.as_deref(), Some("<abc.1@localhost>"));
        assert_eq!(p.in_reply_to.as_deref(), Some("<parent@y.test>"));
    }

    #[test]
    fn markdown_round_trips_outbound_to_inbound() {
        // The tier_1 version of the html-mail round-trip: a markdown compose
        // body, serialized to multipart/alternative, parsed back through the
        // MIME-aware inbound selector, lands as markdown (never raw tags) with
        // formatting preserved.
        let raw = build_message(
            "a@x.test",
            &["b@y.test".to_string()],
            Some("Hi"),
            "# News\n\n**bold** body",
            "<id@x.test>",
            None,
            0,
            &[],
        );
        let (body, fmt) = crate::html_markdown::inbound_mail_body(&raw);
        assert_eq!(fmt, crate::message::BodyFormat::Markdown);
        assert!(body.contains("# News"), "heading round-trips: {body:?}");
        assert!(body.contains("**bold**"), "bold round-trips: {body:?}");
        assert!(
            !body.contains("<p>") && !body.contains("<h1>"),
            "no raw tags: {body:?}"
        );
    }

    #[test]
    fn attachment_wraps_body_in_multipart_mixed_with_base64_part() {
        use base64::Engine;
        let png = b"\x89PNG\r\n\x1a\nfake-image-bytes";
        let raw = build_message(
            "alice@localhost",
            &["bob@external.test".to_string()],
            Some("With a pic"),
            "see attached",
            "<att.1@localhost>",
            None,
            0,
            &[MimeAttachment {
                filename: "pic.png",
                mime_type: "image/png",
                bytes: png,
            }],
        );
        let text = String::from_utf8(raw).unwrap();
        // Top-level is multipart/mixed; the body alternative is nested inside it.
        assert!(
            text.contains(
                "Content-Type: multipart/mixed; boundary=\"----=_Fauna_att1localhost_mix\"\r\n"
            ),
            "{text:?}"
        );
        assert!(
            text.contains(
                "Content-Type: multipart/alternative; boundary=\"----=_Fauna_att1localhost\"\r\n"
            ),
            "nested alternative: {text:?}"
        );
        // The body alternatives still render.
        assert!(text.contains("Content-Type: text/plain; charset=utf-8\r\n"));
        assert!(text.contains("Content-Type: text/html; charset=utf-8\r\n"));
        // The attachment part: declared type, base64 transfer-encoding, and a
        // Content-Disposition naming the file.
        assert!(text.contains("Content-Type: image/png\r\n"), "{text:?}");
        assert!(
            text.contains("Content-Transfer-Encoding: base64\r\n"),
            "{text:?}"
        );
        assert!(
            text.contains("Content-Disposition: attachment; filename=\"pic.png\"\r\n"),
            "{text:?}"
        );
        // The bytes round-trip through base64 (unwrap the CRLF folding first).
        let expected_b64 = base64::engine::general_purpose::STANDARD.encode(png);
        assert!(
            text.replace("\r\n", "").contains(&expected_b64),
            "base64 payload present: {text:?}"
        );
        // Closes with the mixed boundary terminator.
        assert!(
            text.ends_with("\r\n------=_Fauna_att1localhost_mix--\r\n"),
            "{text:?}"
        );
    }

    #[test]
    fn filename_quote_is_stripped_from_disposition() {
        let raw = build_message(
            "a@x.test",
            &["b@y.test".to_string()],
            None,
            "hi",
            "<q@x.test>",
            None,
            0,
            &[MimeAttachment {
                filename: "ev\"il\".txt",
                mime_type: "text/plain",
                bytes: b"x",
            }],
        );
        let text = String::from_utf8(raw).unwrap();
        assert!(
            text.contains("filename=\"evil.txt\"\r\n"),
            "embedded quotes stripped: {text:?}"
        );
    }

    #[test]
    fn extracts_address_from_display_name_and_splits_list() {
        let raw = b"From: Alice Example <alice@x.test>\r\n\
            To: Bob <bob@y.test>, carol@z.test\r\n\
            Subject: Hi\r\n\
            \r\n\
            body text\r\n";
        let p = parse_message(raw);
        assert_eq!(p.from.as_deref(), Some("alice@x.test"));
        assert_eq!(
            p.to,
            vec!["bob@y.test".to_string(), "carol@z.test".to_string()]
        );
        assert_eq!(p.body, "body text");
    }

    #[test]
    fn a_quoted_display_name_containing_a_comma_does_not_split_the_address_list() {
        // A naive comma-split would cut `"Doe, John"` into a bogus `"Doe`
        // recipient — `mail_parser` walks the real RFC 5322 address-list
        // grammar instead.
        let raw = b"From: a@x.test\r\n\
            To: \"Doe, John\" <j@x.test>, carol@z.test\r\n\
            Subject: Hi\r\n\
            \r\n\
            body text\r\n";
        let p = parse_message(raw);
        assert_eq!(
            p.to,
            vec!["j@x.test".to_string(), "carol@z.test".to_string()],
            "the quoted display name's comma must not split into a bogus address"
        );
    }

    #[test]
    fn an_rfc_2047_encoded_word_subject_is_decoded() {
        let raw = b"From: a@x.test\r\nSubject: =?utf-8?B?THVuY2g=?=\r\n\r\nbody\r\n";
        let p = parse_message(raw);
        assert_eq!(p.subject.as_deref(), Some("Lunch"));
    }

    #[test]
    fn encoded_and_plain_spellings_of_one_subject_parse_identically() {
        // The property `conversations.md`'s subject-keyed threading and the
        // own-upgrade compare (`store/threads.rs`) both lean on: an encoded
        // and a plain spelling of the same text are the same subject, not a
        // mismatch, whatever either of those consumers later does with it.
        let plain = parse_message(b"From: a@x.test\r\nSubject: Lunch\r\n\r\nbody\r\n");
        let encoded =
            parse_message(b"From: a@x.test\r\nSubject: =?utf-8?B?THVuY2g=?=\r\n\r\nbody\r\n");
        assert_eq!(plain.subject, encoded.subject);
        assert_eq!(plain.subject.as_deref(), Some("Lunch"));
    }

    #[test]
    fn unfolds_folded_subject_header() {
        // RFC 5322 §2.2.3 folding: a continuation line (leading WSP) joins the
        // previous header value with a single space.
        let raw = b"From: a@x.test\r\nSubject: a very\r\n long subject\r\n\r\nhi\r\n";
        let p = parse_message(raw);
        assert_eq!(p.subject.as_deref(), Some("a very long subject"));
    }

    #[test]
    fn missing_optional_headers_are_none() {
        let raw = b"From: a@x.test\r\nTo: b@y.test\r\n\r\njust a body\r\n";
        let p = parse_message(raw);
        assert_eq!(p.subject, None);
        assert_eq!(p.message_id, None);
        assert_eq!(p.in_reply_to, None);
        assert_eq!(p.body, "just a body");
    }

    #[test]
    fn lf_only_separator_and_no_body() {
        // LF-only line endings (no CRLF) and a header-only message.
        let raw = b"From: a@x.test\nSubject: empty\n";
        let p = parse_message(raw);
        assert_eq!(p.from.as_deref(), Some("a@x.test"));
        assert_eq!(p.subject.as_deref(), Some("empty"));
        assert_eq!(p.body, "");
    }
}
