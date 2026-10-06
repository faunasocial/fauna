//! `__index` rail storage — the nest half of the `fauna.index.{record,list}`
//! plane (`docs/goal/behavior/content-index.md` § Ingest triggers, v1).
//!
//! Structurally the same rail as [`super::drafts`] and [`super::mls_replica`]:
//! a per-actor reserved folder whose blobs are addressed by a virtual `path`
//! string, with a `sync_changes` row per path pointing at the stored blob. That
//! row is what makes the blob replicate to the user's other locations, exactly
//! as `__drafts` does.
//!
//! **What differs from `__drafts`, and why.** A drafts blob crosses the WS-RPC
//! plane inline; an index segment is bulk binary and must not. So the bytes are
//! uploaded over the HTTP blob route and only the *reference* arrives here —
//! which is why [`CacheDb::record_index_blob_change`] takes a hash the caller
//! has already verified is held, never bytes.
//!
//! **The nest reads none of this.** Segments and manifests are AEAD-sealed
//! under keys no nest holds (`content-index.md` § Encryption posture); every
//! method here treats a blob as an opaque, content-addressed byte string.
//!
//! **Nothing on the boot path may delete `__index` content** — see the standing
//! comment in `lib.rs` and the pin `tests/index_survives_nest_restart.rs`.

use super::CacheDb;
use anyhow::Context;

/// One live `__index` path and the blob it points at, as
/// [`CacheDb::list_index_blobs`] returns it.
pub struct IndexBlobRow {
    pub path: String,
    pub blob_hash: [u8; 32],
    pub size_bytes: i64,
}

impl CacheDb {
    /// Record an `__index` blob write for `fauna.index.record`: ensure the
    /// actor's `__index` rail exists, then append a `sync_changes` row pointing
    /// `path` at `blob_hash`, and register the blob metadata.
    ///
    /// The blob bytes are put into the blob store by the caller (raw — already
    /// sealed by the builder), and the caller verifies the store actually holds
    /// them before calling this, so a journal row can never point at a missing
    /// blob. Mirrors [`super::drafts::CacheDb::record_drafts_blob_change`].
    ///
    /// The rail mints lazily here, which is why no `fauna.sync.register` or
    /// folder provisioning precedes a builder's first publish on a fresh nest.
    ///
    /// Collapses the path's superseded history on every write -- see the
    /// comment at the call below for why an unmetered rail owes that.
    pub async fn record_index_blob_change(
        &self,
        actor_id: &[u8; 32],
        path: &str,
        blob_hash: &[u8; 32],
        size_bytes: i64,
    ) -> anyhow::Result<i64> {
        let fs_id = {
            let conn = self.conn.lock().await;
            crate::db::snapshots::get_or_create_reserved_folder(&conn, actor_id, "index")?
        };
        let path_hash: [u8; 32] = fauna_core::sync::path_hash(path);
        let seq = self
            .record_sync_change(
                actor_id,
                &path_hash,
                Some(blob_hash),
                size_bytes,
                // Segments are immutable and written once; the two manifests are
                // rewritten on every flush. `update` is the honest verb for the
                // rail as a whole and is what the `__drafts` twin records.
                "update",
                Some(fs_id),
                None,
                Some(path),
            )
            .await?;
        // Collapse this path's now-unreachable predecessors so they stop
        // pinning their blobs against GC. Same call, same reason, as the
        // `__drafts` twin: a reserved rail may use the UNMETERED recorder only
        // because it is bounded by construction, and the *version count* is the
        // second axis of that bound -- nothing else in production ever marks a
        // reserved rail's rows superseded, so without this every manifest
        // version an honest builder wrote stayed pinned permanently
        // (`db/drafts.rs`, the reserved-rail ruling; closed here for this rail 2026-09-20).
        //
        // Unconditional, and correct per path rather than per kind: the two
        // manifests are rewritten on every flush, which is where the history
        // is, while a segment path is written once -- so this is a no-op there
        // (and on an idempotent re-record of the same segment). Safe because
        // `list_index_blobs` is a per-path head reader (`MAX(seq)` +
        // `superseded_at IS NULL`), so superseding the predecessors changes no
        // answer it gives; in-flight readers keep GC's
        // `superseded_before_millis` buffer.
        //
        // This bounds RETAINED VERSIONS only. A tombstoned segment's blob is
        // deliberately untouched: `content-index.md` rules the compactor
        // tombstone-only and defers physical reclamation to distributed GC, and
        // the nest may not read or sweep `__index` to discover what a fold
        // retired.
        self.collapse_reserved_rail_history(fs_id, &path_hash, seq)
            .await?;
        self.put_blob_metadata(blob_hash, size_bytes, "chunk", None, None)
            .await?;
        Ok(seq)
    }

    /// One page of live `__index` paths for `actor_id` and the blobs they
    /// currently point at — what a device's replica refresh enumerates before
    /// fetching bytes.
    ///
    /// "Live" means the latest non-superseded row per path with a non-null
    /// blob hash; a tombstoned path (null hash) is omitted rather than returned
    /// as an entry a reader would have to special-case.
    ///
    /// **Paged by `path`**, which is the whole order: `path_hash` is
    /// `hash(path)`, so one path is one group and the path string is already a
    /// total, stable, server-independent order key — no rowid tiebreaker is
    /// needed the way the backup rail's `(key, rowid)` cursor needs one.
    /// `after` is exclusive; `limit` bounds the rows read (the caller cuts the
    /// page again at the frame byte budget). A non-positive `limit` reads
    /// everything, which is the pre-paging caller's view.
    pub async fn list_index_blobs(
        &self,
        actor_id: &[u8; 32],
        after: Option<&str>,
        limit: i64,
    ) -> anyhow::Result<Vec<IndexBlobRow>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let fs_id: Option<i64> = {
            use rusqlite::OptionalExtension;
            conn.query_row(
                "SELECT id FROM folders WHERE name = '__index' AND actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .context("get __index folder id")?
        };
        let Some(fs_id) = fs_id else {
            // No rail yet — the first-run state. Not an error: the builder
            // starts from a fresh manifest.
            return Ok(Vec::new());
        };

        // Latest row per path_hash, tombstones excluded. `MAX(seq)` in a
        // bare-columns aggregate picks that row's other columns (SQLite's
        // documented min/max-query behaviour), which is the same shape the
        // rail's `get` twins use.
        //
        // The `after` bound is applied on `path` INSIDE the grouped scan rather
        // than to the grouped result, so SQLite can stop early on a long rail.
        // A group whose rows are filtered out by it is a group whose every row
        // is (they all share one path), so this narrows the page without ever
        // changing which blob a served path resolves to.
        let mut stmt = conn.prepare(
            "SELECT path, manifest_hash, size_bytes, MAX(seq)
             FROM sync_changes
             WHERE folder_id = ?1 AND superseded_at IS NULL
               AND (?2 IS NULL OR path > ?2)
             GROUP BY path_hash
             HAVING manifest_hash IS NOT NULL AND path IS NOT NULL
             ORDER BY path
             LIMIT ?3",
        )?;
        // A non-positive limit means "no row bound" — SQLite spells that -1.
        let row_bound = if limit > 0 { limit } else { -1 };
        let rows = stmt.query_map(rusqlite::params![fs_id, after, row_bound], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (path, hash, size_bytes) = row?;
            let Ok(blob_hash) = <[u8; 32]>::try_from(hash.as_slice()) else {
                // A malformed hash column is a nest-side data defect, not
                // something a reader can act on — skip it rather than hand the
                // client an entry whose blob can never be fetched.
                tracing::warn!(path, "__index row has a non-32-byte blob hash — skipping");
                continue;
            };
            out.push(IndexBlobRow {
                path,
                blob_hash,
                size_bytes,
            });
        }
        Ok(out)
    }
}
