//! Canonical per-actor mail dedup keys — the content-equality pair that lets
//! an import skip mail the actor already holds.
//!
//! `docs/goal/behavior/mailbox-migration.md` § Dedup owns the definition:
//!
//! ```text
//! primary  = "msgid:v1:" ‖ normalize(Message-ID)
//! envelope = "env:v1:"   ‖ hex(sha256(canonical_envelope))
//!
//! dedup_key    = primary when the message carries a Message-ID, else envelope
//! envelope_key = envelope, always
//!
//! normalize(m)       = lowercase(strip_angle_brackets(trim(m)))
//! canonical_envelope = From ‖ 0x00 ‖ To ‖ 0x00 ‖ Cc ‖ 0x00 ‖ Date ‖ 0x00
//!                      ‖ Subject ‖ 0x00 ‖ sha256(crlf_normalize(body))
//! ```
//!
//! The `0x00` separator cannot occur in a valid header value, so no two
//! distinct field tuples can collide by sliding text across a boundary.
//!
//! **The input is the whole raw RFC 5322 message, and nothing else.** Four
//! independent producers compute this key — the user's client for
//! `import_message`, the Go MDA for IMAP `APPEND`, the Go MTA at the
//! perimeter pre-seal, and the nest at its own in-domain delivery — and a key
//! is only useful if all four agree byte for byte. Taking pre-parsed envelope fields would push the join convention for a
//! multi-address `To:`, and the rendering of `Date:`, onto each caller; two
//! callers disagreeing there yields two different keys for one message and
//! dedup silently stops working. So the parse lives here, once.
//!
//! The nest calls the key computation only where it seals plaintext it holds
//! (its in-domain delivery and the sender's Sent copy); at the MTA, MDA and
//! import doors it holds only the sealed body. Either way it
//! stores the pair opaquely in `actor_message_dedup`, looks a hit up
//! by `dedup_key`, and confirms it with [`envelope_keys_agree`] — which is why
//! a nest can dedup mail it cannot read, and why a stranger who reuses a
//! Message-ID cannot make the real message skip at import (§ The envelope key
//! confirms a Message-ID hit).
//!
//! One definition, shared Rust: the Go bridges call this over the UniFFI
//! binding (like [`crate::report_hash::report_hash`]); there is no Go
//! reimplementation.

use mail_parser::{HeaderName, MessageParser};
use sha2::{Digest, Sha256};

/// Version fence on the primary form. A change to `normalize` is a new version,
/// never a silent redefinition — old keys must keep matching old keys.
const MSGID_PREFIX_V1: &str = "msgid:v1:";
/// Version fence on the fallback form. Distinct from the primary prefix so a
/// Message-ID that happens to look like a hex digest can never alias one.
const ENVELOPE_PREFIX_V1: &str = "env:v1:";

/// Normalize a raw `Message-ID:` header value: trim, strip one surrounding pair
/// of angle brackets, case-fold. Returns `None` when nothing survives — an
/// absent, empty, or bracket-only header is *no* Message-ID, which is what
/// selects the envelope fallback.
///
/// Case-folding is per the ratified spec. RFC 5322 technically makes the
/// `local` part case-sensitive, but real-world duplicates differ in case far
/// more often than distinct messages collide by it.
pub fn normalize_message_id(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let stripped = trimmed
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(trimmed)
        .trim();
    if stripped.is_empty() {
        return None;
    }
    Some(stripped.to_lowercase())
}

/// Rewrite every bare `\n` and every bare `\r` to `\r\n`, leaving existing
/// `\r\n` pairs alone. RFC 5322 line endings are CRLF; a body that made a trip
/// through a Unix-line-ending store must still hash to the same key.
fn crlf_normalize(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        match body[i] {
            b'\r' => {
                out.extend_from_slice(b"\r\n");
                // Consume a following \n so an existing CRLF stays one CRLF.
                i += if body.get(i + 1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
            }
            b'\n' => {
                out.extend_from_slice(b"\r\n");
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    out
}

/// The message body: everything after the first empty line (RFC 5322 §2.1).
/// Accepts both `\r\n\r\n` and a Unix-normalized `\n\n` separator, taking
/// whichever appears first — the two byte patterns are disjoint, so there is no
/// ambiguity. A message with no empty line is all headers and an empty body.
fn body_after_headers(raw: &[u8]) -> &[u8] {
    let crlf = raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4);
    let lf = raw.windows(2).position(|w| w == b"\n\n").map(|i| i + 2);
    match (crlf, lf) {
        (Some(a), Some(b)) => &raw[a.min(b)..],
        (Some(a), None) => &raw[a..],
        (None, Some(b)) => &raw[b..],
        (None, None) => &[],
    }
}

/// A header's raw value, trimmed; `None` (→ empty string in the hash) when the
/// header is absent or blank. Mirrors `envelope::header_raw_trimmed`.
fn header(msg: &mail_parser::Message<'_>, name: HeaderName<'static>) -> String {
    msg.header_raw(name)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or_default()
        .to_string()
}

/// The two keys one message is recorded under (`mailbox-migration.md`
/// § Key format): the lookup key and the content-bound envelope key that
/// confirms a hit (§ The envelope key confirms a Message-ID hit).
///
/// Named `…Pair`, not `MailDedupKeys`: uniffi-bindgen-go maps a record and
/// a function whose names differ only in case ([`mail_dedup_keys`]) to one Go
/// identifier, and the binding stops compiling.
///
/// A pair, never one string: a producer that could send the lookup key
/// without the envelope key would re-open the hole the envelope key closes —
/// a stranger-chosen Message-ID standing in for the real message at import.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct MailDedupKeyPair {
    /// `msgid:v1:…` when the message carries a Message-ID, else the envelope
    /// form — what `actor_message_dedup` is looked up by.
    pub dedup_key: String,
    /// `env:v1:…`, always — the canonical-envelope hash over the five header
    /// values and the body.
    pub envelope_key: String,
}

/// The canonical dedup keys for one raw RFC 5322 message.
///
/// Both are printable ASCII strings for the `actor_message_dedup` `TEXT`
/// columns. An unparseable message still yields stable keys (the envelope
/// form over empty headers plus the body hash) rather than failing — the
/// caller is mid-delivery and must not drop mail over a dedup detail.
///
/// Takes `Vec<u8>` because UniFFI cannot export a borrowed slice, and the Go
/// MDA/MTA reach this over that binding. In-process Rust callers — the import
/// client, which already holds the body it is about to send — should call
/// [`mail_dedup_keys_from_slice`] and skip a copy of up to 50 MiB per message.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn mail_dedup_keys(raw_message: Vec<u8>) -> MailDedupKeyPair {
    mail_dedup_keys_from_slice(&raw_message)
}

/// [`mail_dedup_keys`] without the owning copy. **One implementation** — the
/// exported form delegates here, so the Go bridges and the Rust import client
/// cannot drift (§ Key format: "the key is worthless unless all four agree
/// byte for byte").
pub fn mail_dedup_keys_from_slice(raw_message: &[u8]) -> MailDedupKeyPair {
    let parsed = MessageParser::new().parse_headers(raw_message);

    let (from, to, cc, date, subject) = match parsed.as_ref() {
        Some(msg) => (
            header(msg, HeaderName::From),
            header(msg, HeaderName::To),
            header(msg, HeaderName::Cc),
            header(msg, HeaderName::Date),
            header(msg, HeaderName::Subject),
        ),
        None => Default::default(),
    };

    let body_hash = Sha256::digest(crlf_normalize(body_after_headers(raw_message)));

    let mut hasher = Sha256::new();
    for field in [&from, &to, &cc, &date, &subject] {
        hasher.update(field.as_bytes());
        hasher.update([0u8]);
    }
    hasher.update(body_hash);
    let envelope_key = format!("{ENVELOPE_PREFIX_V1}{}", hex_lower(&hasher.finalize()));

    let message_id = parsed.as_ref().and_then(|msg| {
        msg.header_raw(HeaderName::MessageId)
            .and_then(normalize_message_id)
    });
    let dedup_key = match message_id {
        Some(normalized) => format!("{MSGID_PREFIX_V1}{normalized}"),
        None => envelope_key.clone(),
    };

    MailDedupKeyPair {
        dedup_key,
        envelope_key,
    }
}

/// Does a `dedup_key` hit count as a duplicate? (`mailbox-migration.md`
/// § The envelope key confirms a Message-ID hit.) Only when the envelope keys
/// agree — two messages sharing a sender-chosen Message-ID with different
/// envelopes are two messages. Every producer sends the key and every row
/// stores it, so there is no absent side to agree with.
///
/// The nest calls this at `import_message`, so its check cannot drift from
/// the definition beside it.
pub fn envelope_keys_agree(stored: &str, candidate: &str) -> bool {
    stored == candidate
}

/// Refuse an empty half of the pair (`mailbox-migration.md` § The envelope key
/// confirms a Message-ID hit → *There is no absent key*). Both keys are
/// required wire fields, and [`mail_dedup_keys`] never returns an empty one, so
/// an empty string can only be a producer that skipped the shared function —
/// an absent key by another name.
///
/// The nest calls this at every door that records the pair, so the three
/// cannot disagree on what a present key is.
pub fn require_dedup_pair(dedup_key: &str, envelope_key: &str) -> Result<(), &'static str> {
    if dedup_key.is_empty() {
        return Err("dedup_key must not be empty");
    }
    if envelope_key.is_empty() {
        return Err("envelope_key must not be empty");
    }
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(raw: &[u8]) -> String {
        mail_dedup_keys(raw.to_vec()).dedup_key
    }

    fn envelope(raw: &[u8]) -> String {
        mail_dedup_keys(raw.to_vec()).envelope_key
    }

    const WITH_ID: &[u8] = b"From: a@x.test\r\nTo: b@y.test\r\nSubject: Hi\r\n\
Message-ID: <ABC@Example.COM>\r\nDate: Mon, 1 Jan 2024 00:00:00 +0000\r\n\r\nbody text\r\n";

    const NO_ID: &[u8] = b"From: a@x.test\r\nTo: b@y.test\r\nSubject: Hi\r\n\
Date: Mon, 1 Jan 2024 00:00:00 +0000\r\n\r\nbody text\r\n";

    // ─── normalize_message_id ───

    #[test]
    fn message_id_is_bracket_stripped_trimmed_and_case_folded() {
        assert_eq!(
            normalize_message_id("  <ABC@Example.COM>  ").as_deref(),
            Some("abc@example.com")
        );
        assert_eq!(
            normalize_message_id("abc@example.com").as_deref(),
            Some("abc@example.com")
        );
    }

    #[test]
    fn empty_or_bracket_only_message_id_is_absent() {
        assert_eq!(normalize_message_id(""), None);
        assert_eq!(normalize_message_id("   "), None);
        assert_eq!(normalize_message_id("<>"), None);
        assert_eq!(normalize_message_id("< >"), None);
    }

    #[test]
    fn only_one_bracket_pair_is_stripped() {
        assert_eq!(normalize_message_id("<<a@b>>").as_deref(), Some("<a@b>"));
        assert_eq!(normalize_message_id("<a@b").as_deref(), Some("<a@b"));
    }

    /// Cross-check with the provenance verify (`crate::msgid`): a raw
    /// `<…>`-wrapped mint passes only after [`normalize_message_id`], which is
    /// the order the guardian mail gate's seed applies them in.
    #[cfg(feature = "msgid")]
    #[test]
    fn a_normalized_minted_id_passes_the_provenance_verify() {
        let local = crate::msgid::mint_local(&[0x00u8; crate::msgid::MSGID_RANDOM_LEN]);
        let raw = format!("<{local}@Fauna.Test>");
        let normalized = normalize_message_id(&raw).expect("normalizes");
        assert!(crate::msgid::is_fauna_minted_msgid(&normalized));
        assert!(
            !crate::msgid::is_fauna_minted_msgid(&raw),
            "the raw bracketed form is not the seed's input"
        );
    }

    // ─── primary form ───

    #[test]
    fn present_message_id_wins_and_is_normalized() {
        assert_eq!(key(WITH_ID), "msgid:v1:abc@example.com");
    }

    #[test]
    fn message_id_ignores_every_other_field() {
        let other = b"From: totally@different.test\r\nSubject: Nope\r\n\
Message-ID: <abc@example.com>\r\n\r\nan entirely different body\r\n";
        assert_eq!(key(WITH_ID), key(other));
    }

    #[test]
    fn blank_message_id_falls_back_to_envelope() {
        let blank = b"From: a@x.test\r\nMessage-ID: <>\r\n\r\nbody\r\n";
        assert!(key(blank).starts_with("env:v1:"));
    }

    // ─── fallback form ───

    #[test]
    fn envelope_key_is_prefixed_64_lowercase_hex() {
        let k = key(NO_ID);
        let hex = k.strip_prefix("env:v1:").expect("prefix");
        assert_eq!(hex.len(), 64);
        assert!(
            hex.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
        );
    }

    #[test]
    fn every_envelope_header_and_the_body_affect_the_key() {
        let base = key(NO_ID);
        let variants: &[&[u8]] = &[
            b"From: CHANGED@x.test\r\nTo: b@y.test\r\nSubject: Hi\r\nDate: Mon, 1 Jan 2024 00:00:00 +0000\r\n\r\nbody text\r\n",
            b"From: a@x.test\r\nTo: CHANGED@y.test\r\nSubject: Hi\r\nDate: Mon, 1 Jan 2024 00:00:00 +0000\r\n\r\nbody text\r\n",
            b"From: a@x.test\r\nTo: b@y.test\r\nCc: c@z.test\r\nSubject: Hi\r\nDate: Mon, 1 Jan 2024 00:00:00 +0000\r\n\r\nbody text\r\n",
            b"From: a@x.test\r\nTo: b@y.test\r\nSubject: Hi\r\nDate: Tue, 2 Jan 2024 00:00:00 +0000\r\n\r\nbody text\r\n",
            b"From: a@x.test\r\nTo: b@y.test\r\nSubject: CHANGED\r\nDate: Mon, 1 Jan 2024 00:00:00 +0000\r\n\r\nbody text\r\n",
            b"From: a@x.test\r\nTo: b@y.test\r\nSubject: Hi\r\nDate: Mon, 1 Jan 2024 00:00:00 +0000\r\n\r\nDIFFERENT body\r\n",
        ];
        for (i, v) in variants.iter().enumerate() {
            assert_ne!(key(v), base, "variant {i} must change the key");
        }
    }

    #[test]
    fn envelope_fields_cannot_slide_across_the_separator() {
        // Without the 0x00 fence, ("ab", "") and ("a", "b") would concatenate
        // alike. Same total header text, different field boundaries.
        let a = b"From: ab\r\nTo: \r\n\r\n";
        let b = b"From: a\r\nTo: b\r\n\r\n";
        assert_ne!(key(a), key(b));
    }

    #[test]
    fn body_line_endings_are_crlf_normalized() {
        let crlf = key(b"From: a@x.test\r\n\r\none\r\ntwo\r\n");
        assert_eq!(key(b"From: a@x.test\r\n\r\none\ntwo\n"), crlf);
        assert_eq!(key(b"From: a@x.test\r\n\r\none\rtwo\r"), crlf);
        assert_eq!(key(b"From: a@x.test\r\n\r\none\r\ntwo\n"), crlf);
    }

    #[test]
    fn crlf_normalize_does_not_double_existing_pairs() {
        assert_eq!(crlf_normalize(b"a\r\nb"), b"a\r\nb".to_vec());
        assert_eq!(crlf_normalize(b"a\nb"), b"a\r\nb".to_vec());
        assert_eq!(crlf_normalize(b"a\rb"), b"a\r\nb".to_vec());
        assert_eq!(crlf_normalize(b"a\r\n\r\nb"), b"a\r\n\r\nb".to_vec());
        assert_eq!(crlf_normalize(b"a\r"), b"a\r\n".to_vec());
    }

    // ─── header/body split ───

    #[test]
    fn body_split_handles_crlf_lf_and_absent_separators() {
        assert_eq!(body_after_headers(b"H: v\r\n\r\nbody"), b"body");
        assert_eq!(body_after_headers(b"H: v\n\nbody"), b"body");
        // No empty line → all headers, empty body.
        assert_eq!(body_after_headers(b"H: v\r\n"), b"");
        // The earliest separator wins; a later `\n\n` inside the body is not it.
        assert_eq!(body_after_headers(b"H: v\r\n\r\na\n\nb"), b"a\n\nb");
    }

    #[test]
    fn an_empty_body_is_stable_and_distinct_from_a_newline_body() {
        let empty = key(b"From: a@x.test\r\n\r\n");
        assert_eq!(key(b"From: a@x.test\r\n\r\n"), empty);
        assert_ne!(key(b"From: a@x.test\r\n\r\n\r\n"), empty);
    }

    // ─── robustness ───

    #[test]
    fn a_garbage_message_still_yields_a_stable_envelope_key() {
        // Mid-delivery we must never fail over a dedup detail.
        let k1 = key(b"\xff\xfe not a message at all");
        let k2 = key(b"\xff\xfe not a message at all");
        assert_eq!(k1, k2);
        assert!(k1.starts_with("env:v1:"));
    }

    #[test]
    fn the_two_forms_never_collide() {
        assert!(key(WITH_ID).starts_with("msgid:v1:"));
        assert!(key(NO_ID).starts_with("env:v1:"));
        assert_ne!(key(WITH_ID), key(NO_ID));
    }

    /// The whole point of the single-argument shape: identical bytes in, one
    /// pair out, regardless of who computes it.
    ///
    /// Both public entry points must agree — the Go bridges reach the owning
    /// `Vec<u8>` form over UniFFI while the Rust import client calls the
    /// borrowing one, and a divergence between them would silently stop dedup
    /// working across producers (§ Key format).
    #[test]
    fn the_keys_are_a_pure_function_of_the_raw_bytes() {
        for raw in [NO_ID, WITH_ID] {
            assert_eq!(
                mail_dedup_keys(raw.to_vec()),
                mail_dedup_keys_from_slice(raw)
            );
        }
    }

    // ─── the envelope key (§ The envelope key confirms a Message-ID hit) ───

    #[test]
    fn a_message_id_message_carries_both_forms() {
        let keys = mail_dedup_keys(WITH_ID.to_vec());
        assert_eq!(keys.dedup_key, "msgid:v1:abc@example.com");
        assert!(keys.envelope_key.starts_with("env:v1:"));
    }

    #[test]
    fn without_a_message_id_the_two_keys_are_equal() {
        let keys = mail_dedup_keys(NO_ID.to_vec());
        assert_eq!(keys.dedup_key, keys.envelope_key);
    }

    /// The Message-ID header is not one of the five envelope fields, so the
    /// same message with and without it shares its envelope key.
    #[test]
    fn the_envelope_key_ignores_the_message_id() {
        assert_eq!(envelope(WITH_ID), envelope(NO_ID));
    }

    /// The attack the envelope key closes: same sender-chosen Message-ID,
    /// different content → same lookup key, different envelope key.
    #[test]
    fn a_reused_message_id_with_other_content_disagrees_on_the_envelope() {
        let forged = b"From: stranger@evil.test\r\nTo: b@y.test\r\nSubject: Hi\r\n\
Message-ID: <abc@example.com>\r\nDate: Mon, 1 Jan 2024 00:00:00 +0000\r\n\r\nforged\r\n";
        assert_eq!(key(forged), key(WITH_ID));
        assert_ne!(envelope(forged), envelope(WITH_ID));
    }

    #[test]
    fn envelope_keys_agree_is_equality() {
        assert!(envelope_keys_agree("env:v1:a", "env:v1:a"));
        assert!(!envelope_keys_agree("env:v1:a", "env:v1:b"));
    }

    #[test]
    fn an_empty_half_of_the_pair_is_refused_and_a_minted_pair_never_is() {
        assert!(require_dedup_pair("", "env:v1:a").is_err());
        assert!(require_dedup_pair("msgid:v1:a", "").is_err());
        // Even an unparseable or empty message mints two non-empty keys.
        for raw in [WITH_ID, b"".as_slice(), &[0xff, 0xfe]] {
            let keys = mail_dedup_keys_from_slice(raw);
            assert_eq!(
                require_dedup_pair(&keys.dedup_key, &keys.envelope_key),
                Ok(())
            );
        }
    }

    /// Golden vector, derived independently from `mailbox-migration.md` § Dedup
    /// rather than from this implementation:
    ///
    /// ```text
    /// sha256("a@x.test\0b@y.test\0\0Mon, 1 Jan 2024 00:00:00 +0000\0Hi\0"
    ///        ‖ sha256("body text\r\n"))
    /// ```
    ///
    /// It pins the whole fallback contract at once — that `Cc:` absent hashes
    /// as an empty field (its `\0` still fires), that header values arrive
    /// trimmed of the leading space and trailing CRLF, that the body starts
    /// after the empty line, and that the body digest is appended raw rather
    /// than hex. A future refactor that quietly changes any of these breaks
    /// every stored key; this test is the fence.
    #[test]
    fn envelope_form_matches_the_specs_golden_vector() {
        const GOLDEN: &str =
            "env:v1:ae6d5a8b9de5d0e09b958ea8b6ab5d023262dbf015d996cc247c004e4c5ea576";
        assert_eq!(key(NO_ID), GOLDEN);
        assert_eq!(envelope(NO_ID), GOLDEN);
        // The Message-ID is not an envelope field: the same vector binds the
        // envelope key of the message that carries one.
        assert_eq!(envelope(WITH_ID), GOLDEN);
    }
}
