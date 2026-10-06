//! Pure conv-kind encoders for the kind-agnostic segment store.
//!
//! Mirrors `fauna-mail`'s `segments::ops` envelope/floor encoders, but conv's
//! shape is thinner: no `encrypted_index_hint` on the envelope, and no sender
//! field on the floor (the conversation sender is zero-filled / not floor data;
//! it lives only in the anti-spam `sender_behavior` table). This module is the
//! *pure* encoding half — the `SegmentManager`-touching `append`/`read`
//! coordination lives nest-side, so there is intentionally no
//! `fauna_segment_store` dependency here. See spec § D2.

use serde::{Deserialize, Serialize};

pub const KIND: &str = "conv";

/// The per-record payload bytes that ride one segment-record slot for the
/// conv kind. Encoded with canonical dag-cbor; opaque to the segment store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConvRecordEnvelope {
    pub format_version: u8,
    /// Sealed under the channel's MLS read key.
    #[serde(with = "serde_bytes")]
    pub sealed_payload: Vec<u8>,
}

impl ConvRecordEnvelope {
    pub fn new(sealed_payload: Vec<u8>) -> Self {
        Self {
            format_version: 1,
            sealed_payload,
        }
    }

    /// Convenience decoder; delegates to [`parse_envelope`].
    pub fn decode(bytes: &[u8]) -> Result<Self, OpsError> {
        parse_envelope(bytes)
    }
}

/// Floor metadata for one conv record. No sender field: the conversation
/// sender is zero-filled / not floor data (it lives only in the anti-spam
/// `sender_behavior` table).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConvFloorMetadata {
    pub format_version: u8,
    pub received_at: i64,
    pub seq: i64,
}

pub fn serialize_envelope(envelope: &ConvRecordEnvelope) -> Result<Vec<u8>, OpsError> {
    fauna_cbor::encode_canonical(envelope).map_err(|e| OpsError::Encoding(format!("envelope: {e}")))
}

pub fn parse_envelope(bytes: &[u8]) -> Result<ConvRecordEnvelope, OpsError> {
    fauna_cbor::decode_strict(bytes).map_err(|e| OpsError::Encoding(format!("envelope: {e}")))
}

pub fn serialize_floor(floor: &ConvFloorMetadata) -> Result<Vec<u8>, OpsError> {
    fauna_cbor::encode_canonical(floor).map_err(|e| OpsError::Encoding(format!("floor: {e}")))
}

pub fn parse_floor(bytes: &[u8]) -> Result<ConvFloorMetadata, OpsError> {
    fauna_cbor::decode_strict(bytes).map_err(|e| OpsError::Encoding(format!("floor: {e}")))
}

/// Encode one conv record envelope and mint its filing identity in one step —
/// **the single mint for the conv kind**, mirroring
/// `fauna_mail::segments::ops::encode_record`.
///
/// The identity IS the content hash of the stored bytes:
/// `Cid::of_dag_cbor(envelope_bytes)` (`message-segment-store.md` § *Record
/// identity per kind* — ruled 2026-08-17, conv's cutover leg the same day). The
/// CID and the bytes are returned together deliberately: the nest files under
/// the CID *and* writes the bytes, and a second, separate encode is exactly how
/// the two drift apart.
///
/// The pre-image is the **envelope** bytes, not the sealed payload the retired
/// `derive_record_id(channel_id, seq, body)` hashed — so seq no longer
/// participates in identity, and identical envelope bytes on one channel are
/// one record (the nest's pre-append dedup collapses the replay).
pub fn encode_record(
    envelope: &ConvRecordEnvelope,
) -> Result<(fauna_cbor::Cid, Vec<u8>), OpsError> {
    fauna_cbor::encode_with_cid(envelope).map_err(|e| OpsError::Encoding(format!("envelope: {e}")))
}

/// Derive a conv record's filing CID from the **sealed payload** alone — the
/// client-side half of the identity rule.
///
/// A conversation record reaches an app over `fauna.conversations.channel.fetch`
/// as its sealed payload (the entry's `envelope` field, or the body a `send`
/// posted). That is exactly what the nest wraps in a [`ConvRecordEnvelope`]
/// before filing, so the app can *derive* the record's plane identity without
/// ever fetching it — the property `fauna_conversations::plane::plane_ref`
/// rests on. Both sides route through [`encode_record`], so client and nest
/// agree by construction rather than by agreement.
pub fn derive_record_cid(sealed_payload: &[u8]) -> Result<fauna_cbor::Cid, OpsError> {
    let (cid, _bytes) = encode_record(&ConvRecordEnvelope::new(sealed_payload.to_vec()))?;
    Ok(cid)
}

#[derive(Debug, thiserror::Error)]
pub enum OpsError {
    #[error("encoding: {0}")]
    Encoding(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_then_parse_envelope_roundtrips() {
        let env = ConvRecordEnvelope::new(b"sealed".to_vec());
        let bytes = serialize_envelope(&env).unwrap();
        let parsed = parse_envelope(&bytes).unwrap();
        assert_eq!(env, parsed);
    }

    #[test]
    fn serialize_then_parse_floor_roundtrips() {
        let floor = ConvFloorMetadata {
            format_version: 1,
            received_at: 1_700_000_000_000,
            seq: 42,
        };
        let bytes = serialize_floor(&floor).unwrap();
        let parsed = parse_floor(&bytes).unwrap();
        assert_eq!(floor, parsed);
    }

    /// Discriminating red→green: the PRODUCTION conv-record envelope + floor
    /// serializers must emit canonical dag-cbor, not serde_bare. Per
    /// `serialization.md:29` — at-rest CARv2 segment metadata goes through
    /// one canonical encoder. serde_bare bytes fail strict decode; dag-cbor
    /// passes.
    #[test]
    fn conv_envelope_and_floor_at_rest_are_canonical_dagcbor() {
        let env = ConvRecordEnvelope::new(b"sealed".to_vec());
        let env_bytes = serialize_envelope(&env).unwrap();
        fauna_cbor::decode_strict::<ConvRecordEnvelope>(&env_bytes)
            .expect("conv envelope bytes must be canonical dag-cbor");

        let floor = ConvFloorMetadata {
            format_version: 1,
            received_at: 1_700_000_000_000,
            seq: 42,
        };
        let floor_bytes = serialize_floor(&floor).unwrap();
        fauna_cbor::decode_strict::<ConvFloorMetadata>(&floor_bytes)
            .expect("conv floor bytes must be canonical dag-cbor");
    }

    /// The identity rule itself: the minted CID is the content hash of the
    /// bytes returned alongside it — `admit`'s re-hash predicate, asserted at
    /// the mint (`message-segment-store.md` § Record identity per kind).
    #[test]
    fn the_minted_cid_is_the_content_hash_of_the_bytes_it_returns() {
        let env = ConvRecordEnvelope::new(b"sealed".to_vec());
        let (cid, bytes) = encode_record(&env).unwrap();
        assert!(
            cid.matches(&bytes),
            "the filing cid must be blake3 of the very bytes stored under it"
        );
        assert_eq!(cid, fauna_cbor::Cid::of_dag_cbor(&bytes));
    }

    /// The client's payload-only derivation and the nest's encode-and-file mint
    /// are the same identity — the cross-machine property the plane seam rests
    /// on, asserted against the shared function rather than a re-implementation.
    #[test]
    fn the_payload_only_derivation_equals_the_encode_mint() {
        let payload = b"sealed-envelope".to_vec();
        let (minted, _) = encode_record(&ConvRecordEnvelope::new(payload.clone())).unwrap();
        assert_eq!(derive_record_cid(&payload).unwrap(), minted);
    }

    /// Identity discriminates on the payload and nothing else: `seq` left the
    /// pre-image with the retired `derive_record_id`, so two different bodies
    /// differ and a replay of one body is one identity (which is what makes the
    /// nest's scoped pre-append dedup the right shape).
    #[test]
    fn identity_follows_the_payload_alone() {
        let a = derive_record_cid(b"body").unwrap();
        let b = derive_record_cid(b"other").unwrap();
        let a_again = derive_record_cid(b"body").unwrap();
        assert_ne!(a, b, "a different body is a different record");
        assert_eq!(a, a_again, "the same body is the same record");
    }
}
