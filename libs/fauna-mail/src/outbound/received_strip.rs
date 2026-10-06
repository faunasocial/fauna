//! Strip `Received:` headers from outbound messages before relay.
//!
//! The production submission flow (`bins/fauna-bridges/internal/mta/submission.go::Data`)
//! takes the user's raw RFC 5322 bytes and DKIM-signs them before handing
//! the result to the outbound MX worker. Some MUAs and intermediate relays
//! add `Received:` headers exposing internal hostnames or IPs (the user's
//! submitting client IP, our bridge's hostname, etc.). When the bridge
//! relays the message to the recipient's MX, those internal-routing details
//! would land in the recipient's mailbox — a privacy leak.
//!
//! `strip_received_headers` walks the message header section once and emits
//! a copy with all `Received:` headers removed (whole logical header,
//! including any continuation lines). The body is copied verbatim. The
//! output is byte-for-byte the input minus the stripped headers, so the
//! DKIM signature (computed *after* the strip) matches what goes on the
//! wire.
//!
//! Pure computation — exported over UniFFI (`fauna_ffi::strip_received_headers`)
//! so the Go MTA bridge calls this one implementation rather than
//! re-deriving the continuation-aware walk; the in-nest (since retired)
//! `fauna-bridge-smtp::outbound::send_email` reference loop called it
//! directly. Mirrors the always-available, no-feature-gate shape of
//! [`crate::outbound::mta_sts::mx_patterns_match`].

/// Returns a copy of `raw` with all `Received:` headers (case-insensitive
/// field name match) removed. The body section after the first blank line
/// (CRLF or bare LF — `crate::header_walk`'s framing) is copied unchanged.
/// Logical headers are RFC 5322 §2.2.3 continuation-aware: a header continues on subsequent lines that start
/// with SP or HTAB.
///
/// If the input has no header / body separator (an empty line), the whole
/// input is treated as headers. If the input is malformed in ways the
/// walker can't continue past, the remainder is copied verbatim — fail-
/// safe; better to send a slightly leaky message than to corrupt mail.
pub fn strip_received_headers(raw: &[u8]) -> Vec<u8> {
    crate::header_walk::strip_headers_where(raw, |name| name.eq_ignore_ascii_case(b"Received"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_single_received_header() {
        let raw = b"Received: from internal.host (10.0.0.5)\r\nFrom: alice@example.com\r\nSubject: hi\r\n\r\nbody\r\n";
        let out = strip_received_headers(raw);
        assert_eq!(
            std::str::from_utf8(&out).unwrap(),
            "From: alice@example.com\r\nSubject: hi\r\n\r\nbody\r\n"
        );
    }

    #[test]
    fn strips_multiple_received_headers() {
        let raw =
            b"Received: from outer\r\nReceived: from inner\r\nFrom: alice@example.com\r\n\r\nbody";
        let out = strip_received_headers(raw);
        assert_eq!(
            std::str::from_utf8(&out).unwrap(),
            "From: alice@example.com\r\n\r\nbody"
        );
    }

    #[test]
    fn strips_received_with_continuation_lines() {
        // RFC 5322 §2.2.3 continuation: subsequent lines start with SP or HTAB.
        let raw = b"Received: from outer\r\n\tby relay.example\r\n\twith ESMTP\r\nFrom: alice@example.com\r\n\r\nbody";
        let out = strip_received_headers(raw);
        assert_eq!(
            std::str::from_utf8(&out).unwrap(),
            "From: alice@example.com\r\n\r\nbody",
            "continuation lines must travel with their parent header"
        );
    }

    #[test]
    fn case_insensitive_field_name_match() {
        let raw = b"received: lowercase\r\nRECEIVED: uppercase\r\nReCeIvEd: mixed\r\nFrom: alice@example.com\r\n\r\nbody";
        let out = strip_received_headers(raw);
        assert_eq!(
            std::str::from_utf8(&out).unwrap(),
            "From: alice@example.com\r\n\r\nbody"
        );
    }

    #[test]
    fn preserves_non_received_headers_verbatim() {
        let raw = b"From: alice@example.com\r\nTo: bob@example.com\r\nSubject: =?utf-8?B?aGVsbG8=?=\r\n\r\nbody";
        let out = strip_received_headers(raw);
        // No Received: in input → output identical to input.
        assert_eq!(out, raw);
    }

    #[test]
    fn preserves_body_verbatim_when_stripping() {
        let raw = b"Received: garbage\r\nFrom: a@x\r\n\r\nline 1\r\nline 2\r\n.\r\n";
        let out = strip_received_headers(raw);
        // Body is everything after the first \r\n\r\n.
        assert!(out.ends_with(b"\r\n\r\nline 1\r\nline 2\r\n.\r\n"));
    }

    #[test]
    fn handles_message_with_no_body_separator() {
        // Pathological input: no \r\n\r\n. Treat all of it as headers.
        let raw = b"Received: x\r\nFrom: a@x\r\n";
        let out = strip_received_headers(raw);
        assert_eq!(std::str::from_utf8(&out).unwrap(), "From: a@x\r\n");
    }

    #[test]
    fn does_not_strip_received_substring_in_other_field() {
        // A field whose name CONTAINS "received" (e.g. "X-Received-By") must
        // NOT be stripped. We match the full field name, not a substring.
        let raw = b"X-Received-By: us\r\nReceived-SPF: pass\r\nFrom: a@x\r\n\r\nbody";
        let out = strip_received_headers(raw);
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains("X-Received-By"), "X-Received-By must survive");
        assert!(s.contains("Received-SPF"), "Received-SPF must survive");
    }

    #[test]
    fn empty_input_returns_empty() {
        let out = strip_received_headers(b"");
        assert_eq!(out, Vec::<u8>::new());
    }

    #[test]
    fn only_received_header_with_body() {
        let raw = b"Received: x\r\n\r\nbody";
        let out = strip_received_headers(raw);
        // Headers stripped down to nothing; body preserved.
        assert_eq!(std::str::from_utf8(&out).unwrap(), "\r\nbody");
    }
}
