//! RFC 3464 multipart/report DSN builder.
//!
//! Returns the RFC 5322 message bytes for a non-delivery (or delayed)
//! report per docs/goal/behavior/smtp-server.md § Permanent-failure
//! bounce generation. The bridge wraps the result in a null-envelope
//! `MAIL FROM:<>` per RFC 5321 §4.5.5 at SMTP time; the builder is pure
//! and does not allocate sockets.
//!
//! Body-only — the original message body is intentionally NOT carried
//! (RFC 3464 allows headers-only when the body would be large), only
//! the headers, in the `message/rfc822-headers` third MIME part. Avoid
//! letting the DSN be a denial-of-service amplifier.

use rand::Rng;

#[derive(Debug, Clone, Copy)]
pub enum DsnAction {
    /// Permanent failure — retry budget exhausted or 5xx classified
    /// permanent. `Status:` is the 5.x.x enhanced code.
    Failed,
    /// 4 h delay warning — temporary failure that has not yet resolved.
    /// `Status:` is "4.4.7" per the spec.
    Delayed,
}

impl DsnAction {
    fn wire_token(&self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Delayed => "delayed",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DsnReport<'a> {
    /// Our domain (the Reporting-MTA per RFC 3464 §2.2.2).
    pub reporting_mta: &'a str,
    /// Original-message arrival time, RFC 2822 date-time.
    pub arrival_date: &'a str,
    /// Wall-clock of the last delivery attempt, RFC 2822 date-time.
    pub last_attempt_date: &'a str,
    /// The recipient address that failed (used for both the Final-
    /// Recipient: rfc822; ... line and the human-readable summary).
    pub recipient: &'a str,
    /// The original sender (bounce target — the To: header on this
    /// 5322 message). Envelope sender is `<>`; that's the bridge's
    /// concern, not the builder's.
    pub original_sender: &'a str,
    /// Enhanced status (e.g. "5.1.1" or "4.4.7") for the Status: line.
    pub status: &'a str,
    pub action: DsnAction,
    /// Last wire response from the recipient MX, copied into the
    /// Diagnostic-Code: smtp; ... line.
    pub diagnostic_code: &'a str,
    /// Headers of the failed message, copied verbatim into the third
    /// MIME part (message/rfc822-headers). May be empty.
    pub original_headers: &'a [u8],
    /// Human-readable narrative for the first MIME part (text/plain).
    pub failure_summary_text: &'a str,
}

/// Build the RFC 3464 multipart/report wire bytes (RFC 5322 message).
pub fn build_dsn(report: &DsnReport<'_>) -> Vec<u8> {
    let boundary = random_boundary();

    let mut out = Vec::with_capacity(2048 + report.original_headers.len());

    // ── RFC 5322 envelope headers ─────────────────────────────────
    push_line(
        &mut out,
        &format!("From: postmaster@{}", report.reporting_mta),
    );
    push_line(&mut out, &format!("To: {}", report.original_sender));
    push_line(
        &mut out,
        &format!("Subject: Undeliverable: mail to {}", report.recipient),
    );
    push_line(&mut out, "MIME-Version: 1.0");
    push_line(
        &mut out,
        &format!(
            "Content-Type: multipart/report; report-type=delivery-status; boundary=\"{}\"",
            boundary
        ),
    );
    push_line(&mut out, "");

    // ── Part 1: text/plain narrative ───────────────────────────────
    push_line(&mut out, &format!("--{boundary}"));
    push_line(&mut out, "Content-Type: text/plain; charset=utf-8");
    push_line(&mut out, "");
    push_line(&mut out, report.failure_summary_text);
    push_line(
        &mut out,
        &format!(
            "Recipient: {}\r\nLast remote response: {}",
            report.recipient, report.diagnostic_code
        ),
    );
    push_line(&mut out, "");

    // ── Part 2: message/delivery-status ────────────────────────────
    push_line(&mut out, &format!("--{boundary}"));
    push_line(&mut out, "Content-Type: message/delivery-status");
    push_line(&mut out, "");
    // Per-message DSN fields.
    push_line(
        &mut out,
        &format!("Reporting-MTA: dns; {}", report.reporting_mta),
    );
    push_line(&mut out, &format!("Arrival-Date: {}", report.arrival_date));
    push_line(&mut out, "");
    // Per-recipient DSN fields.
    push_line(
        &mut out,
        &format!("Final-Recipient: rfc822; {}", report.recipient),
    );
    push_line(&mut out, &format!("Action: {}", report.action.wire_token()));
    push_line(&mut out, &format!("Status: {}", report.status));
    push_line(
        &mut out,
        &format!("Diagnostic-Code: smtp; {}", report.diagnostic_code),
    );
    push_line(
        &mut out,
        &format!("Last-Attempt-Date: {}", report.last_attempt_date),
    );
    push_line(&mut out, "");

    // ── Part 3: message/rfc822-headers (verbatim original headers) ─
    push_line(&mut out, &format!("--{boundary}"));
    push_line(&mut out, "Content-Type: message/rfc822-headers");
    push_line(&mut out, "");
    out.extend_from_slice(report.original_headers);
    // Ensure the headers section ends with CRLF.
    if !report.original_headers.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    push_line(&mut out, "");

    // ── Closing boundary ───────────────────────────────────────────
    push_line(&mut out, &format!("--{boundary}--"));

    out
}

fn push_line(buf: &mut Vec<u8>, line: &str) {
    // Allow callers to pass a multi-line block already containing CRLF.
    // We just append the bytes and a trailing CRLF.
    buf.extend_from_slice(line.as_bytes());
    buf.extend_from_slice(b"\r\n");
}

fn random_boundary() -> String {
    let mut rng = rand::thread_rng();
    let bytes: [u8; 16] = rng.r#gen();
    let mut s = String::from("=_FaunaDSN-");
    for b in &bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}
