//! IMAP ENVELOPE derivation, shared-Rust side.
//!
//! Drives `fauna_mail::envelope::derive_envelope` against simple,
//! reply-thread, and multi-recipient fixtures. Phase C.4 of the I5
//! mail-bridge MDA arm.

use fauna_mail::envelope::{derive_envelope, from_mailboxes, sender_domain};

#[test]
fn derives_envelope_simple() {
    let raw = b"From: Alice <alice@example.com>\r\n\
To: Bob <bob@example.com>\r\n\
Subject: Hi\r\n\
Date: Wed, 12 Mar 2026 10:30:00 -0700\r\n\
\r\n\
body\r\n";

    let env = derive_envelope(raw).expect("parse should succeed");
    assert_eq!(env.subject.as_deref(), Some("Hi"));
    assert_eq!(
        env.date.as_deref(),
        Some("2026-03-12T10:30:00-07:00"),
        "date should be ISO-8601 with offset, got {:?}",
        env.date
    );
    assert_eq!(env.from.len(), 1);
    assert_eq!(env.from[0].mailbox, "alice");
    assert_eq!(env.from[0].host, "example.com");
    assert_eq!(env.from[0].personal.as_deref(), Some("Alice"));
    assert_eq!(env.to.len(), 1);
    assert_eq!(env.to[0].mailbox, "bob");
    assert_eq!(env.to[0].host, "example.com");
}

#[test]
fn derives_envelope_reply_to_message_id() {
    let raw = b"From: alice@example.com\r\n\
Reply-To: noreply@example.com\r\n\
Subject: Re: Hi\r\n\
Message-ID: <child@example.com>\r\n\
In-Reply-To: <parent@example.com>\r\n\
\r\n\
body\r\n";

    let env = derive_envelope(raw).expect("parse should succeed");
    assert_eq!(env.reply_to.len(), 1);
    assert_eq!(env.reply_to[0].mailbox, "noreply");
    assert_eq!(env.reply_to[0].host, "example.com");
    assert_eq!(
        env.message_id.as_deref(),
        Some("<child@example.com>"),
        "message_id should preserve angle brackets, got {:?}",
        env.message_id
    );
    assert_eq!(
        env.in_reply_to.as_deref(),
        Some("<parent@example.com>"),
        "in_reply_to should preserve angle brackets, got {:?}",
        env.in_reply_to
    );
}

#[test]
fn derives_envelope_multi_to_cc() {
    let raw = b"From: a@example.com\r\n\
To: b@example.com, c@example.com\r\n\
Cc: d@example.com, e@example.com\r\n\
Subject: Many\r\n\
\r\n\
body\r\n";

    let env = derive_envelope(raw).expect("parse should succeed");
    assert_eq!(env.to.len(), 2, "expected 2 To recipients");
    assert_eq!(env.to[0].mailbox, "b");
    assert_eq!(env.to[1].mailbox, "c");
    assert_eq!(env.cc.len(), 2, "expected 2 Cc recipients");
    assert_eq!(env.cc[0].mailbox, "d");
    assert_eq!(env.cc[1].mailbox, "e");
}

// `from_mailboxes` — the list the submission door's From: ownership check
// reads (mail-multidomain.md § From: header ownership). Its filter is
// `sender_domain`'s, so the first entry's host is the DKIM `d=` anchor.

fn from_mailboxes_of(from_field: &str) -> Vec<(String, String)> {
    let raw = format!("From: {from_field}\r\nTo: b@example.com\r\nSubject: s\r\n\r\nbody\r\n");
    from_mailboxes(raw.as_bytes())
        .into_iter()
        .map(|a| (a.mailbox, a.host))
        .collect()
}

#[test]
fn from_mailboxes_yields_the_one_addr_spec_with_its_case_kept() {
    assert_eq!(
        from_mailboxes_of("\"Alice Example\" <Alice@Example.org>"),
        vec![("Alice".to_string(), "Example.org".to_string())]
    );
    assert_eq!(
        from_mailboxes_of("alice@example.com"),
        vec![("alice".to_string(), "example.com".to_string())]
    );
}

#[test]
fn from_mailboxes_flattens_lists_and_groups_in_header_order() {
    assert_eq!(
        from_mailboxes_of("alice@d.test, ceo@bank.example"),
        vec![
            ("alice".to_string(), "d.test".to_string()),
            ("ceo".to_string(), "bank.example".to_string())
        ]
    );
    assert_eq!(
        from_mailboxes_of("Team: alice@d.test, bob@d.test;"),
        vec![
            ("alice".to_string(), "d.test".to_string()),
            ("bob".to_string(), "d.test".to_string())
        ]
    );
}

#[test]
fn from_mailboxes_drops_entries_that_anchor_nothing() {
    // Domain-less and display-only entries own no address and pick no DKIM
    // key; they are invisible to the door, exactly as they are to
    // `sender_domain`.
    assert!(from_mailboxes_of("\"Just A Name\"").is_empty());
    assert!(from_mailboxes_of("bob").is_empty());
    assert!(from_mailboxes(b"To: b@example.com\r\n\r\nbody\r\n").is_empty());
    assert!(from_mailboxes(b"").is_empty());
}

#[test]
fn from_mailboxes_first_host_is_the_sender_domain_anchor() {
    let raw = b"From: Bob <bob@Second.Example>, ceo@bank.example\r\nTo: a@b.test\r\n\r\nx\r\n";
    let first = from_mailboxes(raw).into_iter().next().expect("one mailbox");
    assert_eq!(first.host.to_ascii_lowercase(), sender_domain(raw));
}
