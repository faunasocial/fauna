//! Calendar placement-journal records + manifest.
//!
//! Mirror of `fauna_mail::segments::placement` for CalDAV. The kind
//! identity is `__calendar-placement` (reserved folder per
//! `docs/goal/behavior/file-sync.md` § __calendar-placement Sync).
//!
//! Record schemas (§ D2) and manifest shape (§ D3) mirror the IMAP/CalDAV
//! restore design (tracked internally).
//!
//! ## Format v2 (S6.9)
//!
//! `PutEvent` carries the content-record id (`event_id`) and the effective
//! `encrypted_fauna_ext` sidecar; `DeleteEvent` carries `event_id` +
//! `deleted_at`. Rationale (`message-segment-store.md` § Invariants, rule 2):
//! state mutated in place against a fixed record id (etag, modseq, the
//! sidecar) must live in the placement journal, and snapshot restore sources
//! it from there — and without the id, the placement→content join is
//! `(calendar_id, uid_hash)`, ambiguous across superseded record versions
//! compaction has not yet reclaimed. `deleted_at` is what makes a tombstone
//! retention prune possible at all.
//!
//! The frozen v1 shapes the bump read through — the manifest upgrade in
//! [`CalPlacementManifest::load`], the record fallback in the nest's journal
//! replay, the `Option` placement/tombstone fields that carried what v1 never
//! recorded, and the nest's boot heal that back-filled them — were retired by
//! the compat-remnant sweep (2026-09-24,
//! `docs/goal/architecture/version-compatibility.md` § Dimension 2, program
//! 4): no pre-sweep manifest or journal exists anywhere. A journal frame is
//! decoded strictly as [`CalPlacementRecord`], and every placement entry and
//! tombstone carries its content-record id (and a tombstone its delete time).

use std::path::{Path, PathBuf};

use fauna_segment_store::{KindManifest, SegmentStoreError, VersionedManifest, codec};
use serde::{Deserialize, Serialize};

pub const CAL_PLACEMENT_FORMAT_VERSION: u16 = 2;

/// One CalDAV placement event (format v2). Keyed by `(calendar_id, modseq)`.
///
/// Records are append-only inside immutable segments. Updates are
/// captured as successive records; compaction collapses superseded events.
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `ladder` ground: a new variant raises the format
/// version the reader checks before it decodes, so there is no unknown arm). A
/// new variant is an edit to `tools/check-additive-evolution/enum_ledger.txt`,
/// made in the same change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CalPlacementRecord {
    ProvisionCalendar {
        #[serde(with = "serde_bytes")]
        calendar_id: [u8; 32],
        #[serde(with = "serde_bytes")]
        encrypted_metadata: Vec<u8>,
    },
    UpdateCalendarMetadata {
        #[serde(with = "serde_bytes")]
        calendar_id: [u8; 32],
        #[serde(with = "serde_bytes")]
        encrypted_metadata: Vec<u8>,
        modseq: u64,
    },
    DeleteCalendar {
        #[serde(with = "serde_bytes")]
        calendar_id: [u8; 32],
    },
    PutEvent {
        #[serde(with = "serde_bytes")]
        calendar_id: [u8; 32],
        #[serde(with = "serde_bytes")]
        uid_hash: [u8; 32],
        etag: String,
        modseq: u64,
        ciphertext_size: u32,
        /// Content-record id — the `bridge_caldav_events` PK tail. The
        /// placement→content join for restore; mail's `content_record_id`
        /// analogue. (The record's segment CID is the content hash of its
        /// envelope, not a wrap of this id — the row's `record_cid` column
        /// carries it.)
        #[serde(with = "serde_bytes")]
        event_id: [u8; 32],
        /// The row's **effective** sealed Fauna-extension sidecar after this
        /// write — the DAO's value, not the request's (a MUA write carries
        /// `None` on the wire but *preserves* the prior row's sidecar).
        /// `None` = the row genuinely has no sidecar.
        #[serde(default, with = "serde_bytes")]
        encrypted_fauna_ext: Option<Vec<u8>>,
    },
    DeleteEvent {
        #[serde(with = "serde_bytes")]
        calendar_id: [u8; 32],
        #[serde(with = "serde_bytes")]
        uid_hash: [u8; 32],
        modseq: u64,
        /// Content-record id of the deleted row (`bridge_caldav_expunged`
        /// carries it; restore rebuilds that row from here).
        #[serde(with = "serde_bytes")]
        event_id: [u8; 32],
        /// Server delete time, epoch **seconds**. What makes the tombstone
        /// retention prune possible.
        deleted_at: i64,
    },
}

impl CalPlacementRecord {
    pub fn encode(&self) -> Result<Vec<u8>, SegmentStoreError> {
        codec::encode(self)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, SegmentStoreError> {
        codec::decode(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarState {
    #[serde(with = "serde_bytes")]
    pub calendar_id: [u8; 32],
    #[serde(with = "serde_bytes")]
    pub encrypted_metadata: Vec<u8>,
    pub highestmodseq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPlacement {
    #[serde(with = "serde_bytes")]
    pub calendar_id: [u8; 32],
    #[serde(with = "serde_bytes")]
    pub uid_hash: [u8; 32],
    pub etag: String,
    pub modseq: u64,
    pub ciphertext_size: u32,
    /// Content-record id — the placement→content join restore keys on.
    #[serde(with = "serde_bytes")]
    pub event_id: [u8; 32],
    /// Effective sealed sidecar after the last PUT; `None` = no sidecar.
    #[serde(default, with = "serde_bytes")]
    pub encrypted_fauna_ext: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventTombstoneRef {
    #[serde(with = "serde_bytes")]
    pub calendar_id: [u8; 32],
    #[serde(with = "serde_bytes")]
    pub uid_hash: [u8; 32],
    pub modseq: u64,
    /// Content-record id of the deleted row (restore rebuilds its
    /// `bridge_caldav_expunged` row from here).
    #[serde(with = "serde_bytes")]
    pub event_id: [u8; 32],
    /// Server delete time (epoch seconds) — the retention prune's clock.
    pub deleted_at: i64,
}

/// Compacted current placement state. Rebuildable from segments.
/// Persisted as `manifest.cal-placement` via atomic-rename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalPlacementManifest {
    pub format_version: u16,
    pub kind_manifest: KindManifest,
    pub calendars: Vec<CalendarState>,
    pub events: Vec<EventPlacement>,
    pub tombstones: Vec<EventTombstoneRef>,
}

impl CalPlacementManifest {
    pub fn new() -> Self {
        Self {
            format_version: CAL_PLACEMENT_FORMAT_VERSION,
            kind_manifest: KindManifest::empty(),
            calendars: Vec::new(),
            events: Vec::new(),
            tombstones: Vec::new(),
        }
    }
}

impl VersionedManifest for CalPlacementManifest {
    const CURRENT_VERSION: u16 = CAL_PLACEMENT_FORMAT_VERSION;
    const LABEL: &'static str = "cal-placement";

    fn format_version(&self) -> u16 {
        self.format_version
    }
}

impl Default for CalPlacementManifest {
    fn default() -> Self {
        Self::new()
    }
}

/// On-disk root for an actor's calendar-placement segments.
/// `<data_dir>/segments/__calendar-placement/<actor_id_hex>/`
pub fn cal_placement_segments_root(data_dir: &Path, actor_id: &[u8; 32]) -> PathBuf {
    data_dir
        .join("segments")
        .join("__calendar-placement")
        .join(fauna_core::hex32::encode(actor_id))
}

/// On-disk path to the calendar-placement manifest.
/// `<data_dir>/segments/__calendar-placement/<actor_id_hex>/manifest.cal-placement`
pub fn cal_placement_manifest_path(data_dir: &Path, actor_id: &[u8; 32]) -> PathBuf {
    cal_placement_segments_root(data_dir, actor_id).join("manifest.cal-placement")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Discriminating red→green: the PRODUCTION cal-placement record +
    /// manifest serializers must emit canonical dag-cbor, not serde_bare
    /// (`serialization.md:29` — at-rest CARv2 metadata is one canonical
    /// encoder). serde_bare bytes fail strict decode; dag-cbor passes.
    #[test]
    fn placement_record_and_manifest_at_rest_are_canonical_dagcbor() {
        let record = CalPlacementRecord::PutEvent {
            calendar_id: [0x11; 32],
            uid_hash: [0x22; 32],
            etag: "etag-1".to_string(),
            modseq: 7,
            ciphertext_size: 512,
            event_id: [0x99; 32],
            encrypted_fauna_ext: Some(vec![0xf0, 0x0d]),
        };
        let rec_bytes = record.encode().expect("encode record");
        fauna_cbor::decode_strict::<CalPlacementRecord>(&rec_bytes)
            .expect("cal placement record bytes must be canonical dag-cbor");

        let mut m = CalPlacementManifest::new();
        m.calendars.push(CalendarState {
            calendar_id: [0x42; 32],
            encrypted_metadata: vec![1, 2, 3],
            highestmodseq: 9,
        });
        let man_bytes = m.encode().expect("encode manifest");
        fauna_cbor::decode_strict::<CalPlacementManifest>(&man_bytes)
            .expect("cal placement manifest bytes must be canonical dag-cbor");
    }

    #[test]
    fn save_then_load_round_trips() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xabu8; 32];
        let path = cal_placement_manifest_path(tmp.path(), &actor);
        let mut m = CalPlacementManifest::new();
        m.calendars.push(CalendarState {
            calendar_id: [0x42; 32],
            encrypted_metadata: vec![1, 2, 3],
            highestmodseq: 42,
        });
        m.tombstones.push(EventTombstoneRef {
            calendar_id: [0x42; 32],
            uid_hash: [0x77; 32],
            modseq: 41,
            event_id: [0x88; 32],
            deleted_at: 1_752_000_000,
        });
        m.save_atomic(&path).expect("save");
        let loaded = CalPlacementManifest::load(&path)
            .expect("load")
            .expect("Some");
        assert_eq!(loaded, m);
    }

    /// A v1 manifest on disk is refused, not upgraded. The pre-sweep v1 read
    /// arm (the in-place upgrade whose entries came back with `event_id` /
    /// `deleted_at` as `None`) was retired by the compat-remnant sweep
    /// (program 4, 2026-09-24): no v1 manifest exists anywhere. A populated
    /// v1 entry lacks the now-required `event_id`, so its body does not even
    /// decode — but the stamp is read first, so it is refused as the intact
    /// file of another version it is (`SchemaMismatch`), never as a damaged
    /// one to rebuild over.
    #[test]
    fn a_v1_manifest_on_disk_is_refused() {
        #[derive(Serialize)]
        struct PreSweepEvent {
            #[serde(with = "serde_bytes")]
            calendar_id: [u8; 32],
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
            calendars: Vec<CalendarState>,
            events: Vec<PreSweepEvent>,
            tombstones: Vec<EventTombstoneRef>,
        }
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xa1u8; 32];
        let path = cal_placement_manifest_path(tmp.path(), &actor);
        let m1 = PreSweepManifest {
            format_version: 1,
            kind_manifest: KindManifest::empty(),
            calendars: vec![],
            events: vec![PreSweepEvent {
                calendar_id: [0x42; 32],
                uid_hash: [0x55; 32],
                etag: "\"42\"".to_string(),
                modseq: 42,
                ciphertext_size: 512,
            }],
            tombstones: vec![],
        };
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, fauna_cbor::encode_canonical(&m1).unwrap()).unwrap();

        let err = CalPlacementManifest::load(&path).expect_err("a v1 manifest is refused");
        assert!(
            matches!(err, SegmentStoreError::SchemaMismatch(_)),
            "expected SchemaMismatch, got {err:?}"
        );
    }

    #[test]
    fn load_missing_returns_ok_none() {
        let tmp = TempDir::new().expect("tmp");
        let path = tmp.path().join("does-not-exist.cal-placement");
        let loaded = CalPlacementManifest::load(&path).expect("load");
        assert!(loaded.is_none());
    }

    #[test]
    fn load_wrong_format_version_returns_schema_mismatch() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xcdu8; 32];
        let path = cal_placement_manifest_path(tmp.path(), &actor);
        let mut m = CalPlacementManifest::new();
        m.format_version = 999;
        m.save_atomic(&path).expect("save");
        let err = CalPlacementManifest::load(&path).expect_err("schema mismatch");
        assert!(
            matches!(err, SegmentStoreError::SchemaMismatch(_)),
            "expected SchemaMismatch, got {err:?}"
        );
    }

    /// The three record variants exercised nowhere else round-trip through
    /// the actual `CalPlacementRecord::decode` (not just `decode_strict`
    /// called directly, as the canonical-dagcbor test does for `PutEvent`).
    #[test]
    fn remaining_record_variants_round_trip() {
        let update = CalPlacementRecord::UpdateCalendarMetadata {
            calendar_id: [0x01; 32],
            encrypted_metadata: vec![9, 8, 7],
            modseq: 3,
        };
        let bytes = update.encode().expect("encode UpdateCalendarMetadata");
        assert_eq!(CalPlacementRecord::decode(&bytes).expect("decode"), update);

        let delete_cal = CalPlacementRecord::DeleteCalendar {
            calendar_id: [0x02; 32],
        };
        let bytes = delete_cal.encode().expect("encode DeleteCalendar");
        assert_eq!(
            CalPlacementRecord::decode(&bytes).expect("decode"),
            delete_cal
        );

        let delete_event = CalPlacementRecord::DeleteEvent {
            calendar_id: [0x03; 32],
            uid_hash: [0x04; 32],
            modseq: 5,
            event_id: [0x05; 32],
            deleted_at: 1_752_000_000,
        };
        let bytes = delete_event.encode().expect("encode DeleteEvent");
        assert_eq!(
            CalPlacementRecord::decode(&bytes).expect("decode"),
            delete_event
        );
    }

    /// `decode_any_version`'s two distinct failure paths are each reachable
    /// on their own: genuinely-undecodable bytes must return `Encoding`, never `SchemaMismatch` — the inverse of
    /// `load_wrong_format_version_returns_schema_mismatch`, which covers a
    /// *structurally valid* v2 payload with an unrecognized version number.
    #[test]
    fn decode_any_version_on_undecodable_bytes_returns_encoding_not_schema_mismatch() {
        let err = CalPlacementManifest::decode_any_version(b"not cbor, not a manifest at all")
            .expect_err("garbage must not decode");
        assert!(
            matches!(err, SegmentStoreError::Encoding(_)),
            "expected Encoding, got {err:?}"
        );
    }
}
