//! Draft-persistence storage methods — the nest half of the
//! `fauna.drafts.{get,put}` plane (`docs/goal/behavior/file-sync.md` § Drafts
//! Sync). Structurally the raw-opaque per-`path` rail shape [`super::mls_replica`]
//! and [`super::content_index_rail`] share.
//!
//! A Fauna app seals its `DraftStore` snapshot under its own `BackupKey`
//! (`libs/fauna-conversations`) and persists the **opaque** blob here; the nest
//! never holds that key, so it stores/returns the bytes opaque — no nest path
//! reads drafts in either storage mode.
//!
//! **Per-actor, per-path `__drafts`.** Each actor owns one `__drafts` reserved
//! folder (`folders` is unique on `(name, actor_id)`); within it, each rail's
//! blob is addressed by a `path` string (`"conversations"`, `"posts"`,
//! `"events"`) → `path_hash = blake3(path)`. So one actor can hold several rail blobs
//! side by side, and `get`/`record` both scope by `(actor_id, path_hash)`.
//!
//! Storage is **byte-exact opaque**: the blob is stored *raw* (no `encode_blob`
//! wrapping) under its own content hash, with a `sync_changes` row in the
//! `__drafts` folder, and `get` returns exactly those bytes — the client did
//! the full seal (`bare → zstd → ChaCha20`), so the nest must not re-wrap
//! (the ChaCha20 version byte `0x01` collides with the zstd prefix and
//! `decode_blob` would fail).

use super::{CacheDb, blob_to_array};
use anyhow::Context;
use rusqlite::OptionalExtension;

impl CacheDb {
    /// Raw read of the calling actor's sealed `__drafts` blob for `path`
    /// (`fauna.drafts.get`): returns the **opaque, client-`BackupKey`-sealed
    /// bytes exactly as stored** by `fauna.drafts.put` — no `decode_blob`. The
    /// nest never holds the client's `BackupKey`, so only the client can unseal
    /// these.
    ///
    /// `Ok(None)` when the actor has no drafts for this `path` yet. Looks the
    /// folder up by `(name, actor_id)` (not name alone) so it is correct under
    /// multiple actors on one nest.
    pub async fn get_drafts_blob(
        &self,
        actor_id: &[u8; 32],
        path: &str,
        blob_store: &std::sync::Arc<dyn crate::blob_store::BlobStoreBackend>,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let path_hash: [u8; 32] = *blake3::hash(path.as_bytes()).as_bytes();
        let manifest_hash: Option<Vec<u8>> = {
            let conn = self.conn.lock().await;
            let fs_id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM folders WHERE name = '__drafts' AND actor_id = ?1",
                    rusqlite::params![actor_id.to_vec()],
                    |row| row.get(0),
                )
                .optional()
                .context("get drafts folder id")?;
            let fs_id = match fs_id {
                Some(id) => id,
                None => return Ok(None),
            };
            conn.query_row(
                "SELECT manifest_hash FROM sync_changes
                 WHERE folder_id = ?1 AND path_hash = ?2
                 ORDER BY seq DESC LIMIT 1",
                rusqlite::params![fs_id, path_hash.to_vec()],
                |row| row.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?
            .flatten()
        };

        let manifest_hash = match manifest_hash {
            Some(h) if h.len() == 32 => h,
            // A `delete` (null manifest) or an absent path → no current blob.
            _ => return Ok(None),
        };
        let hash: [u8; 32] = blob_to_array(&manifest_hash, "manifest_hash")?;
        blob_store
            .get(&fauna_core::data::ContentHash::from_digest_raw(hash))
            .await
    }

    /// Record a `__drafts` blob write for `fauna.drafts.put`: ensure the actor's
    /// `__drafts` folder exists, then append a `sync_changes` row pointing
    /// `path` at `manifest_hash`, and register the blob metadata, keyed by a
    /// ratified rail `path`.
    /// The blob bytes are put into the blob store by
    /// the caller (which stores them raw — already sealed by the client).
    ///
    /// **Why the UNMETERED [`CacheDb::record_sync_change`] is correct here**
    /// (the finding asked for
    /// metering *or* this statement). A reserved rail is bounded by
    /// **construction — a fixed path count times a per-blob cap** — not by the
    /// storage quota. Drafts is bounded exactly that way:
    /// [`fauna_protocol::drafts::DRAFT_RAILS`] is closed at three and
    /// `MAX_DRAFTS_BLOB_BYTES` caps each, so an actor's **live** `__drafts`
    /// footprint is bounded at 3 MiB. The defect the finding hit was the missing
    /// *bound*, and the unbounded axis was the **count** of client-chosen paths
    /// — closing the enumeration removes it at the source.
    ///
    /// ⚠ **A second count was still unbounded, and closing the enumeration did
    /// not touch it**: the
    /// count of *retained versions* of each rail. Every call here registers a
    /// fresh blob and appends a row, and nothing in production marks a reserved
    /// rail's rows superseded — `superseded_at`'s only other writer is the
    /// owner-driven M2 `fauna.sync.changes.supersede` RPC — so each rail's
    /// every historical blob stayed GC-pinned **permanently**, not for one GC
    /// cycle. Hence the [`CacheDb::collapse_reserved_rail_history`] call below,
    /// which is what makes the resting bound (writes per GC cycle × 1 MiB per
    /// rail, above the live 3 MiB) true rather than merely claimed. Metering
    /// would not have bounded this axis either — a charge with no delete verb
    /// releases nothing — which is a *fourth* ground for the refusal below.
    ///
    /// Metering was considered and rejected on three counts.
    /// [`CacheDb::record_sync_change_metered`] wants a `device_id` this plane's
    /// wire does not carry and the `path_sealed` discipline reserved rails are a
    /// ratified exemption from; the plane has **no delete verb** at all
    /// (`fauna.drafts.{get,put}`), so charged bytes could be superseded but never
    /// released, making the charge a ratchet; and charging a user's own working
    /// drafts against the tier quota that meters their *files* is a product
    /// change, not a security fix. If a future rail needs an unbounded key space
    /// (the per-`<rail>/<id>` layout `reserved-folders.md` § Drafts Sync used
    /// to leave open), that is when metering becomes the right instrument — and
    /// it needs a delete verb first.
    pub async fn record_drafts_blob_change(
        &self,
        actor_id: &[u8; 32],
        path: &str,
        manifest_hash: &[u8; 32],
        size_bytes: i64,
    ) -> anyhow::Result<()> {
        let fs_id = {
            let conn = self.conn.lock().await;
            crate::db::snapshots::get_or_create_reserved_folder(&conn, actor_id, "drafts")?
        };
        let path_hash: [u8; 32] = *blake3::hash(path.as_bytes()).as_bytes();
        let head_seq = self
            .record_sync_change(
                actor_id,
                &path_hash,
                Some(manifest_hash),
                size_bytes,
                "update",
                Some(fs_id),
                None,
                Some(path),
            )
            .await?;
        // Collapse this rail's now-unreachable predecessors so they stop
        // pinning their blobs against GC (see the helper for
        // why a rail may do this and an ordinary folder may not).
        self.collapse_reserved_rail_history(fs_id, &path_hash, head_seq)
            .await?;
        self.put_blob_metadata(manifest_hash, size_bytes, "chunk", None, None)
            .await?;
        Ok(())
    }
}
