//! `CardFloorMetadata` — the per-record floor blob the segment footer carries
//! for the card kind.
//!
//! Variant-for-variant twin of `fauna_calendar::segments::floor`. The floor
//! holds the **immutable, ingest-time** `bridge_carddav_cards` columns; snapshot
//! restore rebuilds a row from the floor + envelope (content) joined with the
//! placement manifest (allocated state).
//!
//! `etag`/`modseq` are absent for the reason spelled out in the calendar twin's
//! module docs: they are allocated by the SQL transaction, so a floor carrying
//! them could only be encoded after the row commits — but the content record
//! must be durable *before* the row exists. Allocated state rides the placement
//! journal, exactly as mail's `MailPlacementRecord::Append` carries
//! `uid`/`modseq`/`flags` while `MailFloorMetadata` carries none of them.
//!
//! `actor_id` is deliberately absent: it is the segment store's scope, encoded
//! in the on-disk path (`__card/<actor_hex>/`).

use serde::{Deserialize, Serialize};

pub const CARD_FLOOR_FORMAT_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardFloorMetadata {
    pub format_version: u16,
    /// The card's collection. Sub-scope within the actor-scoped segment store.
    #[serde(with = "serde_bytes")]
    pub addressbook_id: [u8; 32],
    /// The `bridge_carddav_cards` PK tail (the record's segment `Cid` is the
    /// content hash of its envelope, not built from this). Derived from
    /// the body bytes once, at insert; opaque and carried thereafter — never
    /// re-derived on read (`db/bridge_carddav.rs` § derive_carddav_card_id).
    #[serde(with = "serde_bytes")]
    pub card_id: [u8; 32],
    /// `Vec<u8>`, not `[u8; 32]`: the column is a plain `BLOB` and
    /// `place_carddav_card` takes `&[u8]`, so a faithful row rebuild must not
    /// assume a width the write path never enforced.
    #[serde(with = "serde_bytes")]
    pub uid_hash: Vec<u8>,
    /// Size of the sealed body as the client supplied it — NOT the encoded
    /// envelope length. Preserved verbatim so the rebuilt row reports the same
    /// `ciphertext_size` a MUA already saw.
    pub ciphertext_size: u32,
    /// The card's own time (epoch **seconds**) — the `timestamp` argument that
    /// `place_carddav_card` both hashes into `card_id` and stores as
    /// `internal_date`.
    pub internal_date: i64,
    /// Server-assigned receive time (epoch **seconds** — mail's `received_at` is
    /// milliseconds; do not copy its `/1000` when computing a bucket).
    /// This, not `internal_date`, is the segment bucket key.
    pub created_at: i64,
}

/// Catalog-aligned default so fixtures are struct-update expressions and an
/// additive field-add can never break a hand-listed `cfg(test)` constructor.
impl Default for CardFloorMetadata {
    fn default() -> Self {
        Self {
            format_version: CARD_FLOOR_FORMAT_VERSION,
            addressbook_id: [0u8; 32],
            card_id: [0u8; 32],
            uid_hash: Vec::new(),
            ciphertext_size: 0,
            internal_date: 0,
            created_at: 0,
        }
    }
}

impl CardFloorMetadata {
    pub fn encode(&self) -> Result<Vec<u8>, fauna_cbor::EncodeError> {
        fauna_cbor::encode_canonical(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, fauna_cbor::DecodeError> {
        let f: CardFloorMetadata = fauna_cbor::decode_strict(bytes)?;
        Ok(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CardFloorMetadata {
        CardFloorMetadata {
            addressbook_id: [0x11u8; 32],
            card_id: [0x22u8; 32],
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
        assert_eq!(CardFloorMetadata::decode(&bytes).expect("decode"), f);
    }

    #[test]
    fn floor_at_rest_is_canonical_dagcbor() {
        let bytes = sample().encode().expect("encode");
        fauna_cbor::decode_strict::<CardFloorMetadata>(&bytes)
            .expect("floor bytes must be canonical dag-cbor");
    }

    /// Twin of the calendar floor's partition pin — every `bridge_carddav_cards`
    /// column has exactly one home across floor / envelope / placement manifest.
    #[test]
    fn floor_covers_every_immutable_non_payload_non_scope_column() {
        // bridge_carddav_cards columns, per db/migrations.rs:
        //   actor_id             -> segment scope (the __card/<actor_hex> dir)
        //   addressbook_id       -> floor
        //   card_id              -> floor (PK tail; also the record Cid digest)
        //   uid_hash             -> floor
        //   ciphertext_size      -> floor
        //   internal_date        -> floor
        //   created_at           -> floor
        //   encrypted_body       -> envelope
        //   encrypted_index_hint -> envelope
        //   encrypted_fauna_ext  -> envelope
        //   etag                 -> placement manifest (allocated by the SQL tx)
        //   modseq               -> placement manifest (allocated by the SQL tx)
        let rebuilt = CardFloorMetadata::decode(&sample().encode().unwrap()).unwrap();
        assert_eq!(rebuilt.addressbook_id, [0x11u8; 32]);
        assert_eq!(rebuilt.card_id, [0x22u8; 32]);
        assert_eq!(rebuilt.uid_hash, vec![0x33u8; 32]);
        assert_eq!(rebuilt.ciphertext_size, 4096);
        assert_eq!(rebuilt.internal_date, 1_714_999_900);
        assert_eq!(rebuilt.created_at, 1_715_000_000);
    }

    /// Twin of `cal`'s guard: the floor must stay encodable before the row
    /// commits, so no SQL-allocated column may creep back in.
    #[test]
    fn floor_carries_no_sql_allocated_state() {
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

    /// `uid_hash` is a BLOB the write path never width-checks.
    #[test]
    fn uid_hash_of_unexpected_width_round_trips() {
        let f = CardFloorMetadata {
            uid_hash: vec![0xAAu8; 5],
            ..Default::default()
        };
        let back = CardFloorMetadata::decode(&f.encode().unwrap()).unwrap();
        assert_eq!(back.uid_hash, vec![0xAAu8; 5]);
    }
}
