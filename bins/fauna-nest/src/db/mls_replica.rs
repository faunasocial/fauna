//! MLS state-replica storage methods — the nest half of the
//! `fauna.mls.{get,put}` plane (`docs/goal/behavior/file-sync.md` § MLS state
//! replica). Structurally the [`super::drafts`] per-`path` raw-opaque pair
//! **plus** a CAS gate: the replica carries
//! user-irrecoverable data (own-message plaintext history, live ratchet
//! state), so a concurrent-device write must conflict, never silently clobber.
//!
//! A Fauna app seals each replica blob under its own `BackupKey`
//! (`libs/fauna-mls::state_replica`, `libs/fauna-conversations::store::history`)
//! and persists the **opaque** bytes here, addressed by a `path` string
//! (`"provider"`, `"history/<channel_hex>"`) → `path_hash = blake3(path)`
//! within the actor's one `__mls` reserved folder. The nest never holds
//! `BackupKey`; storage is byte-exact opaque (stored raw, no `encode_blob`
//! wrapping — same ChaCha20-vs-zstd rationale as `__drafts`).

use super::{CacheDb, blob_to_array};
use anyhow::Context;
use fauna_protocol::mls_replica::ReplicaBase;
use rusqlite::OptionalExtension;

impl CacheDb {
    /// Raw read of the calling actor's sealed `__mls` blob for `path`
    /// (`fauna.mls.get`): returns the opaque, client-`BackupKey`-sealed bytes
    /// exactly as stored. `Ok(None)` when the actor has no blob for this
    /// `path` yet.
    pub async fn get_mls_replica_blob(
        &self,
        actor_id: &[u8; 32],
        path: &str,
        blob_store: &std::sync::Arc<dyn crate::blob_store::BlobStoreBackend>,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let path_hash: [u8; 32] = *blake3::hash(path.as_bytes()).as_bytes();
        let manifest_hash: Option<Vec<u8>> = {
            let conn = self.conn.lock().await;
            current_mls_replica_hash_locked(&conn, actor_id, &path_hash)?.map(|h| h.to_vec())
        };
        let manifest_hash = match manifest_hash {
            Some(h) => h,
            None => return Ok(None),
        };
        // Infallible: `manifest_hash` round-trips through `Vec<u8>` from the
        // `[u8; 32]` `current_mls_replica_hash_locked` returns (line 35 above).
        let hash: [u8; 32] = manifest_hash.as_slice().try_into().unwrap();
        blob_store
            .get(&fauna_core::data::ContentHash::from_digest_raw(hash))
            .await
    }

    /// The content hash of the calling actor's current `__mls` blob for `path`
    /// — the manifest's digest, one indexed row read, no blob-store touch.
    /// `Ok(None)` when the actor has no blob for this `path` yet. Serves the
    /// `fauna.mls.get` `hash_only` probe (`GetMlsReplicaRequest::hash_only`).
    pub async fn get_mls_replica_hash(
        &self,
        actor_id: &[u8; 32],
        path: &str,
    ) -> anyhow::Result<Option<[u8; 32]>> {
        let path_hash: [u8; 32] = *blake3::hash(path.as_bytes()).as_bytes();
        let conn = self.conn.lock().await;
        current_mls_replica_hash_locked(&conn, actor_id, &path_hash)
    }

    /// Record a `__mls` blob write for `fauna.mls.put`, gated on the CAS
    /// precondition when `base` is `Some` — the atomic
    /// check-then-insert happens under the one connection lock, keyed by an
    /// arbitrary replica `path`. Returns `Ok(false)` on a CAS
    /// mismatch (the handler maps it to `fauna.mls.conflict`). The wire always
    /// carries a base (the handler passes `Some`); `None` is an internal
    /// unconditional write for nest-side callers only.
    ///
    /// The blob bytes are put into the blob store by the caller (raw — already
    /// sealed by the client). Blob metadata is registered *before* the CAS
    /// gate so a conflicting put leaves a reachable-unreferenced orphan GC can
    /// reclaim.
    pub async fn cas_record_mls_replica_blob_change(
        &self,
        actor_id: &[u8; 32],
        path: &str,
        base: Option<&ReplicaBase>,
        manifest_hash: &[u8; 32],
        size_bytes: i64,
    ) -> anyhow::Result<bool> {
        let path_hash: [u8; 32] = *blake3::hash(path.as_bytes()).as_bytes();
        let conn = self.conn.lock().await;

        let now_secs = super::now_epoch_secs();
        conn.execute(
            "INSERT OR IGNORE INTO blob_metadata (hash, size_bytes, content_type, created_at, last_accessed, has_c2pa, thumbnail_hash)
             VALUES (?1, ?2, 'chunk', ?3, ?3, NULL, NULL)",
            rusqlite::params![manifest_hash.as_slice(), size_bytes, now_secs],
        )
        .context("put __mls blob metadata")?;

        if let Some(base) = base {
            let current = current_mls_replica_hash_locked(&conn, actor_id, &path_hash)?;
            let matches = match base {
                ReplicaBase::Absent => current.is_none(),
                ReplicaBase::Hash(h) => current.as_ref() == Some(h),
            };
            if !matches {
                return Ok(false);
            }
        }

        let fs_id = crate::db::snapshots::get_or_create_reserved_folder(&conn, actor_id, "mls")?;
        conn.execute(
            "INSERT INTO sync_changes (actor_id, path_hash, manifest_hash, size_bytes, change_type, created_at, folder_id, device_id, path)
             VALUES (?1, ?2, ?3, ?4, 'update', ?5, ?6, NULL, ?7)",
            rusqlite::params![
                actor_id.as_slice(),
                path_hash.as_slice(),
                manifest_hash.as_slice(),
                size_bytes,
                super::now_epoch_millis(),
                fs_id,
                path,
            ],
        )
        .context("record __mls sync change")?;
        // Collapse this rail's now-unreachable predecessors so they stop
        // pinning their blobs against GC; same held lock, so
        // the head is exactly the row just inserted. Per-`path_hash`, so the
        // `provider` snapshot and each `history/<channel>` slice collapse
        // independently.
        super::sync_storage::collapse_reserved_rail_history_in_conn(
            &conn,
            fs_id,
            &path_hash,
            conn.last_insert_rowid(),
        )?;
        Ok(true)
    }
}

/// The actor's current stored `__mls` content hash for `path_hash` (latest
/// `sync_changes.manifest_hash`), or `None` when never written / deleted.
/// Takes the held connection so the CAS check-then-insert stays atomic.
fn current_mls_replica_hash_locked(
    conn: &rusqlite::Connection,
    actor_id: &[u8; 32],
    path_hash: &[u8; 32],
) -> anyhow::Result<Option<[u8; 32]>> {
    let fs_id: Option<i64> = conn
        .query_row(
            "SELECT id FROM folders WHERE name = '__mls' AND actor_id = ?1",
            rusqlite::params![actor_id.as_slice()],
            |row| row.get(0),
        )
        .optional()
        .context("get __mls folder id")?;
    let Some(fs_id) = fs_id else {
        return Ok(None);
    };
    let manifest_hash: Option<Vec<u8>> = conn
        .query_row(
            "SELECT manifest_hash FROM sync_changes
             WHERE folder_id = ?1 AND path_hash = ?2
             ORDER BY seq DESC LIMIT 1",
            rusqlite::params![fs_id, path_hash.as_slice()],
            |row| row.get::<_, Option<Vec<u8>>>(0),
        )
        .optional()?
        .flatten();
    match manifest_hash {
        Some(h) if h.len() == 32 => Ok(Some(blob_to_array(&h, "manifest_hash")?)),
        _ => Ok(None),
    }
}
