//! Snapshot diff: compare two snapshots to find added/removed/modified files.

use crate::db::CacheDb;
use anyhow::{Context, Result};
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct DiffEntry {
    pub path: String,
    pub size_bytes: i64,
    /// The join key this entry was matched on — carried onto the wire so a
    /// sealed-first reader can address the entry without the plaintext path
    /// (path-sealing S2, `docs/goal/behavior/file-sync.md` § Sealed names &
    /// paths).
    pub path_hash: Vec<u8>,
    /// The row's sealed label over `path`, opaque to the nest and copied
    /// verbatim from the snapshot row.
    pub path_sealed: Option<Vec<u8>>,
}

#[derive(Debug, Serialize)]
pub struct ModifiedEntry {
    pub path: String,
    pub old_size: i64,
    pub new_size: i64,
    /// See [`DiffEntry::path_hash`].
    pub path_hash: Vec<u8>,
    /// See [`DiffEntry::path_sealed`].
    pub path_sealed: Option<Vec<u8>>,
}

#[derive(Debug, Serialize)]
pub struct DiffSummary {
    pub added_count: usize,
    pub removed_count: usize,
    pub modified_count: usize,
    pub added_bytes: i64,
    pub removed_bytes: i64,
    pub net_bytes: i64,
}

#[derive(Debug, Serialize)]
pub struct DiffResult {
    pub snapshot_a: i64,
    pub snapshot_b: i64,
    pub added: Vec<DiffEntry>,
    pub removed: Vec<DiffEntry>,
    pub modified: Vec<ModifiedEntry>,
    pub summary: DiffSummary,
    /// The folder both snapshots belong to — verified equal above, so there
    /// is exactly one. Carried into the reply so the client can resolve label
    /// custody for the sealed-path render (`docs/goal/behavior/file-sync.md`
    /// § Sealed names & paths). Always named: a set row gone between the
    /// snapshot read and the name lookup refuses the whole diff (not found)
    /// rather than shipping a nameless reply — the wire field is required.
    pub folder: String,
    /// [`Self::folder`], sealed — see
    /// [`fauna_protocol::folders::FolderSummary::name_sealed`]. Opaque to
    /// the nest; rendered client-side by
    /// `fauna_core::label_custody::render_set_name`. Path-sealing S5c-2.
    pub folder_sealed: Option<Vec<u8>>,
    /// The convergent salt [`Self::folder_sealed`] opens under
    /// (`fauna_core::path_crypto::set_name_hash`) — ships as a pair with the
    /// seal or not at all.
    pub folder_hash: Option<Vec<u8>>,
}

/// Compute the diff between two snapshots.
/// Both must belong to the same folder.
pub async fn snapshot_diff(
    db: &Arc<CacheDb>,
    snapshot_a: i64,
    snapshot_b: i64,
) -> Result<DiffResult> {
    // Verify both snapshots exist and belong to the same folder
    let snap_a = db
        .get_snapshot(snapshot_a)
        .await?
        .context("snapshot A not found")?;
    let snap_b = db
        .get_snapshot(snapshot_b)
        .await?
        .context("snapshot B not found")?;
    if snap_a.folder_id != snap_b.folder_id {
        anyhow::bail!("snapshots belong to different folders");
    }

    let files_a = db.get_snapshot_files(snapshot_a).await?;
    let files_b = db.get_snapshot_files(snapshot_b).await?;

    // Keyed on `path_hash` — the table's PK component since the S9 flip
    // rebuilt `snapshot_files` and dropped the plaintext column (v32,
    // `docs/goal/behavior/file-sync.md` § Sealed names & paths § Migration).
    // The wire entries carry the empty-string scrub sentinel as `path` (the
    // render seams read it as "scrubbed" and degrade to `Omit`) plus the
    // hash + sealed pair a keyed reader renders from.
    fn join_key(f: &crate::db::SnapshotFileRow) -> Vec<u8> {
        f.path_hash.clone()
    }
    // The map values are the rows themselves, so an entry carries its sealed
    // label out to the wire alongside the plaintext it still has this major.
    let map_a: std::collections::HashMap<Vec<u8>, &crate::db::SnapshotFileRow> =
        files_a.iter().map(|f| (join_key(f), f)).collect();
    let map_b: std::collections::HashMap<Vec<u8>, &crate::db::SnapshotFileRow> =
        files_b.iter().map(|f| (join_key(f), f)).collect();

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut modified = Vec::new();

    // Files in B not in A → added; in both with different hash → modified
    for (key, b) in &map_b {
        match map_a.get(key) {
            None => added.push(DiffEntry {
                path: String::new(),
                size_bytes: b.size_bytes,
                path_hash: key.clone(),
                path_sealed: b.path_sealed.clone(),
            }),
            Some(a) => {
                if a.manifest_hash != b.manifest_hash {
                    modified.push(ModifiedEntry {
                        path: String::new(),
                        old_size: a.size_bytes,
                        new_size: b.size_bytes,
                        path_hash: key.clone(),
                        // The B-side seal: `modified` reports the newer row.
                        path_sealed: b.path_sealed.clone(),
                    });
                }
            }
        }
    }

    // Files in A not in B → removed
    for (key, a) in &map_a {
        if !map_b.contains_key(key) {
            removed.push(DiffEntry {
                path: String::new(),
                size_bytes: a.size_bytes,
                path_hash: key.clone(),
                path_sealed: a.path_sealed.clone(),
            });
        }
    }

    let added_bytes: i64 = added.iter().map(|e| e.size_bytes).sum();
    let removed_bytes: i64 = removed.iter().map(|e| e.size_bytes).sum();
    let summary = DiffSummary {
        added_count: added.len(),
        removed_count: removed.len(),
        modified_count: modified.len(),
        added_bytes,
        removed_bytes,
        net_bytes: added_bytes - removed_bytes,
    };

    // Both snapshots share this id (verified above), so one lookup names the
    // set and carries its seal + salt pair (path-sealing S5c-2). A set row
    // gone since the snapshot read (a delete racing this diff) is a
    // not-found, the same answer a snapshot of a deleted set gets — never a
    // reply without its set name.
    let fs = db
        .get_folder_by_id(snap_a.folder_id)
        .await?
        .context("snapshot folder not found")?;
    let folder = fs.name;
    let folder_sealed = fs.name_sealed;
    let folder_hash = fs.name_hash;

    Ok(DiffResult {
        snapshot_a,
        snapshot_b,
        added,
        removed,
        modified,
        summary,
        folder,
        folder_sealed,
        folder_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn diff_detects_added_removed_modified() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let actor_id = [1u8; 32];
        db.create_folder("diff-test", &actor_id).await.unwrap();
        let fs = db.get_folder("diff-test").await.unwrap().unwrap();

        // Snapshot A: file1.txt, file2.txt
        let ph1: [u8; 32] = *blake3::hash(b"file1.txt").as_bytes();
        let ph2: [u8; 32] = *blake3::hash(b"file2.txt").as_bytes();
        db.record_sync_change(
            &actor_id,
            &ph1,
            Some(&[0xAAu8; 32]),
            100,
            "create",
            Some(fs.id),
            None,
            Some("file1.txt"),
        )
        .await
        .unwrap();
        db.record_sync_change(
            &actor_id,
            &ph2,
            Some(&[0xBBu8; 32]),
            200,
            "create",
            Some(fs.id),
            None,
            Some("file2.txt"),
        )
        .await
        .unwrap();
        let snap_a = db
            .create_snapshot_v2(fs.id, None, &[], None, None)
            .await
            .unwrap();

        // Modify file1, delete file2, add file3
        let ph3: [u8; 32] = *blake3::hash(b"file3.txt").as_bytes();
        db.record_sync_change(
            &actor_id,
            &ph1,
            Some(&[0xCCu8; 32]),
            150,
            "create",
            Some(fs.id),
            None,
            Some("file1.txt"),
        )
        .await
        .unwrap();
        db.record_sync_change(
            &actor_id,
            &ph2,
            None,
            0,
            "delete",
            Some(fs.id),
            None,
            Some("file2.txt"),
        )
        .await
        .unwrap();
        db.record_sync_change(
            &actor_id,
            &ph3,
            Some(&[0xDDu8; 32]),
            300,
            "create",
            Some(fs.id),
            None,
            Some("file3.txt"),
        )
        .await
        .unwrap();

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let snap_b = db
            .create_snapshot_v2(fs.id, None, &[], None, None)
            .await
            .unwrap();

        let result = snapshot_diff(&db, snap_a.id, snap_b.id).await.unwrap();
        assert_eq!(result.summary.added_count, 1);
        assert_eq!(result.summary.removed_count, 1);
        assert_eq!(result.summary.modified_count, 1);
        // Post-flip entries are hash-addressed; `path` carries the scrub
        // sentinel and a keyed reader renders from `path_sealed`.
        let h = |p: &str| fauna_core::sync::path_hash(p).to_vec();
        assert_eq!(result.added[0].path_hash, h("file3.txt"));
        assert_eq!(result.removed[0].path_hash, h("file2.txt"));
        assert_eq!(result.modified[0].path_hash, h("file1.txt"));
        assert_eq!(result.added[0].path, "");
    }

    #[tokio::test]
    async fn diff_rejects_different_folders() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let actor_id = [1u8; 32];
        db.create_folder("fs-a", &actor_id).await.unwrap();
        db.create_folder("fs-b", &actor_id).await.unwrap();
        let fsa = db.get_folder("fs-a").await.unwrap().unwrap();
        let fsb = db.get_folder("fs-b").await.unwrap().unwrap();

        let ph: [u8; 32] = *blake3::hash(b"f.txt").as_bytes();
        db.record_sync_change(
            &actor_id,
            &ph,
            Some(&[0xAAu8; 32]),
            100,
            "create",
            Some(fsa.id),
            None,
            Some("f.txt"),
        )
        .await
        .unwrap();
        let snap_a = db
            .create_snapshot_v2(fsa.id, None, &[], None, None)
            .await
            .unwrap();

        db.record_sync_change(
            &actor_id,
            &ph,
            Some(&[0xBBu8; 32]),
            100,
            "create",
            Some(fsb.id),
            None,
            Some("f.txt"),
        )
        .await
        .unwrap();
        let snap_b = db
            .create_snapshot_v2(fsb.id, None, &[], None, None)
            .await
            .unwrap();

        let err = snapshot_diff(&db, snap_a.id, snap_b.id).await.unwrap_err();
        assert!(err.to_string().contains("different folders"));
    }
}
