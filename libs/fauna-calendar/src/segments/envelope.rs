//! `CalRecordEnvelope` — the per-record payload bytes that ride one
//! segment-record slot for the calendar kind.
//!
//! Mirror of `fauna_mail::segments::envelope`. Encoded with canonical
//! dag-cbor; opaque to fauna-segment-store, decoded nest-side (segment read
//! path / restore handler) when it needs the sealed body or sealed index hint.
//! Both payload fields hold sealed blobs verbatim as opaque bytes, so the outer
//! encoding is nest-internal and never crosses to Go.
//!
//! The envelope carries **payloads only** — the `bridge_caldav_events` metadata
//! columns live in [`super::floor::CalFloorMetadata`]. That split is mail's.
//!
//! ### The immutability rule (why `encrypted_fauna_ext` is NOT here)
//!
//! A record's segment CID is the **content hash of the encoded envelope
//! bytes** ([`CalRecordEnvelope::encode_record`] — `message-segment-store.md`
//! § Record identity per kind), and CARv2 segments are append-only: a record
//! can never be rewritten under its own CID — under content addressing that is
//! true *by construction* (changed bytes ARE a different identity). So the
//! content record may hold **only immutable payload state** — anything mutable
//! is guaranteed to go stale at rest.
//!
//! Three `bridge_caldav_events` columns are mutated in place against a *fixed*
//! `event_id` by `replace_caldav_event_by_uid`'s sidecar-only path (a Fauna
//! write that refines the sidecar over a byte-identical VEVENT body + timestamp,
//! so the body-derived `event_id` — and, the payload being byte-identical, the
//! stored `record_cid` — is unchanged): `etag`, `modseq`, and
//! `encrypted_fauna_ext`. All three therefore belong to the **placement
//! journal**, not here — mail's split, where `MailPlacementRecord::Append`
//! carries the mutable placement state and the floor/envelope carry none of it.
//! Restore reads the sidecar from the placement manifest.

use fauna_cbor::Cid;
use serde::{Deserialize, Serialize};

pub const CAL_ENVELOPE_FORMAT_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalRecordEnvelope {
    pub format_version: u16,
    /// Sealed under the owner's MLS-derived recipient key (the kind's inner
    /// seal — see `encryption-at-rest.md` § Calendar-event-content row).
    #[serde(with = "serde_bytes")]
    pub encrypted_body: Vec<u8>,
    /// Sealed under the owner's content-index key. Opaque to the nest.
    /// Immutable per `event_id`: a new body yields a new `event_id`, and the
    /// sidecar-only update path never touches the hint.
    #[serde(with = "serde_bytes")]
    pub encrypted_index_hint: Vec<u8>,
}

impl CalRecordEnvelope {
    pub fn new(encrypted_body: Vec<u8>, encrypted_index_hint: Vec<u8>) -> Self {
        Self {
            format_version: CAL_ENVELOPE_FORMAT_VERSION,
            encrypted_body,
            encrypted_index_hint,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, fauna_cbor::EncodeError> {
        fauna_cbor::encode_canonical(self)
    }

    /// Encode the envelope and derive its filing identity in one step —
    /// `Cid::of_dag_cbor(env_bytes)`: **identity IS the content hash of the
    /// stored block bytes** (`message-segment-store.md` § Record identity per
    /// kind), the exact property `fauna_account_store::segments::admit`
    /// re-hashes every block against. Returning the pair keeps the invariant
    /// structural: a caller cannot file the bytes under any other identity
    /// without bypassing this function. Delegates to the shared
    /// [`fauna_cbor::encode_with_cid`] mint (also `fauna_contacts`' card
    /// twin); `fauna_mail::segments::ops::encode_record` stays hand-rolled
    /// because mail's wire shape is version-negotiated, not a bare
    /// `Serialize`.
    pub fn encode_record(&self) -> Result<(Cid, Vec<u8>), fauna_cbor::EncodeError> {
        fauna_cbor::encode_with_cid(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, fauna_cbor::DecodeError> {
        let env: CalRecordEnvelope = fauna_cbor::decode_strict(bytes)?;
        Ok(env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let env = CalRecordEnvelope::new(b"sealed-body".to_vec(), b"sealed-hint".to_vec());
        let bytes = env.encode().expect("encode");
        assert_eq!(CalRecordEnvelope::decode(&bytes).expect("decode"), env);
    }

    #[test]
    fn empty_payloads_round_trip() {
        let env = CalRecordEnvelope::new(vec![], vec![]);
        let bytes = env.encode().expect("encode");
        assert_eq!(CalRecordEnvelope::decode(&bytes).expect("decode"), env);
    }

    /// Every byte at rest in a CARv2 segment goes through one canonical encoder
    /// (`serialization.md:29`). serde_bare bytes fail `decode_strict`.
    #[test]
    fn envelope_at_rest_is_canonical_dagcbor() {
        let env = CalRecordEnvelope::new(b"body".to_vec(), b"hint".to_vec());
        let bytes = env.encode().expect("encode");
        fauna_cbor::decode_strict::<CalRecordEnvelope>(&bytes)
            .expect("envelope bytes must be canonical dag-cbor");
    }

    /// The content record is keyed by the content hash of its envelope and
    /// CARv2 records are append-only, so it can hold only state functionally
    /// determined by its own bytes. `encrypted_fauna_ext` is mutated in place
    /// against a fixed `event_id` (the sidecar-only update path), so an envelope
    /// copy of it would go stale at rest and restore would resurrect the old
    /// sidecar. Guards against a "restore needs the sidecar" field-add: it does,
    /// but from the placement journal.
    #[test]
    fn envelope_carries_no_mutable_state() {
        let encoded = CalRecordEnvelope::new(b"body".to_vec(), b"hint".to_vec())
            .encode()
            .expect("encode");
        // Canonical dag-cbor encodes struct fields as text keys.
        for forbidden in ["encrypted_fauna_ext", "etag", "modseq"] {
            assert!(
                !encoded
                    .windows(forbidden.len())
                    .any(|w| w == forbidden.as_bytes()),
                "{forbidden} is mutated in place against a fixed event_id, so it \
                 cannot live in an append-only content record (see module docs)"
            );
        }
    }
}
