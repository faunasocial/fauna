//! Repository statistics computation.

use crate::db::CacheDb;
use anyhow::Result;
use serde::Serialize;
use std::sync::Arc;

#[derive(Debug, Serialize)]
pub struct GlobalStats {
    pub total_size_bytes: i64,
    pub total_blobs: i64,
    pub total_snapshots: i64,
    pub total_folders: i64,
    pub dedup_ratio: f64,
    pub blob_types: BlobTypeCounts,
}

#[derive(Debug, Serialize)]
pub struct BlobTypeCounts {
    pub chunk: i64,
    pub manifest: i64,
}

#[derive(Debug, Serialize)]
pub struct FolderStats {
    pub folder: String,
    pub snapshot_count: i64,
    pub latest_snapshot: Option<i64>,
    pub total_files: i64,
    pub raw_size_bytes: i64,
    pub stored_size_bytes: i64,
    pub dedup_ratio: f64,
    pub storage_backend: String,
}

pub async fn compute_global_stats(db: &Arc<CacheDb>) -> Result<GlobalStats> {
    let (total_blobs, total_size, chunk_count, manifest_count, total_snapshots, total_folders) =
        db.global_stats().await?;

    let folders = db.list_folders().await?;
    let mut total_raw: i64 = 0;
    for fs in &folders {
        // Saturating: each folder's raw size is a sum of owner-declared sizes.
        total_raw = total_raw.saturating_add(db.folder_raw_size(fs.id).await?);
    }

    let dedup_ratio = if total_size > 0 {
        total_raw as f64 / total_size as f64
    } else {
        1.0
    };

    Ok(GlobalStats {
        total_size_bytes: total_size,
        total_blobs,
        total_snapshots,
        total_folders,
        dedup_ratio,
        blob_types: BlobTypeCounts {
            chunk: chunk_count,
            manifest: manifest_count,
        },
    })
}

/// Compute a single folder's stats, **scoped to the owning `actor_id`**.
///
/// ST-1 (2026-06-27): a non-owner — or a non-existent set — yields `Ok(None)`,
/// which the caller maps to `not_found`. This is the same owner-scope idiom N1
/// gave `filesync_handlers` (`get_folder_for_actor`, the caller-scoped
/// `WHERE name = ?1 AND actor_id = ?2` lookup), never the name-only
/// `get_folder` (which returns whichever actor's row sorts first → a
/// cross-user IDOR on aggregate backup stats). Folding "not yours" into "not
/// found" also closes the folder existence oracle. `name_hash`, when present,
/// resolves first (S5b, `path-sealing.md` § the set-name plane) — the same
/// owner-scoped idiom keyed on the address that outlives the plaintext name —
/// and a carried row wins over both (`set-addressing.md` § The row form).
pub(crate) async fn compute_folder_stats(
    db: &Arc<CacheDb>,
    address: crate::folder_authz::FolderAddress<'_>,
    actor_id: &[u8; 32],
) -> Result<Option<FolderStats>> {
    let fs = crate::folder_authz::resolve_owned_folder(db, address, actor_id).await?;
    let Some(fs) = fs else {
        return Ok(None);
    };

    let snapshots = db.list_snapshots(fs.id).await?;
    let snapshot_count = snapshots.len() as i64;
    let latest_snapshot = snapshots.first().map(|s| s.created_at);
    let total_files: i64 = snapshots.first().map(|s| s.file_count).unwrap_or(0);
    let raw_size = db.folder_raw_size(fs.id).await?;

    let snap_ids: Vec<i64> = snapshots.iter().map(|s| s.id).collect();
    let manifest_hashes = db.snapshot_manifest_hashes(&snap_ids).await?;
    let mut stored_size: i64 = 0;
    for mh in &manifest_hashes {
        if let Ok(arr) = <[u8; 32]>::try_from(mh.as_slice())
            && let Ok(Some(meta)) = db.get_blob_metadata(&arr).await
        {
            stored_size = stored_size.saturating_add(meta.size_bytes);
        }
    }

    let dedup_ratio = if stored_size > 0 {
        raw_size as f64 / stored_size as f64
    } else {
        1.0
    };

    // "local", constantly: the backend used to be derived from the phantom
    // `folder_destinations` rail (deleted, folders re-model row 7), whose
    // list was empty on every production nest — so this constant is the value
    // the derivation always produced.
    let storage_backend = "local".to_string();

    Ok(Some(FolderStats {
        // The resolved row's name: a hash-addressed request carries none, and
        // a sealed set's row rests the empty sentinel.
        folder: fs.name.clone(),
        snapshot_count,
        latest_snapshot,
        total_files,
        raw_size_bytes: raw_size,
        stored_size_bytes: stored_size,
        dedup_ratio,
        storage_backend,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn global_stats_empty_repo() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let stats = compute_global_stats(&db).await.unwrap();
        assert_eq!(stats.total_blobs, 0);
        assert_eq!(stats.total_snapshots, 0);
        assert_eq!(stats.dedup_ratio, 1.0);
    }
}
