//! Post storage and indexing methods.

use super::{CacheDb, content, now_epoch_millis};
use super::{extract_post_metadata, post_body_schema, write_post_index};
use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

/// Decode a [`Post`](fauna_core::data::Post) from its stored `content`-table
/// payload bytes — the single decode path every reader of a stored post shares.
/// A stored post is either the **embed-as-bytes** wire shape (signed posts, via
/// [`CacheDb::put_post`]) or a **bare** canonical `Post` (bridge-translated
/// unsigned posts, via [`CacheDb::put_post_with_source`]); this tries the former
/// (envelope + inner bytes), then falls back to the latter. Returns `None` if
/// the bytes are neither.
pub fn decode_stored_post(data: &[u8]) -> Option<fauna_core::data::Post> {
    fauna_core::data::Post::decode_resolved_bytes(data)
}

/// A `Reference`'s `PostId` → the 32-byte `content`-table digest it keys.
///
/// After Task 2.8's `PostId → Cid` collapse a reference stores the full 36-byte
/// CID (`v1 + dag-cbor + blake3-256 + len 32`); the `content`/`content_meta`
/// tables are keyed on the trailing 32-byte BLAKE3 digest. Strip the 4-byte
/// prefix — the same conversion [`CacheDb::get_post_references`] does inline.
/// `PostId` is always a 36-byte `Cid`, so this is infallible.
pub(crate) fn cid_to_digest(post_id: &fauna_core::data::PostId) -> [u8; 32] {
    let full = post_id.as_bytes();
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&full[full.len() - 32..]);
    digest
}

impl CacheDb {
    /// Store a post by its ID.
    pub async fn put_post(
        &self,
        post_id: &[u8; 32],
        data: &[u8],
        blob_hash: Option<&[u8; 32]>,
    ) -> Result<()> {
        let post_id = *post_id;
        let data = data.to_vec();
        let blob_hash = blob_hash.copied();
        let conn = self.conn.lock().await;

        let db_payload = if blob_hash.is_some() {
            &[][..]
        } else {
            &data[..]
        };

        // Indexing path tries the new embed-as-bytes wire shape first,
        // then falls back to the bare-Post shape (unsigned-but-
        // structured bridge ingest). (Bridge translators produce raw Post
        // without signing; they go through put_post_with_source.)
        let decoded_post = fauna_core::encoding::canonical_decode::<
            fauna_core::encoding::EmbedAsBytes,
        >(&data)
        .ok()
        .and_then(|wire| {
            fauna_core::encoding::decode_signed_bytes::<fauna_core::data::Post>(&wire.bytes).ok()
        })
        .or_else(|| fauna_core::encoding::canonical_decode::<fauna_core::data::Post>(&data).ok());

        if let Some(post) = decoded_post {
            let schema = post_body_schema(&post);
            let meta = extract_post_metadata(&post);

            // FTS index
            let body_text = post.body_text();
            let tags_str = meta.tags.join(" ");
            content::insert_and_index(
                &conn,
                &post_id,
                schema,
                &meta.author,
                meta.created_at,
                db_payload,
                None,
                &meta.source,
                blob_hash.as_ref(),
                "",
                &body_text,
                "",
                &tags_str,
            )
            .context("put post")?;

            let _ = write_post_index(&conn, &post_id, &meta);
        } else {
            let zero_author = [0u8; 32];
            let now = now_epoch_millis() * 1000;
            content::insert_content(
                &conn,
                &post_id,
                "post/text",
                &zero_author,
                now,
                db_payload,
                None,
                "fauna",
                blob_hash.as_ref(),
            )
            .context("put post (raw)")?;
        }

        Ok(())
    }

    /// Store a post with a specific protocol source (e.g., "bluesky", "email").
    /// Used by bridges to ingest external content into the unified feed.
    pub async fn put_post_with_source(
        &self,
        post_id: &[u8; 32],
        data: &[u8],
        source: &str,
    ) -> Result<()> {
        let post_id = *post_id;
        let data = data.to_vec();
        let source = source.to_string();
        let conn = self.conn.lock().await;

        match fauna_core::encoding::canonical_decode::<fauna_core::data::Post>(&data) {
            Ok(post) => {
                let schema = post_body_schema(&post);
                let mut meta = extract_post_metadata(&post);
                meta.source = source;

                content::insert_content(
                    &conn,
                    &post_id,
                    schema,
                    &meta.author,
                    meta.created_at,
                    &data,
                    None,
                    &meta.source,
                    None,
                )
                .context("put post with source")?;

                let _ = write_post_index(&conn, &post_id, &meta);
            }
            Err(e) => {
                tracing::warn!("put_post_with_source: failed to decode post for indexing: {e}");
            }
        }
        Ok(())
    }

    /// Store a post's feed-index projection ONLY — the `content` row + FTS /
    /// meta / links — with an **empty** `content.payload` and no `blob_hash`.
    /// The body bytes live in the `__post` segment store
    /// (`segments::post::append_body`), the post-cutover authoritative body
    /// store. The cutover analogue of [`CacheDb::put_post`] (which still stores
    /// the body inline for residual non-segment callers + the worker
    /// read-fallback cache); orchestrated by `segments::post::store_post`.
    pub async fn put_post_index_only(&self, post_id: &[u8; 32], data: &[u8]) -> Result<()> {
        let conn = self.conn.lock().await;
        put_post_index_only_on(&conn, post_id, data)
    }

    /// Source-tagged sibling of [`CacheDb::put_post_index_only`] — the cutover
    /// analogue of [`CacheDb::put_post_with_source`]. Bridge-/federation-
    /// ingested posts decode bare (not embed-as-bytes) and, as before, are
    /// **not** FTS-indexed; the projection row carries an empty payload. Their
    /// list-card text rides `content_meta.preview`, written by the same
    /// `write_post_index` every post passes (`ui/feed.md` § The read model →
    /// *The list-card preview*) — never an FTS lookup.
    /// `expires_at` (epoch seconds) is a source-side expiry the bridge's own
    /// sweep retracts on (`segments::post::store_post_with_expiry`).
    pub async fn put_post_with_source_index_only(
        &self,
        post_id: &[u8; 32],
        data: &[u8],
        source: &str,
        expires_at: Option<i64>,
    ) -> Result<()> {
        let post_id = *post_id;
        let data = data.to_vec();
        let source = source.to_string();
        let conn = self.conn.lock().await;

        match fauna_core::encoding::canonical_decode::<fauna_core::data::Post>(&data) {
            Ok(post) => {
                let schema = post_body_schema(&post);
                let mut meta = extract_post_metadata(&post);
                meta.source = source;

                content::insert_content(
                    &conn,
                    &post_id,
                    schema,
                    &meta.author,
                    meta.created_at,
                    &[], // empty payload — body in the __post segment store
                    expires_at,
                    &meta.source,
                    None, // no blob_hash
                )
                .context("put post with source index-only")?;

                let _ = write_post_index(&conn, &post_id, &meta);
            }
            Err(e) => {
                tracing::warn!(
                    "put_post_with_source_index_only: failed to decode post for indexing: {e}"
                );
            }
        }
        Ok(())
    }

    /// Whether a `content` row exists for this post id. The newly-stored gate
    /// the forwarded-post receive reads before running the create-side bridge
    /// fan-out — `segments::post::store_post` is idempotent (a re-delivery
    /// upserts and reports `Ok`), so arrival order cannot be read off its
    /// result (`private-mode.md` § Post Forwarding step 6).
    pub async fn post_exists(&self, post_id: &[u8; 32]) -> Result<bool> {
        let post_id = *post_id;
        let conn = self.conn.lock().await;
        let exists: i64 = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM content WHERE id = ?1)",
            [post_id.as_slice()],
            |row| row.get(0),
        )?;
        Ok(exists != 0)
    }

    /// Whether this nest has seen the post deleted: its `tombstone/post`
    /// witness row is present AND no `content` row remains at the post id.
    /// Every door through which a peer hands this nest a post's bytes (the
    /// forwarded-post receive, the trending exchange's fetch) checks it first,
    /// so a deletion is never undone by replaying the post's public signed
    /// bytes (`feed.md` § State & data shape → *Post deletion*, "never
    /// re-ingested by replay").
    ///
    /// The `content` half is load-bearing: a legal takedown writes the same
    /// witness over a still-live post (`post_legal_takedown_txn`), and a
    /// withheld post is not a deleted one.
    pub async fn post_was_deleted(&self, post_id: &[u8; 32]) -> Result<bool> {
        let witness_id = super::content_id_for_document(
            super::atproto_projection::POST_TOMBSTONE_SCHEMA,
            &hex::encode(post_id),
        );
        let conn = self.conn.lock().await;
        let deleted: i64 = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM content WHERE id = ?1 AND schema = ?2)
                AND NOT EXISTS(SELECT 1 FROM content WHERE id = ?3)",
            rusqlite::params![
                witness_id.as_slice(),
                super::atproto_projection::POST_TOMBSTONE_SCHEMA,
                post_id.as_slice()
            ],
            |row| row.get(0),
        )?;
        Ok(deleted != 0)
    }

    /// The schema of the `content` row at this id, if any — which plane holds the
    /// id. `segments::post::store_post` reads it before appending a body, so a
    /// post never takes an id another plane's row already holds.
    pub async fn content_schema(&self, id: &[u8; 32]) -> Result<Option<String>> {
        let id = *id;
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT schema FROM content WHERE id = ?1",
            [id.as_slice()],
            |row| row.get(0),
        )
        .optional()
        .context("read content schema")
    }

    /// Retrieve a post by its ID. Returns (payload, blob_hash).
    ///
    /// Answers for **post rows only**. The `content` table is shared — other
    /// planes' rows rest in it too — and every post-fetch door (`GET
    /// /api/v1/posts/{id}`, `fauna.posts.get`, `fauna.federation.post.get`)
    /// reads a caller-supplied id through here, so an unfiltered read handed a
    /// members-only row of another plane to anyone holding its id, signed in
    /// or not.
    pub async fn get_post(&self, post_id: &[u8; 32]) -> Result<Option<(Vec<u8>, Option<Vec<u8>>)>> {
        let post_id = *post_id;
        let conn = self.conn.lock().await;
        content::get_post_content(&conn, &post_id)
    }

    /// Load a post's references (Reply, Repost, Quote) from its already-loaded
    /// body bytes and resolve each referenced post's author from the content
    /// table.
    ///
    /// Takes the body directly (the caller reads it via
    /// `segments::post::load_post_body`, segment-first) rather than reading
    /// `content.payload` — after the segment-store cutover the payload column is
    /// empty for posts, so the body must come from the `__post` segment store.
    /// The per-reference author/origin lookups below still hit the `content`
    /// projection (unchanged).
    ///
    /// Returns `(post_id, author, ref_type, origin_nest_url)` tuples for each
    /// resolved reference — the fourth field is `None` for a reference this
    /// nest hosts and `Some(nest_url)` for one it indexed via discovery
    /// (`content.origin_nest_url`, row 740).
    pub async fn get_post_references(
        &self,
        body: &[u8],
    ) -> Result<Vec<(Vec<u8>, Vec<u8>, String, Option<String>)>> {
        // Decode the post (embed-as-bytes signed shape, or bare for
        // bridge-translated unsigned posts).
        let Some(post) = decode_stored_post(body) else {
            return Ok(vec![]);
        };

        let conn = self.conn.lock().await;

        let mut refs = Vec::new();
        for reference in &post.references {
            // After Task 2.8's PostId → Cid collapse, references store the
            // full 36-byte CID. The `content` table is keyed on the 32-byte
            // BLAKE3 digest (see `db::content::insert_content`), so we
            // strip the 4-byte `v1 + dag-cbor + blake3-256 + len 32` prefix
            // before the lookup. The returned `ref_post_id` keeps the
            // 36-byte CID shape — that's what the peer-query wire layer
            // expects (`peer_query.rs` hex-encodes it for the response).
            let (ref_post_id, ref_post_digest, ref_type) = match reference {
                fauna_core::data::Reference::Reply { post_id } => {
                    let full = post_id.as_bytes().to_vec();
                    let mut digest = [0u8; 32];
                    digest.copy_from_slice(&full[4..]);
                    (full, digest, "reply")
                }
                fauna_core::data::Reference::Repost { post_id } => {
                    let full = post_id.as_bytes().to_vec();
                    let mut digest = [0u8; 32];
                    digest.copy_from_slice(&full[4..]);
                    (full, digest, "repost")
                }
                fauna_core::data::Reference::Quote { post_id } => {
                    let full = post_id.as_bytes().to_vec();
                    let mut digest = [0u8; 32];
                    digest.copy_from_slice(&full[4..]);
                    (full, digest, "quote")
                }
                _ => continue,
            };

            // Look up author and origin nest from the content table (keyed on
            // the 32-byte digest). `origin_nest_url` is NULL for a
            // post this nest hosts and Some(_) for one it indexed via
            // discovery — `source` no longer doubles as the locator, so no
            // string-parsing is needed to recover it.
            match conn.query_row(
                "SELECT author, origin_nest_url FROM content WHERE id = ?1",
                rusqlite::params![ref_post_digest.as_slice()],
                |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Option<String>>(1)?)),
            ) {
                Ok((author, nest_url)) => {
                    refs.push((ref_post_id, author, ref_type.to_string(), nest_url));
                }
                Err(_) => continue, // Referenced post not on this nest
            }
        }

        Ok(refs)
    }

    /// Maintain the interaction counters (`reply_count` / `repost_count` /
    /// `quote_count` on `content_meta`) for the `Reference::{Reply,Repost,Quote}`
    /// references a freshly-created post carries — the post-create half of the
    /// interaction-bar feature (`feed.md` § Interaction bar; the `like` half
    /// rides `fauna.posts.interact`). For each such reference, idempotently bump
    /// the *target's* counter, deduped via the `engagement_events` log keyed by
    /// the **referencing** post id (`compute_reference_event_id`): a byte-identical
    /// re-create — which content-addressed `store_post` dedups — re-inserts
    /// nothing, so it never double-counts, while distinct posts each count.
    ///
    /// Best-effort + non-fatal (mirrors `engagement_handlers::record_handler`'s
    /// first-insert side-effects): a counter hiccup must not fail post creation,
    /// so the caller logs and proceeds. An off-nest target simply has no
    /// `content_meta` row, so the `UPDATE … WHERE content_id` no-ops harmlessly.
    pub async fn record_reference_engagements(
        &self,
        referencing_post_id: &[u8; 32],
        referencing_author: &[u8; 32],
        body: &[u8],
        now_us: i64,
    ) -> Result<()> {
        let Some(post) = decode_stored_post(body) else {
            return Ok(());
        };
        let referencing = fauna_core::data::ContentHash::from_digest_raw(*referencing_post_id);
        for reference in &post.references {
            // PostId → 32-byte content digest: strip the 4-byte CID prefix
            // (`v1 + dag-cbor + blake3-256 + len 32`), mirroring
            // `get_post_references` above. React/Upvote/Downvote are not
            // interaction-bar counts.
            let (target_digest, tag) = match reference {
                fauna_core::data::Reference::Reply { post_id } => (cid_to_digest(post_id), "reply"),
                fauna_core::data::Reference::Repost { post_id } => {
                    (cid_to_digest(post_id), "repost")
                }
                fauna_core::data::Reference::Quote { post_id } => (cid_to_digest(post_id), "quote"),
                _ => continue,
            };
            let target = fauna_core::data::ContentHash::from_digest_raw(target_digest);
            let event_id =
                fauna_core::engagement::compute_reference_event_id(&referencing, &target, tag);
            // First-insert gate: only the first time this reference is recorded
            // does the counter move (idempotent on re-create).
            let first = self
                .insert_engagement_event(
                    &event_id.digest(),
                    &target_digest,
                    Some(referencing_author),
                    tag,
                    None,
                    now_us,
                )
                .await?;
            if first {
                self.increment_engagement_count(&target_digest, tag).await?;
            }
        }
        Ok(())
    }

    /// The `content` row's `author` column for a post id — the ownership
    /// oracle the delete path uses for a non-segment row (still written by the
    /// residual `put_post` callers), where no
    /// segment-record scope exists to check against.
    pub async fn get_post_author(&self, post_id: &[u8; 32]) -> Result<Option<[u8; 32]>> {
        let post_id = *post_id;
        let conn = self.conn.lock().await;
        let author: Option<Vec<u8>> = conn
            .query_row(
                "SELECT author FROM content WHERE id = ?1",
                rusqlite::params![post_id.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .context("get post author")?;
        Ok(author.and_then(|a| a.try_into().ok()))
    }

    /// Remove a deleted post's feed-index projection — the serving gate of
    /// self-service post deletion (`feed.md` § State & data shape → *Post
    /// deletion*): the `content` row plus every satellite row keyed on it
    /// (`content_meta`, `content_links`, `content_labels`, the
    /// `engagement_events` targeting it) and the `content_fts` entry. After
    /// this, `get_post` and every feed query are dark for the id. Idempotent;
    /// returns whether a `content` row was actually removed (the reply's
    /// `deleted` flag).
    ///
    /// All six removals run in **one transaction** (mirrors `put_spam_model_with_history`'s pattern, `db/moderation.rs`):
    /// without it, a crash between the `content` delete and the `content_fts`
    /// removal durably drops `content` (so `get`/feeds go dark) but leaves the
    /// FTS row, and the public search path's `LEFT JOIN content` (no
    /// null-content filter) would then keep serving the deleted post's text as
    /// a search snippet indefinitely. One transaction makes that window
    /// unrepresentable — either every row is gone or none are.
    pub async fn delete_post_projection(&self, post_id: &[u8; 32]) -> Result<bool> {
        self.delete_post_projection_with_witness(post_id, None)
            .await
    }

    /// [`Self::delete_post_projection`], plus the ATProto post-delete journal
    /// row written **in the same transaction** — `witness` is
    /// `(author, bare canonical Tombstone bytes, delete instant micros)`.
    ///
    /// Why the same transaction (the tx-wrap precedent above): the journal
    /// row is the *only* durable, timestamped signal the bridge's forward-only
    /// incremental cursor can ever see for a deletion. Written separately and
    /// best-effort, a failure after the projection delete committed left the
    /// client with a success it never retries, no cursor-visible delete, and —
    /// since `rebuild(sources)` is not implemented — no backstop, so an
    /// already-projected post stayed live on Bluesky forever. Fail-direction is
    /// *content persists after the user retracted it*, which the
    /// user-controls-their-data invariant does not permit. One transaction
    /// makes "removed ∧ witnessed" atomic: either both land or neither does,
    /// and the client's retry (`AlreadyGone` arm) heals a crash as before.
    pub async fn delete_post_projection_with_witness(
        &self,
        post_id: &[u8; 32],
        witness: Option<(&[u8; 32], &[u8], i64)>,
    ) -> Result<bool> {
        let post_id = *post_id;
        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction()?;
        let existed = content::delete_content(&tx, &post_id)?;
        tx.execute(
            "DELETE FROM content_meta WHERE content_id = ?1",
            rusqlite::params![post_id.as_slice()],
        )
        .context("delete content_meta")?;
        // A still-web-published post: its rendered page, index entry and feed
        // item are a derivation the delete must revoke, and the render that
        // revokes them runs after this commit. So the revoke rides THIS
        // transaction as an owed-render marker, decided here off the very link
        // the next statement cascades away — never by a read before the
        // transaction, which a retry would find already gone
        // (`web-content-hosting.md` § Routing, render, serving → *A revoke is
        // durable*).
        super::web::mark_web_render_owed_for_post(&tx, &post_id)?;
        tx.execute(
            "DELETE FROM content_links WHERE source_id = ?1 OR target_id = ?1",
            rusqlite::params![post_id.as_slice()],
        )
        .context("delete content_links")?;
        // `content_labels.content_id` is TEXT (lowercase hex), unlike the
        // BLOB keys above; scoped to the post type like its writers.
        tx.execute(
            "DELETE FROM content_labels WHERE content_type = 'post' AND content_id = ?1",
            rusqlite::params![hex::encode(post_id)],
        )
        .context("delete content_labels")?;
        tx.execute(
            "DELETE FROM engagement_events WHERE content_id = ?1",
            rusqlite::params![post_id.as_slice()],
        )
        .context("delete engagement_events")?;
        // Propagate (not `let _ =`-swallow) so an FTS-removal failure rolls the
        // WHOLE transaction back — an FTS5 error class that leaves the tx
        // committable (does not auto-abort) must not let the other five deletes
        // commit while the `content_fts` row (and its public-search snippet)
        // survives. That is the exact "every row gone or none" invariant this
        // wrap exists for (residual on the
        // phase-1 crash-window fix; the crash-window itself was already closed).
        super::search::remove_content(&tx, &post_id).context("delete content_fts")?;
        // The ATProto delete witness, inside the same transaction and
        // `?`-propagated for the reason in the doc comment: a witness this
        // write loses is a retraction the bridge can never learn about.
        if let Some((author, tombstone_payload, deleted_at_micros)) = witness {
            let id = super::content_id_for_document(
                super::atproto_projection::POST_TOMBSTONE_SCHEMA,
                &hex::encode(post_id),
            );
            content::insert_content(
                &tx,
                &id,
                super::atproto_projection::POST_TOMBSTONE_SCHEMA,
                author,
                deleted_at_micros,
                tombstone_payload,
                None,
                "fauna",
                None,
            )
            .context("insert atproto post-delete witness")?;
        }
        tx.commit().context("commit post-projection delete")?;
        Ok(existed)
    }

    /// Reverse the interaction counters [`Self::record_reference_engagements`]
    /// bumped when this post landed — the post-delete half (`feed.md` § State
    /// & data shape → *Post deletion*): for each `Reference::{Reply,Repost,
    /// Quote}` the deleted post carried, delete the deduped reference event
    /// and, when it existed, decrement the *target's* counter. Idempotent via
    /// the same `engagement_events` log; best-effort + non-fatal like its
    /// mirror (a counter hiccup must not fail the delete).
    pub async fn reverse_reference_engagements(
        &self,
        referencing_post_id: &[u8; 32],
        body: &[u8],
    ) -> Result<()> {
        let Some(post) = decode_stored_post(body) else {
            return Ok(());
        };
        let referencing = fauna_core::data::ContentHash::from_digest_raw(*referencing_post_id);
        for reference in &post.references {
            let (target_digest, tag) = match reference {
                fauna_core::data::Reference::Reply { post_id } => (cid_to_digest(post_id), "reply"),
                fauna_core::data::Reference::Repost { post_id } => {
                    (cid_to_digest(post_id), "repost")
                }
                fauna_core::data::Reference::Quote { post_id } => (cid_to_digest(post_id), "quote"),
                _ => continue,
            };
            let target = fauna_core::data::ContentHash::from_digest_raw(target_digest);
            let event_id =
                fauna_core::engagement::compute_reference_event_id(&referencing, &target, tag);
            // Only a still-present reference event moves the counter —
            // idempotent on a repeated delete, symmetric with the
            // first-insert gate on the create side.
            if self.delete_engagement_event(&event_id.digest()).await? {
                self.decrement_engagement_count(&target_digest, tag).await?;
            }
        }
        Ok(())
    }

    // ==================== Post Indexing ====================

    /// Index a decoded Post into content_meta + content_links for feed queries.
    pub async fn index_post(
        &self,
        post_id: &[u8; 32],
        post: &fauna_core::data::Post,
    ) -> Result<()> {
        let post_id = *post_id;
        let meta = extract_post_metadata(post);
        let conn = self.conn.lock().await;
        write_post_index(&conn, &post_id, &meta)?;
        Ok(())
    }

    /// List **all** post IDs authored by the given actor, oldest-first and
    /// unpaged — a whole-corpus read for nest-internal projection work (today:
    /// the ATProto backfill).
    ///
    /// ⚠ Not the door for a client enumeration: it has no cursor, no limit and
    /// no body text, and its `ORDER BY created_at` carries no tiebreak. The
    /// wire-facing enumeration behind `fauna.posts.list` is
    /// [`super::content::list_authored_posts_page`].
    pub async fn list_posts_by_author(&self, author: &[u8; 32]) -> Result<Vec<Vec<u8>>> {
        let author = *author;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT id FROM content WHERE author = ?1 AND schema LIKE 'post/%' ORDER BY created_at")
            .context("prepare list_posts_by_author")?;
        let rows = stmt
            .query_map(rusqlite::params![author.as_slice()], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .context("query posts_by_author")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read post_id")?);
        }
        Ok(results)
    }

    /// List **every** post id on this box, oldest-first — every `content` row
    /// whose `schema LIKE 'post/%'`, regardless of author or migration state.
    /// A whole-corpus read for a nest-internal walk (the legal-takedown blob
    /// withhold rebuild and the blob GC's reachability walk).
    ///
    /// ⚠ No cursor, no limit: not the door for a client enumeration.
    pub async fn list_all_post_ids(&self) -> Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT id FROM content WHERE schema LIKE 'post/%' ORDER BY created_at")
            .context("prepare list_all_post_ids")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .context("query list_all_post_ids")?;
        let mut results = Vec::new();
        for row in rows {
            let id = row.context("read post id")?;
            if id.len() == 32 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&id);
                results.push(arr);
            } else {
                tracing::warn!(
                    id = %hex::encode(&id),
                    "list_all_post_ids: skipping content id with non-32-byte length"
                );
            }
        }
        Ok(results)
    }

    /// Get the source protocol for a post from the content table.
    pub async fn get_post_source(&self, post_id: &[u8; 32]) -> Result<Option<String>> {
        let post_id = *post_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT source FROM content WHERE id = ?1")
            .context("prepare get_post_source")?;
        let mut rows = stmt
            .query_map(rusqlite::params![post_id.as_slice()], |row| {
                row.get::<_, String>(0)
            })
            .context("query get_post_source")?;
        match rows.next() {
            Some(row) => Ok(Some(row.context("read source")?)),
            None => Ok(None),
        }
    }

    /// Whether `post_id` is a discovery-indexed federation stub this nest
    /// does not itself host — `Some(nest_url)` if so, naming the nest it was
    /// fetched from; `None` for a post this nest hosts (local, bridged, or
    /// archive-imported) or one it has no record of at all. `interact_
    /// routes.rs`'s door reads this to keep such a stub out of the native and
    /// bridge arms regardless of its `source` token — `content.
    /// payload` is empty for these rows (§10.2 of the discovery-feeds design),
    /// so this nest has nothing to act on locally; the client follows `fetch_
    /// url`/`origin` and interacts with the originating nest directly.
    pub async fn get_post_origin_nest_url(&self, post_id: &[u8; 32]) -> Result<Option<String>> {
        let post_id = *post_id;
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT origin_nest_url FROM content WHERE id = ?1",
            rusqlite::params![post_id.as_slice()],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .context("query get_post_origin_nest_url")
        .map(|opt| opt.flatten())
    }
}

/// [`CacheDb::put_post_index_only`] on a connection the caller already holds —
/// so a restore can write a post's projection inside the same transaction as
/// its `segment_records` mirror row (`segments::post::reassert_records`).
pub(crate) fn put_post_index_only_on(
    conn: &rusqlite::Connection,
    post_id: &[u8; 32],
    data: &[u8],
) -> Result<()> {
    let Some(post) = decode_stored_post(data) else {
        // Undecodable bodies never reach here from `store_post` (it keeps
        // them inline); a direct call on undecodable bytes writes no
        // projection (matching `put_post`'s raw branch storing zero-author
        // bytes, but here the body is expected in the segment).
        tracing::warn!("put_post_index_only: undecodable post, no projection written");
        return Ok(());
    };

    let schema = post_body_schema(&post);
    let meta = extract_post_metadata(&post);
    let body_text = post.body_text();
    let tags_str = meta.tags.join(" ");
    content::insert_and_index(
        conn,
        post_id,
        schema,
        &meta.author,
        meta.created_at,
        &[], // empty payload — body in the __post segment store
        None,
        &meta.source,
        None, // no blob_hash — body in the segment, not the blob store
        "",
        &body_text,
        "",
        &tags_str,
    )
    .context("put post index-only")?;

    let _ = write_post_index(conn, post_id, &meta);
    Ok(())
}

#[cfg(test)]
mod delete_projection_atomicity_tests {
    use crate::db::CacheDb;
    use fauna_core::data::{Post, PostBody, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
    use fauna_core::identity::ActorKeypair;

    fn signed_post_wire(author: &ActorKeypair, content: &str) -> (Vec<u8>, [u8; 32]) {
        let post = Post {
            author: author.actor_id(),
            created_at: Timestamp::now(),
            body: PostBody::Text {
                content: content.into(),
                facets: vec![],
            },
            references: vec![],
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let (bytes, env) = sign_envelope(author, &post).unwrap();
        let wire = EmbedAsBytes::from_signed(bytes, env);
        let wire_bytes = canonical_encode(&wire).unwrap();
        let post_id = *blake3::hash(&wire_bytes).as_bytes();
        (wire_bytes, post_id)
    }

    /// An FTS-removal error must roll back the WHOLE post-projection delete — the
    /// `content` row survives, nothing half-commits. Before the fix the FTS
    /// removal was `let _ =`-swallowed, so a `content_fts` DELETE error (an FTS5
    /// error class that does NOT auto-abort the transaction) let the other five
    /// deletes commit while the `content_fts` row + its public-search snippet
    /// survived — a deleted post's text stayed searchable (residual on the phase-1 crash-window fix). The atomic wrap's own
    /// invariant is "every row gone or none".
    #[tokio::test]
    async fn fts_removal_error_rolls_back_the_whole_projection_delete() {
        let db = CacheDb::open_in_memory().unwrap();
        let author = ActorKeypair::from_secret([3u8; 32]);
        let (wire, post_id) = signed_post_wire(&author, "secret words");
        db.put_post_index_only(&post_id, &wire).await.unwrap();
        assert!(
            db.get_post_author(&post_id).await.unwrap().is_some(),
            "precondition: the content row exists after indexing"
        );

        // Fault-inject: drop the FTS table so `remove_content`'s
        // `DELETE FROM content_fts` errors, while `content_fts_map` (a separate
        // table, untouched) still yields the rowid — an error class that leaves
        // the transaction committable rather than auto-aborting it.
        {
            let conn = db.conn().await;
            conn.execute_batch("DROP TABLE content_fts").unwrap();
        }

        let result = db.delete_post_projection(&post_id).await;
        assert!(
            result.is_err(),
            "an FTS-removal failure must fail the whole delete, not half-commit"
        );

        // Nothing half-committed: the content row (and every satellite) survives
        // the rolled-back transaction.
        assert!(
            db.get_post_author(&post_id).await.unwrap().is_some(),
            "the content row must survive the rolled-back delete — no partial commit"
        );
    }
}
