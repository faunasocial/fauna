//! Full-text search and document indexing methods.
//!
//! **`content_id` is emitted as LOWERCASE hex** — `lower(hex(...))`, not bare
//! `hex(...)`, at all three query sites below. SQLite's `hex()` is uppercase,
//! which made this the one 32-byte-id surface in the nest that disagreed with
//! every other: `fauna.posts.create`/`get`, actor ids and
//! `fauna_core::hex32::encode` all speak lowercase, and `hex32`'s own doc
//! comment calls lowercase the convention. The divergence was invisible while
//! `content_id` had a single producer, but the Search page now **merges two
//! backends and dedups post-class rows by `(kind class, content_id)` as raw
//! strings** (`ui/search.md` § State & data shape) — a spelling mismatch there
//! doesn't error, it just silently never matches, so every post present in both
//! backends would render twice and "local wins" would never fire.
//! `content_id` is not persisted anywhere (search.md § Persistence: none) and
//! every writer class stores exactly 32 bytes, so lowercasing is total,
//! lossless and needs no migration. Clients dedup on the raw string, so
//! this lowercase spelling is part of the wire contract.

use super::{
    CacheDb, SearchResult, content_id_for_document, now_epoch_micros, sanitize_fts_query, search,
};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

impl CacheDb {
    /// Index a document in the unified FTS5 search index.
    /// Idempotent: re-indexing the same (content_type, content_id) replaces the old entry.
    pub async fn index_document(
        &self,
        content_type: &str,
        content_id: &str,
        title: &str,
        body: &str,
        author_name: &str,
        tags: &str,
        created_at: i64,
    ) -> Result<()> {
        let content_type = content_type.to_string();
        let content_id = content_id.to_string();
        let title = title.to_string();
        let body = body.to_string();
        let author_name = author_name.to_string();
        let tags = tags.to_string();
        let conn = self.conn.lock().await;
        Self::index_document_inner(
            &conn,
            &content_type,
            &content_id,
            &title,
            &body,
            &author_name,
            &tags,
            created_at,
        )
    }

    /// Index a profile for full-text search. Idempotent — re-indexes on update.
    ///
    /// TEST-ONLY: production profile rows have one writer,
    /// [`sync_profile_row`], which derives the row from `users.handle` so it
    /// can never name a handle its actor no longer holds.
    #[cfg(test)]
    pub async fn index_profile(
        &self,
        actor_id: &[u8; 32],
        display_name: &str,
        bio: &str,
    ) -> Result<()> {
        let actor_hex = hex::encode(actor_id);
        // Epoch MICROSECONDS — the unit `content.created_at` stores and the
        // unit `SearchRequest`'s `before`/`after` cursors are declared in
        // (`fauna_protocol::search`). The search window compares
        // `COALESCE(c.created_at, m.created_at)`, so this stamp must share
        // `content.created_at`'s unit or a profile hit lands at ~1970 and every
        // `after` window silently drops it.
        //
        // This line previously read `now_epoch_secs()` on the strength of a
        // comment asserting the `content` table used seconds. It does not, and
        // never did — that claim was contradicted by `Timestamp` (micros), by
        // `profile_handlers.rs`'s own "matching every other `content` row"
        // note, and by ten of the eleven production writers.
        let now = now_epoch_micros();
        self.index_document("profile", &actor_hex, display_name, bio, "", "", now)
            .await
    }

    fn index_document_inner(
        conn: &Connection,
        content_type: &str,
        content_id_str: &str,
        title: &str,
        body: &str,
        author_name: &str,
        tags: &str,
        created_at: i64,
    ) -> Result<()> {
        let blob_key = content_id_for_document(content_type, content_id_str);
        Self::remove_document_inner(conn, content_type, content_id_str)?;
        search::index_content(
            conn,
            &blob_key,
            content_type,
            title,
            body,
            author_name,
            tags,
            created_at,
        )
    }

    /// Remove a document from the FTS5 search index by (content_type, content_id).
    pub async fn remove_document(&self, content_type: &str, content_id: &str) -> Result<()> {
        let content_type = content_type.to_string();
        let content_id = content_id.to_string();
        let conn = self.conn.lock().await;
        Self::remove_document_inner(&conn, &content_type, &content_id)
    }

    fn remove_document_inner(
        conn: &Connection,
        content_type: &str,
        content_id_str: &str,
    ) -> Result<()> {
        let blob_key = content_id_for_document(content_type, content_id_str);
        search::remove_content(conn, &blob_key)
    }

    /// Query the FTS5 search index.
    ///
    /// Every serve path's `snippet()` passes column **-1** — FTS5's "the column
    /// that matched" — never a fixed one: a hit's snippet is the text that
    /// matched (`ui/search.md` § State & data shape), and a profile is matched
    /// on its NAME with an empty bio, so a body-pinned snippet painted every
    /// such row blank (`db::tests::a_hit_snippets_the_column_that_matched`).
    ///
    /// A `content_type` filter names a CLASS — the type itself or any
    /// `<type>/<subtype>` of it — because a post is stored under its body
    /// subtype (`post/text`, `post/media`, …) while the Search page filters on
    /// `post`; an exact match found no post at all
    /// (`db::tests::a_post_filter_finds_every_post_subtype_and_nothing_else`).
    pub async fn search_fts(
        &self,
        query: &str,
        content_type: Option<&str>,
        before: Option<i64>,
        after: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<SearchResult>> {
        let query = match sanitize_fts_query(query) {
            Some(q) => q,
            None => return Ok(Vec::new()),
        };
        let content_type = content_type.map(|s| s.to_string());
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT f.schema, lower(hex(m.content_id)), COALESCE(c.created_at, m.created_at), f.rank, \
                    snippet(content_fts, -1, '<b>', '</b>', '\u{2026}', 32) \
             FROM content_fts f \
             JOIN content_fts_map m ON m.fts_rowid = f.rowid \
             LEFT JOIN content c ON c.id = m.content_id \
             WHERE content_fts MATCH ?1 \
               AND (?2 IS NULL OR f.schema = ?2 OR f.schema LIKE ?2 || '/%') \
               AND (?3 IS NULL OR COALESCE(c.created_at, m.created_at) < ?3) \
               AND (?4 IS NULL OR COALESCE(c.created_at, m.created_at) > ?4) \
             ORDER BY f.rank \
             LIMIT ?5 OFFSET ?6",
            )
            .context("prepare search_fts")?;
        let rows = stmt
            .query_map(
                rusqlite::params![query, content_type, before, after, limit, offset],
                |row| {
                    Ok(SearchResult {
                        content_type: row.get(0)?,
                        content_id: row.get(1)?,
                        created_at: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                        rank: row.get(3)?,
                        snippet: row.get(4)?,
                    })
                },
            )
            .context("query search_fts")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read search result")?);
        }
        Ok(results)
    }

    /// Search with access control: public content visible to all, bridge messages only to owner.
    pub async fn search_with_scoping(
        &self,
        query: &str,
        actor_id: &[u8; 32],
        content_type_filter: Option<&str>,
        before: Option<i64>,
        after: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<SearchResult>> {
        let mut results = Vec::new();
        // Fetch (limit + offset) from each sub-query so we can merge-sort correctly,
        // then apply offset after merging.
        let fetch = limit + offset;

        // Phase 1: public content. A bridge-corpus row (`bridge.<id>`) is
        // public content by the policy's own class split — a bridge's PUBLIC
        // posts/events are exactly what may enter this table, and its private
        // content is structurally excluded at ingest — so it belongs in this
        // phase, not the phase-2 own-content arm (which scopes by
        // `content.author = actor`, a join a foreign-authored bridge row has no
        // row for). `content-index.md` § Bridge content in the Search corpus.
        let is_public_type = |t: &str| {
            t == "profile"
                || t.starts_with("post/")
                || t == "post"
                || fauna_protocol::bridge_search_policy::is_bridge_content_type(t)
        };
        let search_public =
            content_type_filter.is_none() || content_type_filter.is_some_and(&is_public_type);
        if search_public {
            let type_filter = if content_type_filter.is_some() {
                content_type_filter
            } else {
                None // We'll filter in the query
            };
            let public = self
                .search_fts_public(query, type_filter, before, after, fetch, 0)
                .await?;
            results.extend(public);
        }

        // Phase 2: user's bridge messages
        let search_bridge =
            content_type_filter.is_none() || !content_type_filter.is_some_and(is_public_type);
        if search_bridge {
            let bridge = self
                .search_fts_bridge(
                    query,
                    actor_id,
                    content_type_filter,
                    before,
                    after,
                    fetch,
                    0,
                )
                .await?;
            results.extend(bridge);
        }

        // Sort by rank (ascending = best first with negative BM25), skip offset, take limit
        results.sort_by(|a, b| {
            a.rank
                .partial_cmp(&b.rank)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if offset > 0 {
            results = results.into_iter().skip(offset as usize).collect();
        }
        results.truncate(limit as usize);
        Ok(results)
    }

    async fn search_fts_public(
        &self,
        query: &str,
        content_type: Option<&str>,
        before: Option<i64>,
        after: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<SearchResult>> {
        let query = match sanitize_fts_query(query) {
            Some(q) => q,
            None => return Ok(Vec::new()),
        };
        let conn = self.conn.lock().await;
        let (type_clause, type_param): (String, Option<String>) = match content_type {
            // A filter names a CLASS: `post` must reach every `post/<subtype>`
            // a post is stored under (`post_body_schema`), while a full subtype
            // still narrows to itself — see `search_fts`.
            Some(t) => (
                "AND (f.schema = ?2 OR f.schema LIKE ?2 || '/%')".to_string(),
                Some(t.to_string()),
            ),
            None => (
                "AND (f.schema LIKE 'post/%' OR f.schema = 'post' OR f.schema = 'profile' \
                      OR f.schema LIKE 'bridge.%')"
                    .to_string(),
                None,
            ),
        };
        // `COALESCE(c.created_at, m.created_at)`: a bridge-corpus row is keyed
        // by blake3(content_type:natural_id), so it has no `content` row to
        // join — without the fallback its timestamp reads NULL and every
        // before/after comparison drops it silently, which would make the
        // Search page's own time window the thing that hides bridge results.
        //
        // The moderation gate: every hit carries a SNIPPET of the indexed body
        // text, so this is a serve path for the post body, to any signed-in
        // caller, and a flagged post must not surface here any more than in a
        // feed (`moderation.md` § Legal takedown → *Posts*). Until 2026-09-10 it
        // applied no flag at all — searching for a word of a taken-down post
        // quoted the withheld body back. Profile and bridge-corpus rows have no
        // `content_meta` row, carry no flag, and pass (the LEFT JOIN).
        let sql = format!(
            "SELECT f.schema, lower(hex(m.content_id)), COALESCE(c.created_at, m.created_at), f.rank, \
                    snippet(content_fts, -1, '<b>', '</b>', '\u{2026}', 32) \
             FROM content_fts f \
             JOIN content_fts_map m ON m.fts_rowid = f.rowid \
             LEFT JOIN content c ON c.id = m.content_id \
             LEFT JOIN content_meta cm ON cm.content_id = m.content_id \
             WHERE content_fts MATCH ?1 \
               {type_clause} \
               AND {moderation} \
               AND (?3 IS NULL OR COALESCE(c.created_at, m.created_at) < ?3) \
               AND (?4 IS NULL OR COALESCE(c.created_at, m.created_at) > ?4) \
             ORDER BY f.rank \
             LIMIT ?5 OFFSET ?6",
            moderation = super::public_servability::MODERATION_SERVABLE,
        );
        let mut stmt = conn.prepare(&sql).context("prepare search_fts_public")?;
        let rows = stmt
            .query_map(
                rusqlite::params![query, type_param, before, after, limit, offset],
                |row| {
                    Ok(SearchResult {
                        content_type: row.get(0)?,
                        content_id: row.get(1)?,
                        created_at: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                        rank: row.get(3)?,
                        snippet: row.get(4)?,
                    })
                },
            )
            .context("query search_fts_public")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read public search result")?);
        }
        Ok(results)
    }

    async fn search_fts_bridge(
        &self,
        query: &str,
        actor_id: &[u8; 32],
        bridge_type_filter: Option<&str>,
        before: Option<i64>,
        after: Option<i64>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<SearchResult>> {
        let query_san = match sanitize_fts_query(query) {
            Some(q) => q,
            None => return Ok(Vec::new()),
        };
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;

        // Bridge messages are in content table with source != 'fauna'.
        // Scope to this user's content by content.author = actor_id.
        //
        // NB two different senses of "bridge" meet here: this phase serves the
        // user's OWN bridge-delivered content (mail and friends — real `content`
        // rows, `source != 'fauna'`), while a `bridge.<id>` schema is the
        // foreign-authored PUBLIC corpus, which is phase-1 work. Counting the
        // latter as public here is what keeps an explicit
        // `content_type = "bridge.nostr"` filter from being routed into this
        // author-scoped query, where it could only ever return nothing.
        let is_public = |t: &str| {
            t == "profile"
                || t.starts_with("post/")
                || t == "post"
                || fauna_protocol::bridge_search_policy::is_bridge_content_type(t)
        };
        let (source_clause, source_param): (String, Option<String>) = match bridge_type_filter {
            Some(bt) if !is_public(bt) => ("AND c.source = ?3".to_string(), Some(bt.to_string())),
            _ => ("AND c.source != 'fauna'".to_string(), None),
        };
        // The caller is the AUTHOR here, so only the legal-takedown arm binds:
        // a takedown withholds the body from its author too, while quarantine
        // is author-visible by design — the same split `fauna.posts.list`
        // makes (`db/content.rs::list_authored_posts_page`). An archive import
        // is authored by the importer with a platform `source`, so a
        // taken-down one would otherwise be quoted back to its author here.
        let sql = format!(
            "SELECT f.schema, lower(hex(m.content_id)), c.created_at, f.rank, \
                    snippet(content_fts, -1, '<b>', '</b>', '\u{2026}', 32) \
             FROM content_fts f \
             JOIN content_fts_map m ON m.fts_rowid = f.rowid \
             JOIN content c ON c.id = m.content_id \
             LEFT JOIN content_meta cm ON cm.content_id = c.id \
             WHERE content_fts MATCH ?1 \
               AND c.author = ?2 \
               {source_clause} \
               AND cm.legal_takedown_ref IS NULL \
               AND (?4 IS NULL OR c.created_at < ?4) \
               AND (?5 IS NULL OR c.created_at > ?5) \
             ORDER BY f.rank \
             LIMIT ?6 OFFSET ?7"
        );
        let mut stmt = conn.prepare(&sql).context("prepare search_fts_bridge")?;
        let rows = stmt
            .query_map(
                rusqlite::params![
                    query_san,
                    actor_id.as_slice(),
                    source_param,
                    before,
                    after,
                    limit,
                    offset
                ],
                |row| {
                    Ok(SearchResult {
                        content_type: row.get(0)?,
                        content_id: row.get(1)?,
                        created_at: row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                        rank: row.get(3)?,
                        snippet: row.get(4)?,
                    })
                },
            )
            .context("query search_fts_bridge")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read bridge search result")?);
        }
        Ok(results)
    }
}

/// Re-derive `actor_id`'s `profile` row from `users.handle` — the ONE writer
/// of a profile row. A user holding a non-empty handle has exactly one row,
/// naming that handle; any other actor (handle cleared, account deleted,
/// identity succeeded) has none.
///
/// A profile row mirrors no `content` row: it is keyed on
/// `blake3("profile:" + hex(actor))`, so no content purge reaches it and no
/// column walk sees the actor inside the key. It used to be written once at
/// account creation and never again, which left every handle the nest ever
/// issued in the corpus — quoted back verbatim by the `snippet(-1)` serve and
/// enumerable with a `raw:` prefix query, and linkable to its account by
/// anyone holding the actor id. So every statement that writes or clears
/// `users.handle`, or deletes a `users` row, calls this on the same connection
/// (inside its transaction, where it has one); [`reconcile_profile_rows`]
/// heals whatever a writer without it left behind.
///
/// Idempotent: a row already naming the held handle is left untouched, keeping
/// its recency stamp.
pub(crate) fn sync_profile_row(conn: &Connection, actor_id: &[u8]) -> Result<bool> {
    let handle: Option<String> = conn
        .query_row(
            "SELECT handle FROM users WHERE actor_id = ?1",
            rusqlite::params![actor_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .context("read handle for the profile row")?
        .flatten()
        .filter(|h| !h.is_empty());
    let actor_hex = hex::encode(actor_id);
    let key = content_id_for_document("profile", &actor_hex);
    let indexed: Option<String> = conn
        .query_row(
            "SELECT f.title FROM content_fts_map m \
             JOIN content_fts f ON f.rowid = m.fts_rowid \
             WHERE m.content_id = ?1",
            rusqlite::params![key.as_slice()],
            |row| row.get(0),
        )
        .optional()
        .context("read the indexed profile row")?;
    match handle {
        Some(h) if indexed.as_deref() == Some(h.as_str()) => Ok(false),
        Some(h) => {
            // Epoch MICROSECONDS — see `index_profile`'s note on the unit.
            CacheDb::index_document_inner(
                conn,
                "profile",
                &actor_hex,
                &h,
                "",
                "",
                "",
                now_epoch_micros(),
            )?;
            Ok(true)
        }
        None if indexed.is_some() => {
            search::remove_content(conn, &key)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

/// Re-derive EVERY `profile` row from `users` — STANDING, run at each boot
/// (`migrations::run_migrations`): drop the rows whose actor has no user or no
/// handle, re-index the ones naming a handle their actor no longer holds, and
/// index each handled account that has none. Returns how many rows it changed.
///
/// This rewrites derived data only — the index is rebuildable from
/// `users.handle` at any time — so dropping a row here loses nothing a user
/// cannot see again (no user-data-loss question). Standing rather than
/// one-shot because a writer that bypasses [`sync_profile_row`] (an older
/// binary against this database, or a future site) leaves residue this heals
/// on the next boot.
pub(crate) fn reconcile_profile_rows(conn: &Connection) -> Result<usize> {
    let mut held: std::collections::HashMap<[u8; 32], Vec<u8>> = std::collections::HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT actor_id FROM users WHERE handle IS NOT NULL AND handle != ''")
            .context("prepare handled-user scan")?;
        let actors = stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .context("scan handled users")?;
        for actor in actors {
            let actor = actor?;
            held.insert(
                content_id_for_document("profile", &hex::encode(&actor)),
                actor,
            );
        }
    }
    let indexed: Vec<Vec<u8>> = {
        let mut stmt = conn
            .prepare(
                "SELECT m.content_id FROM content_fts_map m \
                 JOIN content_fts f ON f.rowid = m.fts_rowid \
                 WHERE f.schema = 'profile'",
            )
            .context("prepare profile-row scan")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .context("scan profile rows")?;
        rows.collect::<Result<_, _>>()?
    };

    let mut changed = 0;
    for key in indexed {
        let Ok(key) = <[u8; 32]>::try_from(key.as_slice()) else {
            continue;
        };
        if !held.contains_key(&key) {
            search::remove_content(conn, &key)?;
            changed += 1;
        }
    }
    for actor in held.values() {
        if sync_profile_row(conn, actor)? {
            changed += 1;
        }
    }
    Ok(changed)
}

/// The `profile` row follows `users.handle` — every handle this nest ever
/// issued used to stay in the Search corpus, quoted back verbatim by the
/// `snippet(-1)` serve and enumerable through a `raw:` prefix query
/// (`ui/search.md` § State & data shape; `principles.md` § The user always
/// controls their data). Each transition below has its witness, and every
/// witness asks under a `raw:` prefix too, since that is the enumeration a
/// retired name must not survive.
#[cfg(test)]
mod profile_row_tests {
    use super::*;

    const ACTOR: [u8; 32] = [0x5A; 32];
    const NEW: [u8; 32] = [0x5B; 32];

    fn key(actor: &[u8; 32]) -> String {
        hex::encode(content_id_for_document("profile", &hex::encode(actor)))
    }

    /// Every profile hit's `content_id` for `query`, on the unscoped arm and
    /// the one `fauna.search.query` serves.
    async fn profile_hits(db: &CacheDb, query: &str) -> Vec<String> {
        let mut hits: Vec<String> = db
            .search_fts(query, Some("profile"), None, None, 100, 0)
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.content_id)
            .collect();
        let scoped = db
            .search_with_scoping(query, &[0xEE; 32], Some("profile"), None, None, 100, 0)
            .await
            .unwrap();
        hits.extend(scoped.into_iter().map(|r| r.content_id));
        hits.sort();
        hits.dedup();
        hits
    }

    async fn open() -> CacheDb {
        let db = CacheDb::open_in_memory().unwrap();
        db.conn
            .lock()
            .await
            .execute(
                "INSERT OR IGNORE INTO tiers (name, max_inbox_bytes, max_storage_bytes, \
                 max_devices, max_blob_size) VALUES ('free', 1, 1, 1, 1)",
                [],
            )
            .unwrap();
        db
    }

    async fn created(handle: &str) -> CacheDb {
        let db = open().await;
        db.create_user_with_handle(&ACTOR, "free", handle, None)
            .await
            .unwrap();
        db
    }

    #[tokio::test]
    async fn an_account_created_under_a_handle_is_found_by_it() {
        let db = created("quillon").await;
        assert_eq!(profile_hits(&db, "quillon").await, vec![key(&ACTOR)]);
    }

    #[tokio::test]
    async fn a_renamed_account_is_found_by_its_new_handle_and_never_its_old() {
        let db = created("quillon").await;
        db.set_handle(&ACTOR, "tessellate").await.unwrap();

        assert_eq!(profile_hits(&db, "tessellate").await, vec![key(&ACTOR)]);
        assert!(profile_hits(&db, "quillon").await.is_empty());
        assert!(profile_hits(&db, "raw:quil*").await.is_empty());
        assert!(profile_hits(&db, "raw:q*").await.is_empty());
    }

    #[tokio::test]
    async fn a_cleared_handle_leaves_no_profile_hit() {
        let db = created("quillon").await;
        db.set_handle(&ACTOR, "").await.unwrap();
        assert!(profile_hits(&db, "raw:q*").await.is_empty());

        // The cooldown release is the same clear by another door.
        db.set_handle(&ACTOR, "quillon").await.unwrap();
        assert_eq!(profile_hits(&db, "quillon").await, vec![key(&ACTOR)]);
        db.release_handle_with_cooldown(&ACTOR, "quillon")
            .await
            .unwrap();
        assert!(profile_hits(&db, "raw:q*").await.is_empty());
    }

    #[tokio::test]
    async fn a_deleted_account_leaves_no_profile_hit() {
        let db = created("quillon").await;
        assert!(db.delete_user(&ACTOR).await.unwrap());
        assert!(profile_hits(&db, "raw:q*").await.is_empty());
    }

    // An evicted account's hit goes in the eviction ladder's finalize, which
    // needs `AppState`: `eviction::tests::an_evicted_account_leaves_no_profile_hit`.

    #[tokio::test]
    async fn a_succession_moves_the_handle_hit_to_the_successor() {
        let db = created("quillon").await;
        db.record_succession(&ACTOR, &NEW, b"s", 1)
            .await
            .unwrap()
            .expect("succession applies");
        assert_eq!(profile_hits(&db, "quillon").await, vec![key(&NEW)]);
        assert_eq!(profile_hits(&db, "raw:q*").await, vec![key(&NEW)]);
    }

    /// The standing boot reconcile re-derives every profile row from
    /// `users.handle`: a row for an actor with no user, one naming a handle its
    /// actor no longer holds, and a handled account with no row at all are all
    /// healed; a correct row is left alone.
    #[tokio::test]
    async fn the_reconcile_rederives_every_profile_row_from_users() {
        let db = open().await;
        let (gone, renamed, missing, fine) = ([1u8; 32], [2u8; 32], [3u8; 32], [4u8; 32]);
        // The stale residue: rows written once and never followed, written
        // straight to the index the way a writer that bypasses
        // `sync_profile_row` leaves them.
        {
            let conn = db.conn.lock().await;
            for (actor, handle) in [
                (gone, "departed"),
                (renamed, "formername"),
                (fine, "steady"),
            ] {
                CacheDb::index_document_inner(
                    &conn,
                    "profile",
                    &hex::encode(actor),
                    handle,
                    "",
                    "",
                    "",
                    1,
                )
                .unwrap();
            }
            for (actor, handle) in [
                (renamed, "brandnew"),
                (missing, "unindexed"),
                (fine, "steady"),
            ] {
                conn.execute(
                    "INSERT INTO users (actor_id, tier, label, created_at, handle) \
                     VALUES (?1, 'free', '', 1, ?2)",
                    rusqlite::params![actor.as_slice(), handle],
                )
                .unwrap();
            }
        }

        let healed = {
            let conn = db.conn.lock().await;
            reconcile_profile_rows(&conn).unwrap()
        };
        assert_eq!(healed, 3, "gone + renamed + missing");

        assert!(profile_hits(&db, "departed").await.is_empty());
        assert!(profile_hits(&db, "formername").await.is_empty());
        assert_eq!(profile_hits(&db, "brandnew").await, vec![key(&renamed)]);
        assert_eq!(profile_hits(&db, "unindexed").await, vec![key(&missing)]);
        assert_eq!(profile_hits(&db, "steady").await, vec![key(&fine)]);

        // Idempotent: a second boot finds nothing to heal.
        let conn = db.conn.lock().await;
        assert_eq!(reconcile_profile_rows(&conn).unwrap(), 0);
    }

    /// The heal runs where a restored backup meets it — every `CacheDb::open`
    /// (`migrations::run_migrations`), not only a caller that invokes the pass
    /// by hand. A database carrying a retired handle's row, left by a writer
    /// that bypassed [`sync_profile_row`], comes back from a reopen without it.
    #[tokio::test]
    async fn a_reopened_database_quotes_no_retired_handle() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("nest.db");
        {
            let db = CacheDb::open(&path).unwrap();
            db.conn
                .lock()
                .await
                .execute(
                    "INSERT OR IGNORE INTO tiers (name, max_inbox_bytes, max_storage_bytes, \
                     max_devices, max_blob_size) VALUES ('free', 1, 1, 1, 1)",
                    [],
                )
                .unwrap();
            db.create_user_with_handle(&ACTOR, "free", "quillon", None)
                .await
                .unwrap();
            // The bypassing writer: the handle clears under the index.
            db.conn
                .lock()
                .await
                .execute(
                    "UPDATE users SET handle = '' WHERE actor_id = ?1",
                    rusqlite::params![ACTOR.as_slice()],
                )
                .unwrap();
            assert_eq!(
                profile_hits(&db, "raw:q*").await,
                vec![key(&ACTOR)],
                "the residue rests before the reopen"
            );
        }
        let db = CacheDb::open(&path).unwrap();
        assert!(profile_hits(&db, "raw:q*").await.is_empty());
    }
}
