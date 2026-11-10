//! The materializer's per-record work: open a sealed mail, calendar or card
//! record under the held key of its kind, shape it into the client builder's
//! doc, and render a query-time snippet (`encryption-at-rest.md` § Capability tiering → *The
//! materialization grant*, parts (c) and (d)).
//!
//! Pure functions over bytes — the orchestration (arm, trickle, retire,
//! revoke, the boot reconcile) is [`super::Materializer`]'s.

use fauna_index::{ContentId, ContentKind, FieldKind, IndexedDoc, IndexedField};
use fauna_mls::wrapped_blob::RecordKind;

/// Records the catch-up walk indexes per batch before it yields (and lets a
/// revoke take the area's lock).
pub const CATCH_UP_BATCH: i64 = 200;

/// Characters of body text a query-time snippet carries.
pub const SNIPPET_CHARS: usize = 160;

/// Open one sealed mail body — the **inner** envelope the mail segment rests
/// (`segments::mail::read_sealed_body_with_floor`) — under a
/// `content.read{mail}` key the materializer holds. `None` when the key does
/// not open it (a lineage no wrap covers yet, part (e)): the caller skips the
/// record and counts it.
pub fn open_mail_body(sealed_body: &[u8], key: &[u8]) -> Option<Vec<u8>> {
    let inner =
        fauna_mls::wrapped_blob::MailRecordEnvelope::from_canonical_bytes(sealed_body).ok()?;
    fauna_mls::wrapped_blob::unseal_mail_record_with_derived_key(&inner, key).ok()
}

/// The doc's content id: the RFC `Message-ID` header value exactly as the
/// owner's client keys its own doc (the raw header, trimmed, angle brackets
/// kept), or the client's own fallback `<hex(nest id)@fauna.inbox>` when the
/// message carries none — so the shared snapshot dedups the nest's hit
/// against a native app's own by `(kind, content_id)`
/// (`fauna-conversations`' SMTP receive path is the spelling's owner).
pub fn mail_content_id(parsed: &fauna_mail::parser::ParsedMessage, nest_id: &[u8; 32]) -> String {
    parsed
        .headers
        .iter()
        .find(|h| h.name.eq_ignore_ascii_case("message-id"))
        .map(|h| h.value.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("<{}@fauna.inbox>", hex::encode(nest_id)))
}

/// Shape one opened record into the client builder's [`IndexedDoc`]: content
/// id the RFC `Message-ID`, `secondary_id` the nest message id, the subject a
/// title field and the text body a body field — no stored source text (the
/// schema stores none). `None` for bytes that do not parse as a message.
pub fn mail_doc(rfc5322: &[u8], nest_id: &[u8; 32], received_at_ms: i64) -> Option<IndexedDoc> {
    let parsed = fauna_mail::parser::parse_rfc5322(rfc5322).ok()?;
    let mut fields = Vec::with_capacity(2);
    if let Some(subject) = parsed.subject.as_deref().filter(|s| !s.is_empty()) {
        fields.push(IndexedField {
            kind: FieldKind::Title,
            text: subject.to_string(),
        });
    }
    if !parsed.body_text.is_empty() {
        fields.push(IndexedField {
            kind: FieldKind::Body,
            text: parsed.body_text.clone(),
        });
    }
    Some(IndexedDoc {
        kind: ContentKind::Mail,
        content_id: ContentId(mail_content_id(&parsed, nest_id).into_bytes()),
        timestamp_ns: received_at_ms.saturating_mul(1_000_000),
        sender_actor_id: None,
        secondary_id: Some(nest_id.to_vec()),
        fields,
    })
}

/// Open one sealed DAV body — a `bridge_caldav_events` / `bridge_carddav_cards`
/// record's `encrypted_body` — under the `content.read{calendar|card}` key the
/// materializer holds for that kind. The kind is checked first, so a body of
/// another kind is refused before any trial; a key of another kind fails the
/// AEAD (the per-kind key split, `owner-key-material.md` § Path B-sibling-5).
/// `None` when the key does not open it: the caller skips and counts it.
pub fn open_dav_body(kind: RecordKind, sealed_body: &[u8], key: &[u8]) -> Option<Vec<u8>> {
    let env = fauna_mls::wrapped_blob::MailRecordEnvelope::from_canonical_bytes_of_kind(
        sealed_body,
        kind,
    )
    .ok()?;
    fauna_mls::wrapped_blob::unseal_record_with_derived_key(kind, &env, key).ok()
}

/// A DAV doc's content id: the hex-lowercase `uid_hash`, the spelling the
/// owner's client keys its own event and contact docs by, so the shared
/// snapshot dedups the nest's hit against a device's own.
pub fn dav_content_id(uid_hash: &[u8; 32]) -> String {
    hex::encode(uid_hash)
}

/// Shape one opened event into the client builder's doc: the `SUMMARY` the
/// title, description and location the body ([`fauna_core::ical::event_search_parts`],
/// the one derivation both sides read), `secondary_id` the record's CID bytes
/// (the snippet's way back to the sealed record). `None` for bytes that do
/// not parse as an event.
pub fn event_doc(
    ical: &[u8],
    uid_hash: &[u8; 32],
    record_cid: &[u8],
    created_at_secs: i64,
) -> Option<IndexedDoc> {
    let fields = fauna_core::ical::parse_ical(std::str::from_utf8(ical).ok()?).ok()?;
    let (summary, text) = fauna_core::ical::event_search_parts(&fields);
    let mut doc = IndexedDoc::titled(
        ContentKind::Calendar,
        dav_content_id(uid_hash).into_bytes(),
        summary.as_deref(),
        &text,
        created_at_secs.saturating_mul(1_000_000_000),
    );
    doc.secondary_id = Some(record_cid.to_vec());
    Some(doc)
}

/// Shape one opened card into the client builder's doc: the formatted name
/// the title, [`fauna_core::vcard::search_text`] the body — the parsed fields,
/// never the vCard scaffolding. `None` for bytes that are not UTF-8.
pub fn card_doc(
    vcard: &[u8],
    uid_hash: &[u8; 32],
    record_cid: &[u8],
    created_at_secs: i64,
) -> Option<IndexedDoc> {
    let card = fauna_core::vcard::parse_vcard(std::str::from_utf8(vcard).ok()?);
    let name = Some(card.formatted_name.as_str()).filter(|n| !n.is_empty());
    let mut doc = IndexedDoc::titled(
        ContentKind::Contact,
        dav_content_id(uid_hash).into_bytes(),
        name,
        &fauna_core::vcard::search_text(&card),
        created_at_secs.saturating_mul(1_000_000_000),
    );
    doc.secondary_id = Some(record_cid.to_vec());
    Some(doc)
}

/// A query-time snippet of an opened event: its body text around the match,
/// the summary when the event carries no other text.
pub fn event_snippet(ical: &[u8], query: &str) -> String {
    let Some(fields) = std::str::from_utf8(ical)
        .ok()
        .and_then(|t| fauna_core::ical::parse_ical(t).ok())
    else {
        return String::new();
    };
    let (summary, text) = fauna_core::ical::event_search_parts(&fields);
    text_snippet(&text, query).unwrap_or_else(|| summary.unwrap_or_default())
}

/// A query-time snippet of an opened card: its searchable text around the
/// match, the formatted name when there is none.
pub fn card_snippet(vcard: &[u8], query: &str) -> String {
    let Ok(text) = std::str::from_utf8(vcard) else {
        return String::new();
    };
    let card = fauna_core::vcard::parse_vcard(text);
    text_snippet(&fauna_core::vcard::search_text(&card), query)
        .unwrap_or_else(|| card.formatted_name.clone())
}

/// A bounded snippet of an opened record for a hit on `query`: the body
/// window around the first query word it contains (or the body's start),
/// at most [`SNIPPET_CHARS`] characters, the subject when the body is empty.
pub fn mail_snippet(rfc5322: &[u8], query: &str) -> String {
    let Ok(parsed) = fauna_mail::parser::parse_rfc5322(rfc5322) else {
        return String::new();
    };
    text_snippet(&parsed.body_text, query).unwrap_or_else(|| parsed.subject.unwrap_or_default())
}

/// The window of `text` around the first query word it contains (or its
/// start), whitespace collapsed, at most [`SNIPPET_CHARS`] characters, an
/// ellipsis on each cut side. `None` when the text is empty.
pub fn text_snippet(text: &str, query: &str) -> Option<String> {
    let body: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if body.is_empty() {
        return None;
    }
    let lower = body.to_lowercase();
    let start_byte = query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|w| !w.is_empty())
        .filter_map(|w| lower.find(&w))
        .min()
        .unwrap_or(0);
    // Lowercasing can shift byte offsets for a few scripts; fall back to the
    // body's start rather than slice off a char boundary.
    let start_byte = if body.is_char_boundary(start_byte) {
        start_byte
    } else {
        0
    };
    let lead = body[..start_byte]
        .chars()
        .rev()
        .take(SNIPPET_CHARS / 4)
        .count();
    let from = body[..start_byte]
        .char_indices()
        .rev()
        .nth(lead.saturating_sub(1))
        .map_or(start_byte, |(i, _)| i);
    let from = if lead == 0 { start_byte } else { from };
    let window: String = body[from..].chars().take(SNIPPET_CHARS).collect();
    let mut out = String::new();
    if from > 0 {
        out.push('…');
    }
    out.push_str(&window);
    if body[from..].chars().count() > SNIPPET_CHARS {
        out.push('…');
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_mls::wrapped_blob::{derive_recipient_hpke_keypair, seal_to_recipient};

    const RAW: &[u8] = b"From: a@example.org\r\nTo: b@example.org\r\nSubject: Hello there\r\nMessage-ID: <m1@example.org>\r\n\r\nThe quarterly harvest report is attached for review.\r\n";

    #[test]
    fn a_doc_carries_the_message_id_the_nest_id_and_no_source_text() {
        let nest = [5u8; 32];
        let doc = mail_doc(RAW, &nest, 1_000).unwrap();
        assert_eq!(doc.content_id.0, b"<m1@example.org>");
        assert_eq!(doc.secondary_id.as_deref(), Some(&nest[..]));
        assert_eq!(doc.timestamp_ns, 1_000_000_000);
        assert_eq!(doc.kind, ContentKind::Mail);
        assert!(doc.fields.iter().any(|f| f.text.contains("harvest")));
    }

    #[test]
    fn a_message_without_message_id_takes_the_clients_fallback() {
        let nest = [0xABu8; 32];
        let raw = b"From: a@example.org\r\nSubject: s\r\n\r\nbody\r\n";
        let doc = mail_doc(raw, &nest, 0).unwrap();
        assert_eq!(
            String::from_utf8(doc.content_id.0).unwrap(),
            format!("<{}@fauna.inbox>", hex::encode(nest))
        );
    }

    #[test]
    fn the_held_key_opens_a_standing_sealed_body_and_another_does_not() {
        let msek = [3u8; 32];
        let (secret, public) = derive_recipient_hpke_keypair(&msek);
        let env = seal_to_recipient(RAW, &public).unwrap();
        let sealed = env.to_canonical_bytes().unwrap();
        assert_eq!(open_mail_body(&sealed, &secret).as_deref(), Some(RAW));
        assert!(open_mail_body(&sealed, &[9u8; 32]).is_none());
        assert!(open_mail_body(b"not an envelope", &secret).is_none());
    }

    #[test]
    fn a_snippet_is_bounded_and_centred_on_the_match() {
        let s = mail_snippet(RAW, "harvest");
        assert!(s.contains("harvest"), "{s}");
        assert!(s.chars().count() <= SNIPPET_CHARS + 2);
        let long = format!(
            "From: a@x\r\nSubject: s\r\n\r\n{} needle {}\r\n",
            "word ".repeat(200),
            "tail ".repeat(200)
        );
        let s = mail_snippet(long.as_bytes(), "needle");
        assert!(
            s.starts_with('…') && s.ends_with('…') && s.contains("needle"),
            "{s}"
        );
        assert!(s.chars().count() <= SNIPPET_CHARS + 2);
    }

    const ICS: &[u8] = b"BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:u1\r\nSUMMARY:Harvest festival\r\nDESCRIPTION:bring the lanterns\r\nLOCATION:Old mill\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    const VCF: &[u8] = b"BEGIN:VCARD\r\nVERSION:4.0\r\nUID:c1\r\nFN:Ada Lanternmaker\r\nNOTE:met at the mill\r\nEND:VCARD\r\n";

    #[test]
    fn a_dav_body_opens_under_its_own_kinds_key_only() {
        use fauna_mls::wrapped_blob::{
            DavKind, dav_body::seal_dav_body_xwing, derive_recipient_calendar_capability_secret,
            derive_recipient_card_capability_secret,
        };
        let msek = [4u8; 32];
        let cal_key = derive_recipient_calendar_capability_secret(&msek);
        let card_key = derive_recipient_card_capability_secret(&msek);
        let sealed = seal_dav_body_xwing(DavKind::Calendar, ICS, &msek).unwrap();
        assert_eq!(
            open_dav_body(RecordKind::Calendar, &sealed, &cal_key).as_deref(),
            Some(ICS)
        );
        assert!(open_dav_body(RecordKind::Calendar, &sealed, &card_key).is_none());
        assert!(
            open_dav_body(RecordKind::Card, &sealed, &card_key).is_none(),
            "a calendar record is refused by the card opener"
        );
    }

    #[test]
    fn dav_docs_take_the_client_builders_id_and_shape() {
        let uid = [0xABu8; 32];
        let ev = event_doc(ICS, &uid, b"cid", 7).unwrap();
        assert_eq!(ev.kind, ContentKind::Calendar);
        assert_eq!(ev.content_id.0, hex::encode(uid).into_bytes());
        assert_eq!(ev.secondary_id.as_deref(), Some(&b"cid"[..]));
        assert_eq!(ev.timestamp_ns, 7_000_000_000);
        assert!(
            ev.fields
                .iter()
                .any(|f| f.kind == FieldKind::Title && f.text == "Harvest festival")
        );
        assert!(ev.fields.iter().any(|f| f.text.contains("lanterns")));
        let card = card_doc(VCF, &uid, b"cid", 0).unwrap();
        assert_eq!(card.kind, ContentKind::Contact);
        assert!(
            card.fields
                .iter()
                .any(|f| f.kind == FieldKind::Title && f.text == "Ada Lanternmaker")
        );
        assert!(event_snippet(ICS, "lanterns").contains("lanterns"));
        assert!(card_snippet(VCF, "mill").contains("mill"));
    }
}
