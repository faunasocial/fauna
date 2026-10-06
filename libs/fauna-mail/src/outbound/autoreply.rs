//! RFC 5322 vacation auto-reply composer (Sieve `vacation`, RFC 5230).
//!
//! Returns the message bytes for a canned auto-reply, composed at the MTA
//! perimeter from the user-authored `subject`/`body` filter config plus the
//! triggering message's envelope + Message-ID. The caller (`server.go`) DKIM-signs
//! the result with the existing outbound signer and hands it to
//! `fauna.bridges.send_auto_reply`, which enqueues it with a null envelope-from
//! (`MAIL FROM:<>` — an auto-reply must never itself be bounceable into a loop,
//! RFC 3834). Pure and allocation-only; no sockets, no signing.
//!
//! Header-injection safety: `subject`/`body` are user config and `to_addr` is the
//! attacker-influenced envelope sender, so every header value is stripped of
//! CR/LF before it goes on a header line — a value can never smuggle an extra
//! header or terminate the header section early.

/// The inputs for one composed auto-reply. All addresses/dates/ids are formatted
/// by the caller (mirrors `dsn::DsnReport`, where the caller formats dates).
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoReplyMessage {
    /// `From:` — the recipient's address (the vacationing user). The reply is
    /// DKIM-signed under this address's domain by the caller.
    pub from_addr: String,
    /// `To:` — the envelope sender we are replying to.
    pub to_addr: String,
    /// `Subject:` — user-authored (sanitized to a single header line here).
    pub subject: String,
    /// The text/plain body — user-authored.
    pub body: String,
    /// The triggering message's `Message-ID` (with angle brackets) for
    /// `In-Reply-To`/`References`; empty if it had none (those headers are then
    /// omitted).
    pub in_reply_to: String,
    /// A freshly-generated `Message-ID` for this reply (with angle brackets).
    pub message_id: String,
    /// RFC 5322 date-time for the `Date:` header.
    pub date: String,
}

/// Compose the RFC 5322 wire bytes for a vacation auto-reply. `Auto-Submitted:
/// auto-replied` marks it as automated so a conforming peer won't auto-reply back
/// (RFC 3834), closing the reply-to-a-reply loop.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn compose_auto_reply(msg: &AutoReplyMessage) -> Vec<u8> {
    let mut out = Vec::with_capacity(512 + msg.body.len());
    push_header(&mut out, "From", &msg.from_addr);
    push_header(&mut out, "To", &msg.to_addr);
    push_header(&mut out, "Subject", &msg.subject);
    push_header(&mut out, "Date", &msg.date);
    push_header(&mut out, "Message-ID", &msg.message_id);
    let in_reply_to = sanitize_header_value(&msg.in_reply_to);
    if !in_reply_to.is_empty() {
        push_raw_line(&mut out, &format!("In-Reply-To: {in_reply_to}"));
        push_raw_line(&mut out, &format!("References: {in_reply_to}"));
    }
    push_raw_line(&mut out, "Auto-Submitted: auto-replied");
    push_raw_line(&mut out, "MIME-Version: 1.0");
    push_raw_line(&mut out, "Content-Type: text/plain; charset=utf-8");
    push_raw_line(&mut out, "Content-Transfer-Encoding: 8bit");
    push_raw_line(&mut out, "");
    out.extend_from_slice(&normalize_crlf(&msg.body));
    if !msg.body.ends_with('\n') {
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// Append `Name: <sanitized value>` + CRLF. The value is stripped of CR/LF and
/// other control bytes (header-injection guard) and capped.
fn push_header(buf: &mut Vec<u8>, name: &str, value: &str) {
    push_raw_line(buf, &format!("{name}: {}", sanitize_header_value(value)));
}

/// Append a line verbatim + CRLF (the line must already be single-line + safe).
fn push_raw_line(buf: &mut Vec<u8>, line: &str) {
    buf.extend_from_slice(line.as_bytes());
    buf.extend_from_slice(b"\r\n");
}

/// Collapse a header value to a single safe line: every ASCII control byte
/// (CR/LF/TAB included) becomes a space, runs of space collapse, the ends are
/// trimmed, and the result is rune-capped. Prevents CRLF header injection from a
/// user-authored subject or an attacker-controlled envelope sender.
fn sanitize_header_value(value: &str) -> String {
    let mut s = String::with_capacity(value.len());
    let mut prev_space = false;
    for c in value.chars() {
        let c = if (c as u32) < 0x20 || c == '\u{7f}' {
            ' '
        } else {
            c
        };
        if c == ' ' {
            if prev_space {
                continue;
            }
            prev_space = true;
        } else {
            prev_space = false;
        }
        s.push(c);
    }
    let trimmed = s.trim();
    const MAX_RUNES: usize = 998; // RFC 5322 line-length ceiling (sans CRLF).
    if trimmed.chars().count() > MAX_RUNES {
        trimmed
            .chars()
            .take(MAX_RUNES)
            .collect::<String>()
            .trim_end()
            .to_string()
    } else {
        trimmed.to_string()
    }
}

/// Normalize body line endings to CRLF: a bare LF (not already preceded by CR)
/// becomes CRLF; existing CRLF is left intact. SMTP dot-stuffing is the relay's
/// job, not the composer's.
fn normalize_crlf(body: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + body.len() / 16 + 1);
    let bytes = body.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'\n' && (i == 0 || bytes[i - 1] != b'\r') {
            out.push(b'\r');
        }
        out.push(b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg() -> AutoReplyMessage {
        AutoReplyMessage {
            from_addr: "bob@fauna.test".into(),
            to_addr: "alice@elsewhere.test".into(),
            subject: "Out of office".into(),
            body: "I am away until June.\nBob".into(),
            in_reply_to: "<orig-123@elsewhere.test>".into(),
            message_id: "<reply-456@fauna.test>".into(),
            date: "Mon, 25 May 2026 12:00:00 +0000".into(),
        }
    }

    fn as_str(v: &[u8]) -> String {
        String::from_utf8(v.to_vec()).unwrap()
    }

    #[test]
    fn composes_expected_headers_and_body() {
        let out = as_str(&compose_auto_reply(&msg()));
        for line in [
            "From: bob@fauna.test\r\n",
            "To: alice@elsewhere.test\r\n",
            "Subject: Out of office\r\n",
            "Date: Mon, 25 May 2026 12:00:00 +0000\r\n",
            "Message-ID: <reply-456@fauna.test>\r\n",
            "In-Reply-To: <orig-123@elsewhere.test>\r\n",
            "References: <orig-123@elsewhere.test>\r\n",
            "Auto-Submitted: auto-replied\r\n",
            "Content-Type: text/plain; charset=utf-8\r\n",
        ] {
            assert!(
                out.contains(line),
                "missing header line {line:?} in:\n{out}"
            );
        }
        // Header/body separator, body present, bare LF normalized to CRLF.
        assert!(
            out.contains("\r\n\r\nI am away until June.\r\nBob\r\n"),
            "body:\n{out}"
        );
    }

    #[test]
    fn omits_reply_headers_when_no_message_id() {
        let mut m = msg();
        m.in_reply_to = String::new();
        let out = as_str(&compose_auto_reply(&m));
        assert!(!out.contains("In-Reply-To:"));
        assert!(!out.contains("References:"));
    }

    #[test]
    fn strips_crlf_injection_from_subject_and_sender() {
        let mut m = msg();
        m.subject = "Away\r\nBcc: victim@evil.test".into();
        m.to_addr = "alice@x.test\r\nX-Injected: 1".into();
        let out = as_str(&compose_auto_reply(&m));
        // The injected text survives only folded into its own header VALUE on a
        // single line — never as a new header line (no CRLF before it).
        assert!(
            out.contains("Subject: Away Bcc: victim@evil.test\r\n"),
            "{out}"
        );
        assert!(
            !out.contains("\r\nBcc:"),
            "subject smuggled a Bcc header:\n{out}"
        );
        assert!(out.contains("To: alice@x.test X-Injected: 1\r\n"), "{out}");
        assert!(
            !out.contains("\r\nX-Injected:"),
            "sender smuggled a header:\n{out}"
        );
    }

    #[test]
    fn no_lone_lf_or_cr_in_output() {
        let out = compose_auto_reply(&msg());
        for (i, &b) in out.iter().enumerate() {
            if b == b'\n' {
                assert!(i > 0 && out[i - 1] == b'\r', "lone LF at {i}");
            }
        }
    }
}
