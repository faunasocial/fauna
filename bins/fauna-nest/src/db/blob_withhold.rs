//! The legal-takedown **blob-serve withhold** set — the store side.
//!
//! `moderation.md` § Legal takedown withholds a taken-down record's body from
//! every per-record serve path, but a record's *attachments* are separate
//! content-addressed blobs served by the unauthenticated
//! `GET /api/v1/blob/{id}`, which knew nothing about the flag: anyone who had
//! already learned the cid (every member of the conversation, every viewer of
//! the post) kept fetching the bytes after the withhold. This table is what
//! that door consults — one indexed lookup, no walk, on a hot unauthenticated
//! path.
//!
//! **Derived, never a floor.** Every row is recomputable from
//! `legal_takedown_ref` joined against reference sets the box already holds
//! (`conv_attachment_refs` for conversations, the live post bodies for posts);
//! it records nothing new about user data and is rebuilt wholesale rather than
//! maintained incrementally. The predicate and the walk live in
//! [`crate::moderation_withhold`]; this module is only its SQL.
//!
//! **Withheld is not deleted.** The GC pin is untouched — a taken-down record
//! is a live mirror row and keeps pinning its blobs — so `restore=true`
//! re-serves the very same bytes (tombstone-not-delete).

use super::CacheDb;
use anyhow::{Context, Result};
use rusqlite::OptionalExtension;
use std::collections::HashSet;

impl CacheDb {
    /// Is this blob withheld by a legal takedown? The blob-serve door's whole
    /// question — one primary-key probe, so it is affordable on every
    /// `GET /api/v1/blob/{id}`.
    pub async fn blob_is_legally_withheld(&self, blob_hash: &[u8; 32]) -> Result<bool> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare_cached("SELECT 1 FROM blob_legal_withhold WHERE blob_hash = ?1")
            .context("prepare blob_is_legally_withheld")?;
        let found = stmt
            .exists(rusqlite::params![&blob_hash[..]])
            .context("query blob_is_legally_withheld")?;
        Ok(found)
    }

    /// The whole withheld set, for a caller that must test **many** digests
    /// against it in one pass — the folder mirror
    /// (`segment_backup::NestBackupCoordinator::run_folder_once`) checks every
    /// store key of every path it would push or has pushed. The set is small
    /// (the attachments of the box's taken-down records) and empty on nearly
    /// every box, so one read per pass and an in-memory membership test beats
    /// a probe per chunk; a hot per-request door keeps using
    /// [`Self::blob_is_legally_withheld`].
    pub async fn list_blob_legal_withhold(&self) -> Result<HashSet<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare_cached("SELECT blob_hash FROM blob_legal_withhold")
            .context("prepare list_blob_legal_withhold")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, Vec<u8>>(0))
            .context("query list_blob_legal_withhold")?;
        let mut out = HashSet::new();
        for row in rows {
            let bytes = row.context("read a withheld blob hash")?;
            if let Ok(digest) = <[u8; 32]>::try_from(bytes.as_slice()) {
                out.insert(digest);
            }
        }
        Ok(out)
    }

    /// Replace the withheld set wholesale, in one transaction.
    ///
    /// Wholesale because the predicate is a *difference* of two sets that both
    /// move as records come and go ([`crate::moderation_withhold`]): an
    /// incremental edit would need a hook on every record write and delete on
    /// the box, and a hook that is ever missed drifts in the direction that
    /// keeps serving legally compelled bytes. Recomputing costs one walk on a
    /// rare admin act and one on a sweep that already walks.
    ///
    /// ⚠ Only ever call this from a **complete** walk. A walk that could not
    /// read every live record does not know which blobs the records it missed
    /// name, so replacing from it can *release* a blob whose only remaining
    /// namer is one of them — the one direction a compelled withhold may never
    /// fail in. An incomplete walk uses [`Self::add_blob_legal_withhold`].
    pub async fn replace_blob_legal_withhold(&self, hashes: &[[u8; 32]]) -> Result<usize> {
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin replace_blob_legal_withhold")?;
        tx.execute("DELETE FROM blob_legal_withhold", [])
            .context("clear blob_legal_withhold")?;
        {
            let mut stmt = tx
                .prepare("INSERT OR IGNORE INTO blob_legal_withhold (blob_hash) VALUES (?1)")
                .context("prepare insert blob_legal_withhold")?;
            for h in hashes {
                stmt.execute(rusqlite::params![&h[..]])
                    .context("insert blob_legal_withhold row")?;
            }
        }
        tx.commit().context("commit replace_blob_legal_withhold")?;
        Ok(hashes.len())
    }

    /// Add to the withheld set without clearing it — what an **incomplete**
    /// walk is allowed to do.
    ///
    /// A run that could not read every live record still knows that the
    /// records it *did* read under a takedown name these blobs, so withholding
    /// them is sound; what it cannot justify is releasing anything, because a
    /// record it failed to read may have been some blob's last flagged namer.
    /// So a partial walk only ever adds: it fails toward over-withholding,
    /// which the next complete walk corrects, rather than toward serving
    /// legally compelled bytes, which nothing corrects.
    pub async fn add_blob_legal_withhold(&self, hashes: &[[u8; 32]]) -> Result<usize> {
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .context("begin add_blob_legal_withhold")?;
        {
            let mut stmt = tx
                .prepare("INSERT OR IGNORE INTO blob_legal_withhold (blob_hash) VALUES (?1)")
                .context("prepare insert blob_legal_withhold")?;
            for h in hashes {
                stmt.execute(rusqlite::params![&h[..]])
                    .context("insert blob_legal_withhold row")?;
            }
        }
        tx.commit().context("commit add_blob_legal_withhold")?;
        Ok(hashes.len())
    }

    /// Every post currently under a legal takedown. Tiny by construction (a
    /// legal takedown is a rare, compelled act), so the withhold walk carries
    /// it as a set and asks it per post rather than joining per row.
    pub async fn taken_down_post_ids(&self) -> Result<HashSet<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT content_id FROM content_meta WHERE legal_takedown_ref IS NOT NULL")
            .context("prepare taken_down_post_ids")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, Vec<u8>>(0))
            .context("query taken_down_post_ids")?;
        let mut out = HashSet::new();
        for row in rows {
            let id = row.context("read content_meta.content_id")?;
            if let Ok(arr) = <[u8; 32]>::try_from(id.as_slice()) {
                out.insert(arr);
            }
        }
        Ok(out)
    }

    /// Record that `post_id` was destroyed by its own author **while under a
    /// legal takedown**, together with the blob digests the record named — the
    /// one moment the compelled fact and those digests are both still
    /// readable (`moderation.md` § Legal takedown → *Posts*).
    ///
    /// Idempotent (`INSERT OR REPLACE`): a delete retry after a crash lands
    /// the same row. Called only from inside the delete, only when the flag
    /// was live, so an ordinary delete writes nothing here.
    pub async fn record_taken_down_post_deleted(
        &self,
        post_id: &[u8; 32],
        author: &[u8; 32],
        legal_reference: &str,
        blob_digests: &[[u8; 32]],
        deleted_at_us: i64,
    ) -> Result<()> {
        let flat: Vec<u8> = blob_digests
            .iter()
            .flat_map(|d| d.iter().copied())
            .collect();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO legal_takedown_deleted_posts
                (post_id, author_id, legal_reference, blob_digests, deleted_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                post_id.as_slice(),
                author.as_slice(),
                legal_reference,
                flat,
                deleted_at_us
            ],
        )
        .context("insert legal_takedown_deleted_posts")?;
        Ok(())
    }

    /// The post ids an author deleted while taken down — what the export's
    /// segment-pair withhold set unions with the still-flagged live posts, so
    /// a pair is withheld whether the compelled record is live-and-flagged or
    /// tombstoned-and-uncompacted.
    pub async fn taken_down_deleted_post_ids_for_author(
        &self,
        author: &[u8; 32],
    ) -> Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT post_id FROM legal_takedown_deleted_posts WHERE author_id = ?1")
            .context("prepare taken_down_deleted_post_ids_for_author")?;
        let rows = stmt
            .query_map(rusqlite::params![author.as_slice()], |r| {
                r.get::<_, Vec<u8>>(0)
            })
            .context("query taken_down_deleted_post_ids_for_author")?;
        let mut out = Vec::new();
        for row in rows {
            let id = row.context("read legal_takedown_deleted_posts.post_id")?;
            if let Ok(arr) = <[u8; 32]>::try_from(id.as_slice()) {
                out.push(arr);
            }
        }
        Ok(out)
    }

    /// Every blob digest named by a post its author deleted while taken down
    /// — the withheld side's second source, beside the live flagged records.
    ///
    /// The blob door's rebuild cannot recover these by reading: a tombstoned
    /// record's body answers `None`, which is exactly why they are captured in
    /// the delete. A digest here is still only *withheld* if no live unflagged
    /// record also names it — the exemption in `moderation.md` § the
    /// blob-serve door is applied by the caller, over both sources at once.
    pub async fn taken_down_deleted_post_blob_digests(&self) -> Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT blob_digests FROM legal_takedown_deleted_posts")
            .context("prepare taken_down_deleted_post_blob_digests")?;
        let rows = stmt
            .query_map([], |r| r.get::<_, Vec<u8>>(0))
            .context("query taken_down_deleted_post_blob_digests")?;
        let mut out = Vec::new();
        for row in rows {
            let flat = row.context("read legal_takedown_deleted_posts.blob_digests")?;
            // `as_chunks` yields `[u8; 32]` directly, so the digests need no
            // per-chunk `try_from`. A trailing partial chunk lands in `.1` and
            // is dropped, exactly as `chunks_exact` dropped it.
            out.extend_from_slice(flat.as_chunks::<32>().0);
        }
        Ok(out)
    }

    /// One `legal_takedown_deleted_posts` row by its post id — `(author,
    /// legal_reference)`, or `None` if `post_id` was never recorded there.
    ///
    /// What the legal-takedown handler's `restore=true` arm needs to decide
    /// an overturn against a post whose content row is already gone: the row
    /// this reads is the permanent, inert floor (`moderation.md` § Legal
    /// takedown → *Posts*) — a `Some` here is enough on its own to answer
    /// "restored" (audit only, nothing to clear), without needing the
    /// segment to still hold the bytes (compaction may have reclaimed them
    /// long ago; the floor holds regardless).
    pub async fn get_taken_down_deleted_post(
        &self,
        post_id: &[u8; 32],
    ) -> Result<Option<([u8; 32], String)>> {
        let conn = self.conn.lock().await;
        let result: Option<(Vec<u8>, String)> = conn
            .query_row(
                "SELECT author_id, legal_reference FROM legal_takedown_deleted_posts \
                 WHERE post_id = ?1",
                rusqlite::params![post_id.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .context("get_taken_down_deleted_post")?;
        Ok(result.and_then(|(a, r)| <[u8; 32]>::try_from(a.as_slice()).ok().map(|a| (a, r))))
    }

    /// Every attachment hash a LIVE conversation record names, paired with
    /// whether that record is under a legal takedown.
    ///
    /// The conversation half of the withhold predicate, in one query: the same
    /// `conv_attachment_refs ⋈ segment_records` liveness join the GC's step 2g
    /// pin uses, split on `legal_takedown_ref` instead of collapsed. A hash
    /// named by both a flagged and an unflagged live record appears twice —
    /// the caller's set difference is what resolves it, and the answer is
    /// "served", because a takedown withholds a record, never bytes another
    /// live record still stands behind.
    pub async fn conv_attachment_refs_by_takedown(&self) -> Result<Vec<([u8; 32], bool)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT r.blob_hash, s.legal_takedown_ref IS NOT NULL
                   FROM conv_attachment_refs r
                   JOIN segment_records s
                     ON s.scope_id = r.channel_id
                    AND s.kind = 'conv'
                    AND s.seq = r.seq
                    AND s.tombstoned = 0",
            )
            .context("prepare conv_attachment_refs_by_takedown")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, bool>(1)?)))
            .context("query conv_attachment_refs_by_takedown")?;
        let mut out = Vec::new();
        for row in rows {
            let (hash, flagged) = row.context("read conv_attachment_refs_by_takedown row")?;
            if let Ok(arr) = <[u8; 32]>::try_from(hash.as_slice()) {
                out.push((arr, flagged));
            }
        }
        Ok(out)
    }
}
