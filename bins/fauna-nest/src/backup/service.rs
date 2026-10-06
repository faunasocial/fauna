//! BackupService: centralized backup operations.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use fauna_core::crypto::BackupKey;

use crate::blob_store::{BlobStoreBackend, DiskBlobStore};
use crate::db::CacheDb;

/// Record returned by `backup_database`, describing the completed hot-copy snapshot.
pub struct BackupRecord {
    pub blob_hash: [u8; 32],
    pub size_bytes: i64,
    pub format: String,
    pub created_at: i64,
}

/// How many hot-copy (`sqlite`) DB backups to retain. Older ones are pruned —
/// blob + manifest + row — after each new backup. Bounds the nest self-backup
/// store; without this the scheduler filled the disk (the example.com outage). See `docs/goal/behavior/backup-restore.md` § Self-backup retention.
pub const KEEP_SQLITE_BACKUPS: usize = 24;
/// How many logical-dump backups to retain (daily cadence ⇒ ~1 week).
pub const KEEP_LOGICAL_BACKUPS: usize = 7;

pub struct BackupService {
    pub(crate) db: Arc<CacheDb>,
    pub(crate) encryption_key: Option<BackupKey>,
    compression: bool,
    local_blob_root: PathBuf,
    local_store: Arc<dyn BlobStoreBackend>,
    db_path: Option<PathBuf>,
}

impl BackupService {
    pub fn new(
        db: Arc<CacheDb>,
        encryption_key: Option<BackupKey>,
        compression: bool,
        local_blob_root: PathBuf,
        db_path: Option<PathBuf>,
    ) -> Result<Self> {
        let local_store: Arc<dyn BlobStoreBackend> =
            Arc::new(DiskBlobStore::new(&local_blob_root)?);
        Ok(Self {
            db,
            encryption_key,
            compression,
            local_blob_root,
            local_store,
            db_path,
        })
    }

    /// [`Self::new`] over a caller-supplied local store, for this crate's own
    /// tests that must observe what the nest reads from it — a counting wrapper
    /// over a real [`DiskBlobStore`]. `#[cfg(test)]`: no deployment chooses its
    /// store; `new` builds the one the blob root names.
    #[cfg(test)]
    pub(crate) fn with_local_store(
        db: Arc<CacheDb>,
        local_blob_root: PathBuf,
        local_store: Arc<dyn BlobStoreBackend>,
    ) -> Self {
        Self {
            db,
            encryption_key: None,
            compression: false,
            local_blob_root,
            local_store,
            db_path: None,
        }
    }

    /// Get the blob store, used by GC, route handlers, and schedulers.
    pub fn local_blob_store(&self) -> Arc<dyn BlobStoreBackend> {
        self.local_store.clone()
    }

    /// Get the encryption key (if set).
    pub fn encryption_key(&self) -> Option<&BackupKey> {
        self.encryption_key.as_ref()
    }

    /// Get the compression flag.
    pub fn compression(&self) -> bool {
        self.compression
    }

    /// Get the database handle.
    pub fn db(&self) -> &Arc<CacheDb> {
        &self.db
    }

    /// Prune `backup_snapshots` of `format` beyond the newest `keep`, deleting
    /// each pruned row and — for any hash no longer referenced by a remaining row
    /// — its underlying blob file and its `backups/<hash>.manifest.json`. This is
    /// the keep-last-N retention that bounds the nest self-backup store so the
    /// snapshot scheduler can never fill the disk. Filesystem
    /// deletes are best-effort (a missing blob/manifest is not fatal).
    ///
    /// The hash-still-referenced check matters because the store is
    /// content-addressed: an unchanged DB across cycles hashes identically, so
    /// two `backup_snapshots` rows can share one blob — pruning one must not
    /// delete a blob the other still needs.
    async fn prune_backup_snapshots(&self, format: &str, keep: usize) -> Result<()> {
        let snapshots = self.db.list_backup_snapshots(format).await?; // newest-first
        if snapshots.len() <= keep {
            return Ok(());
        }
        let backups_dir = self.local_blob_root.join("backups");
        for (id, hash, _created_at) in &snapshots[keep..] {
            self.db.delete_backup_snapshot(*id).await?;
            if self.db.count_backup_snapshots_with_hash(hash).await? == 0 {
                let _ = self
                    .local_store
                    .delete(&fauna_core::data::ContentHash::from_digest_raw(*hash))
                    .await;
                let manifest = backups_dir.join(format!("{}.manifest.json", hex::encode(hash)));
                let _ = std::fs::remove_file(&manifest);
            }
        }
        Ok(())
    }

    /// Create a hot-copy backup of the SQLite database using the sqlite3_backup API.
    ///
    /// Opens a separate read-only connection so the main connection is not blocked.
    /// The raw database bytes are hashed, encoded (compressed/encrypted per config),
    /// stored in the local blob store, and recorded in `backup_snapshots`.
    pub async fn backup_database(&self) -> Result<BackupRecord> {
        let db_path = self
            .db_path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("database backup requires a file-based database"))?
            .clone();

        // Run the SQLite backup in a blocking thread so non-Send types
        // (rusqlite::Connection, rusqlite::backup::Backup) don't cross await points.
        let raw = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            // Open separate read-only connection (doesn't block the main connection)
            let src = rusqlite::Connection::open_with_flags(
                &db_path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;

            // Backup to temp file
            let tmp_path = db_path.with_extension("db.backup");
            let mut dest = rusqlite::Connection::open(&tmp_path)?;
            {
                let backup = rusqlite::backup::Backup::new(&src, &mut dest)?;
                backup.run_to_completion(1000, std::time::Duration::from_millis(10), None)?;
            }
            drop(dest);
            drop(src);

            let raw = std::fs::read(&tmp_path)?;
            let _ = std::fs::remove_file(&tmp_path);
            Ok(raw)
        })
        .await??;
        let hash = fauna_core::data::ContentHash::of_raw(&raw);
        let encoded = crate::backup::encode_blob(&raw, self.encryption_key(), self.compression())?;
        self.local_store.put(&hash, &encoded).await?;

        // Write manifest to filesystem (not content-addressed blob store)
        let backups_dir = self.local_blob_root.join("backups");
        std::fs::create_dir_all(&backups_dir)?;
        let now = fauna_core::data::Timestamp::now_secs();
        let manifest = serde_json::json!({
            "format": "sqlite",
            "size_bytes": raw.len(),
            "created_at": now,
            "blob_hash": hex::encode(hash.digest()),
        });
        let manifest_path =
            backups_dir.join(format!("{}.manifest.json", hex::encode(hash.digest())));
        std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;

        // Record in DB
        let size = raw.len() as i64;
        self.db
            .record_backup_snapshot(&hash.digest(), size, "sqlite", now)
            .await?;

        // Keep-last-N retention: bound the self-backup store (best-effort —
        // a prune failure must not fail the backup that just succeeded).
        if let Err(e) = self
            .prune_backup_snapshots("sqlite", KEEP_SQLITE_BACKUPS)
            .await
        {
            tracing::warn!("sqlite backup retention prune failed: {e:#}");
        }

        Ok(BackupRecord {
            blob_hash: hash.digest(),
            size_bytes: size,
            format: "sqlite".into(),
            created_at: now,
        })
    }

    /// Create a logical dump of the [`DUMP_TABLES`] subset of the database as
    /// NDJSON in a tar.zst archive.
    ///
    /// Unlike `backup_database` (which does a hot SQLite copy), this produces a
    /// portable, human-readable archive that works with in-memory databases and
    /// excludes sensitive payload blobs from the `content` table.
    ///
    /// ⚠ It is **not** "all database tables" (as this comment claimed until
    /// 2026-08-15) — the subset is a ratified confidentiality boundary, not
    /// drift: the S9 self-backup ruling rests on this dump carrying no sealed
    /// name plane, unlike the hot-copies above. Read [`DUMP_TABLES`]'s doc
    /// comment before widening it.
    ///
    /// [`DUMP_TABLES`]: crate::export::logical::DUMP_TABLES
    pub async fn logical_dump(&self) -> Result<BackupRecord> {
        let db = self.db.clone();
        let raw = tokio::task::spawn_blocking(move || {
            let conn = db.conn_blocking();
            let mut buf = Vec::new();
            crate::export::logical::write_logical_dump(&conn, &mut buf)?;
            Ok::<Vec<u8>, anyhow::Error>(buf)
        })
        .await??;

        let hash = fauna_core::data::ContentHash::of_raw(&raw);
        let encoded = crate::backup::encode_blob(&raw, self.encryption_key(), self.compression())?;
        self.local_store.put(&hash, &encoded).await?;

        // Write manifest
        let backups_dir = self.local_blob_root.join("backups");
        std::fs::create_dir_all(&backups_dir)?;
        let now = fauna_core::data::Timestamp::now_secs();
        let manifest = serde_json::json!({
            "format": "logical",
            "size_bytes": raw.len(),
            "created_at": now,
            "blob_hash": hex::encode(hash.digest()),
        });
        std::fs::write(
            backups_dir.join(format!("{}.manifest.json", hex::encode(hash.digest()))),
            serde_json::to_string_pretty(&manifest)?,
        )?;

        let size = raw.len() as i64;
        self.db
            .record_backup_snapshot(&hash.digest(), size, "logical", now)
            .await?;

        if let Err(e) = self
            .prune_backup_snapshots("logical", KEEP_LOGICAL_BACKUPS)
            .await
        {
            tracing::warn!("logical backup retention prune failed: {e:#}");
        }

        Ok(BackupRecord {
            blob_hash: hash.digest(),
            size_bytes: size,
            format: "logical".into(),
            created_at: now,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::ContentHash;

    #[tokio::test]
    async fn backup_database_creates_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("test.db");
        let db = Arc::new(CacheDb::open(&db_path).unwrap());
        let svc = BackupService::new(
            db.clone(),
            None,
            false,
            tmp.path().join("blobs"),
            Some(db_path.clone()),
        )
        .unwrap();

        let record = svc.backup_database().await.unwrap();
        assert!(record.size_bytes > 0);
        assert_eq!(record.format, "sqlite");

        // Verify the blob exists in the store
        let blob = svc
            .local_blob_store()
            .get(&fauna_core::data::ContentHash::from_digest_raw(
                record.blob_hash,
            ))
            .await
            .unwrap();
        assert!(blob.is_some());

        // Verify manifest file exists
        let manifest_path = tmp
            .path()
            .join("blobs")
            .join("backups")
            .join(format!("{}.manifest.json", hex::encode(record.blob_hash)));
        assert!(manifest_path.exists());
    }

    /// keep-last-N retention prunes the oldest backups beyond N — deleting the
    /// row, the blob, and the manifest. Regression guard for the example.com
    /// disk-fill (unbounded 60s DB hot-copies).
    #[tokio::test]
    async fn prune_keeps_last_n_deletes_old_blobs_and_manifests() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let svc =
            BackupService::new(db.clone(), None, false, tmp.path().to_path_buf(), None).unwrap();
        let backups_dir = tmp.path().join("backups");
        std::fs::create_dir_all(&backups_dir).unwrap();

        let mut hashes = Vec::new();
        for i in 0..5u8 {
            let data = vec![i; 100 + i as usize]; // distinct content per i
            let hash = ContentHash::of_raw(&data);
            svc.local_blob_store().put(&hash, &data).await.unwrap();
            let digest = hash.digest();
            std::fs::write(
                backups_dir.join(format!("{}.manifest.json", hex::encode(digest))),
                b"{}",
            )
            .unwrap();
            db.record_backup_snapshot(&digest, data.len() as i64, "sqlite", 1000 + i as i64)
                .await
                .unwrap();
            hashes.push(hash);
        }

        // Keep the 2 newest (i=3,4); the 3 oldest (i=0,1,2) are pruned.
        svc.prune_backup_snapshots("sqlite", 2).await.unwrap();

        assert_eq!(db.list_backup_snapshots("sqlite").await.unwrap().len(), 2);
        assert!(!svc.local_blob_store().exists(&hashes[0]).await.unwrap());
        assert!(!svc.local_blob_store().exists(&hashes[2]).await.unwrap());
        assert!(svc.local_blob_store().exists(&hashes[4]).await.unwrap());
        assert!(
            !backups_dir
                .join(format!("{}.manifest.json", hex::encode(hashes[0].digest())))
                .exists(),
            "pruned manifest must be deleted"
        );
        assert!(
            backups_dir
                .join(format!("{}.manifest.json", hex::encode(hashes[4].digest())))
                .exists(),
            "retained manifest must survive"
        );
    }

    /// A blob shared by a remaining row (content-addressed: unchanged DB hashes
    /// identically across cycles) must NOT be deleted when an older row is pruned.
    #[tokio::test]
    async fn prune_preserves_blob_shared_by_remaining_row() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let svc =
            BackupService::new(db.clone(), None, false, tmp.path().to_path_buf(), None).unwrap();
        let data = b"unchanged-db-content";
        let hash = ContentHash::of_raw(data);
        svc.local_blob_store().put(&hash, data).await.unwrap();
        let digest = hash.digest();
        db.record_backup_snapshot(&digest, data.len() as i64, "sqlite", 1000)
            .await
            .unwrap();
        db.record_backup_snapshot(&digest, data.len() as i64, "sqlite", 2000)
            .await
            .unwrap();

        // Keep 1 → prune the older row, but its blob is still referenced by the newer.
        svc.prune_backup_snapshots("sqlite", 1).await.unwrap();

        assert_eq!(db.list_backup_snapshots("sqlite").await.unwrap().len(), 1);
        assert!(
            svc.local_blob_store().exists(&hash).await.unwrap(),
            "a blob still referenced by a remaining row must not be deleted"
        );
    }

    use crate::partition_scan::Gate;

    /// What the census keys on: every production call site onto the store.
    ///
    /// Acquiring the handle is not the whole of it. `start_server` hands the
    /// handle to three long-lived holders — the chunk resolver, the payload
    /// store and the web-content service — which read through it for the life
    /// of the process, so a use of a held handle is a call site too, and so is
    /// a call of any holder method that hands stored bytes back to its caller
    /// (each such method is a [`Use::Primitive`] entry, and naming it here is
    /// what puts its callers in the table). The last four are the fns the
    /// web-content handle is passed into by its one taker, the site serve path.
    const NEEDLES: &[&str] = &[
        "local_blob_store(",
        "self.blob_store",
        "relay_for_folder(",
        "resolve_payload(",
        "load_blob_bytes(",
        ".blob_store()",
        "try_serve_sealed(",
        "serve_user_file(",
        "serve_blob(",
        "fetch_blob_bytes(",
    ];

    /// What a call site onto the blob store ([`NEEDLES`]) does with the store's
    /// bytes. The strings are the partition's reasons, kept beside the entries
    /// so a reader of the table never has to re-derive them.
    enum Use {
        /// Bytes, or anything derived from them, can leave the process for a
        /// digest the **caller named**. The string names the gate that
        /// withholds a taken-down record's blobs on the way, as a flow; the
        /// [`Gate`] is what the test holds the production body to.
        Gated(Gate, &'static str),
        /// Bytes can leave, but no caller can aim the read at a digest: it
        /// comes from a row this nest wrote, or from bytes the caller itself
        /// supplied. The string says which.
        Scoped(&'static str),
        /// A holder method that reads for a digest its CALLER passes and hands
        /// the bytes back rather than serving them. Its name is in
        /// [`NEEDLES`], so every caller is classified by an entry of its own.
        Primitive(&'static str),
        /// Nothing this caller does puts stored bytes, or anything derived
        /// from them, on a wire. The string says what it does instead.
        NoServe(&'static str),
    }

    /// Every production call site onto the blob store, keyed by
    /// `(path under src/, enclosing fn)`.
    ///
    /// The order is the table's own: reader-named doors first (the ones a
    /// takedown is bypassed through), then owner-scoped serve paths, then the
    /// holders' primitives, then the callers that serve nothing.
    const PARTITION: &[(&str, &str, Use)] = &[
        // ── reader-named doors: the caller supplies the digest ──
        (
            "blob_routes.rs",
            "download_blob",
            Use::Gated(
                Gate::Calls(&["legal_takedown_gate("]),
                "`legal_takedown_gate` on the RESOLVED digest before either branch and \
                 before the `?thumb=1` lookup — a hex-only gate is bypassed by asking \
                 for the same bytes under their `b…` CID, and a withheld original must \
                 not serve a thumbnail of itself",
            ),
        ),
        (
            "blob_routes.rs",
            "get_blob_by_cid_inner",
            Use::Gated(
                Gate::OnlyFrom(&["download_blob"]),
                "reachable only from `download_blob`, after that gate — it is `async fn` \
                 private to this module and `/api/v1/blob/{id}` is the one route onto it",
            ),
        ),
        (
            "chunk_routes.rs",
            "download_chunk",
            Use::Gated(
                Gate::Calls(&["legal_takedown_gate("]),
                "`legal_takedown_gate` on the parsed digest before the store read AND \
                 before the folder-hinted relay arm, which would otherwise pull the \
                 compelled bytes back from a seat and — on a full-residency folder — \
                 rest them here again",
            ),
        ),
        (
            "chunk_routes.rs",
            "relay_chunk_for_folder",
            Use::Gated(
                Gate::OnlyFrom(&["download_chunk"]),
                "the chunk door's store-miss relay arm reads through the resolver's held \
                 handle for the digest the URL named, so it is a reader-named door in its \
                 own right — reached only from `download_chunk`, after that fn's gate",
            ),
        ),
        (
            "chunk_routes.rs",
            "download_manifest",
            Use::Gated(
                Gate::Calls(&["legal_takedown_gate("]),
                "`legal_takedown_gate` on the parsed digest before the store read; a \
                 video post's manifest hash is in the withheld set in its own right \
                 (`Post::blob_refs`)",
            ),
        ),
        (
            "video_routes.rs",
            "download_segment",
            Use::Gated(
                Gate::Calls(&["legal_takedown_gate("]),
                "`legal_takedown_gate` before the store read and before the \
                 cache-on-fetch arm that would re-import the segment from a peer",
            ),
        ),
        (
            "share_routes.rs",
            "serve_plaintext",
            Use::Gated(
                Gate::Via {
                    hops: &[
                        ("web_content/file_bytes.rs", "read_file_by_manifest"),
                        ("web_content/file_bytes.rs", "fetch_decoded"),
                    ],
                    gates: &["is_legally_withheld("],
                },
                "`handle_share`'s public-link arm, after the shared `admit` prelude: \
                 serves a caller-named manifest through \
                 `web_content::file_bytes::read_file_by_manifest`, which consults the \
                 withhold on the manifest hash and on every chunk it reads and answers \
                 `FileOpenError::Withheld` → 451; a `BulkWriteAuth` writer can mint a \
                 manifest naming any digest, so gating the walk is what binds this door. \
                 `read_file_by_manifest` is itself a seam — the withhold predicate lives one \
                 hop below it, in `fetch_decoded` — so both links are held, not only the \
                 door's own call of the seam",
            ),
        ),
        (
            "share_routes.rs",
            "private_manifest",
            Use::Gated(
                Gate::Calls(&["is_legally_withheld("]),
                "a fragment-keyed share link's gate: the token names the manifest, and a \
                 `BulkWriteAuth` writer can mint one naming any digest. It reads the manifest \
                 through `file_bytes::fetch_decoded` (withheld → 451) and then calls \
                 `blob_routes::is_legally_withheld` on EVERY store key the manifest names \
                 before any private-arm answer — the viewer page, the manifest, a chunk — \
                 so a link over a withheld blob is refused whole, as the public walk refuses it",
            ),
        ),
        (
            "share_routes.rs",
            "handle_share_chunk",
            Use::Gated(
                Gate::Via {
                    hops: &[("web_content/file_bytes.rs", "fetch_decoded")],
                    gates: &["is_legally_withheld("],
                },
                "serves the i-th ciphertext chunk of a private link's manifest, by its store \
                 key, only after `private_manifest` cleared every key of that manifest; the \
                 read itself goes through `fetch_decoded`, which consults the withhold on the \
                 digest again before the store read",
            ),
        ),
        (
            "web_content/service.rs",
            "load_web_file_bytes",
            Use::Gated(
                Gate::Via {
                    hops: &[
                        ("web_content/file_bytes.rs", "read_file_by_manifest"),
                        ("web_content/file_bytes.rs", "fetch_decoded"),
                    ],
                    gates: &["is_legally_withheld("],
                },
                "a render input's `web_files` row names a manifest, and a `BulkWriteAuth` \
                 writer can mint one naming any digest; the read runs through the \
                 withhold-consulting manifest walk, so a withheld chunk never reaches a \
                 render. The same seam as `handle_share` above — held to `fetch_decoded`, one \
                 hop below `read_file_by_manifest`, not only to the door's own call of it",
            ),
        ),
        // ── owner-scoped serve paths: no caller can aim these at a digest ──
        (
            "drafts_handlers.rs",
            "drafts_get_handler",
            Use::Scoped(
                "`get_drafts_blob` resolves the digest from the caller's own drafts row \
                 at their own path, written from bytes they supplied",
            ),
        ),
        (
            "mail_body_plane.rs",
            "resolve_staged_body",
            Use::Scoped(
                "the digests come off the request here too, but `join_and_open_staged_body` \
                 authenticates the join with an AEAD tag under the key the CALLER's own \
                 `StagedBodyRef` carries — a blob they merely named cannot satisfy it, so \
                 nothing foreign to their own staging is ever returned (witnessed: \
                 `a_body_ref_naming_a_withheld_blob_is_refused` names the withheld digest \
                 in a staged reference, and it does not resolve)",
            ),
        ),
        (
            "mls_replica_handlers.rs",
            "mls_get_handler",
            Use::Scoped(
                "`get_mls_replica_blob` resolves the digest from the caller's own replica \
                 row, written from bytes they supplied; `hash_only` reads no blob at all",
            ),
        ),
        (
            "mail_body_plane.rs",
            "resolve_body_ref",
            Use::Gated(
                Gate::Calls(&["is_legally_withheld("]),
                "`blob_routes::is_legally_withheld` per chunk before the store read — the \
                 digests come off the REQUEST (`req.body_ref`) and the join is checked \
                 only against the declared total, so a bridge session could otherwise \
                 name a withheld digest and read the bytes back as its own message",
            ),
        ),
        (
            "inbox_handlers.rs",
            "fetch_handler",
            Use::Scoped(
                "the caller's own undelivered inbox rows (`poll_inbox_after` on the \
                 caller's actor); the payload store's digest is each row's own",
            ),
        ),
        (
            "web_content/service.rs",
            "decrypt_gated_full_text",
            Use::Scoped(
                "the digest is the gated post's own `encrypted_ref`, off a record \
                 `render_published_posts` enumerated through \
                 `list_web_published_servable_capped` (MODERATION_SERVABLE); the bytes are \
                 opened under the holder's live grant and only ever rendered into that \
                 post's sealed page",
            ),
        ),
        (
            "web_content/serve.rs",
            "serve_web_content_with_token",
            Use::Scoped(
                "a visitor names a path, never a digest. The rendered arms serve the \
                 `web_rendered` row the render wrote for that path — the nest's own \
                 output, rendered only from posts MODERATION_SERVABLE admits and \
                 re-rendered fail-closed on a takedown; the sealed arm serves a \
                 `web_rendered_sealed` row only to a valid token under a live grant; the \
                 user-file arms (`serve.rs:420,509,573`) read through the same \
                 withhold-consulting manifest walk `handle_share` and `load_web_file_bytes` \
                 above name — a THIRD consumer of it, Scoped rather than Gated because no \
                 caller can aim it at a digest, but structurally covered by the same held \
                 seam: `read_file_by_manifest`'s predicate lives in `fetch_decoded`, which \
                 this fn reaches too, and the scanner holds that fn's body once, not per \
                 caller",
            ),
        ),
        (
            "conversations_handlers.rs",
            "open",
            Use::Scoped(
                "opens a room record's attachments to build labeler facets; the digests \
                 come from `conv_attachment_refs`, the plaintext floor this nest wrote \
                 for that record, capped at `MAX_ATTACHMENT_REFS_PER_RECORD`",
            ),
        ),
        (
            "room_post_view.rs",
            "index_room_post",
            Use::Scoped(
                "the digest is the stored post's own `gated.encrypted_ref`, never a \
                 caller's, read in the act that stores the post — before any flag can \
                 exist on it; what leaves this fn is a derived room view written to the \
                 nest's index, and the view's serve paths carry the withhold: the \
                 search map and the verdict map both apply \
                 `db::rooms::ROOM_POST_VIEW_MODERATION` (`MODERATION_SERVABLE` over \
                 `content_meta`), so a taken-down room post yields no hit and no \
                 verdict while the flag stands and the kept rows re-serve on the \
                 overturn (path 4 of the owner-scoped ruling)",
            ),
        ),
        (
            "backup/materialize.rs",
            "materialize_segment_set",
            Use::Scoped(
                "reconstitutes the owner's own `__mail` set into their own empty scope \
                 under the key they granted at enrollment; digests come from \
                 `list_backup_custody_in_set` for that actor",
            ),
        ),
        (
            "backup/materialize.rs",
            "materialize_folder_set",
            Use::NoServe(
                "`held_manifest` reads each owner custody row's manifest (digests from \
                 `list_backup_custody_in_set` for that actor) for its `total_size`, the \
                 re-home statement's size; what leaves is an integer and a \
                 manifest-unreadable refusal",
            ),
        ),
        (
            "backup/recover.rs",
            "recover_segment_set",
            Use::Scoped(
                "the lived-in recovery: reconstitutes the owner's own custody set into \
                 their own scope under the key they granted at enrollment, exactly as \
                 materialize does; digests come from that actor's own custody mirror, \
                 never from the request, and the bytes land in the segment area, not \
                 on a wire",
            ),
        ),
        (
            "export_routes.rs",
            "gather_export_data",
            Use::Scoped(
                "the author's own archive; blob digests come from the refs the walk \
                 gathered off their own records, never from the request. The leg is \
                 DORMANT: `scan_blob_refs` is a no-op, so `blob_refs` is always empty \
                 and this loop ships no store blob today; the archive's real blob \
                 carriage is the `post` segment-pair leg, already ruled (withheld whole \
                 and declared). The ruling this leg inherits the day an extractor lands \
                 (path 1 of the owner-scoped ruling): the author's own archive is \
                 bound — § Posts withholds a taken-down body from the author too — so \
                 each gathered digest passes `blob_routes::is_legally_withheld` and a \
                 withheld one is declared in the manifest, never silently dropped. Its \
                 inbox leg resolves payload overflow through the payload store, by \
                 the digest each exported row names",
            ),
        ),
        (
            "admin_export_routes.rs",
            "handle_admin_export_all",
            Use::Scoped(
                "the box admin's whole-store export; digests come from `list_all_hashes`, \
                 never from the request. Carries withheld bytes, and MUST (path 2 of the \
                 owner-scoped ruling): this is the box's own custody moving with the box \
                 — the operator the order was served on already holds every byte under \
                 their own root, `content_meta` and `audit_log` ride beside them in \
                 `tables/`, and an archive that dropped the withheld bytes would turn \
                 tombstone-not-delete into a deletion at the next restore, so an \
                 overturn could re-serve nothing",
            ),
        ),
        (
            "segment_backup.rs",
            "run_folder_once",
            Use::Scoped(
                "mirrors the owner's folder to the destination the owner configured; \
                 digests come from that folder's own file rows. Consults the withhold \
                 all the same (path 3 of the owner-scoped ruling): the destination's \
                 own `/api/v1/chunks/{hash}` door holds no flag and would serve a \
                 mirrored chunk to anyone with the hex this nest answers 451 for — the \
                 path-prefix substitution with the host substituted instead. A live \
                 path naming a withheld digest (`list_blob_legal_withhold`, tested in \
                 memory, before any chunk read) is skipped, counted in \
                 `FolderRunReport::withheld_paths`, retracted if an earlier pass \
                 mirrored it, and re-tried every pass so it mirrors the moment the \
                 flag lifts; the local file is untouched",
            ),
        ),
        // ── the holders' primitives: they hand bytes back; each caller is an entry ──
        (
            "chunk_relay.rs",
            "relay_for_folder",
            Use::Primitive(
                "reads the resolver's held handle for the digest its caller passes, then \
                 asks the connections that announced the folder; its one caller is the \
                 chunk door's relay arm above",
            ),
        ),
        (
            "payload_store.rs",
            "resolve_payload",
            Use::Primitive(
                "reads the overflow digest its caller passes and hands the bytes back; \
                 each caller's entry names the row that digest came from",
            ),
        ),
        (
            "web_content/service.rs",
            "load_blob_bytes",
            Use::Primitive(
                "reads a verbatim blob for the digest its caller passes; its one caller \
                 is the render's gated-post decrypt above",
            ),
        ),
        (
            "web_content/service.rs",
            "blob_store",
            Use::Primitive(
                "the accessor hands the held handle itself out; its one caller is the \
                 site serve path, classified below",
            ),
        ),
        // ── callers from which no stored bytes leave ──
        (
            "blob_routes.rs",
            "upload_blob",
            Use::NoServe("writes the uploaded blob; reads nothing back to the caller"),
        ),
        (
            "blob_routes.rs",
            "put_blob_by_cid",
            Use::NoServe(
                "writes a CID-verified blob; the reply is `created`/`exists`, never bytes",
            ),
        ),
        (
            "chunk_routes.rs",
            "upload_chunk",
            Use::NoServe("writes the uploaded chunk under its verified content address"),
        ),
        (
            "chunk_routes.rs",
            "upload_manifest",
            Use::NoServe("writes a canonical-`ChunkManifest`-validated body"),
        ),
        (
            "chunk_routes.rs",
            "check_chunks",
            Use::NoServe(
                "`exists_batch` answers a presence bit per caller-named digest, never \
                 bytes — and tombstone-not-delete keeps the bytes on the box across a \
                 takedown, so the bit is identical before and after one and discloses \
                 nothing the withhold removed",
            ),
        ),
        (
            "video_routes.rs",
            "upload_segment",
            Use::NoServe("writes the uploaded segment under its blake3 address"),
        ),
        (
            "drafts_handlers.rs",
            "drafts_put_handler",
            Use::NoServe("writes the already-sealed drafts blob raw"),
        ),
        (
            "mls_replica_handlers.rs",
            "mls_put_handler",
            Use::NoServe("writes the already-sealed replica blob raw"),
        ),
        (
            "mail_body_plane.rs",
            "stage_sealed_body",
            Use::NoServe("splits and writes a sealed mail body; reads nothing"),
        ),
        (
            "mail_body_plane.rs",
            "stage_staged_body",
            Use::NoServe("seals, splits and writes a staged mail body; reads nothing"),
        ),
        (
            "link_preview_handlers.rs",
            "store_preview_image",
            Use::NoServe("writes the EXIF-stripped preview image; returns its hex hash"),
        ),
        (
            "content_index_handlers.rs",
            "index_record_handler",
            Use::NoServe(
                "`exists` on the caller-named digest to refuse an unresolvable index row \
                 (upload-before-record) — a presence bit, never bytes, and unchanged by a \
                 takedown for the same tombstone-not-delete reason as `check_chunks`",
            ),
        ),
        (
            "share_handlers.rs",
            "manifest_is_e2ee",
            Use::NoServe(
                "reads the token's manifest at MINT time to answer one bool — is this set \
                 content-key sealed — so a dead share link is refused at creation; the \
                 bytes never leave, and the serve-side 403 stays authoritative",
            ),
        ),
        (
            "sync_handlers.rs",
            "record_change_core",
            Use::NoServe(
                "`derive_held_custody_charge` sizes the manifest the change records so \
                 the charge is honest; what leaves is an integer and a bytes-not-held \
                 refusal",
            ),
        ),
        (
            "filesync_handlers.rs",
            "check_handler",
            Use::NoServe(
                "`integrity_check` walks the folder's manifests and chunks and answers \
                 counts plus structured errors — never the bytes it verified",
            ),
        ),
        (
            "admin_ws_handlers.rs",
            "gc_handler",
            Use::NoServe("the admin GC: deletes unreferenced blobs, answers counters"),
        ),
        (
            "backup/gc_scheduler.rs",
            "run_gc_cycle",
            Use::NoServe("the scheduled GC cycle; same deletes, same counters"),
        ),
        (
            "folder_handlers.rs",
            "update_handler",
            Use::NoServe(
                "the metadata-only residency flip drops this folder's nest-held chunk \
                 bytes (`drop_folder_chunk_bytes`) — a delete, not a read",
            ),
        ),
        (
            "chunk_relay.rs",
            "cache_fetched",
            Use::NoServe(
                "writes relay-fetched bytes, already verified against their address, \
                 into the store; reads nothing back",
            ),
        ),
        (
            "payload_store.rs",
            "store",
            Use::NoServe("writes an over-threshold payload under its content address"),
        ),
        // NOT LISTED — `render_for_actor` and `store_rendered` touch no
        // primitive of their own: both now write bytes only by calling
        // `store_render_body` below, which is where the census finds the
        // actual `self.blob_store` touch. (The reads `render_for_actor`
        // drives, `load_web_file_bytes` and `load_blob_bytes`, are each
        // classified in their own right, elsewhere in this table.)
        (
            "web_content/service.rs",
            "store_render_body",
            Use::NoServe(
                "writes the rendered body under its content hash and records its \
                 `blob_metadata` row; returns only the hash, never the bytes — its two \
                 callers, `store_rendered` and the sealed-page arm of `render_for_actor`, \
                 never see the bytes back either",
            ),
        ),
        (
            "lib.rs",
            "web_content_or_info",
            Use::NoServe(
                "reads nothing itself: it hands the web-content handle to \
                 `serve::serve_web_content_with_token`, whose every read of it is a call \
                 site classified here",
            ),
        ),
        (
            "lib.rs",
            "start_server",
            Use::NoServe(
                "reads nothing itself — it hands the handle to three long-lived holders, \
                 and each is held to this table rather than discharged in prose: every \
                 use of a held handle is an entry, and every holder method that hands \
                 stored bytes back is a primitive whose callers are entries (NEEDLES)",
            ),
        ),
    ];

    /// **Every route onto the blob store is classified** (`moderation.md`
    /// § Legal takedown → *The blob-serve door*).
    ///
    /// The legal-takedown withhold is a property of the BYTES, so it binds
    /// every route onto this store rather than one door — and the goal doc
    /// states that as a standing duty: a new route is enrolled on the day it
    /// is written. That duty went unchecked long enough for FOUR doors to ship
    /// ungated — `/api/v1/blob/{id}` itself until 2026-09-09, the video segment
    /// door until 2026-09-10, and the chunk and manifest doors until
    /// 2026-09-11 — each found only when somebody happened to look.
    ///
    /// This turns the duty into a gate, the blob-store twin of
    /// `segments::post`'s `every_flag_blind_post_body_read_is_partitioned`. It
    /// walks every production source file (unit-test modules cut), finds each
    /// call site onto the store ([`NEEDLES`]), and requires the enclosing fn to
    /// appear in [`PARTITION`] — as a reader-named door naming its gate, an
    /// owner-scoped serve path saying why no caller can aim it, a holder's
    /// primitive, or a caller from which no bytes leave. A new caller fails here
    /// until someone decides which it is; a removed one fails until its entry
    /// goes, so the table can never describe callers that no longer exist.
    ///
    /// Two more checks keep the table honest rather than merely complete: a
    /// door's gate is held to its production body, so deleting the gate call
    /// fails here and not only in the door's own witness; and a primitive must
    /// be a needle, or its callers would silently fall outside the table.
    #[test]
    fn every_blob_store_serve_path_is_partitioned() {
        let found = crate::partition_scan::callers_of(NEEDLES);

        let table: std::collections::BTreeSet<(String, String)> = PARTITION
            .iter()
            .map(|(file, func, _)| (file.to_string(), func.to_string()))
            .collect();
        assert_eq!(
            table.len(),
            PARTITION.len(),
            "a (file, fn) pair is listed twice in PARTITION"
        );
        for (_, _, used) in PARTITION {
            let (Use::Gated(_, why) | Use::Scoped(why) | Use::Primitive(why) | Use::NoServe(why)) =
                used;
            assert!(
                !why.trim().is_empty(),
                "every partition entry states its reason"
            );
        }
        for (file, func, used) in PARTITION {
            if let Use::Primitive(_) = used {
                let call = format!("{func}(");
                assert!(
                    NEEDLES.iter().any(|needle| needle.contains(call.as_str())),
                    "{file}::{func} is classified as a primitive, but no census needle names \
                     it, so its callers are not in the table — add `{call}` to NEEDLES"
                );
            }
        }
        let gated: Vec<(&str, &str, &Gate)> = PARTITION
            .iter()
            .filter_map(|(file, func, used)| match used {
                Use::Gated(gate, _) => Some((*file, *func, gate)),
                _ => None,
            })
            .collect();
        crate::partition_scan::assert_gates_hold(&gated);

        crate::partition_scan::assert_partitioned(
            &found,
            &table,
            "The legal-takedown withhold binds the BYTES, so every route onto this store \
             inherits it (`moderation.md` § Legal takedown → The blob-serve door). Decide \
             what this caller does and add it to PARTITION: a door that reads a digest the \
             CALLER named must call `blob_routes::legal_takedown_gate` before the store read \
             and before any arm that would fetch the bytes back from elsewhere, and name \
             that flow here; an owner-scoped serve path must say why no caller can aim it at \
             a digest; a caller from which no stored bytes leave must say what it does \
             instead.",
        );
    }
}
