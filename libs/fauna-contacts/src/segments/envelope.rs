//! `CardRecordEnvelope` — the per-record payload bytes that ride one
//! segment-record slot for the card kind.
//!
//! Variant-for-variant twin of `fauna_calendar::segments::envelope` (which is
//! itself mail's). Encoded with canonical dag-cbor; opaque to
//! fauna-segment-store, decoded nest-side when it needs the sealed vCard body or
//! the sealed index hint.
//!
//! The envelope carries **payloads only** — the `bridge_carddav_cards` metadata
//! columns live in [`super::floor::CardFloorMetadata`].
//!
//! `encrypted_fauna_ext` is deliberately absent, for the reason spelled out in
//! the calendar twin's module docs: the record's CID is the content hash of
//! the encoded envelope bytes ([`CardRecordEnvelope::encode_record`] —
//! `message-segment-store.md` § Record identity per kind) and CARv2 records
//! are append-only — under content addressing, by construction. The content
//! record may hold only immutable payload state.
//! `replace_carddav_card_by_uid`'s sidecar-only path mutates
//! `encrypted_fauna_ext`/`etag`/`modseq` in place against a fixed `card_id`, so
//! all three ride the placement journal instead.

use fauna_cbor::Cid;
use serde::{Deserialize, Serialize};

pub const CARD_ENVELOPE_FORMAT_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardRecordEnvelope {
    pub format_version: u16,
    /// Sealed under the owner's MLS-derived recipient key — the same Path B
    /// primitive CalDAV events use (`carddav-server.md` § Storage model).
    /// Cards are sealed at ingest in **both** storage modes; there has never
    /// been a raw-at-rest card, which is why the S4 seal back-fill has no card
    /// arm.
    #[serde(with = "serde_bytes")]
    pub encrypted_body: Vec<u8>,
    /// Sealed under the owner's content-index key. Opaque to the nest.
    /// Immutable per `card_id`: a new body yields a new `card_id`, and the
    /// sidecar-only update path never touches the hint.
    #[serde(with = "serde_bytes")]
    pub encrypted_index_hint: Vec<u8>,
}

impl CardRecordEnvelope {
    pub fn new(encrypted_body: Vec<u8>, encrypted_index_hint: Vec<u8>) -> Self {
        Self {
            format_version: CARD_ENVELOPE_FORMAT_VERSION,
            encrypted_body,
            encrypted_index_hint,
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, fauna_cbor::EncodeError> {
        fauna_cbor::encode_canonical(self)
    }

    /// Encode the envelope and derive its filing identity in one step —
    /// `Cid::of_dag_cbor(env_bytes)`: identity IS the content hash of the
    /// stored block bytes (`message-segment-store.md` § Record identity per
    /// kind). Mirror of the calendar twin's `encode_record` — both are the
    /// shared [`fauna_cbor::encode_with_cid`] mint.
    pub fn encode_record(&self) -> Result<(Cid, Vec<u8>), fauna_cbor::EncodeError> {
        fauna_cbor::encode_with_cid(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, fauna_cbor::DecodeError> {
        let env: CardRecordEnvelope = fauna_cbor::decode_strict(bytes)?;
        Ok(env)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let env = CardRecordEnvelope::new(b"sealed-vcard".to_vec(), b"sealed-hint".to_vec());
        let bytes = env.encode().expect("encode");
        assert_eq!(CardRecordEnvelope::decode(&bytes).expect("decode"), env);
    }

    #[test]
    fn empty_payloads_round_trip() {
        let env = CardRecordEnvelope::new(vec![], vec![]);
        let bytes = env.encode().expect("encode");
        assert_eq!(CardRecordEnvelope::decode(&bytes).expect("decode"), env);
    }

    /// Every byte at rest in a CARv2 segment goes through one canonical encoder
    /// (`serialization.md:29`). serde_bare bytes fail `decode_strict`.
    #[test]
    fn envelope_at_rest_is_canonical_dagcbor() {
        let env = CardRecordEnvelope::new(b"body".to_vec(), b"hint".to_vec());
        let bytes = env.encode().expect("encode");
        fauna_cbor::decode_strict::<CardRecordEnvelope>(&bytes)
            .expect("envelope bytes must be canonical dag-cbor");
    }

    /// Twin of `cal`'s guard — an append-only content record cannot hold state
    /// that is mutated in place against a fixed `card_id`.
    #[test]
    fn envelope_carries_no_mutable_state() {
        let encoded = CardRecordEnvelope::new(b"body".to_vec(), b"hint".to_vec())
            .encode()
            .expect("encode");
        for forbidden in ["encrypted_fauna_ext", "etag", "modseq"] {
            assert!(
                !encoded
                    .windows(forbidden.len())
                    .any(|w| w == forbidden.as_bytes()),
                "{forbidden} is mutated in place against a fixed card_id, so it \
                 cannot live in an append-only content record (see module docs)"
            );
        }
    }
}
