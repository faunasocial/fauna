//! RFC 3464 multipart/report DSN builder.
//!
//! Asserts the wire-shape required by docs/goal/behavior/smtp-server.md
//! § Permanent-failure bounce generation. The function output is the
//! RFC 5322 message bytes; the bridge wraps that in a null-envelope-
//! sender `MAIL FROM:<>` at SMTP time — that's not the builder's
//! concern.

#![cfg(feature = "outbound")]

use fauna_mail::outbound::dsn::{DsnAction, DsnReport, build_dsn};

fn sample_headers() -> &'static [u8] {
    b"From: alice@example.com\r\n\
      To: bob@example.com\r\n\
      Subject: Hi\r\n\
      Message-ID: <abc@example.com>\r\n\
      Date: Thu, 14 May 2026 08:00:00 +0000\r\n"
}

fn run(report: &DsnReport<'_>) -> String {
    String::from_utf8(build_dsn(report)).expect("DSN bytes are valid UTF-8")
}

#[test]
fn dsn_has_three_mime_parts_and_required_fields() {
    let report = DsnReport {
        reporting_mta: "fauna.example",
        arrival_date: "Thu, 14 May 2026 08:00:00 +0000",
        last_attempt_date: "Tue, 19 May 2026 08:00:00 +0000",
        recipient: "bob@example.com",
        original_sender: "alice@example.com",
        status: "5.1.1",
        action: DsnAction::Failed,
        diagnostic_code: "550 5.1.1 no such user",
        original_headers: sample_headers(),
        failure_summary_text: "Message to <bob@example.com> was not delivered after 5 days of retries.",
    };
    let body = run(&report);

    // Outer Content-Type.
    assert!(
        body.contains("Content-Type: multipart/report; report-type=delivery-status; boundary="),
        "outer Content-Type missing or malformed:\n{body}"
    );

    // Three parts: text/plain, message/delivery-status, message/rfc822-headers.
    assert!(body.contains("Content-Type: text/plain; charset=utf-8"));
    assert!(body.contains("Content-Type: message/delivery-status"));
    assert!(body.contains("Content-Type: message/rfc822-headers"));

    // Per-recipient delivery-status fields per RFC 3464 § 2.3.
    assert!(body.contains("Reporting-MTA: dns; fauna.example"));
    assert!(body.contains("Arrival-Date: Thu, 14 May 2026 08:00:00 +0000"));
    assert!(body.contains("Final-Recipient: rfc822; bob@example.com"));
    assert!(body.contains("Action: failed"));
    assert!(body.contains("Status: 5.1.1"));
    assert!(body.contains("Diagnostic-Code: smtp; 550 5.1.1 no such user"));
    assert!(body.contains("Last-Attempt-Date: Tue, 19 May 2026 08:00:00 +0000"));

    // Human-readable summary
    assert!(body.contains("not delivered after 5 days"));

    // Outer RFC 5322 envelope headers.
    assert!(body.starts_with("From: postmaster@fauna.example\r\n"));
    assert!(body.contains("To: alice@example.com\r\n"));
    assert!(body.contains("Subject: Undeliverable: mail to bob@example.com\r\n"));

    // Per RFC 5321 §4.5.5 the *envelope* sender is null; the From: header
    // in this 5322 message is postmaster@<reporting-mta>. The envelope
    // sender is the bridge's job at MAIL FROM time, not the builder's.
}

#[test]
fn delayed_action_uses_4_4_7_status() {
    let report = DsnReport {
        reporting_mta: "fauna.example",
        arrival_date: "Thu, 14 May 2026 08:00:00 +0000",
        last_attempt_date: "Thu, 14 May 2026 12:00:00 +0000",
        recipient: "bob@example.com",
        original_sender: "alice@example.com",
        status: "4.4.7",
        action: DsnAction::Delayed,
        diagnostic_code: "421 try again later",
        original_headers: sample_headers(),
        failure_summary_text: "Your message hasn't been delivered yet.",
    };
    let body = run(&report);

    assert!(body.contains("Action: delayed"));
    assert!(body.contains("Status: 4.4.7"));
    assert!(body.contains("Diagnostic-Code: smtp; 421 try again later"));
}

#[test]
fn original_headers_appear_verbatim_in_third_part() {
    let report = DsnReport {
        reporting_mta: "fauna.example",
        arrival_date: "Thu, 14 May 2026 08:00:00 +0000",
        last_attempt_date: "Tue, 19 May 2026 08:00:00 +0000",
        recipient: "bob@example.com",
        original_sender: "alice@example.com",
        status: "5.1.1",
        action: DsnAction::Failed,
        diagnostic_code: "550 no such user",
        original_headers: sample_headers(),
        failure_summary_text: "failed",
    };
    let body = run(&report);

    // The exact header bytes must appear after the third MIME part marker.
    let headers_str = std::str::from_utf8(sample_headers()).unwrap();
    let rfc822_part_pos = body
        .find("Content-Type: message/rfc822-headers")
        .expect("missing rfc822-headers part");
    let tail = &body[rfc822_part_pos..];
    assert!(
        tail.contains(headers_str),
        "original_headers should appear verbatim after the rfc822-headers part"
    );
}

#[test]
fn body_uses_crlf_line_endings() {
    let report = DsnReport {
        reporting_mta: "fauna.example",
        arrival_date: "Thu, 14 May 2026 08:00:00 +0000",
        last_attempt_date: "Tue, 19 May 2026 08:00:00 +0000",
        recipient: "bob@example.com",
        original_sender: "alice@example.com",
        status: "5.1.1",
        action: DsnAction::Failed,
        diagnostic_code: "550 no such user",
        original_headers: sample_headers(),
        failure_summary_text: "failed",
    };
    let bytes = build_dsn(&report);

    // Every \n in the output is preceded by \r (RFC 5322 SMTP wire format).
    let mut prev = 0u8;
    for &b in &bytes {
        if b == b'\n' {
            assert_eq!(prev, b'\r', "bare LF in DSN bytes; SMTP requires CRLF");
        }
        prev = b;
    }
}
