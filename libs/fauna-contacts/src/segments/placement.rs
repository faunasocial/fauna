//! CardDAV placement-journal records + manifest.
//!
//! Mirror of `fauna_calendar::segments::placement` for CardDAV. The kind
//! identity is `__card-placement` (reserved folder, the twin of calendar's
//! `__calendar-placement`).
//!
//! This third kind is a small wrapper (design tracked internally, § D1);
//! see `docs/goal/behavior/carddav-server.md` § Storage model → Durability &
//! disaster recovery (the card-placement journal is the `CalPlacementManifest`
//! twin).
//!
//! The frozen v1 shapes the v2 bump read through (manifest upgrade, record
//! fallback, the `Option` fields v1 never recorded, the nest's boot heal) were
//! retired by the compat-remnant sweep (2026-09-24,
//! `docs/goal/architecture/version-compatibility.md` § Dimension 2, program
//! 4) — see `fauna_calendar::segments::placement`, the twin, for the full note.

use std::path::{Path, PathBuf};

use fauna_segment_store::{KindManifest, SegmentStoreError, VersionedManifest, codec};
use serde::{Deserialize, Serialize};

pub const CARD_PLACEMENT_FORMAT_VERSION: u16 = 2;

/// One CardDAV placement event (format v2). Keyed by `(addressbook_id, modseq)`.
///
/// Records are append-only inside immutable segments. Updates are
/// captured as successive records; compaction collapses superseded records.
///
/// Format v2 (S6.9) is the calendar twin's bump verbatim: `PutCard` grows
/// the content-record id + effective sidecar, `DeleteCard` grows the id +
/// `deleted_at` — see `fauna_calendar::segments::placement` § Format v2 for
/// the full rationale (`message-segment-store.md` § Invariants, rule 2).
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `ladder` ground: a new variant raises the format
/// version the reader checks before it decodes, so there is no unknown arm). A
/// new variant is an edit to `tools/check-additive-evolution/enum_ledger.txt`,
/// made in the same change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CardPlacementRecord {
    ProvisionAddressbook {
        #[serde(with = "serde_bytes")]
        addressbook_id: [u8; 32],
        #[serde(with = "serde_bytes")]
        encrypted_metadata: Vec<u8>,
    },
    UpdateAddressbookMetadata {
        #[serde(with = "serde_bytes")]
        addressbook_id: [u8; 32],
        #[serde(with = "serde_bytes")]
        encrypted_metadata: Vec<u8>,
        modseq: u64,
    },
    DeleteAddressbook {
        #[serde(with = "serde_bytes")]
        addressbook_id: [u8; 32],
    },
    PutCard {
        #[serde(with = "serde_bytes")]
        addressbook_id: [u8; 32],
        #[serde(with = "serde_bytes")]
        uid_hash: [u8; 32],
        etag: String,
        modseq: u64,
        ciphertext_size: u32,
        /// Content-record id — the `bridge_carddav_cards` PK tail. The
        /// placement→content join for restore. (The record's segment CID is
        /// the content hash of its envelope, not a wrap of this id — the
        /// row's `record_cid` column carries it.)
        #[serde(with = "serde_bytes")]
        card_id: [u8; 32],
        /// The row's **effective** sealed Fauna-extension sidecar after this
        /// write — the DAO's value, not the request's (a MUA write carries
        /// `None` but *preserves* the prior row's sidecar).
        #[serde(default, with = "serde_bytes")]
        encrypted_fauna_ext: Option<Vec<u8>>,
    },
    DeleteCard {
        #[serde(with = "serde_bytes")]
        addressbook_id: [u8; 32],
        #[serde(with = "serde_bytes")]
        uid_hash: [u8; 32],
        modseq: u64,
        /// Content-record id of the deleted row (`bridge_carddav_expunged`
        /// carries it; restore rebuilds that row from here).
        #[serde(with = "serde_bytes")]
        card_id: [u8; 32],
        /// Server delete time, epoch **seconds** — what makes the tombstone
        /// retention prune possible.
        deleted_at: i64,
    },
}

impl CardPlacementRecord {
    pub fn encode(&self) -> Result<Vec<u8>, SegmentStoreError> {
        codec::encode(self)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, SegmentStoreError> {
        codec::decode(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressbookState {
    #[serde(with = "serde_bytes")]
    pub addressbook_id: [u8; 32],
    #[serde(with = "serde_bytes")]
    pub encrypted_metadata: Vec<u8>,
    pub highestmodseq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardPlacement {
    #[serde(with = "serde_bytes")]
    pub addressbook_id: [u8; 32],
    #[serde(with = "serde_bytes")]
    pub uid_hash: [u8; 32],
    pub etag: String,
    pub modseq: u64,
    pub ciphertext_size: u32,
    /// Content-record id — the placement→content join restore keys on.
    #[serde(with = "serde_bytes")]
    pub card_id: [u8; 32],
    /// Effective sealed sidecar after the last PUT; `None` = no sidecar.
    #[serde(default, with = "serde_bytes")]
    pub encrypted_fauna_ext: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardTombstoneRef {
    #[serde(with = "serde_bytes")]
    pub addressbook_id: [u8; 32],
    #[serde(with = "serde_bytes")]
    pub uid_hash: [u8; 32],
    pub modseq: u64,
    /// Content-record id of the deleted row (restore rebuilds its
    /// `bridge_carddav_expunged` row from here).
    #[serde(with = "serde_bytes")]
    pub card_id: [u8; 32],
    /// Server delete time (epoch seconds) — the retention prune's clock.
    pub deleted_at: i64,
}

/// Compacted current placement state. Rebuildable from segments.
/// Persisted as `manifest.card-placement` via atomic-rename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardPlacementManifest {
    pub format_version: u16,
    pub kind_manifest: KindManifest,
    pub addressbooks: Vec<AddressbookState>,
    pub cards: Vec<CardPlacement>,
    pub tombstones: Vec<CardTombstoneRef>,
}

impl CardPlacementManifest {
    pub fn new() -> Self {
        Self {
            format_version: CARD_PLACEMENT_FORMAT_VERSION,
            kind_manifest: KindManifest::empty(),
            addressbooks: Vec::new(),
            cards: Vec::new(),
            tombstones: Vec::new(),
        }
    }
}

impl VersionedManifest for CardPlacementManifest {
    const CURRENT_VERSION: u16 = CARD_PLACEMENT_FORMAT_VERSION;
    const LABEL: &'static str = "card-placement";

    fn format_version(&self) -> u16 {
        self.format_version
    }
}

impl Default for CardPlacementManifest {
    fn default() -> Self {
        Self::new()
    }
}

/// On-disk root for an actor's card-placement segments.
/// `<data_dir>/segments/__card-placement/<actor_id_hex>/`
pub fn card_placement_segments_root(data_dir: &Path, actor_id: &[u8; 32]) -> PathBuf {
    data_dir
        .join("segments")
        .join("__card-placement")
        .join(fauna_core::hex32::encode(actor_id))
}

/// On-disk path to the card-placement manifest.
/// `<data_dir>/segments/__card-placement/<actor_id_hex>/manifest.card-placement`
pub fn card_placement_manifest_path(data_dir: &Path, actor_id: &[u8; 32]) -> PathBuf {
    card_placement_segments_root(data_dir, actor_id).join("manifest.card-placement")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Discriminating red→green: the PRODUCTION card-placement record +
    /// manifest serializers must emit canonical dag-cbor, not serde_bare
    /// (`serialization.md:29` — at-rest CARv2 metadata is one canonical
    /// encoder). serde_bare bytes fail strict decode; dag-cbor passes.
    #[test]
    fn placement_record_and_manifest_at_rest_are_canonical_dagcbor() {
        let record = CardPlacementRecord::PutCard {
            addressbook_id: [0x11; 32],
            uid_hash: [0x22; 32],
            etag: "etag-1".to_string(),
            modseq: 7,
            ciphertext_size: 512,
            card_id: [0x99; 32],
            encrypted_fauna_ext: Some(vec![0xf0, 0x0d]),
        };
        let rec_bytes = record.encode().expect("encode record");
        fauna_cbor::decode_strict::<CardPlacementRecord>(&rec_bytes)
            .expect("card placement record bytes must be canonical dag-cbor");

        let mut m = CardPlacementManifest::new();
        m.addressbooks.push(AddressbookState {
            addressbook_id: [0x42; 32],
            encrypted_metadata: vec![1, 2, 3],
            highestmodseq: 9,
        });
        let man_bytes = m.encode().expect("encode manifest");
        fauna_cbor::decode_strict::<CardPlacementManifest>(&man_bytes)
            .expect("card placement manifest bytes must be canonical dag-cbor");
    }

    #[test]
    fn save_then_load_round_trips() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xabu8; 32];
        let path = card_placement_manifest_path(tmp.path(), &actor);
        let mut m = CardPlacementManifest::new();
        m.addressbooks.push(AddressbookState {
            addressbook_id: [0x42; 32],
            encrypted_metadata: vec![1, 2, 3],
            highestmodseq: 42,
        });
        m.tombstones.push(CardTombstoneRef {
            addressbook_id: [0x42; 32],
            uid_hash: [0x77; 32],
            modseq: 41,
            card_id: [0x88; 32],
            deleted_at: 1_752_000_000,
        });
        m.save_atomic(&path).expect("save");
        let loaded = CardPlacementManifest::load(&path)
            .expect("load")
            .expect("Some");
        assert_eq!(loaded, m);
    }

    /// A v1 manifest on disk is refused, not upgraded — the card twin of
    /// `fauna_calendar`'s pin. The pre-sweep v1 read arm was retired by the
    /// compat-remnant sweep (program 4, 2026-09-24): no v1 manifest exists
    /// anywhere. A populated v1 entry lacks the now-required `card_id`, so
    /// its body does not even decode — but the stamp is read first, so it is
    /// refused as the intact file of another version it is
    /// (`SchemaMismatch`), never as a damaged one to rebuild over.
    #[test]
    fn a_v1_manifest_on_disk_is_refused() {
        #[derive(Serialize)]
        struct PreSweepCard {
            #[serde(with = "serde_bytes")]
            addressbook_id: [u8; 32],
            #[serde(with = "serde_bytes")]
            uid_hash: [u8; 32],
            etag: String,
            modseq: u64,
            ciphertext_size: u32,
        }
        #[derive(Serialize)]
        struct PreSweepManifest {
            format_version: u16,
            kind_manifest: KindManifest,
            addressbooks: Vec<AddressbookState>,
            cards: Vec<PreSweepCard>,
            tombstones: Vec<CardTombstoneRef>,
        }
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xa1u8; 32];
        let path = card_placement_manifest_path(tmp.path(), &actor);
        let m1 = PreSweepManifest {
            format_version: 1,
            kind_manifest: KindManifest::empty(),
            addressbooks: vec![],
            cards: vec![PreSweepCard {
                addressbook_id: [0x42; 32],
                uid_hash: [0x55; 32],
                etag: "\"42\"".to_string(),
                modseq: 42,
                ciphertext_size: 512,
            }],
            tombstones: vec![],
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, fauna_cbor::encode_canonical(&m1).unwrap()).unwrap();

        let err = CardPlacementManifest::load(&path).expect_err("a v1 manifest is refused");
        assert!(
            matches!(err, SegmentStoreError::SchemaMismatch(_)),
            "expected SchemaMismatch, got {err:?}"
        );
    }

    #[test]
    fn load_missing_returns_ok_none() {
        let tmp = TempDir::new().expect("tmp");
        let path = tmp.path().join("does-not-exist.card-placement");
        let loaded = CardPlacementManifest::load(&path).expect("load");
        assert!(loaded.is_none());
    }

    #[test]
    fn load_wrong_format_version_returns_schema_mismatch() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xcdu8; 32];
        let path = card_placement_manifest_path(tmp.path(), &actor);
        let mut m = CardPlacementManifest::new();
        m.format_version = 999;
        m.save_atomic(&path).expect("save");
        let err = CardPlacementManifest::load(&path).expect_err("schema mismatch");
        assert!(
            matches!(err, SegmentStoreError::SchemaMismatch(_)),
            "expected SchemaMismatch, got {err:?}"
        );
    }

    /// The three record variants exercised nowhere else round-trip through
    /// the actual `CardPlacementRecord::decode` (not just `decode_strict`
    /// called directly, as the canonical-dagcbor test does for `PutCard`).
    #[test]
    fn remaining_record_variants_round_trip() {
        let update = CardPlacementRecord::UpdateAddressbookMetadata {
            addressbook_id: [0x01; 32],
            encrypted_metadata: vec![9, 8, 7],
            modseq: 3,
        };
        let bytes = update.encode().expect("encode UpdateAddressbookMetadata");
        assert_eq!(CardPlacementRecord::decode(&bytes).expect("decode"), update);

        let delete_book = CardPlacementRecord::DeleteAddressbook {
            addressbook_id: [0x02; 32],
        };
        let bytes = delete_book.encode().expect("encode DeleteAddressbook");
        assert_eq!(
            CardPlacementRecord::decode(&bytes).expect("decode"),
            delete_book
        );

        let delete_card = CardPlacementRecord::DeleteCard {
            addressbook_id: [0x03; 32],
            uid_hash: [0x04; 32],
            modseq: 5,
            card_id: [0x05; 32],
            deleted_at: 1_752_000_000,
        };
        let bytes = delete_card.encode().expect("encode DeleteCard");
        assert_eq!(
            CardPlacementRecord::decode(&bytes).expect("decode"),
            delete_card
        );
    }

    /// `decode_any_version`'s two distinct failure paths are each reachable
    /// on their own: genuinely-undecodable bytes must return `Encoding`, never `SchemaMismatch` — the inverse of
    /// `load_wrong_format_version_returns_schema_mismatch`, which covers a
    /// *structurally valid* v2 payload with an unrecognized version number.
    #[test]
    fn decode_any_version_on_undecodable_bytes_returns_encoding_not_schema_mismatch() {
        let err = CardPlacementManifest::decode_any_version(b"not cbor, not a manifest at all")
            .expect_err("garbage must not decode");
        assert!(
            matches!(err, SegmentStoreError::Encoding(_)),
            "expected Encoding, got {err:?}"
        );
    }
}
