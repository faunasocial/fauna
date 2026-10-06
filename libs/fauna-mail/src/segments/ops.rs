//! Mail-specific encoding for the kind-agnostic `SegmentManager`.
//! Composes serialize_envelope + serialize_floor +
//! `mgr.append_record_with_bucket`. See spec § D2.

use fauna_cbor::Cid;
use fauna_segment_store::{AppendOutcome, ManagerError, SegmentManager};

use crate::segments::{MailFloorMetadata, MailRecordEnvelope, bucket_for};

pub const KIND: &str = "mail";

pub fn serialize_envelope(envelope: &MailRecordEnvelope) -> Result<Vec<u8>, OpsError> {
    // Via the envelope's own versioned encoder (never raw encode_canonical):
    // the wire shape depends on `envelope.format_version`.
    envelope
        .encode()
        .map_err(|e| OpsError::Encoding(format!("envelope: {e}")))
}

pub fn parse_envelope(bytes: &[u8]) -> Result<MailRecordEnvelope, OpsError> {
    // Via the envelope's dual-shape decoder (never raw decode_strict): v1 and
    // v2 records are dispatched on their format_version.
    MailRecordEnvelope::decode(bytes).map_err(|e| OpsError::Encoding(format!("envelope: {e}")))
}

pub fn serialize_floor(floor: &MailFloorMetadata) -> Result<Vec<u8>, OpsError> {
    fauna_cbor::encode_canonical(floor).map_err(|e| OpsError::Encoding(format!("floor: {e}")))
}

pub fn parse_floor(bytes: &[u8]) -> Result<MailFloorMetadata, OpsError> {
    fauna_cbor::decode_strict(bytes).map_err(|e| OpsError::Encoding(format!("floor: {e}")))
}

/// Encode a mail record envelope and derive its filing identity in one step.
///
/// The CID is `Cid::of_dag_cbor(env_bytes)` — **identity IS the content hash
/// of the stored block bytes** (`message-segment-store.md` § Record identity
/// per kind), the exact property `fauna_account_store::segments::admit`
/// re-hashes every block against. Returning the pair keeps the invariant
/// structural: a caller cannot file the bytes under any other identity without
/// bypassing this function.
pub fn encode_record(envelope: &MailRecordEnvelope) -> Result<(Cid, Vec<u8>), OpsError> {
    let env_bytes = serialize_envelope(envelope)?;
    let cid = Cid::of_dag_cbor(&env_bytes);
    Ok((cid, env_bytes))
}

/// Append one mail record to the segment store.
///
/// Convenience over [`encode_record`] + [`append_encoded`]; returns the derived
/// content-hash CID alongside the manager outcome. The bucket is derived from
/// `floor.received_at` (epoch milliseconds → epoch seconds).
pub async fn append(
    mgr: &SegmentManager,
    actor_id: &[u8; 32],
    envelope: &MailRecordEnvelope,
    floor: &MailFloorMetadata,
) -> Result<(Cid, AppendOutcome), OpsError> {
    let (cid, env_bytes) = encode_record(envelope)?;
    let outcome = append_encoded(mgr, actor_id, cid, &env_bytes, floor).await?;
    Ok((cid, outcome))
}

/// Append pre-encoded envelope bytes under their (already-derived) content-hash
/// CID. Split from [`append`] so the nest layer can size-guard and dedup on the
/// encoded bytes before the file append without encoding twice.
pub async fn append_encoded(
    mgr: &SegmentManager,
    actor_id: &[u8; 32],
    cid: Cid,
    env_bytes: &[u8],
    floor: &MailFloorMetadata,
) -> Result<AppendOutcome, OpsError> {
    let floor_bytes = serialize_floor(floor)?;
    // received_at is epoch ms; bucket_for takes epoch secs.
    let bucket = bucket_for(floor.received_at / 1000);
    mgr.append_record_with_bucket(actor_id, cid, env_bytes, &floor_bytes, &bucket)
        .await
        .map_err(OpsError::Manager)
}

/// Read just the record's envelope, without floor metadata.
/// For callers that need floor data, use `read_with_floor` instead.
pub async fn read_envelope_only(
    mgr: &SegmentManager,
    actor_id: &[u8; 32],
    segment_id: u32,
    cid: &Cid,
) -> Result<MailRecordEnvelope, OpsError> {
    let bytes = mgr.read_envelope_bytes(actor_id, segment_id, cid).await?;
    parse_envelope(&bytes)
}

pub async fn read_with_floor(
    mgr: &SegmentManager,
    actor_id: &[u8; 32],
    segment_id: u32,
    cid: &Cid,
) -> Result<(MailRecordEnvelope, MailFloorMetadata), OpsError> {
    let (env_bytes, floor_bytes) = mgr
        .read_record_with_floor_bytes(actor_id, segment_id, cid)
        .await?;
    Ok((parse_envelope(&env_bytes)?, parse_floor(&floor_bytes)?))
}

#[derive(Debug, thiserror::Error)]
pub enum OpsError {
    #[error("manager: {0}")]
    Manager(#[from] ManagerError),
    #[error("encoding: {0}")]
    Encoding(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_segment_store::SegmentManager;
    use tempfile::TempDir;

    fn sample_envelope() -> MailRecordEnvelope {
        MailRecordEnvelope::new(b"sealed-body".to_vec(), b"sealed-hint".to_vec())
    }

    fn sample_floor() -> MailFloorMetadata {
        MailFloorMetadata {
            received_at: 1_700_000_000_000,
            timestamp: 1_700_000_000,
            ciphertext_size: 512,
            sender_domain: "example.com".into(),
            spam_disposition: "accept".into(),
            spf: "pass".into(),
            dkim: "pass".into(),
            dmarc: "pass".into(),
            dmarc_policy: "reject".into(),
            arc: "pass".into(),
            ..Default::default()
        }
    }

    #[test]
    fn serialize_then_parse_envelope_roundtrips() {
        // v1 has no production encoder (envelope.rs module doc) — use the
        // default (v2) shape `sample_envelope()` already builds via `::new()`.
        let env = sample_envelope();
        let bytes = serialize_envelope(&env).unwrap();
        let parsed = parse_envelope(&bytes).unwrap();
        assert_eq!(env, parsed);
    }

    #[test]
    fn serialize_then_parse_floor_roundtrips() {
        let floor = sample_floor();
        let bytes = serialize_floor(&floor).unwrap();
        let parsed = parse_floor(&bytes).unwrap();
        assert_eq!(floor, parsed);
    }

    /// Discriminating red→green: the PRODUCTION mail-record envelope + floor
    /// serializers must emit canonical dag-cbor, not serde_bare. Per
    /// `serialization.md:29` — "every byte … on disk in CARv2 segments …
    /// goes through one canonical encoder." serde_bare bytes fail strict
    /// decode (`NotCanonical`/`NotValidCbor`); canonical dag-cbor passes.
    #[test]
    fn mail_envelope_and_floor_at_rest_are_canonical_dagcbor() {
        let env = sample_envelope();
        let env_bytes = serialize_envelope(&env).unwrap();
        // The envelope's own decoder is version-dispatched, so probe
        // canonicality with the raw ipld value decode instead.
        fauna_cbor::decode_strict::<fauna_cbor::Value>(&env_bytes)
            .expect("envelope bytes must be canonical dag-cbor");
        MailRecordEnvelope::decode(&env_bytes).expect("envelope decodes via the versioned decoder");

        let floor = sample_floor();
        let floor_bytes = serialize_floor(&floor).unwrap();
        fauna_cbor::decode_strict::<MailFloorMetadata>(&floor_bytes)
            .expect("floor bytes must be canonical dag-cbor");
    }

    #[tokio::test]
    async fn append_then_read_round_trip() {
        let dir = TempDir::new().unwrap();
        let mgr = SegmentManager::new(dir.path().to_path_buf(), "mail");
        let actor_id = [7u8; 32];
        let env = sample_envelope();
        let floor = sample_floor();
        // The filing identity is derived, never supplied: append returns the
        // content-hash CID of the encoded envelope bytes.
        let env_bytes = serialize_envelope(&env).unwrap();

        let (cid, outcome) = append(&mgr, &actor_id, &env, &floor).await.unwrap();
        assert_eq!(
            cid,
            Cid::of_dag_cbor(&env_bytes),
            "filing CID is the content hash of the stored block bytes"
        );

        let env_back = read_envelope_only(&mgr, &actor_id, outcome.segment_id, &cid)
            .await
            .unwrap();
        assert_eq!(env_back, env);

        let (env_back2, floor_back) = read_with_floor(&mgr, &actor_id, outcome.segment_id, &cid)
            .await
            .unwrap();
        assert_eq!(env_back2, env);
        assert_eq!(floor_back, floor);
    }
}
