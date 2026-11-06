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

    /// The projection of a post envelope a **federated follow delivered** from
    /// its author's home (`federated-follow.md` § Decision 3): the `content`
    /// row (empty payload — the body is in the author's `__post` segment) and
    /// the list-card preview `write_post_index` writes for any post, marked
    /// `origin_nest_url = home_url`, `origin_nest_id = home_nest_id` (the
    /// delivery's verified origin — the home identity the feed item carries)
    /// and `follow_delivered = 1` — and **no
    /// `content_fts` row**: the Search corpus is nest-wide and a delivered post
    /// is the readers' who follow its author (`db::feeds::FeedLocality`).
    ///
    /// **Custody is per home** (`federated-follow.md` § Decision 3, *What a
    /// reader-named home is trusted for*): every delivery inserts — or,
    /// re-delivered, refreshes the URL of — the delivering origin's
    /// `follow_delivery_homes` row, and nothing else of a row this nest
    /// already holds. The first delivery stores the projection and the
    /// row's `content.origin_nest_*` ([`DeliveredStore::Stored`]); a later
    /// delivery from another home adds its custody and leaves those columns
    /// alone ([`DeliveredStore::CustodyAdded`]), so a delivery never relabels
    /// whose a row is nor overwrites another home's record; a re-delivery from
    /// a custodian is idempotent ([`DeliveredStore::Stored`], `delivered_at`
    /// untouched). A row this nest already lists nest-wide (a discovery stub, a
    /// forwarded post) takes no custody and keeps its standing AND its origin
    /// columns ([`DeliveredStore::KeptNestWide`] — the delivery adds the body
    /// to the segment, never narrows who sees a row nor relabels whose it is).
    /// Orchestrated by `segments::post::store_delivered_post`.
    pub async fn put_delivered_post_index_only(
        &self,
        post_id: &[u8; 32],
        data: &[u8],
        home_url: &str,
        home_nest_id: &[u8; 32],
    ) -> Result<DeliveredStore> {
        let Some(post) = decode_stored_post(data) else {
            anyhow::bail!("a delivered post must decode");
        };
        let schema = post_body_schema(&post);
        let meta = extract_post_metadata(&post);
        let now = fauna_core::data::Timestamp::now().0 as i64;
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction().context("begin delivered post")?;
        let standing = delivered_standing_tx(&tx, post_id)?;
        match standing {
            Some(DeliveredStanding::NestWide) => {
                write_post_index(&tx, post_id, &meta)?;
                tx.commit().context("commit delivered post")?;
                return Ok(DeliveredStore::KeptNestWide);
            }
            Some(DeliveredStanding::Delivered(_)) => {
                let refreshed = tx
                    .execute(
                        "UPDATE follow_delivery_homes SET origin_nest_url = ?3
                          WHERE post_id = ?1 AND origin_nest_id = ?2",
                        rusqlite::params![post_id.as_slice(), home_nest_id.as_slice(), home_url],
                    )
                    .context("refresh the home's custody")?;
                let outcome = if refreshed > 0 {
                    // The row's columns follow their own custodian's URL.
                    tx.execute(
                        "UPDATE content SET origin_nest_url = ?2
                          WHERE id = ?1 AND origin_nest_id = ?3",
                        rusqlite::params![post_id.as_slice(), home_url, home_nest_id.as_slice()],
                    )
                    .context("refresh the delivered post's home URL")?;
                    DeliveredStore::Stored
                } else {
                    insert_custody_tx(&tx, post_id, home_nest_id, home_url, now)?;
                    DeliveredStore::CustodyAdded
                };
                tx.commit().context("commit delivered post")?;
                return Ok(outcome);
            }
            None => {}
        }
        content::insert_content(
            &tx,
            post_id,
            schema,
            &meta.author,
            meta.created_at,
            &[], // empty payload — body in the __post segment store
            None,
            &meta.source,
            None,
        )
        .context("put delivered post")?;
        tx.execute(
            "UPDATE content SET origin_nest_url = ?2, origin_nest_id = ?3, follow_delivered = 1
             WHERE id = ?1",
            rusqlite::params![post_id.as_slice(), home_url, home_nest_id.as_slice()],
        )
        .context("mark the delivered post")?;
        insert_custody_tx(&tx, post_id, home_nest_id, home_url, now)?;
        write_post_index(&tx, post_id, &meta)?;
        tx.commit().context("commit delivered post")?;
        Ok(DeliveredStore::Stored)
    }

    /// Whether `origin` holds custody of a follow-delivered row
    /// (`follow_delivery_homes`): `None` for a row no follow delivered (or
    /// none at all), else `Some(held)` — the tombstone door of
    /// `federated-follow.md` § Decision 5.
    pub async fn delivered_custody_of(
        &self,
        post_id: &[u8; 32],
        origin: &[u8; 32],
    ) -> Result<Option<bool>> {
        let conn = self.conn.lock().await;
        let row: Option<bool> = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM follow_delivery_homes
                                WHERE post_id = c.id AND origin_nest_id = ?2)
                   FROM content c WHERE c.id = ?1 AND c.follow_delivered = 1",
                rusqlite::params![post_id.as_slice(), origin.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .context("read a delivered post's custody")?;
        Ok(row)
    }

    /// The homes holding custody of a follow-delivered row, oldest delivery
    /// first: `(origin_nest_id, origin_nest_url)`. Empty for any other row.
    pub async fn delivered_post_homes(
        &self,
        post_id: &[u8; 32],
    ) -> Result<Vec<([u8; 32], String)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT origin_nest_id, origin_nest_url FROM follow_delivery_homes
              WHERE post_id = ?1 ORDER BY delivered_at, origin_nest_id",
        )?;
        let homes = stmt
            .query_map([post_id.as_slice()], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?))
            })?
            .filter_map(|r| r.ok())
            .filter_map(|(id, url)| <[u8; 32]>::try_from(id).ok().map(|id| (id, url)))
            .collect();
        Ok(homes)
    }

    /// How this nest already holds a post id, for a delivery about to land on
    /// it: `None` — no row; `NestWide` — a row every reader may list;
    /// `Delivered(origin)` — a federated follow's row, with the home it came
    /// through (`None` only if that column was never written).
    pub async fn delivered_standing(
        &self,
        post_id: &[u8; 32],
    ) -> Result<Option<DeliveredStanding>> {
        let conn = self.conn.lock().await;
        delivered_standing_tx(&conn, post_id)
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
            // `get_post_references` above. `React` is a tally projected from
            // the `react` link, never a counter; `Downvote` is inert
            // (`feed.md` § Layout & flow → *Reaction posts count and paint*,
            // rulings 2 and 3).
            let (target_digest, tag) = match reference {
                fauna_core::data::Reference::Reply { post_id } => (cid_to_digest(post_id), "reply"),
                fauna_core::data::Reference::Repost { post_id } => {
                    (cid_to_digest(post_id), "repost")
                }
                fauna_core::data::Reference::Quote { post_id } => (cid_to_digest(post_id), "quote"),
                fauna_core::data::Reference::Upvote { post_id } => (cid_to_digest(post_id), "like"),
                _ => continue,
            };
            let target = fauna_core::data::ContentHash::from_digest_raw(target_digest);
            // An `Upvote` post is a like (ruling 1): it dedupes on the interact
            // door's own per-(actor, target) key, so a door like and an
            // `Upvote` post by one actor count once, and its row names its
            // carrier in `event_data` so only this post's deletion reverses it.
            let (event_id, carrier) = if tag == "like" {
                (
                    fauna_core::engagement::compute_toggle_event_id(&post.author, &target, "like"),
                    Some(referencing_post_id.as_slice()),
                )
            } else {
                (
                    fauna_core::engagement::compute_reference_event_id(&referencing, &target, tag),
                    None,
                )
            };
            // First-insert gate: only the first time this reference is recorded
            // does the counter move (idempotent on re-create).
            let first = self
                .insert_engagement_event(
                    &event_id.digest(),
                    &target_digest,
                    Some(referencing_author),
                    tag,
                    carrier,
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
        // A follow-delivered row goes whole: every home's custody with it
        // (`federated-follow.md` § Decision 5).
        tx.execute(
            "DELETE FROM follow_delivery_homes WHERE post_id = ?1",
            rusqlite::params![post_id.as_slice()],
        )
        .context("delete follow_delivery_homes")?;
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
                fauna_core::data::Reference::Upvote { post_id } => (cid_to_digest(post_id), "like"),
                _ => continue,
            };
            let target = fauna_core::data::ContentHash::from_digest_raw(target_digest);
            // Only a still-present reference event moves the counter —
            // idempotent on a repeated delete, symmetric with the
            // first-insert gate on the create side. An `Upvote`'s like row is
            // removed only when it names THIS post (ruling 1): a door like or
            // another `Upvote` post by the same actor survives.
            let removed = if tag == "like" {
                let event_id =
                    fauna_core::engagement::compute_toggle_event_id(&post.author, &target, "like");
                self.delete_engagement_event_carried_by(&event_id.digest(), referencing_post_id)
                    .await?
            } else {
                let event_id =
                    fauna_core::engagement::compute_reference_event_id(&referencing, &target, tag);
                self.delete_engagement_event(&event_id.digest()).await?
            };
            if removed {
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

/// The reaction arms of the reference funnel and the `react` projection
/// (`docs/goal/ui/feed.md` § Layout & flow → *Reaction posts count and paint*,
/// rulings 1 and 2).
#[cfg(test)]
mod reaction_post_tests {
    use crate::db::CacheDb;
    use fauna_core::data::{Post, PostBody, Reference, Timestamp};
    use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
    use fauna_core::identity::ActorKeypair;

    /// Index a signed post the way the create path's projection does and
    /// return `(wire bytes, content id)`.
    async fn land(
        db: &CacheDb,
        author: &ActorKeypair,
        created_at: u64,
        references: Vec<Reference>,
    ) -> (Vec<u8>, [u8; 32]) {
        let post = Post {
            author: author.actor_id(),
            created_at: Timestamp(created_at),
            body: PostBody::Text {
                content: String::new(),
                facets: vec![],
            },
            references,
            expires_at: None,
            gated: None,
            content_warning: None,
            origin: None,
        };
        let (bytes, env) = sign_envelope(author, &post).unwrap();
        let wire = canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).unwrap();
        let post_id = *blake3::hash(&wire).as_bytes();
        db.put_post_index_only(&post_id, &wire).await.unwrap();
        (wire, post_id)
    }

    /// Land a post AND run the create funnel on it, as `fauna.posts.create` does.
    async fn create(
        db: &CacheDb,
        author: &ActorKeypair,
        created_at: u64,
        references: Vec<Reference>,
    ) -> (Vec<u8>, [u8; 32]) {
        let (wire, id) = land(db, author, created_at, references).await;
        db.record_reference_engagements(&id, &author.actor_id().0, &wire, 0)
            .await
            .unwrap();
        (wire, id)
    }

    fn upvote(target: [u8; 32]) -> Vec<Reference> {
        vec![Reference::Upvote {
            post_id: fauna_cbor::Cid::from_digest_dag_cbor(target),
        }]
    }

    fn react(target: [u8; 32], emoji: &str) -> Vec<Reference> {
        vec![Reference::React {
            post_id: fauna_cbor::Cid::from_digest_dag_cbor(target),
            emoji: emoji.into(),
        }]
    }

    async fn likes(db: &CacheDb, target: &[u8; 32]) -> i64 {
        db.get_engagement_counts(target).await.unwrap().like_count
    }

    fn door_key(actor: &ActorKeypair, target: [u8; 32]) -> [u8; 32] {
        fauna_core::engagement::compute_toggle_event_id(
            &actor.actor_id(),
            &fauna_core::data::ContentHash::from_digest_raw(target),
            "like",
        )
        .digest()
    }

    #[tokio::test]
    async fn an_upvote_post_is_a_like_on_the_doors_key() {
        let db = CacheDb::open_in_memory().unwrap();
        let author = ActorKeypair::from_secret([1u8; 32]);
        let bob = ActorKeypair::from_secret([2u8; 32]);
        let (_, target) = land(&db, &author, 1, vec![]).await;

        // First Upvote moves the counter; the row names its carrier.
        let (u1_wire, u1) = create(&db, &bob, 2, upvote(target)).await;
        assert_eq!(likes(&db, &target).await, 1);
        assert_eq!(
            db.get_engagement_event_data(&door_key(&bob, target))
                .await
                .unwrap(),
            Some(Some(u1.to_vec())),
            "the like row names the Upvote post that carries it"
        );
        // A second Upvote by the same actor dedupes on the same key.
        let (u2_wire, u2) = create(&db, &bob, 3, upvote(target)).await;
        assert_eq!(likes(&db, &target).await, 1, "one actor, one like");
        // A door like by that actor dedupes too.
        let inserted = db
            .insert_engagement_event(
                &door_key(&bob, target),
                &target,
                Some(&bob.actor_id().0),
                "like",
                None,
                0,
            )
            .await
            .unwrap();
        assert!(!inserted, "the door like hits the Upvote's row");

        // Deleting the redundant Upvote reverses nothing — the row names u1.
        db.reverse_reference_engagements(&u2, &u2_wire)
            .await
            .unwrap();
        assert_eq!(likes(&db, &target).await, 1);
        // Deleting the carrier reverses its own row, once.
        db.reverse_reference_engagements(&u1, &u1_wire)
            .await
            .unwrap();
        assert_eq!(likes(&db, &target).await, 0);
        db.reverse_reference_engagements(&u1, &u1_wire)
            .await
            .unwrap();
        assert_eq!(
            likes(&db, &target).await,
            0,
            "idempotent on a repeated delete"
        );
    }

    #[tokio::test]
    async fn a_door_like_survives_the_deletion_of_a_redundant_upvote_post() {
        let db = CacheDb::open_in_memory().unwrap();
        let author = ActorKeypair::from_secret([1u8; 32]);
        let carol = ActorKeypair::from_secret([3u8; 32]);
        let (_, target) = land(&db, &author, 1, vec![]).await;
        assert!(
            db.insert_engagement_event(
                &door_key(&carol, target),
                &target,
                Some(&carol.actor_id().0),
                "like",
                None,
                0,
            )
            .await
            .unwrap()
        );
        db.increment_engagement_count(&target, "like")
            .await
            .unwrap();

        let (wire, upvote_id) = create(&db, &carol, 2, upvote(target)).await;
        assert_eq!(
            likes(&db, &target).await,
            1,
            "the Upvote dedupes on the door like"
        );
        db.reverse_reference_engagements(&upvote_id, &wire)
            .await
            .unwrap();
        assert_eq!(
            likes(&db, &target).await,
            1,
            "the door like (NULL carrier) survives"
        );
        assert_eq!(
            db.get_engagement_event_data(&door_key(&carol, target))
                .await
                .unwrap(),
            Some(None)
        );
    }

    #[tokio::test]
    async fn the_react_link_projects_a_per_emoji_tally_of_distinct_actors() {
        let db = CacheDb::open_in_memory().unwrap();
        let author = ActorKeypair::from_secret([1u8; 32]);
        let bob = ActorKeypair::from_secret([2u8; 32]);
        let carol = ActorKeypair::from_secret([3u8; 32]);
        let (_, target) = land(&db, &author, 1, vec![]).await;

        let (_, bob_heart_1) = create(&db, &bob, 2, react(target, "❤️")).await;
        let (_, bob_heart_2) = create(&db, &bob, 3, react(target, "❤️")).await;
        let (_, carol_fire) = create(&db, &carol, 4, react(target, "🔥")).await;
        let (_, _carol_heart) = create(&db, &carol, 5, react(target, "❤️")).await;

        assert_eq!(likes(&db, &target).await, 0, "a reaction moves no counter");
        let tally = db
            .get_reaction_tally(&target, Some(&bob.actor_id().0))
            .await
            .unwrap();
        let shape: Vec<(&str, i64, Option<String>)> = tally
            .iter()
            .map(|g| (g.emoji.as_str(), g.count, g.viewer_react_id.clone()))
            .collect();
        assert_eq!(
            shape,
            vec![("❤️", 2, Some(hex::encode(bob_heart_2))), ("🔥", 1, None),],
            "distinct actors per emoji, first-added order, the viewer's latest post"
        );
        let _ = bob_heart_1;

        // Carol's view: her own fire, her own heart.
        let tally = db
            .get_reaction_tally(&target, Some(&carol.actor_id().0))
            .await
            .unwrap();
        assert_eq!(tally[1].viewer_react_id, Some(hex::encode(carol_fire)));
        assert!(tally[0].viewer_react_id.is_some());

        // A deleted React post leaves the tally with its link.
        db.delete_post_projection(&carol_fire).await.unwrap();
        let tally = db.get_reaction_tally(&target, None).await.unwrap();
        assert_eq!(tally.len(), 1);
        assert_eq!(tally[0].emoji, "❤️");
    }

    /// Ruling 2's bound on the emoji, at the index: a `React` post whose
    /// emoji is text, two emoji or over the byte bound mints no `react` link,
    /// so it joins no tally.
    #[tokio::test]
    async fn an_emoji_that_fails_the_rule_mints_no_react_link() {
        let db = CacheDb::open_in_memory().unwrap();
        let author = ActorKeypair::from_secret([1u8; 32]);
        let bob = ActorKeypair::from_secret([2u8; 32]);
        let (_, target) = land(&db, &author, 1, vec![]).await;

        let oversized = format!(
            ":{}:",
            "a".repeat(fauna_core::data::REACTION_EMOJI_MAX_BYTES)
        );
        for (i, bad) in ["what a great post", "🔥🔥", oversized.as_str()]
            .into_iter()
            .enumerate()
        {
            create(&db, &bob, 2 + i as u64, react(target, bad)).await;
        }
        assert!(
            db.get_reaction_tally(&target, None)
                .await
                .unwrap()
                .is_empty(),
            "no failing emoji reaches the tally"
        );
        create(&db, &bob, 9, react(target, "🔥")).await;
        assert_eq!(db.get_reaction_tally(&target, None).await.unwrap().len(), 1);
    }

    /// Ruling 2's bound on the groups, at the read: a post with more distinct
    /// emoji than `REACTION_TALLY_MAX_GROUPS` returns exactly that many — the
    /// most-reacted, ties to the earliest — still in first-added order, on the
    /// single read every path shares (the feed pass, the author read and
    /// `fauna.posts.get` prepare the same statements).
    #[tokio::test]
    async fn the_tally_returns_at_most_the_group_cap_most_reacted_first_added_order() {
        use crate::db::feeds::REACTION_TALLY_MAX_GROUPS as CAP;
        let db = CacheDb::open_in_memory().unwrap();
        let author = ActorKeypair::from_secret([1u8; 32]);
        let bob = ActorKeypair::from_secret([2u8; 32]);
        let carol = ActorKeypair::from_secret([3u8; 32]);
        let (_, target) = land(&db, &author, 1, vec![]).await;

        let emoji = |i: usize| char::from_u32(0x1F600 + i as u32).unwrap().to_string();
        let total = CAP + 5;
        for i in 0..total {
            create(&db, &bob, 10 + i as u64, react(target, &emoji(i))).await;
        }
        // The last-added emoji is the most reacted: it survives the cap.
        create(&db, &carol, 100, react(target, &emoji(total - 1))).await;

        let tally = db.get_reaction_tally(&target, None).await.unwrap();
        assert_eq!(tally.len(), CAP, "the read is capped");
        let mut expect: Vec<String> = (0..CAP - 1).map(emoji).collect();
        expect.push(emoji(total - 1));
        let got: Vec<String> = tally.iter().map(|g| g.emoji.clone()).collect();
        assert_eq!(
            got, expect,
            "top by count, ties earliest, first-added order"
        );
        assert_eq!(tally[CAP - 1].count, 2);
    }
}

/// What a federated follow's delivery did to the projection
/// ([`CacheDb::put_delivered_post_index_only`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveredStore {
    /// Stored as a delivered row, or re-delivered by a home already in its
    /// custody (that custody's URL refreshed).
    Stored,
    /// The row was already listed nest-wide and is left as it was.
    KeptNestWide,
    /// The row was already delivered by another home: this home's custody is
    /// added beside it, the row and its origin columns untouched.
    CustodyAdded,
}

/// Insert one home's custody of a delivered row (`follow_delivery_homes`).
fn insert_custody_tx(
    conn: &rusqlite::Connection,
    post_id: &[u8; 32],
    home_nest_id: &[u8; 32],
    home_url: &str,
    delivered_at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO follow_delivery_homes (post_id, origin_nest_id, origin_nest_url, delivered_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (post_id, origin_nest_id) DO UPDATE SET origin_nest_url = excluded.origin_nest_url",
        rusqlite::params![
            post_id.as_slice(),
            home_nest_id.as_slice(),
            home_url,
            delivered_at
        ],
    )
    .context("record the home's custody")?;
    Ok(())
}

/// How a `content` row is held, as a delivery sees it
/// ([`CacheDb::delivered_standing`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveredStanding {
    NestWide,
    Delivered(Option<[u8; 32]>),
}

fn delivered_standing_tx(
    conn: &rusqlite::Connection,
    post_id: &[u8; 32],
) -> Result<Option<DeliveredStanding>> {
    let row: Option<(bool, Option<Vec<u8>>)> = conn
        .query_row(
            "SELECT follow_delivered = 1, origin_nest_id FROM content WHERE id = ?1",
            [post_id.as_slice()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .context("read the prior row's standing")?;
    Ok(row.map(|(delivered, origin)| {
        if delivered {
            DeliveredStanding::Delivered(origin.and_then(|o| <[u8; 32]>::try_from(o).ok()))
        } else {
            DeliveredStanding::NestWide
        }
    }))
}
