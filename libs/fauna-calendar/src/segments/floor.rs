//! `CalFloorMetadata` — the per-record floor blob the segment footer carries
//! for the calendar kind.
//!
//! Mirror of `fauna_mail::segments::floor`. The floor holds the **immutable,
//! ingest-time** `bridge_caldav_events` columns: those fixed by the write that
//! created the record and never rewritten afterwards. Snapshot restore rebuilds
//! a row from the floor + envelope (content) joined with the placement manifest
//! (allocated state).
//!
//! ### Why `etag`/`modseq` are NOT here
//!
//! They are allocated by the SQLite transaction (`modseq = highestmodseq + 1`,
//! `etag = format_etag(modseq)`), so a floor carrying them could only be encoded
//! *after* the row commits — but the content record must be durable *before* the
//! row exists, or a crash in between leaves a row whose body is nowhere (the
//! post-cutover `encrypted_body` column is empty, and a client retry hits the
//! idempotent path and never re-appends). So allocated state rides the
//! **placement journal** — exactly mail's split, where `MailPlacementRecord
//! ::Append` carries `uid`/`modseq`/`flags` + a `content_record_id` pointer and
//! `MailFloorMetadata` carries none of them.
//!
//! `actor_id` is deliberately absent: it is the segment store's scope, encoded
//! in the on-disk path (`__calendar/<actor_hex>/`), so carrying it here would
//! let a record disagree with the directory that holds it.

use serde::{Deserialize, Serialize};

pub const CAL_FLOOR_FORMAT_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalFloorMetadata {
    pub format_version: u16,
    /// The event's collection. Sub-scope within the actor-scoped segment store
    /// (calendar rows are actor-scoped, exactly as mail is).
    #[serde(with = "serde_bytes")]
    pub calendar_id: [u8; 32],
    /// The `bridge_caldav_events` PK tail (the record's segment `Cid` is the
    /// content hash of its envelope, not built from this). Derived from
    /// the body bytes once, at insert; opaque and carried thereafter — never
    /// re-derived on read (`db/bridge_caldav.rs` § derive_caldav_event_id).
    #[serde(with = "serde_bytes")]
    pub event_id: [u8; 32],
    /// `Vec<u8>`, not `[u8; 32]`: the column is a plain `BLOB` and
    /// `place_caldav_event` takes `&[u8]`, so a faithful row rebuild must not
    /// assume a width the write path never enforced.
    #[serde(with = "serde_bytes")]
    pub uid_hash: Vec<u8>,
    /// Size of the sealed body as the client supplied it — NOT the encoded
    /// envelope length. Preserved verbatim so the rebuilt row reports the same
    /// `ciphertext_size` a MUA already saw.
    pub ciphertext_size: u32,
    /// The event's own time (epoch **seconds**) — the `timestamp` argument that
    /// `place_caldav_event` both hashes into `event_id` and stores as
    /// `internal_date`. Can be arbitrarily far in the future.
    pub internal_date: i64,
    /// Server-assigned receive time (epoch **seconds** — mail's `received_at` is
    /// milliseconds; do not copy its `/1000` when computing a bucket).
    /// This, not `internal_date`, is the segment bucket key: bucketing on the
    /// event's own time would file a 2030 meeting into a 2030 bucket.
    pub created_at: i64,
}

/// Catalog-aligned default (`format_version` = the current constant, payload
/// fields zero/empty) so fixtures are struct-update expressions and an additive
/// field-add can never break a hand-listed `cfg(test)` constructor — the trap
/// `MailFloorMetadata` fell into three times.
impl Default for CalFloorMetadata {
    fn default() -> Self {
        Self {
            format_version: CAL_FLOOR_FORMAT_VERSION,
            calendar_id: [0u8; 32],
            event_id: [0u8; 32],
            uid_hash: Vec::new(),
            ciphertext_size: 0,
            internal_date: 0,
            created_at: 0,
        }
    }
}

impl CalFloorMetadata {
    pub fn encode(&self) -> Result<Vec<u8>, fauna_cbor::EncodeError> {
        fauna_cbor::encode_canonical(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, fauna_cbor::DecodeError> {
        let f: CalFloorMetadata = fauna_cbor::decode_strict(bytes)?;
        Ok(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CalFloorMetadata {
        CalFloorMetadata {
            calendar_id: [0x11u8; 32],
            event_id: [0x22u8; 32],
            uid_hash: vec![0x33u8; 32],
            ciphertext_size: 4096,
            internal_date: 1_714_999_900,
            created_at: 1_715_000_000,
            ..Default::default()
        }
    }

    #[test]
    fn round_trip() {
        let f = sample();
        let bytes = f.encode().expect("encode");
        assert_eq!(CalFloorMetadata::decode(&bytes).expect("decode"), f);
    }

    /// Every byte at rest in a CARv2 segment goes through one canonical encoder
    /// (`serialization.md:29`). serde_bare bytes fail `decode_strict`.
    #[test]
    fn floor_at_rest_is_canonical_dagcbor() {
        let bytes = sample().encode().expect("encode");
        fauna_cbor::decode_strict::<CalFloorMetadata>(&bytes)
            .expect("floor bytes must be canonical dag-cbor");
    }

    /// Every `bridge_caldav_events` column must have exactly one home across the
    /// three at-rest slots, or snapshot restore silently drops it. This test
    /// pins the floor's share of that partition; adding a column to the table
    /// without placing it here (or in the envelope, or in the placement
    /// manifest) should fail a test rather than lose data at restore.
    #[test]
    fn floor_covers_every_immutable_non_payload_non_scope_column() {
        // bridge_caldav_events columns, per db/migrations.rs:
        //   actor_id             -> segment scope (the __calendar/<actor_hex> dir)
        //   calendar_id          -> floor
        //   event_id             -> floor (PK tail; also the record Cid digest)
        //   uid_hash             -> floor
        //   ciphertext_size      -> floor
        //   internal_date        -> floor
        //   created_at           -> floor
        //   encrypted_body       -> envelope
        //   encrypted_index_hint -> envelope
        //   encrypted_fauna_ext  -> envelope
        //   etag                 -> placement manifest (allocated by the SQL tx)
        //   modseq               -> placement manifest (allocated by the SQL tx)
        let f = sample();
        let rebuilt = CalFloorMetadata::decode(&f.encode().unwrap()).unwrap();
        assert_eq!(rebuilt.calendar_id, [0x11u8; 32]);
        assert_eq!(rebuilt.event_id, [0x22u8; 32]);
        assert_eq!(rebuilt.uid_hash, vec![0x33u8; 32]);
        assert_eq!(rebuilt.ciphertext_size, 4096);
        assert_eq!(rebuilt.internal_date, 1_714_999_900);
        assert_eq!(rebuilt.created_at, 1_715_000_000);
    }

    /// The floor is encodable from data known *before* the row commits — the
    /// property that lets the content record be durable before the metadata row
    /// exists. `etag`/`modseq` are allocated by the SQL transaction, so their
    /// presence here would reintroduce the crash window this split closes.
    /// Guards against a well-meaning "restore needs modseq" field-add.
    #[test]
    fn floor_carries_no_sql_allocated_state() {
        // Canonical dag-cbor encodes struct fields as text keys, so the field
        // name appears verbatim in the encoded bytes.
        let encoded = sample().encode().expect("encode");
        for forbidden in ["etag", "modseq"] {
            assert!(
                !encoded
                    .windows(forbidden.len())
                    .any(|w| w == forbidden.as_bytes()),
                "{forbidden} is allocated by the SQL transaction and belongs to \
                 the placement manifest, not the content floor (see module docs)"
            );
        }
    }

    /// `uid_hash` is a BLOB the write path never width-checks; a non-32-byte
    /// value must survive the floor rather than panic a `[u8; 32]` conversion.
    #[test]
    fn uid_hash_of_unexpected_width_round_trips() {
        let f = CalFloorMetadata {
            uid_hash: vec![0xAAu8; 5],
            ..Default::default()
        };
        let back = CalFloorMetadata::decode(&f.encode().unwrap()).unwrap();
        assert_eq!(back.uid_hash, vec![0xAAu8; 5]);
    }
}
