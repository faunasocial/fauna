//! Holds `from_field_count` against the two readers whose disagreement it
//! exists to prevent (`smtp-server.md` § Architectural rules): `mail-auth`,
//! whose `from()` DMARC aligns against, and `mail-parser`, whose From every app
//! displays and `sender_domain` indexes.
//!
//! The mail doors accept a message only when the count is one, so the property
//! pinned here is: **over every adversarial shape, a count of one means both
//! readers choose the same address**, and the count is never lower than the
//! number of From fields `mail-parser` itself sees.
//!
//! The corpus opens with the twenty From shapes of the security review that
//! found the split, numbered as there, then
//! adds mixed line endings, continuation lines and near-miss field names.
#![cfg(all(feature = "parser", feature = "auth"))]

use fauna_mail::from_field::from_field_count;
use mail_parser::{Address, HeaderName, MessageParser};

const CORPUS: &[(&str, &[u8])] = &[
    ("1 two From fields", b"From: attacker@evil.test\r\nFrom: ceo@bank.test\r\n\r\nbody"),
    ("2 two From fields, swapped", b"From: ceo@bank.test\r\nFrom: attacker@evil.test\r\n\r\nbody"),
    ("3 upper then lower case", b"FROM: attacker@evil.test\r\nfrom: ceo@bank.test\r\n\r\nbody"),
    ("4 two mailboxes in one field", b"From: ceo@bank.test, attacker@evil.test\r\n\r\nbody"),
    ("5 whole From an encoded-word (Q)", b"From: =?utf-8?q?ceo=40bank.test?=\r\n\r\nbody"),
    ("6 whole From an encoded-word (B)", b"From: =?utf-8?b?Y2VvQGJhbmsudGVzdA==?=\r\n\r\nbody"),
    ("7 encoded-word inside <>", b"From: <=?utf-8?q?ceo=40bank.test?=>\r\n\r\nbody"),
    ("8 encoded-word local part", b"From: =?utf-8?q?ceo?=@bank.test\r\n\r\nbody"),
    ("9 encoded display name", b"From: =?utf-8?q?ceo=40bank.test?= <attacker@evil.test>\r\n\r\nbody"),
    ("10 comment", b"From: attacker@evil.test (ceo@bank.test)\r\n\r\nbody"),
    ("11 quoted display name", b"From: \"ceo@bank.test\" <attacker@evil.test>\r\n\r\nbody"),
    ("12 unquoted display name", b"From: ceo@bank.test <attacker@evil.test>\r\n\r\nbody"),
    ("13 group", b"From: Bank: ceo@bank.test;\r\n\r\nbody"),
    ("14 obs-route", b"From: <@evil.test:ceo@bank.test>\r\n\r\nbody"),
    ("15 two angle-addrs", b"From: <ceo@bank.test> <attacker@evil.test>\r\n\r\nbody"),
    ("16 empty From then From", b"From:\r\nFrom: ceo@bank.test\r\n\r\nbody"),
    ("17 LF-only", b"From: attacker@evil.test\nFrom: ceo@bank.test\n\nbody"),
    ("18 folded onto a comment", b"From: attacker@evil.test\r\n (ceo@bank.test)\r\n\r\nbody"),
    ("19 bare CR between two Froms", b"From: attacker@evil.test\rFrom: ceo@bank.test\r\n\r\nbody"),
    ("20 space before the colon", b"From : ceo@bank.test\r\nFrom: attacker@evil.test\r\n\r\nbody"),
    // Mixed line endings around the header/body boundary.
    ("CRLF then LF blank line", b"From: attacker@evil.test\r\n\nFrom: ceo@bank.test\r\n\r\nbody"),
    ("LF then CRLF blank line", b"From: attacker@evil.test\n\r\nFrom: ceo@bank.test\r\n\r\nbody"),
    ("CR CR LF line end", b"From: attacker@evil.test\r\r\nFrom: ceo@bank.test\r\n\r\nbody"),
    ("CR CR mid-section", b"Subject: s\r\rFrom: attacker@evil.test\r\nFrom: ceo@bank.test\r\n\r\nbody"),
    ("no body separator", b"From: attacker@evil.test\r\nFrom: ceo@bank.test"),
    ("LF-only, no body separator", b"From: attacker@evil.test\nFrom: ceo@bank.test"),
    // Continuations and leading whitespace.
    ("leading space on the first line", b" From: attacker@evil.test\r\nFrom: ceo@bank.test\r\n\r\nbody"),
    ("leading tab on the first line", b"\tFrom: attacker@evil.test\r\nFrom: ceo@bank.test\r\n\r\nbody"),
    ("From on a space continuation", b"Subject: s\r\n From: attacker@evil.test\r\nFrom: ceo@bank.test\r\n\r\nbody"),
    ("From on a tab continuation", b"Subject: s\r\n\tFrom: attacker@evil.test\r\nFrom: ceo@bank.test\r\n\r\nbody"),
    ("From continuation after From", b"From: ceo@bank.test\r\n From: attacker@evil.test\r\n\r\nbody"),
    ("LF-only continuation", b"Subject: s\n From: attacker@evil.test\nFrom: ceo@bank.test\n\nbody"),
    // Field-name spellings.
    ("no space after the colon", b"From:attacker@evil.test\r\nFrom:ceo@bank.test\r\n\r\nbody"),
    ("tab before the colon", b"From\t: attacker@evil.test\r\nFrom: ceo@bank.test\r\n\r\nbody"),
    ("mixed case", b"fRoM: attacker@evil.test\r\nFrom: ceo@bank.test\r\n\r\nbody"),
    ("a garbage line between", b"From: attacker@evil.test\r\nnot a field\r\nFrom: ceo@bank.test\r\n\r\nbody"),
    ("two apart", b"From: attacker@evil.test\r\nSubject: s\r\nTo: x@y.test\r\nFrom: ceo@bank.test\r\n\r\nbody"),
    // Shapes that carry exactly one From field.
    ("one From", b"From: ceo@bank.test\r\nTo: x@y.test\r\n\r\nbody"),
    ("one From, LF-only", b"From: ceo@bank.test\nTo: x@y.test\n\nbody"),
    ("near-miss names", b"X-From: attacker@evil.test\r\nResent-From: attacker@evil.test\r\nFrom: ceo@bank.test\r\n\r\nbody"),
    ("From in the body", b"From: ceo@bank.test\r\n\r\nFrom: attacker@evil.test\r\n"),
    ("From in an LF body", b"From: ceo@bank.test\n\nFrom: attacker@evil.test\n"),
    ("Sender and Reply-To", b"Sender: attacker@evil.test\r\nReply-To: attacker@evil.test\r\nFrom: ceo@bank.test\r\n\r\nbody"),
    // No From field at all.
    ("no From", b"Subject: s\r\n\r\nbody"),
    ("blank first line", b"\r\nFrom: attacker@evil.test\r\n\r\nbody"),
];

/// The From address DMARC aligns against — `verify_inbound` hands this parse
/// to `verify_dmarc`. `None` when mail-auth refuses the message (the DATA stage
/// then answers 554 on its own).
fn auth_from(raw: &[u8]) -> Option<String> {
    mail_auth::AuthenticatedMessage::parse(raw).map(|m| m.from().to_ascii_lowercase())
}

/// The From address the apps display and `sender_domain` indexes: mail-parser's
/// first non-empty mailbox of the From field it chooses.
fn display_from(raw: &[u8]) -> String {
    let Some(msg) = MessageParser::default().parse_headers(raw) else {
        return String::new();
    };
    let first = match msg.from() {
        Some(Address::List(list)) => list
            .iter()
            .find_map(|a| a.address.as_deref().filter(|s| !s.is_empty())),
        Some(Address::Group(groups)) => groups
            .iter()
            .flat_map(|g| g.addresses.iter())
            .find_map(|a| a.address.as_deref().filter(|s| !s.is_empty())),
        None => None,
    };
    first.unwrap_or_default().to_ascii_lowercase()
}

/// How many From fields mail-parser itself sees.
fn parser_from_fields(raw: &[u8]) -> usize {
    MessageParser::default()
        .parse_headers(raw)
        .map(|m| {
            m.headers()
                .iter()
                .filter(|h| matches!(h.name, HeaderName::From))
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn wherever_the_count_is_one_both_readers_choose_the_same_address() {
    let mut splits = Vec::new();
    for (label, raw) in CORPUS {
        if from_field_count(raw) != 1 {
            continue;
        }
        let Some(auth) = auth_from(raw) else { continue };
        let display = display_from(raw);
        if auth != display {
            splits.push(format!(
                "{label}: mail-auth {auth:?}, mail-parser {display:?}"
            ));
        }
    }
    assert!(
        splits.is_empty(),
        "a count of one let the readers split:\n{}",
        splits.join("\n")
    );
}

#[test]
fn the_count_is_never_below_what_mail_parser_sees() {
    let mut under = Vec::new();
    for (label, raw) in CORPUS {
        let (count, parser) = (from_field_count(raw), parser_from_fields(raw));
        if (count as usize) < parser {
            under.push(format!("{label}: count {count}, mail-parser {parser}"));
        }
    }
    assert!(under.is_empty(), "under-counted:\n{}", under.join("\n"));
}

/// The five shapes on which the review measured the split. A red here on the
/// disagreement half means a dependency changed which From field it reads —
/// re-examine the count's rules before relaxing it.
#[test]
fn the_review_split_shapes_are_refused_and_still_split() {
    for (label, raw) in CORPUS.iter().filter(|(label, _)| {
        ["1 ", "2 ", "3 ", "17 ", "20 "]
            .iter()
            .any(|n| label.starts_with(n))
    }) {
        assert!(
            from_field_count(raw) >= 2,
            "{label}: counted {}",
            from_field_count(raw)
        );
        assert_ne!(
            auth_from(raw).as_deref(),
            Some(display_from(raw).as_str()),
            "{label}: the readers now agree"
        );
    }
}
