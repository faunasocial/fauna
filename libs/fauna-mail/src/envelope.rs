//! IMAP ENVELOPE derivation from raw RFC 5322 / MIME bytes.
//!
//! Returns the [`Envelope`] shape RFC 9051 §7.5.2 prescribes: address
//! lists for `from`/`sender`/`reply_to`/`to`/`cc`/`bcc`, plus
//! `in_reply_to`, `message_id`, `date`, and `subject`. Sender defaults
//! to `from`, and `reply_to` defaults to `from` per RFC 5322 § 3.6.2.
//!
//! Date is rendered as ISO-8601 (RFC 3339) so the Go bridge can hand it
//! directly to emersion's `imap.Envelope.Date` (which uses Go `time.Time`,
//! parseable from RFC 3339).
//!
//! `message_id` and `in_reply_to` preserve their angle-bracket form, since
//! the IMAP wire format includes the brackets and clients (and emersion's
//! `imap.Envelope.MessageID` field) expect them as-is.
//!
//! Phase C.4 of the I5 mail-bridge MDA arm (tracked internally).

use crate::parser::ParseError;
use mail_parser::{Address, HeaderName, MessageParser};
use serde::{Deserialize, Serialize};

/// One IMAP ENVELOPE address: the `(personal mailbox host)` triple from
/// RFC 9051 §7.5.2 (we drop SMTP at-domain-list, which has been
/// deprecated since RFC 821 and is always NIL in practice).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct EnvelopeAddress {
    /// Display name (RFC 2047 decoded by mail-parser). `None` for bare
    /// `addr@host` syntax.
    pub personal: Option<String>,
    /// Local part (left of `@`).
    pub mailbox: String,
    /// Domain (right of `@`). Empty when the source address has no `@`.
    pub host: String,
}

/// Derived IMAP ENVELOPE.
///
/// Field order mirrors RFC 9051 §7.5.2's parenthesised list. Address
/// lists are empty (not absent) when the corresponding header is missing
/// or unparseable. `sender` and `reply_to` carry the `from` fallback the
/// RFC mandates so the Go bridge doesn't repeat the rule.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct Envelope {
    /// ISO-8601 / RFC 3339 timestamp of the `Date:` header. `None` when
    /// the header is absent or unparseable.
    pub date: Option<String>,
    /// `Subject:` header, RFC 2047 decoded.
    pub subject: Option<String>,
    pub from: Vec<EnvelopeAddress>,
    /// Defaults to `from` when the header is absent (RFC 5322 § 3.6.2).
    pub sender: Vec<EnvelopeAddress>,
    /// Defaults to `from` when the header is absent (RFC 5322 § 3.6.2).
    pub reply_to: Vec<EnvelopeAddress>,
    pub to: Vec<EnvelopeAddress>,
    pub cc: Vec<EnvelopeAddress>,
    pub bcc: Vec<EnvelopeAddress>,
    /// `In-Reply-To:` raw header value with angle brackets preserved.
    pub in_reply_to: Option<String>,
    /// `Message-ID:` raw header value with angle brackets preserved.
    pub message_id: Option<String>,
}

/// Derive an IMAP ENVELOPE from raw RFC 5322 bytes.
///
/// Re-parses internally via `mail-parser`; callers supply the wire bytes
/// (post-decryption in the encrypted-mode path).
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn derive_envelope(raw: &[u8]) -> Result<Envelope, ParseError> {
    let msg = MessageParser::default()
        .parse(raw)
        .ok_or(ParseError::Malformed)?;

    let from = collect_addresses(msg.from());
    let sender_raw = collect_addresses(msg.sender());
    let reply_to_raw = collect_addresses(msg.reply_to());

    let sender = if sender_raw.is_empty() {
        from.clone()
    } else {
        sender_raw
    };
    let reply_to = if reply_to_raw.is_empty() {
        from.clone()
    } else {
        reply_to_raw
    };

    Ok(Envelope {
        date: msg.date().map(|d| d.to_rfc3339()),
        subject: msg.subject().map(str::to_string),
        from,
        sender,
        reply_to,
        to: collect_addresses(msg.to()),
        cc: collect_addresses(msg.cc()),
        bcc: collect_addresses(msg.bcc()),
        in_reply_to: header_raw_trimmed(&msg, HeaderName::InReplyTo),
        message_id: header_raw_trimmed(&msg, HeaderName::MessageId),
    })
}

/// The lowercased domain of the message's `From:` header — the value that
/// populates `bridge_imap_messages.from_norm` (IMAP `SEARCH FROM`, the
/// SPF-audit path) as `ImportMessageItem.sender_domain` /
/// `AppendMessageRequest.sender_domain`.
///
/// Empty when the header is absent, unparseable, or lacks a local part or a
/// domain — nest's import + APPEND handlers both accept `""`.
///
/// Parses **headers only** (`parse_headers`), so a 50 MiB import message costs
/// a header scan rather than a full MIME walk. Sibling of
/// [`crate::dedup_key::mail_dedup_keys`], which the import client calls on the
/// same bytes.
///
/// The Go MDA/MTA callers reach this rule through
/// [`sender_domain_with_envelope_fallback`] below (the 2026-07-12 lift that
/// retired the Go `ExtractSenderDomain` copy — see its doc for the two
/// deliberate semantic deltas that cutover made).
pub fn sender_domain(raw: &[u8]) -> String {
    let Some(msg) = MessageParser::default().parse_headers(raw) else {
        return String::new();
    };
    collect_addresses(msg.from())
        .into_iter()
        .find(|a| !a.mailbox.is_empty() && !a.host.is_empty())
        .map(|a| a.host.to_ascii_lowercase())
        .unwrap_or_default()
}

/// [`sender_domain`] plus the SMTP-envelope fallback the Go MTA/MDA callers
/// need: when the `From:` header yields no domain (absent, unparseable, or no
/// address carrying both a local part and a domain), fall back to the domain
/// of `mail_from` — the SMTP `MAIL FROM:<…>` reverse-path addr-spec. Callers
/// with no envelope (IMAP APPEND, the DKIM `d=` anchor) pass `mail_from = ""`.
///
/// This is the ONE implementation of the `from_norm` rule (IMAP `SEARCH
/// FROM`, the SPF-audit path); it replaced the Go
/// `mailfauna.ExtractSenderDomain` copy (lifted 2026-07-12, priority #2).
/// Two deliberate semantic deltas from that copy, pinned by the golden corpus
/// (`the_golden_corpus_pins_the_lifted_from_norm_rule` here, mirrored by the
/// Go `TestSenderDomainGoldenCorpus` over the FFI):
///
/// 1. A **multi-address or group** `From:` yields the FIRST address's domain.
///    The Go copy's strict `net/mail.ParseAddress` errored on those and fell
///    through to the envelope (or `""`), so `SEARCH FROM` / the SPF audit got
///    the bounce address's domain — or nothing — for real-world mail.
/// 2. Header parsing is `mail-parser`'s **lenient** real-world grammar — the
///    same parser that produced the `parse_rfc5322` result the callers hold —
///    rather than strict RFC 5322, so `from_norm` can no longer disagree with
///    the rest of the parse about the same bytes.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn sender_domain_with_envelope_fallback(raw: Vec<u8>, mail_from: String) -> String {
    sender_domain_with_envelope_fallback_from_slice(&raw, &mail_from)
}

/// Slice-based twin of [`sender_domain_with_envelope_fallback`] for native
/// callers (the uniffi surface needs owned params).
pub fn sender_domain_with_envelope_fallback_from_slice(raw: &[u8], mail_from: &str) -> String {
    let from_header = sender_domain(raw);
    if !from_header.is_empty() {
        return from_header;
    }
    envelope_addr_domain(mail_from)
}

/// Every mailbox the message's `From:` field names that carries both a local
/// part and a domain — the addresses a door that checks *who the message
/// claims to be from* has to look at, in header order, headers only
/// (`parse_headers`, like [`sender_domain`]).
///
/// The filter is [`sender_domain`]'s, so the first entry's `host` (lowercased)
/// IS the DKIM `d=` anchor that function returns: a door that refuses on this
/// list and a signer that keys on that anchor can never disagree about which
/// address the message is from. Display-only or domain-less entries are
/// dropped for the same reason — they anchor nothing and own nothing. Group
/// syntax is flattened, so a `From:` group with two members counts two.
///
/// The submission door (465/587) refuses a count other than one and then
/// checks that the one address, when the deployment signs for its domain, is
/// owned by the authenticated actor (`mail-multidomain.md` § From: header
/// ownership). `mailbox` keeps the header's case; the door compares local
/// parts case-insensitively.
//
// Provenance kept out of the `///` run on purpose: the UniFFI checksum covers
// docstrings, and the publish transform excises a  span, so a span inside
// the docstring would leave the public tree's tracked Go binding unable to
// load.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn from_mailboxes(raw: &[u8]) -> Vec<EnvelopeAddress> {
    let Some(msg) = MessageParser::default().parse_headers(raw) else {
        return Vec::new();
    };
    collect_addresses(msg.from())
        .into_iter()
        .filter(|a| !a.mailbox.is_empty() && !a.host.is_empty())
        .collect()
}

/// Domain of a bare SMTP envelope addr-spec (`a@b` or `<a@b>`; `<>` and
/// garbage yield `""`), lowercased. MAIL FROM already passed the SMTP
/// layer's syntax gate, so this is a split, not a validator — mirroring the
/// Go copy's `at > 0 && at < len-1` guard.
fn envelope_addr_domain(addr: &str) -> String {
    let trimmed = addr.trim();
    let trimmed = trimmed
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(trimmed);
    match trimmed.rfind('@') {
        Some(at) if at > 0 && at + 1 < trimmed.len() => trimmed[at + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

fn collect_addresses(addr: Option<&Address<'_>>) -> Vec<EnvelopeAddress> {
    let Some(a) = addr else { return Vec::new() };
    let mut out = Vec::new();
    let mut push_addr = |entry: &mail_parser::Addr<'_>| {
        let Some(full) = entry.address.as_deref() else {
            return;
        };
        let (mailbox, host) = match full.rfind('@') {
            Some(idx) => (full[..idx].to_string(), full[idx + 1..].to_string()),
            None => (full.to_string(), String::new()),
        };
        out.push(EnvelopeAddress {
            personal: entry.name.as_deref().map(str::to_string),
            mailbox,
            host,
        });
    };
    match a {
        Address::List(list) => {
            for entry in list {
                push_addr(entry);
            }
        }
        Address::Group(groups) => {
            for group in groups {
                for entry in &group.addresses {
                    push_addr(entry);
                }
            }
        }
    }
    out
}

fn header_raw_trimmed(msg: &mail_parser::Message<'_>, name: HeaderName<'static>) -> Option<String> {
    let raw = msg.header_raw(name)?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(test)]
mod sender_domain_tests {
    use super::{sender_domain, sender_domain_with_envelope_fallback_from_slice};

    #[test]
    fn the_domain_is_case_folded() {
        assert_eq!(
            sender_domain(b"From: Alice <a@Example.COM>\r\n\r\nbody"),
            "example.com"
        );
    }

    #[test]
    fn a_bare_address_without_a_display_name_works() {
        assert_eq!(sender_domain(b"From: a@x.test\r\n\r\nbody"), "x.test");
    }

    #[test]
    fn a_missing_or_malformed_from_yields_the_empty_string() {
        // Nest's import + APPEND handlers both accept `sender_domain = ""`.
        assert_eq!(sender_domain(b"Subject: no from\r\n\r\nbody"), "");
        assert_eq!(sender_domain(b"From: not-an-address\r\n\r\nbody"), "");
        assert_eq!(sender_domain(b""), "");
    }

    #[test]
    fn an_empty_local_part_or_empty_domain_is_rejected() {
        // Mirrors the Go copy's `at > 0 && at < len-1` guard.
        assert_eq!(sender_domain(b"From: <@x.test>\r\n\r\nbody"), "");
        assert_eq!(sender_domain(b"From: <a@>\r\n\r\nbody"), "");
    }

    #[test]
    fn a_multi_address_from_takes_the_first() {
        // The 2026-07-12 lift made this THE shared rule (the retired Go copy's
        // strict `net/mail.ParseAddress` errored on multi-address `From:` and
        // fell through to the envelope fallback) — see
        // `sender_domain_with_envelope_fallback`'s doc, delta 1.
        assert_eq!(
            sender_domain(b"From: a@first.test, b@second.test\r\n\r\nbody"),
            "first.test"
        );
    }

    #[test]
    fn the_group_syntax_is_walked_rather_than_skipped() {
        assert_eq!(
            sender_domain(b"From: Team:a@x.test,b@y.test;\r\n\r\nbody"),
            "x.test"
        );
    }

    /// The golden corpus for the lifted `from_norm` rule — mirrored verbatim
    /// by the Go `TestSenderDomainGoldenCorpus` over the FFI binding, so the
    /// two sides cannot drift (the `dedup_key` golden-vector pattern).
    #[test]
    fn the_golden_corpus_pins_the_lifted_from_norm_rule() {
        let corpus: &[(&[u8], &str, &str)] = &[
            // (raw message, mail_from, expected)
            (
                b"From: Alice <alice@Example.COM>\r\n\r\nbody",
                "bounce@env.test",
                "example.com", // header wins over envelope; lowercased
            ),
            (
                b"From: a@first.test, b@second.test\r\n\r\nbody",
                "bounce@env.test",
                "first.test", // delta 1: multi-address takes the first
            ),
            (
                b"From: Team:a@x.test,b@y.test;\r\n\r\nbody",
                "",
                "x.test", // delta 1: group syntax is flattened
            ),
            (
                b"Subject: no from\r\n\r\nbody",
                "bounce@env.test",
                "env.test", // no header -> envelope fallback
            ),
            (
                b"Subject: no from\r\n\r\nbody",
                "<bounce@ENV.test>",
                "env.test", // angle-bracket addr-spec accepted, lowercased
            ),
            (b"Subject: no from\r\n\r\nbody", "<>", ""), // null reverse-path
            (b"Subject: no from\r\n\r\nbody", "", ""),   // no envelope (APPEND)
            (
                b"From: not-an-address\r\n\r\nbody",
                "bounce@env.test",
                "env.test", // unparseable header -> fallback
            ),
            (
                b"From: <a@>\r\n\r\nbody",
                "bounce@env.test",
                "env.test", // empty header domain -> fallback
            ),
            (
                b"From: <@x.test>\r\n\r\nbody",
                "@y.test",
                "", // empty local parts on both -> ""
            ),
        ];
        for (raw, mail_from, expected) in corpus {
            assert_eq!(
                sender_domain_with_envelope_fallback_from_slice(raw, mail_from),
                *expected,
                "raw={:?} mail_from={mail_from:?}",
                String::from_utf8_lossy(raw),
            );
        }
    }
}
