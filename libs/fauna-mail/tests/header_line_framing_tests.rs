//! The shared header walk frames lines the way the parser and the From door do:
//! a line ends at LF, with or without a preceding CR, and the header section
//! ends at the first empty line. A walker that framed on CRLF alone read a
//! bare-LF header section as ONE field named by its first header, so every
//! consumer below silently skipped (or swallowed) the headers after it.
//!
//! Each case runs a bare-LF and a mixed-ending header section through a
//! consumer and checks it against `parse_rfc5322` — the reader whose view
//! decides what a filter rule, a client, or an archive actually sees.

use fauna_mail::aliases::read_spam_threshold_stamp;
use fauna_mail::from_field::from_field_count;
use fauna_mail::lists::stamp_list_headers_on_message;
use fauna_mail::outbound::received_strip::strip_received_headers;
use fauna_mail::parse_rfc5322;
use fauna_mail::received_header::strip_fauna_headers;

/// The inbound forgery shape: a remote sender's own `X-Fauna-*` delivery
/// stamps, plus the one reserved header the strip must keep.
const FORGED_LF: &[u8] = b"From: attacker@evil.test\n\
To: victim@fauna.test\n\
X-Fauna-Address-Matched: forged-alias\n\
X-Fauna-Spam-Threshold: 99\n\
X-Fauna-Forwarded-By: actor=abc; t=1; rule=forward-all\n\
Subject: hi\n\
\n\
X-Fauna-Address-Matched: body text, not a header\n";

/// The same message with only its first line CRLF-terminated.
const FORGED_MIXED: &[u8] = b"From: attacker@evil.test\r\n\
To: victim@fauna.test\n\
X-Fauna-Address-Matched: forged-alias\n\
X-Fauna-Spam-Threshold: 99\r\n\
X-Fauna-Forwarded-By: actor=abc; t=1; rule=forward-all\n\
Subject: hi\n\
\n\
X-Fauna-Address-Matched: body text, not a header\n";

/// Every header name the parser reports for `raw`, lowercased.
fn parsed_header_names(raw: &[u8]) -> Vec<String> {
    parse_rfc5322(raw)
        .expect("parses")
        .headers
        .iter()
        .map(|h| h.name.to_ascii_lowercase())
        .collect()
}

#[test]
fn the_inbound_forgery_strip_removes_bare_lf_stamps_before_the_parser_sees_them() {
    for (label, raw) in [("bare LF", FORGED_LF), ("mixed", FORGED_MIXED)] {
        // The From door passes both shapes, so the strip is the only guard.
        assert_eq!(from_field_count(raw), 1, "{label}");
        // The unstripped parse proves the fixture really carries the forgery.
        let before = parsed_header_names(raw);
        assert!(
            before.iter().any(|n| n == "x-fauna-address-matched"),
            "{label}: fixture must reach the parser as a header, or this proves nothing"
        );

        let stripped = strip_fauna_headers(raw);
        let after = parsed_header_names(&stripped);
        assert!(
            !after
                .iter()
                .any(|n| n.starts_with("x-fauna-") && n != "x-fauna-forwarded-by"),
            "{label}: a forged stamp survived the strip: {after:?}"
        );
        assert!(
            after.iter().any(|n| n == "x-fauna-forwarded-by"),
            "{label}: the forward-loop trace must survive: {after:?}"
        );
        for kept in ["from", "to", "subject"] {
            assert!(after.iter().any(|n| n == kept), "{label}: lost {kept}");
        }
        let text = String::from_utf8_lossy(&stripped);
        assert!(
            text.ends_with("\nX-Fauna-Address-Matched: body text, not a header\n"),
            "{label}: the strip reached into the body: {text:?}"
        );
    }
}

#[test]
fn the_forgery_strip_keeps_a_bare_cr_inside_a_value_as_content() {
    // The parser keeps a bare CR inside a field value (it does not end a
    // line), so the walker must not split there either: the Subject is one
    // field and is kept whole.
    let raw = b"From: a@x.test\r\nSubject: s\rX-Fauna-Address-Matched: in-value\r\n\r\nbody\r\n";
    assert_eq!(strip_fauna_headers(raw), raw);
}

#[test]
fn a_whitespace_padded_name_is_matched_as_the_parser_reads_it() {
    // A leading bare CR on a line reaches the parser as the header it names.
    let raw = b"From: a@x.test\r\n\rX-Fauna-Address-Matched: forged\r\n\r\nbody\r\n";
    assert!(
        parsed_header_names(raw)
            .iter()
            .any(|n| n == "x-fauna-address-matched")
    );
    let after = parsed_header_names(&strip_fauna_headers(raw));
    assert!(
        !after.iter().any(|n| n == "x-fauna-address-matched"),
        "{after:?}"
    );
}

#[test]
fn the_header_section_ends_at_the_first_empty_line_whatever_its_ending() {
    // `\r\n\n`: the parser ends the section at the bare-LF empty line, so what
    // follows is body and the strip must leave it alone.
    let raw = b"From: a@x.test\r\n\nX-Fauna-Address-Matched: body\r\n\r\nmore\r\n";
    assert_eq!(parsed_header_names(raw), ["from"]);
    assert_eq!(strip_fauna_headers(raw), raw);
}

#[test]
fn the_outbound_received_strip_frames_bare_lf_lines() {
    let raw = b"Received: from a\nReceived: from b\n\tby c\nFrom: s@x.test\nSubject: s\n\nReceived: body\n";
    let out = strip_received_headers(raw);
    assert_eq!(
        String::from_utf8_lossy(&out),
        "From: s@x.test\nSubject: s\n\nReceived: body\n"
    );
}

#[test]
fn the_list_stamp_replaces_bare_lf_client_list_headers() {
    let raw = b"From: bob@x.test\nList-Id: spoof\nList-Unsubscribe: <https://evil.test/u>\nSubject: s\n\nbody\n";
    let out = stamp_list_headers_on_message(raw, &[("List-Id", "<real.x.test>".to_string())]);
    let text = String::from_utf8_lossy(&out);
    assert!(!text.contains("spoof"), "{text:?}");
    assert!(!text.contains("evil.test"), "{text:?}");
    assert!(text.contains("From: bob@x.test\n"), "{text:?}");
    assert!(text.contains("Subject: s\n"), "{text:?}");
    assert!(text.ends_with("\n\nbody\n"), "{text:?}");
}

#[test]
fn the_spam_threshold_reader_finds_a_stamp_after_a_bare_lf_line() {
    // The genuine stamp is prepended CRLF-terminated at delivery onto the
    // sender's bytes, whose own lines may end in bare LF.
    let raw = b"From: a@x.test\nX-Fauna-Spam-Threshold: 6\nSubject: s\n\nbody\n";
    assert_eq!(read_spam_threshold_stamp(raw), Some(6));
}
