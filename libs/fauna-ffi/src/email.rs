use fauna_client_core::email as core_email;

use crate::{FfiError, bytes_to_actor_id, keypair_from_bytes};

/// Build a signed email payload (ContactRequest + Post) for the inbox.
/// Returns canonical dag-cbor-encoded bytes ready to POST to `/api/v1/inbox/{actor_id}`.
#[uniffi::export]
pub fn build_signed_email(
    secret: Vec<u8>,
    to: Vec<u8>,
    subject: String,
    body: String,
    node_url: String,
) -> Result<Vec<u8>, FfiError> {
    let kp = keypair_from_bytes(&secret)?;
    let recipient = bytes_to_actor_id(&to)?;
    core_email::build_signed_email(&kp, &recipient.0, &subject, &body, &node_url)
        .map_err(|e| FfiError::General { msg: e.0 })
}

/// Build a signed **knock** (contact-request) payload — [`build_signed_email`]
/// with the canonical knock subject/body baked in
/// ([`fauna_client_core::email::KNOCK_SUBJECT`] / `KNOCK_BODY`), so a native
/// app builds its outbound knock without carrying its own copy of the wire
/// sentinel literal (priority #2). Returns canonical dag-cbor-encoded bytes
/// ready for the `fauna.inbox.send` WS-RPC kind.
#[uniffi::export]
pub fn build_knock_payload(
    secret: Vec<u8>,
    to: Vec<u8>,
    node_url: String,
) -> Result<Vec<u8>, FfiError> {
    let kp = keypair_from_bytes(&secret)?;
    let recipient = bytes_to_actor_id(&to)?;
    core_email::build_knock_payload(&kp, &recipient.0, &node_url)
        .map_err(|e| FfiError::General { msg: e.0 })
}

// NOTE (2026-07-15 dark-rail audit): the `build_signed_email_with_media` FFI
// face (and its wasm twin) were deleted — no client ever called either; media
// DMs ride the MLS conversation attachment path (`ConversationsManager::
// add_attachment`), and the web JS wrapper was already removed in the MLS
// cutover. The non-media `build_signed_email` above remains live.

/// Decode a canonical dag-cbor-encoded email payload (ContactRequest + Post).
/// Returns a JSON string with fields: from, subject, body, timestamp, valid,
/// encrypted, post_id, sender_node, attachments.
#[uniffi::export]
pub fn decode_email(payload: Vec<u8>) -> Result<String, FfiError> {
    core_email::decode_email_json(&payload).map_err(|e| FfiError::General { msg: e.0 })
}

/// True iff `connected_host` matches at least one of `mx_patterns` per the
/// MTA-STS `mx:` matching rules (RFC 8461 §4.1): case-insensitive,
/// trailing-dot tolerant, exact match for non-wildcard patterns, and
/// `*.<base>` patterns matching exactly one prepended label.
///
/// Pure delegate to [`fauna_mail::outbound::mta_sts::mx_patterns_match`] so
/// the Go mail bridge can enforce a fetched policy's `mx` set against the
/// host it actually connected to without re-implementing the wildcard logic.
#[uniffi::export]
pub fn mta_sts_mx_matches(mx_patterns: Vec<String>, connected_host: String) -> bool {
    fauna_mail::outbound::mta_sts::mx_patterns_match(&mx_patterns, &connected_host)
}

/// Returns a copy of `raw` with all `Received:` headers removed (whole
/// logical header, continuation lines included), the body copied verbatim.
/// Outbound mail must not leak the submitter's IP or our internal
/// hostnames; the Go MTA bridge calls this before DKIM-signing so the
/// signature covers the stripped form.
///
/// Pure delegate to
/// [`fauna_mail::outbound::received_strip::strip_received_headers`] —
/// case-insensitive field-name match, RFC 5322 §2.2.3 continuation-aware,
/// substring-safe (`X-Received-By` / `Received-SPF` survive).
#[uniffi::export]
pub fn strip_received_headers(raw: Vec<u8>) -> Vec<u8> {
    fauna_mail::outbound::received_strip::strip_received_headers(&raw)
}

/// Returns a copy of `raw` with every reserved `X-Fauna-*` delivery-stamp header
/// removed (`X-Fauna-Scan-*` / `X-Fauna-Address-*`, …) **except** the
/// inbound-consumed `X-Fauna-Forwarded-By` forward-loop trace. The inbound MTA
/// bridge calls this at the DATA stage — before parsing for the filter context
/// and before prepending its own genuine stamps — so a sender cannot forge a
/// trust-stamp header that matches a recipient's alias-metadata filter rule or
/// lands in the sealed copy a client reads.
///
/// Pure delegate to [`fauna_mail::received_header::strip_fauna_headers`] —
/// case-insensitive `X-Fauna-` field-name prefix match, RFC 5322 §2.2.3
/// continuation-aware, substring-safe (`X-Not-Fauna` survives). Inbound sibling
/// of [`strip_received_headers`].
#[uniffi::export]
pub fn strip_fauna_headers(raw: Vec<u8>) -> Vec<u8> {
    fauna_mail::received_header::strip_fauna_headers(&raw)
}

/// Read the `X-Fauna-Spam-Threshold` delivery-stamp off a decrypted message —
/// *this* message's `spam_folder` tier in whole points, folded nest-side at
/// delivery from per-alias > per-account > admin default (`mail-aliases.md`
/// § Spam-threshold override). `None` = no stamp (a copy that never went through the
/// resolver), and the caller falls back to its session policy — never to
/// a hard-coded number, and never to `0`, which would disable Junk routing.
///
/// Pure delegate to [`fauna_mail::aliases::read_spam_threshold_stamp`]. It is
/// crossed here rather than re-implemented as a Go header scan so the stamping
/// and reading halves of the contract cannot drift on the field name or the
/// parse — the same reason `strip_fauna_headers` above is a delegate.
#[uniffi::export]
pub fn read_spam_threshold_stamp(raw: Vec<u8>) -> Option<u32> {
    fauna_mail::aliases::read_spam_threshold_stamp(&raw)
}

/// Carry a fired `Allow` filter rule past delivery: the recipient's stamped
/// copy with its `X-Fauna-Spam-Threshold` stamp(s) replaced by ONE leading
/// `X-Fauna-Spam-Threshold: 0`, so no post-delivery scorer re-files it to Junk
/// (`email-filters.md` § Multi-action composition).
///
/// Pure delegate to [`fauna_mail::aliases::stamp_filter_allow`] — the writer
/// sits beside the reader above so the two cannot drift on the field name.
#[uniffi::export]
pub fn stamp_filter_allow(raw: Vec<u8>) -> Vec<u8> {
    fauna_mail::aliases::stamp_filter_allow(&raw)
}

/// One DANE/TLSA record (RFC 6698) the Go MTA bridge pins an outbound TLS
/// handshake against. Named `DaneTlsaRecord` (not bare `TlsaRecord`) so the
/// generated `uniffi::Record` can't collide with another binding's record
/// type in the fauna-ffi surface. Mirrors
/// [`fauna_mail::outbound::dane::TlsaRecord`].
#[derive(uniffi::Record)]
pub struct DaneTlsaRecord {
    /// 0 PKIX-TA / 1 PKIX-EE / 2 DANE-TA / 3 DANE-EE.
    pub usage: u8,
    /// 0 Full cert / 1 SubjectPublicKeyInfo.
    pub selector: u8,
    /// 0 Exact / 1 SHA-256 / 2 SHA-512.
    pub matching: u8,
    /// Certificate-association data (raw bytes / digest).
    pub data: Vec<u8>,
}

/// True iff the presented `cert_chain_der` (DER-encoded, leaf-first — the
/// `rawCerts` Go hands a TLS `VerifyPeerCertificate` callback) satisfies one
/// of `tlsa_records` for the MX host `mx_host` was dialled as. The Go MTA
/// bridge calls this during outbound DANE pinning (RFC 7672): nest fetches
/// the DNSSEC-secure TLSA records over `fauna.bridges.fetch_tlsa`, Go does
/// the handshake and asks this fn whether the peer's chain satisfies them. A
/// `false` for a DANE-pinned host is a hard failure (no plaintext fallback).
///
/// Pure delegate to [`fauna_mail::outbound::dane::dane_chain_matches`] —
/// DANE-EE matches the leaf and nothing else is checked; **DANE-TA requires
/// the leaf to chain to the matched cert and to carry `mx_host`'s name**, a
/// bare hash match being no proof at all for a public anchor; PKIX usages
/// skipped; selector 0/1 (full cert / SPKI) × matching 0/1/2 (exact /
/// SHA-256 / SHA-512) per RFC 6698.
#[uniffi::export]
pub fn dane_chain_matches(
    tlsa_records: Vec<DaneTlsaRecord>,
    cert_chain_der: Vec<Vec<u8>>,
    mx_host: String,
) -> bool {
    let records: Vec<fauna_mail::outbound::dane::TlsaRecord> = tlsa_records
        .into_iter()
        .map(|r| fauna_mail::outbound::dane::TlsaRecord {
            usage: r.usage,
            selector: r.selector,
            matching: r.matching,
            data: r.data,
        })
        .collect();
    fauna_mail::outbound::dane::dane_chain_matches(&records, &cert_chain_der, &mx_host)
}
