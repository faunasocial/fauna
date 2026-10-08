//! Mail placement-journal records + manifest.
//!
//! Append-only event stream for IMAP placement state changes. Consumed by
//! the `MailPlacementSegmentManager` in nest. Records ride one reserved
//! folder (`__mail-placement/<actor>/`); manifest is the compacted
//! current placement state, rebuildable from segments.
//!
//! Design tracked internally: § D2 (record schemas), § D3 (manifest
//! shape), § D6 (ε) (high-cadence flush).
//!
//! ## Format v2 (IMAP tombstone pruning)
//!
//! `Expunge` and `Move` carry `deleted_at: i64` (server delete time, epoch
//! seconds) and `TombstoneRef` carries `deleted_at: i64` — what makes a
//! retention prune possible at all, mirroring the CalDAV/CardDAV siblings'
//! S6.8d2/S6.9 bump (`fauna_calendar::segments::placement`).
//!
//! The frozen v1 shapes this bump read through — the manifest upgrade in
//! `load`, the record fallback in the nest's journal replay, and the boot heal
//! that back-filled the fields v1 never recorded — were retired by the
//! compat-remnant sweep (2026-09-24,
//! `docs/goal/architecture/version-compatibility.md` § Dimension 2, program
//! 4): no pre-sweep manifest or journal exists anywhere. A journal frame is
//! now decoded strictly as [`MailPlacementRecord`], and
//! [`MailPlacementManifest::load`] refuses any other `format_version`.
//!
//! The nest's corruption-recovery rebuild replays raw journal segments through
//! [`MailPlacementRecord::decode`] (mail was the one placement kind with no
//! rebuild path until 2026-08-23 — an undecodable `manifest.mail-placement`
//! then failed every append, read and prune for that actor, fixable only
//! off-box).

use std::path::{Path, PathBuf};

use fauna_segment_store::{KindManifest, SegmentStoreError, VersionedManifest, codec};
use serde::{Deserialize, Serialize};

pub const MAIL_PLACEMENT_FORMAT_VERSION: u16 = 2;

/// One placement event. Keyed by `(mailbox, modseq)` for ordering.
///
/// Records are append-only inside immutable segments. Updates (e.g., a
/// `StoreFlags` superseded by a later `StoreFlags`) are captured as
/// successive records; compaction collapses superseded events.
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `ladder` ground: a new variant raises the format
/// version the reader checks before it decodes, so there is no unknown arm). A
/// new variant is an edit to `tools/check-additive-evolution/enum_ledger.txt`,
/// made in the same change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MailPlacementRecord {
    Create {
        mailbox: String,
        uid_validity: u32,
        attrs: Vec<String>,
    },
    Delete {
        mailbox: String,
    },
    Rename {
        old: String,
        new: String,
    },
    Append {
        mailbox: String,
        uid: u32,
        modseq: u64,
        flags: Vec<String>,
        #[serde(with = "serde_bytes")]
        content_record_id: Vec<u8>,
        internal_date: i64,
    },
    StoreFlags {
        mailbox: String,
        uid_set: Vec<u32>,
        modseq: u64,
        before_flags: Vec<String>,
        after_flags: Vec<String>,
    },
    Move {
        src_mailbox: String,
        src_uid_set: Vec<u32>,
        dst_mailbox: String,
        dst_uid_set: Vec<u32>,
        modseq_src: u64,
        modseq_dst: u64,
        /// Server move time, epoch seconds — stamped on the source UIDs'
        /// tombstones. Carried in the record (not read from the clock at
        /// fold time) so a journal replay reproduces the same stamp.
        deleted_at: i64,
    },
    Copy {
        src_mailbox: String,
        src_uid_set: Vec<u32>,
        dst_mailbox: String,
        dst_uid_set: Vec<u32>,
        modseq_dst: u64,
    },
    Expunge {
        mailbox: String,
        uid_set: Vec<u32>,
        modseq: u64,
        /// Server delete time, epoch seconds. What makes the tombstone
        /// retention prune possible.
        deleted_at: i64,
    },
    Subscribe {
        mailbox: String,
    },
    Unsubscribe {
        mailbox: String,
    },
}

impl MailPlacementRecord {
    pub fn encode(&self) -> Result<Vec<u8>, SegmentStoreError> {
        codec::encode(self)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, SegmentStoreError> {
        codec::decode(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailboxState {
    pub name: String,
    pub uid_validity: u32,
    pub uid_next: u32,
    pub highestmodseq: u64,
    pub attrs: Vec<String>,
    /// The prune floor: the highest modseq any tombstone the retention prune
    /// dropped from this mailbox carried (0 = none ever pruned). Below it the
    /// manifest's tombstone list is incomplete, so a replica rebuilt from it
    /// must not enumerate expunges since an older modseq — QRESYNC falls back
    /// to `OK [HIGHESTMODSEQ]` (`imap-server.md` § QRESYNC). Omitted at rest
    /// while 0, so a never-pruned manifest encodes as before.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pruned_modseq: u64,
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordPlacement {
    pub mailbox: String,
    pub uid: u32,
    pub modseq: u64,
    pub flags: Vec<String>,
    #[serde(with = "serde_bytes")]
    pub content_record_id: Vec<u8>,
    pub internal_date: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TombstoneRef {
    pub mailbox: String,
    pub uid: u32,
    pub modseq: u64,
    /// Server delete time (epoch seconds), from the `Expunge` or `Move`
    /// record that produced the tombstone. The retention prune ages it out.
    pub deleted_at: i64,
}

/// Compacted current placement state. Rebuildable from segments — the nest's
/// `rebuild_mail_placement_manifest_from_segments` is that rebuild.
/// Persisted as `manifest.mail-placement` via atomic-rename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailPlacementManifest {
    pub format_version: u16,
    pub kind_manifest: KindManifest,
    pub mailboxes: Vec<MailboxState>,
    pub placements: Vec<RecordPlacement>,
    pub tombstones: Vec<TombstoneRef>,
    pub subscriptions: Vec<String>,
}

impl MailPlacementManifest {
    pub fn new() -> Self {
        Self {
            format_version: MAIL_PLACEMENT_FORMAT_VERSION,
            kind_manifest: KindManifest::empty(),
            mailboxes: Vec::new(),
            placements: Vec::new(),
            tombstones: Vec::new(),
            subscriptions: Vec::new(),
        }
    }
}

impl VersionedManifest for MailPlacementManifest {
    const CURRENT_VERSION: u16 = MAIL_PLACEMENT_FORMAT_VERSION;
    const LABEL: &'static str = "mail-placement";

    fn format_version(&self) -> u16 {
        self.format_version
    }
}

impl Default for MailPlacementManifest {
    fn default() -> Self {
        Self::new()
    }
}

/// On-disk root for an actor's mail-placement segments.
/// `<data_dir>/segments/__mail-placement/<actor_id_hex>/`
pub fn mail_placement_segments_root(data_dir: &Path, actor_id: &[u8; 32]) -> PathBuf {
    data_dir
        .join("segments")
        .join("__mail-placement")
        .join(fauna_core::hex32::encode(actor_id))
}

/// On-disk path to the mail-placement manifest.
/// `<data_dir>/segments/__mail-placement/<actor_id_hex>/manifest.mail-placement`
pub fn mail_placement_manifest_path(data_dir: &Path, actor_id: &[u8; 32]) -> PathBuf {
    mail_placement_segments_root(data_dir, actor_id).join("manifest.mail-placement")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Discriminating red→green: the PRODUCTION mail-placement record +
    /// manifest serializers must emit canonical dag-cbor, not serde_bare
    /// (`serialization.md:29` — at-rest CARv2 metadata is one canonical
    /// encoder). serde_bare bytes fail strict decode; dag-cbor passes.
    #[test]
    fn placement_record_and_manifest_at_rest_are_canonical_dagcbor() {
        let record = MailPlacementRecord::Append {
            mailbox: "INBOX".to_string(),
            uid: 1,
            modseq: 1,
            flags: vec!["\\Seen".to_string()],
            content_record_id: vec![1, 2, 3, 4],
            internal_date: 1_715_000_000,
        };
        let rec_bytes = record.encode().expect("encode record");
        fauna_cbor::decode_strict::<MailPlacementRecord>(&rec_bytes)
            .expect("placement record bytes must be canonical dag-cbor");

        let mut m = MailPlacementManifest::new();
        m.subscriptions.push("INBOX".to_string());
        let man_bytes = m.encode().expect("encode manifest");
        fauna_cbor::decode_strict::<MailPlacementManifest>(&man_bytes)
            .expect("placement manifest bytes must be canonical dag-cbor");
    }

    #[test]
    fn save_then_load_round_trips() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xabu8; 32];
        let path = mail_placement_manifest_path(tmp.path(), &actor);
        let mut m = MailPlacementManifest::new();
        m.subscriptions.push("INBOX".to_string());
        m.tombstones.push(TombstoneRef {
            mailbox: "INBOX".to_string(),
            uid: 7,
            modseq: 42,
            deleted_at: 1_752_000_000,
        });
        m.save_atomic(&path).expect("save");
        let loaded = MailPlacementManifest::load(&path)
            .expect("load")
            .expect("Some");
        assert_eq!(loaded, m);
    }

    #[test]
    fn load_missing_returns_ok_none() {
        let tmp = TempDir::new().expect("tmp");
        let path = tmp.path().join("does-not-exist.mail-placement");
        let loaded = MailPlacementManifest::load(&path).expect("load");
        assert!(loaded.is_none());
    }

    #[test]
    fn load_wrong_format_version_returns_schema_mismatch() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xcdu8; 32];
        let path = mail_placement_manifest_path(tmp.path(), &actor);
        let mut m = MailPlacementManifest::new();
        m.format_version = 999;
        m.save_atomic(&path).expect("save");
        let err = MailPlacementManifest::load(&path).expect_err("schema mismatch");
        assert!(
            matches!(err, SegmentStoreError::SchemaMismatch(_)),
            "expected SchemaMismatch, got {err:?}"
        );
    }

    /// A v1-stamped manifest on disk is refused, not upgraded. The pre-sweep
    /// v1 read arm (the in-place upgrade whose tombstones came back with
    /// `deleted_at: None`) was retired by the compat-remnant sweep (program
    /// 4, 2026-09-24): no v1 manifest exists anywhere. The blob decodes as
    /// the current struct, so this pins the version-field check.
    #[test]
    fn a_v1_manifest_on_disk_is_refused() {
        let tmp = TempDir::new().expect("tmp");
        let actor = [0xa1u8; 32];
        let path = mail_placement_manifest_path(tmp.path(), &actor);
        let mut m1 = MailPlacementManifest::new();
        m1.format_version = 1;
        m1.subscriptions.push("INBOX".to_string());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, fauna_cbor::encode_canonical(&m1).unwrap()).unwrap();

        let err = MailPlacementManifest::load(&path).expect_err("a v1 manifest is refused");
        assert!(
            matches!(err, SegmentStoreError::SchemaMismatch(_)),
            "expected SchemaMismatch, got {err:?}"
        );
    }

    /// `decode_any_version`'s undecodable-bytes path must return `Encoding`,
    /// never `SchemaMismatch` — the inverse of
    /// `load_wrong_format_version_returns_schema_mismatch`, which covers a
    /// *structurally valid* v2 payload with an unrecognized version number.
    #[test]
    fn decode_any_version_on_undecodable_bytes_returns_encoding_not_schema_mismatch() {
        let err = MailPlacementManifest::decode_any_version(b"not cbor, not a manifest at all")
            .expect_err("garbage must not decode as either version");
        assert!(
            matches!(err, SegmentStoreError::Encoding(_)),
            "expected Encoding, got {err:?}"
        );
    }
}
