//! Build the canonical inbound `Received:` trace header the MTA prepends to a
//! message before sealing + `ingest_inbound_mail` (smtp-server.md § Architectural
//! rules: "the bridge MUST prepend a single canonical `Received:` header … any
//! CR / LF / non-printable byte forces the literal `unknown` placeholder").
//!
//! This is the inbound sibling of [`crate::outbound::received_strip`]: outbound
//! mail has its `Received:` chain *stripped* before relay (privacy); inbound mail
//! gets exactly one Fauna `Received:` header *prepended* (provenance trace, like a
//! hardened postfix). Both are pure shared-Rust fns crossed over UniFFI so the Go
//! MTA bridge calls one implementation (priority #2) — the production retired
//! `bins/fauna-bridge-imap` terminator did this as `BuildReceivedHeader`; the I5
//! daemon→WS-RPC migration dropped it on the new `fauna-mail-bridge` path until
//! this restored it.
//!
//! Pure / Go-side split (mirrors the scan-gate split, content-scoring.md): the
//! *I/O* — reading the wall clock and minting a random queue id — stays Go-side;
//! the *formatting + sanitization* lives here. `now_unix_secs` and `queue_id` are
//! passed in so the build is a deterministic fn of its inputs (and the unit tests
//! assert exact bytes, including the date line).

use chrono::{DateTime, Utc};

/// Per-transaction context the bridge folds into the `Received:` header. See
/// RFC 5321 §4.4 / RFC 5322 §3.6.7; TLS-extension fields per RFC 8314 §4.1.
///
/// `server_hostname`, `tls_version`, `tls_cipher`, `queue_id` are bridge-owned
/// (not sender-controlled). `helo_domain` and `client_ip` are sender-controlled
/// and sanitized via [`safe_token`] — a hostile EHLO carrying `\r\n` cannot forge
/// a second header line.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ReceivedHeaderOpts {
    /// Our receiving host for the `by` clause — the bridge's first local
    /// (mail-hosting) domain or OS hostname. Empty / unsafe ⇒ `fauna-bridge.invalid`.
    pub server_hostname: String,
    /// EHLO/HELO the sender announced; sanitized to `unknown` if it carries any
    /// CR / LF / non-printable byte.
    pub helo_domain: String,
    /// Remote peer IP (no port); sanitized to `unknown` likewise.
    pub client_ip: String,
    /// TLS protocol version, e.g. `TLS1.3`; empty ⇒ cleartext (`with ESMTP`,
    /// no cipher parenthetical). Non-empty ⇒ `with ESMTPS (<ver> <cipher>)`.
    pub tls_version: String,
    /// TLS cipher suite name, e.g. `TLS_AES_256_GCM_SHA384`. Ignored when
    /// `tls_version` is empty.
    pub tls_cipher: String,
    /// Short opaque transaction id for the `id` clause (Go mints it with
    /// crypto/rand — that's the I/O half). Empty ⇒ a fixed all-zero placeholder.
    pub queue_id: String,
    /// Single-recipient envelope address for the `for <addr>;` clause. Empty for
    /// a multi-recipient delivery — the one prepended header is sealed to every
    /// recipient, so emitting `for` would leak cross-recipient correlation.
    pub recipient: String,
}

/// Build one CRLF-folded canonical `Received:` header field, ready to hand to the
/// Go `prependHeaders` as a single element (internal continuation lines are
/// tab-indented per RFC 5322 §2.2.3; **no** trailing CRLF — the caller adds the
/// one terminator). The final clause before the date carries the RFC 5321 §4.4
/// `;`. Returns a `String` (ASCII).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn build_received_header(now_unix_secs: i64, opts: ReceivedHeaderOpts) -> String {
    let helo = safe_token(&opts.helo_domain);
    let ip = safe_token(&opts.client_ip);
    let mut server = safe_token(&opts.server_hostname);
    if server == "unknown" {
        // Empty / unsafe hostname is a misconfiguration; a placeholder beats an
        // empty `by ` clause that won't parse.
        server = "fauna-bridge.invalid".to_string();
    }
    let queue_id = if opts.queue_id.is_empty() {
        // Go always supplies a fresh id; defensive fallback only.
        "0000000000000000".to_string()
    } else {
        safe_token(&opts.queue_id)
    };
    let date = DateTime::<Utc>::from_timestamp(now_unix_secs, 0)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).expect("epoch is in range"))
        .to_rfc2822();

    // Physical lines joined by CRLF; first line unindented, continuations tabbed.
    let mut lines: Vec<String> = Vec::with_capacity(5);
    lines.push(format!("Received: from {helo} ([{ip}])"));
    if opts.tls_version.is_empty() {
        lines.push(format!("\tby {server} with ESMTP"));
    } else {
        let ver = safe_token(&opts.tls_version);
        let cipher = safe_token(&opts.tls_cipher);
        lines.push(format!("\tby {server} with ESMTPS ({ver} {cipher})"));
    }

    let recipient = if opts.recipient.is_empty() {
        String::new()
    } else {
        safe_token(&opts.recipient)
    };
    if recipient.is_empty() {
        // No `for` clause — the `id` line carries the closing `;`.
        lines.push(format!("\tid {queue_id};"));
    } else {
        lines.push(format!("\tid {queue_id}"));
        lines.push(format!("\tfor <{recipient}>;"));
    }
    lines.push(format!("\t{date}"));

    lines.join("\r\n")
}

/// Returns `s` verbatim when, after trimming surrounding whitespace, it is
/// non-empty and every byte is printable ASCII (`0x20..=0x7e`); otherwise the
/// canonical `unknown`. Any CR / LF / HTAB / control / non-ASCII byte trips it —
/// defeating a hostile EHLO like `evil.example\r\nReceived: from spoofed` that
/// would otherwise inject a forged header line.
fn safe_token(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return "unknown".to_string();
    }
    for b in trimmed.bytes() {
        if !(0x20..=0x7e).contains(&b) {
            return "unknown".to_string();
        }
    }
    trimmed.to_string()
}

/// The one `X-Fauna-*` header that legitimately rides the inbound wire and is
/// **consumed inbound** — the forward-loop trace
/// (`crate::forward_loop::HEADER_FORWARDED_BY`, `mail-forwarding.md` § Loop
/// detection: parsed on inbound to suppress a self-loop, and "never stripped …
/// stripping would break the cooperative loop-detection floor"). Every *other*
/// `X-Fauna-*` header is a bridge **delivery-stamp** (`X-Fauna-Scan-*` /
/// `X-Fauna-Address-*`) that the bridge re-stamps genuinely downstream, so a copy
/// already present on the inbound message is sender-forged. Kept as a literal
/// (not a `forward_loop::` reference) so `received-header` need not pull the
/// `forward-loop` feature; a `#[cfg(feature = "forward-loop")]` test pins the two
/// equal so they cannot drift.
const PRESERVED_FAUNA_HEADER: &[u8] = b"X-Fauna-Forwarded-By";

/// Returns a copy of `raw` with every header in the reserved `X-Fauna-*`
/// namespace removed — **except** [`PRESERVED_FAUNA_HEADER`] — the body copied
/// verbatim.
///
/// Inbound counterpart of [`crate::outbound::received_strip::strip_received_headers`]:
/// the inbound bridge stamps its own genuine `X-Fauna-Scan-*` / `X-Fauna-Address-*`
/// trace (+ the canonical `Received:`) onto the sealed copy at delivery, so any
/// `X-Fauna-*` already present is sender-forged — a Fauna nest only ever stamps
/// these at delivery, never on the wire. Calling this at the inbound DATA stage,
/// before the genuine prepend, generalizes the "exactly one Fauna trace" intent
/// (`smtp-server.md` § Architectural rules) from `Received:` to the whole reserved
/// namespace, so a forged `X-Fauna-Address-Matched` can neither match a
/// recipient's alias-metadata filter rule nor land in the copy a client reads.
///
/// The `X-Fauna-Forwarded-By` carve-out is load-bearing — that header is read
/// inbound by the forward-loop floor and `mail-forwarding.md` mandates it is never
/// stripped; a blanket strip would tornado a forward loop. The match is a
/// case-insensitive `X-Fauna-` prefix (RFC 5322 §2.2.3 continuation-aware),
/// substring-safe: a header merely *containing* `x-fauna` (e.g. `X-Not-Fauna`) is
/// not stripped. Fail-safe like its outbound sibling — if the walker can't make
/// progress, the remainder is copied verbatim.
pub fn strip_fauna_headers(raw: &[u8]) -> Vec<u8> {
    crate::header_walk::strip_headers_where(raw, is_forged_fauna_stamp)
}

/// True when `name` is a reserved `X-Fauna-*` delivery-stamp field name that a
/// sender must not supply — i.e. the `X-Fauna-` prefix (case-insensitive) and not
/// the inbound-consumed [`PRESERVED_FAUNA_HEADER`]. Trims surrounding ASCII
/// whitespace defensively (RFC 5322 forbids WSP before the colon, but a malformed
/// name could carry it — and the forward-loop reader likewise matches the exact
/// name, so a whitespace-mangled "Forwarded-By" is not the live header anyway).
fn is_forged_fauna_stamp(name: &[u8]) -> bool {
    let name = name.trim_ascii();
    name.len() >= 8
        && name[..8].eq_ignore_ascii_case(b"X-Fauna-")
        && !name.eq_ignore_ascii_case(PRESERVED_FAUNA_HEADER)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2023-11-14T22:13:20Z — a Tuesday. chrono to_rfc2822 → "+0000", zero-padded.
    const TS: i64 = 1_700_000_000;
    const DATE: &str = "Tue, 14 Nov 2023 22:13:20 +0000";

    fn opts() -> ReceivedHeaderOpts {
        ReceivedHeaderOpts {
            server_hostname: "mx.example.com".to_string(),
            helo_domain: "sender.example".to_string(),
            client_ip: "203.0.113.7".to_string(),
            tls_version: String::new(),
            tls_cipher: String::new(),
            queue_id: "a1b2c3d4e5f60718".to_string(),
            recipient: String::new(),
        }
    }

    #[test]
    fn cleartext_multi_recipient_exact() {
        let got = build_received_header(TS, opts());
        let want = format!(
            "Received: from sender.example ([203.0.113.7])\r\n\
             \tby mx.example.com with ESMTP\r\n\
             \tid a1b2c3d4e5f60718;\r\n\
             \t{DATE}"
        );
        assert_eq!(got, want);
        // No trailing CRLF — prependHeaders adds the single terminator.
        assert!(!got.ends_with("\r\n"));
        // Exactly one `Received:` field.
        assert_eq!(got.matches("Received:").count(), 1);
    }

    #[test]
    fn tls_single_recipient_has_esmtps_cipher_and_for_clause() {
        let mut o = opts();
        o.tls_version = "TLS1.3".to_string();
        o.tls_cipher = "TLS_AES_256_GCM_SHA384".to_string();
        o.recipient = "alice@example.com".to_string();
        let got = build_received_header(TS, o);
        let want = format!(
            "Received: from sender.example ([203.0.113.7])\r\n\
             \tby mx.example.com with ESMTPS (TLS1.3 TLS_AES_256_GCM_SHA384)\r\n\
             \tid a1b2c3d4e5f60718\r\n\
             \tfor <alice@example.com>;\r\n\
             \t{DATE}"
        );
        assert_eq!(got, want);
    }

    #[test]
    fn helo_with_crlf_is_sanitized_to_unknown() {
        let mut o = opts();
        // A header-injection attempt: the CRLF would otherwise start a 2nd field.
        o.helo_domain = "evil.example\r\nReceived: from spoofed.example".to_string();
        let got = build_received_header(TS, o);
        assert!(got.contains("from unknown (["));
        // Injection defeated: still exactly one Received field, no `spoofed`.
        assert_eq!(got.matches("Received:").count(), 1);
        assert!(!got.contains("spoofed"));
    }

    #[test]
    fn non_printable_and_nonascii_ip_sanitized() {
        let mut o = opts();
        o.client_ip = "203.0.113.7\x00".to_string(); // NUL control byte
        assert!(build_received_header(TS, o).contains("([unknown])"));
        let mut o2 = opts();
        o2.client_ip = "203.0.113.7€".to_string(); // non-ASCII
        assert!(build_received_header(TS, o2).contains("([unknown])"));
    }

    #[test]
    fn empty_server_hostname_falls_back_to_invalid_placeholder() {
        let mut o = opts();
        o.server_hostname = String::new();
        assert!(build_received_header(TS, o).contains("\tby fauna-bridge.invalid with ESMTP"));
    }

    #[test]
    fn empty_queue_id_uses_zero_placeholder() {
        let mut o = opts();
        o.queue_id = String::new();
        assert!(build_received_header(TS, o).contains("\tid 0000000000000000;"));
    }

    #[test]
    fn safe_token_allows_interior_dots_and_at() {
        assert_eq!(safe_token("  mx.example.com  "), "mx.example.com");
        assert_eq!(safe_token("alice@example.com"), "alice@example.com");
        assert_eq!(safe_token(""), "unknown");
        assert_eq!(safe_token("   "), "unknown");
        assert_eq!(safe_token("a\tb"), "unknown");
    }

    // ── strip_fauna_headers (EF-2) ─────────────────────────────────

    #[test]
    fn strips_forged_scan_and_address_stamps() {
        // A sender forges the bridge's own delivery-stamp namespace, trying to
        // spoof a clean scan verdict and a matched-alias metadata header.
        let raw = b"X-Fauna-Scan-Clamav: clean\r\n\
                    X-Fauna-Address-Suffix: admin\r\n\
                    From: attacker@evil.example\r\n\
                    Subject: hi\r\n\r\nbody\r\n";
        let out = strip_fauna_headers(raw);
        let s = std::str::from_utf8(&out).unwrap();
        assert!(
            !s.contains("X-Fauna-Scan-Clamav"),
            "forged scan stamp stripped"
        );
        assert!(
            !s.contains("X-Fauna-Address-Suffix"),
            "forged address stamp stripped"
        );
        // Legitimate sender headers and the body are untouched.
        assert_eq!(
            s,
            "From: attacker@evil.example\r\nSubject: hi\r\n\r\nbody\r\n"
        );
    }

    #[test]
    fn preserves_forwarded_by_for_loop_detection() {
        // X-Fauna-Forwarded-By is read inbound by the forward-loop floor and
        // mail-forwarding.md mandates it is never stripped — a blanket strip
        // would tornado a loop. It MUST survive.
        let raw = b"X-Fauna-Forwarded-By: actor=abc; t=1; rule=forward-all\r\n\
                    X-Fauna-Scan-Rspamd-Score: 0.0\r\n\
                    From: a@x\r\n\r\nbody";
        let out = strip_fauna_headers(raw);
        let s = std::str::from_utf8(&out).unwrap();
        assert!(
            s.contains("X-Fauna-Forwarded-By: actor=abc; t=1; rule=forward-all"),
            "forward-loop trace must be preserved"
        );
        assert!(
            !s.contains("X-Fauna-Scan-Rspamd-Score"),
            "the forged scan stamp beside it is still stripped"
        );
    }

    #[test]
    fn strips_arbitrary_unknown_fauna_header_fail_safe() {
        // Fail-safe: any X-Fauna-* the sender invents (a future stamp namespace,
        // a typo'd spoof) is stripped by default — only the explicit carve-out
        // survives.
        let raw = b"X-Fauna-Whatever: spoof\r\nFrom: a@x\r\n\r\nb";
        let out = strip_fauna_headers(raw);
        assert_eq!(std::str::from_utf8(&out).unwrap(), "From: a@x\r\n\r\nb");
    }

    #[test]
    fn case_insensitive_and_prefix_exact() {
        let raw = b"x-fauna-scan-clamav: lower\r\n\
                    X-FAUNA-ADDRESS-CATCHALL: upper\r\n\
                    X-Not-Fauna: keep\r\n\
                    X-Faunatic: keep-too\r\n\
                    From: a@x\r\n\r\nbody";
        let out = strip_fauna_headers(raw);
        let s = std::str::from_utf8(&out).unwrap();
        assert!(!s.contains("lower"), "lowercase forged stamp stripped");
        assert!(!s.contains("upper"), "uppercase forged stamp stripped");
        assert!(
            s.contains("X-Not-Fauna: keep"),
            "a header not starting with X-Fauna- survives"
        );
        assert!(
            s.contains("X-Faunatic: keep-too"),
            "X-Faunatic lacks the trailing '-', so it is not the X-Fauna- namespace"
        );
    }

    #[test]
    fn strips_forged_stamp_with_continuation_line() {
        // A folded (continuation) forged header travels with its parent and is
        // dropped whole.
        let raw = b"X-Fauna-Address-Wildcard-Suffix: foo\r\n bar\r\nFrom: a@x\r\n\r\nbody";
        let out = strip_fauna_headers(raw);
        assert_eq!(std::str::from_utf8(&out).unwrap(), "From: a@x\r\n\r\nbody");
    }

    #[test]
    fn no_fauna_headers_returns_input_verbatim() {
        let raw = b"From: a@x\r\nTo: b@y\r\nSubject: hi\r\n\r\nbody\r\n";
        assert_eq!(strip_fauna_headers(raw), raw);
    }

    #[test]
    fn empty_input_returns_empty() {
        assert_eq!(strip_fauna_headers(b""), Vec::<u8>::new());
    }

    // The preserved carve-out name must stay byte-identical to the forward-loop
    // header the inbound floor reads — else a strip here would silently break
    // loop detection. Pinned whenever both features compile.
    #[cfg(feature = "forward-loop")]
    #[test]
    fn preserved_name_matches_forward_loop_header() {
        assert_eq!(
            PRESERVED_FAUNA_HEADER,
            crate::forward_loop::HEADER_FORWARDED_BY.as_bytes()
        );
    }
}
