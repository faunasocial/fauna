//! File-versions WS-RPC payload types — the version-history surface backing the
//! `file-version-history` client component (`docs/goal/behavior/file-sync.md`
//! § File Versions, ratified 2026-07-09). Version history is a **projection over
//! the append-only `sync_changes` table**: every recorded change IS a version,
//! so history is retroactive and a version's chunks stay GC-pinned.
//!
//! Two kinds:
//!
//! - `fauna.files.versions.list` — the version history of one synced file
//!   (by `path_hash`, optionally scoped to one `folder`), oldest→newest.
//! - `fauna.files.versions.get` — one version's metadata by
//!   `(path_hash, version_num)`; missing → `fauna.files.not_found`. Metadata
//!   only: the version's content is reconstructed client-side from
//!   `manifest_hash` via the manifest/chunk blob routes.
//!
//! Both ride the **bearer** connection and are **readable-folder scoped**: the
//! handler resolves the caller's readable sets
//! (`folder_authz::enumerate_readable_folders`, the `fauna.media.list`
//! boundary) and reads only their rows — a `path_hash` in a set the caller
//! cannot read is invisible (list: absent; get: `not_found`).
//!
//! `version_num` is the recording `sync_changes` row's **`seq`** — stable and
//! unique, never renumbered (a dense ordinal would shift under an M2 supersede
//! of a middle row); display ordinals are derived client-side from list
//! position. Restore semantics (re-point via an ordinary
//! `fauna.sync.changes.record` `modify`) are owned by file-sync.md § Restore —
//! there is no separate restore kind.
//!
//! Wire convention (matching `account.rs` / `bridge_routing.rs`): hashes ride as
//! raw bytes ([`ByteBuf`], the dag-cbor-native `actor_id` convention); every
//! numeric field avoids floats (dag-cbor); additive optionals use
//! `#[serde(default, skip_serializing_if)]` (the `sync.rs` convention);
//! `#[serde(flatten, default)] extra` carries forward-compat fields.
//!
//! Kind registry: `kind.rs::register_files_versions_kinds`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::{ByteBuf, Value};

// ── fauna.files.versions.list (≡ GET …/versions) ────────────────────────────

/// List the version history of one synced file.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FilesVersionsListRequest {
    /// 32-byte BLAKE3 hash of the normalized folder-relative path
    /// (file-sync.md § Path hashing).
    pub path_hash: ByteBuf,
    /// Folder name to scope the history to. `None` (an unscoped request) unions across all the caller's readable sets
    /// containing this `path_hash` — a bare `path_hash` is set-relative and can
    /// collide across sets, so scoped is the normal client call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// `Some(true)` = the **recovery browse** (`file-versions.md` § Retention
    /// (3)): include soft-pruned versions — rows inside their 30-day
    /// `purge_after` window, recoverable via `fauna.files.versions.undelete` —
    /// each marked by [`FileVersionInfo::pruned`]. Absent/`Some(false)` = the
    /// default projection, which excludes them. Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_pruned: Option<bool>,
    /// Hash-first addressing (S5b) — see `crate::folders::FolderUpdateRequest::name_hash`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_hash: Option<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One file version's metadata (identical for `list` items and the `get`
/// reply).
///
/// `Default` is derived so fixtures can be written struct-update style
/// (`FileVersionInfo { version_num: 4, ..Default::default() }`). This type grows
/// additively (it gained `folder` + `content_key_version` in the 2026-07-09
/// projection landing), and hand-listing every field in each fixture is what makes
/// two branches collide on the grown axis.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FileVersionInfo {
    pub path_hash: ByteBuf,
    /// The recording `sync_changes` row's `seq` — stable, unique, never
    /// renumbered. Display ordinals come from list position.
    pub version_num: i64,
    /// 32-byte manifest hash; the client reconstructs the version's content from
    /// this via the manifest/chunk blob routes.
    pub manifest_hash: ByteBuf,
    pub size_bytes: i64,
    pub created_at: i64,
    /// Name of the folder this version belongs to (useful when the request
    /// unioned across sets). Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// The same set's address — its `name_hash` (32 bytes). The carrier a
    /// reader matches the version's set by: a sealed set's name rests blank
    /// on the nest (`path-sealing.md` § the set-name plane). Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_hash: Option<ByteBuf>,
    /// The M2 content-key **generation** this version's chunks were sealed under
    /// (see [`crate::sync::SyncChange::content_key_version`]). `None` for
    /// owner-only sets. A restore record must carry this
    /// verbatim so readers select `key_for(version)` (file-sync.md § Restore).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_key_version: Option<u64>,
    /// Hex actor id of the authenticated actor that recorded this version —
    /// **nest-stamped**, never client-asserted (multi-writer Phase 1
    /// attribution; see [`crate::sync::SyncChange::author_actor_id`]). The
    /// version-history UI renders "who wrote this" from it. Always present:
    /// every version row has a recorder.
    pub author_actor_id: String,
    /// The author's nest-resolved handle; absent when unknown (remote / unset)
    /// — the display folds handle + id via `account_display_label` at the
    /// client transcribe (the `owner_handle`/`shared_by_handle` precedent),
    /// never a per-app chooser. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_handle: Option<String>,
    /// `Some(true)` = this version is **soft-pruned** (`file-versions.md`
    /// § Retention (3)): out of the default projection, recoverable via
    /// `fauna.files.versions.undelete` until [`Self::purge_after`]. Present
    /// only on an `include_pruned` listing. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pruned: Option<bool>,
    /// Epoch seconds after which a soft-pruned version's purge may run (the
    /// 30-day recovery deadline the recovery browse renders). Present only with
    /// [`Self::pruned`]. Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purge_after: Option<i64>,
    // ── The rest of the version row's writer-signed statement
    // (`mls-group-key-material.md` § M2 → *Writer-signed change records*,
    // ruling (2)), so a restore verifies the version it re-points through
    // [`Self::as_change_row`]. All wire-additive. ──
    /// The recording sync device id (32 bytes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<ByteBuf>,
    /// The version row's `change_type`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_type: Option<String>,
    /// The version row's sealed path label (see
    /// [`crate::sync::SyncChange::path_sealed`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_sealed: Option<ByteBuf>,
    /// Hex thumbnail-blob hash the recorder stamped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_through: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_resolution: Option<bool>,
    /// `Some(true)` = this version is a conflict report's retained loser
    /// (`sync::SyncChange::is_retention`). A covered field of its reporter's
    /// statement (writer-signed change records ruling (10)(d)), so it rides
    /// into [`Self::as_change_row`]; the reader judges such a version by its
    /// signature like any other (ruling (10)(e)). Wire-additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_retention: Option<bool>,
    /// The writer's signature over the version row's `SignedChange` (64 bytes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<ByteBuf>,
    /// The key [`Self::signature`] verifies under (32 bytes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signer_key: Option<ByteBuf>,
    /// **Reader-stamped, never on the wire** — see
    /// [`crate::media::MediaItem::signed_as_current`]: `true` only once the
    /// shared judge verified this version's row as signed under the reading
    /// account's current identity. A restore of any other version must open
    /// its bytes first, never re-sign it unopened (ruling (8)(d)).
    #[serde(skip)]
    pub signed_as_current: bool,
    /// **Reader-stamped, never on the wire** — see
    /// [`crate::media::MediaItem::signed_as`]: the identity the shared judge
    /// verified this version's row as signed as.
    #[serde(skip)]
    pub signed_as: Option<[u8; 32]>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

impl FileVersionInfo {
    /// The version's change row, rebuilt for the one reader-side verifier
    /// ([`crate::sync_writer_sig::verify_row`]). `None` when the reply carries
    /// no statement (a device-less version row).
    pub fn as_change_row(&self) -> Option<crate::sync::SyncChange> {
        Some(crate::sync::SyncChange {
            seq: self.version_num,
            path_hash: hex::encode(&self.path_hash[..]),
            manifest_hash: Some(hex::encode(&self.manifest_hash[..])),
            size_bytes: self.size_bytes,
            change_type: self.change_type.clone()?,
            created_at: self.created_at,
            device_id: Some(hex::encode(&self.device_id.as_ref()?[..])),
            content_key_version: self.content_key_version,
            thumbnail_hash: self.thumbnail_hash.clone(),
            author_actor_id: Some(self.author_actor_id.clone()),
            path_sealed: self.path_sealed.clone(),
            derived_through: self.derived_through,
            is_resolution: self.is_resolution,
            is_retention: self.is_retention,
            signature: self.signature.clone(),
            signer_key: self.signer_key.clone(),
            ..Default::default()
        })
    }
}

/// The list reply — every version of the file, oldest→newest (the twin's
/// `ORDER BY version_num`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FilesVersionsListReply {
    pub versions: Vec<FileVersionInfo>,
    /// The `signer_certs` side table for the listed versions, exactly as
    /// [`crate::sync::SyncChangesListReply::signer_certs`]. Wire-additive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signer_certs: Vec<fauna_core::encoding::EmbedAsBytes>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.files.versions.get (≡ GET …/versions/{version_num}) ───────────────

/// Fetch one version's metadata by `path_hash` + `version_num`. The reply is a
/// bare [`FileVersionInfo`]; a missing version maps to `fauna.files.not_found`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FilesVersionsGetRequest {
    pub path_hash: ByteBuf,
    pub version_num: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ── fauna.files.versions.undelete ────────────────────────────────────────────

/// Restore a **soft-pruned** version to the listable population
/// (`file-versions.md` § Retention (3) — the Layer-3 recovery verb, the
/// `fauna.filesync.snapshot.undelete` twin on the version plane). Owner-scoped:
/// only the set owner may undo their own retention pipeline. A version that is
/// not soft-pruned (never pruned, or already purged) maps to
/// `fauna.files.not_found` — a purged row's chunks may already be reclaimed, so
/// reviving it would resurrect a version whose content is gone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FilesVersionsUndeleteRequest {
    pub path_hash: ByteBuf,
    /// The recording `sync_changes` row's `seq`, as listed by an
    /// `include_pruned` recovery browse.
    pub version_num: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FilesVersionsUndeleteReply {
    pub undeleted: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::test_support::assert_round_trips;
    use crate::codec::{decode_strict as decode, encode_canonical};

    fn info(version_num: i64) -> FileVersionInfo {
        FileVersionInfo {
            path_hash: ByteBuf::from(vec![0xab; 32]),
            version_num,
            manifest_hash: ByteBuf::from(vec![0xcd; 32]),
            size_bytes: 4096,
            created_at: 1_700_000_000,
            folder: Some("documents".into()),
            content_key_version: Some(3),
            ..Default::default()
        }
    }

    #[test]
    fn list_request_and_reply_round_trip() {
        assert_round_trips(&FilesVersionsListRequest {
            path_hash: ByteBuf::from(vec![0x11; 32]),
            folder: Some("documents".into()),
            include_pruned: None,
            ..Default::default()
        });
        // Compat: a request without the additive folder field still decodes.
        assert_round_trips(&FilesVersionsListRequest {
            path_hash: ByteBuf::from(vec![0x11; 32]),
            folder: None,
            include_pruned: None,
            ..Default::default()
        });
        assert_round_trips(&FilesVersionsListReply {
            versions: vec![info(1), info(2), info(3)],
            signer_certs: Vec::new(),
            extra: BTreeMap::new(),
        });
        // Empty history round-trips to an empty Vec.
        let empty = FilesVersionsListReply {
            versions: vec![],
            signer_certs: Vec::new(),
            extra: BTreeMap::new(),
        };
        let decoded: FilesVersionsListReply = decode(&encode_canonical(&empty).unwrap()).unwrap();
        assert!(decoded.versions.is_empty());
    }

    #[test]
    fn get_request_and_info_reply_round_trip() {
        assert_round_trips(&FilesVersionsGetRequest {
            path_hash: ByteBuf::from(vec![0x22; 32]),
            version_num: 7,
            extra: BTreeMap::new(),
        });
        assert_round_trips(&info(7));
        // Compat: the pre-projection info shape (no folder /
        // content_key_version on the wire) still decodes.
        assert_round_trips(&FileVersionInfo {
            folder: None,
            content_key_version: None,
            ..info(7)
        });
    }

    /// Ruling (10)(e): the projection carries `is_retention` (additive) into
    /// the change row a reader judges — the flag is a covered field, so the
    /// row must be rebuilt with it or a signed retention version cannot
    /// verify.
    #[test]
    fn the_retention_flag_rides_the_projection_into_the_change_row() {
        let retained = FileVersionInfo {
            device_id: Some(ByteBuf::from(vec![0x3a; 32])),
            change_type: Some("modify".into()),
            is_retention: Some(true),
            ..info(5)
        };
        assert_round_trips(&retained);
        assert_eq!(retained.as_change_row().unwrap().is_retention, Some(true));
        let ordinary = FileVersionInfo {
            is_retention: None,
            ..retained
        };
        assert_eq!(ordinary.as_change_row().unwrap().is_retention, None);
    }

    #[test]
    fn recovery_browse_shapes_round_trip() {
        // The include_pruned request flag and the pruned/purge_after markers
        // are additive: absent on old wire, round-trip when present.
        assert_round_trips(&FilesVersionsListRequest {
            path_hash: ByteBuf::from(vec![0x33; 32]),
            folder: Some("documents".into()),
            include_pruned: Some(true),
            ..Default::default()
        });
        assert_round_trips(&FileVersionInfo {
            pruned: Some(true),
            purge_after: Some(1_702_600_000),
            ..info(9)
        });
        assert_round_trips(&FilesVersionsUndeleteRequest {
            path_hash: ByteBuf::from(vec![0x44; 32]),
            version_num: 9,
            extra: BTreeMap::new(),
        });
        assert_round_trips(&FilesVersionsUndeleteReply {
            undeleted: true,
            extra: BTreeMap::new(),
        });
    }
}
