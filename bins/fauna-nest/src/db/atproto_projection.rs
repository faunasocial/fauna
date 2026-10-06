//! Public-projection reads for the ATProto PDS bridge (S3,
//! `atproto-pds-bridge.md` § Where logic lives) plus the post-delete
//! tombstone journal they interleave.
//!
//! The bridge pulls one oldest-first stream per author over
//! `fauna.bridges.atproto.fetch_public_posts`: live public posts (the shared
//! off-box servability predicate — [`crate::db::public_servability::
//! PUBLIC_POST_SERVABLE`], which the ActivityPub actor routes apply too)
//! interleaved with post-delete journal rows (`schema = 'tombstone/post'`),
//! keyset-paged by `(created_at ASC, id ASC)`.
//!
//! Why a journal row exists at all: `routes::delete_post_core` physically
//! removes the post's projection rows and only flips `segment_records.
//! tombstoned` (no delete timestamp, and compaction reclaims the row), so
//! without a journal an incremental cursor consumer whose watermark had
//! passed the post would never learn of its deletion. The journal row's
//! `created_at` is the **delete** instant (`Tombstone.created_at`), so the
//! delete interleaves at the time it happened, not at the deleted post's
//! original position. Journal rows are tiny (one bare canonical `Tombstone`
//! per deleted post, deterministic id ⇒ at most one per post) and rest in
//! `content` beside the rows they retire.

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use super::{CacheDb, blob_col_to_array};

/// `content.schema` of a post-delete journal row. Deliberately NOT matched by
/// the `'post/%'` LIKE filter every public post read uses, so journal rows
/// are invisible to feeds/outbox/trends — only the projection page below
/// selects them.
pub const POST_TOMBSTONE_SCHEMA: &str = "tombstone/post";

/// Ceiling for one `fetch_public_posts` page (the handler clamps to this).
pub const MAX_PUBLIC_PROJECTION_PAGE: u32 = 200;

/// One row of the public projection stream, in `(created_at, id)` order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicProjectionRow {
    /// The content-row id: the post digest for post rows; the deterministic
    /// journal-row id for tombstone rows.
    pub id: [u8; 32],
    /// `content.created_at`, epoch microseconds (creation instant for posts,
    /// delete instant for tombstones).
    pub created_at: i64,
    pub is_tombstone: bool,
    /// The row's inline `content.payload`. For tombstone rows this is the
    /// bare canonical `Tombstone`; for post rows it is empty post-cutover
    /// (the body lives in the `__post` segment store — the handler resolves
    /// it via `segments::post::load_post_body`).
    pub inline_payload: Vec<u8>,
}

impl CacheDb {
    /// One page of `author`'s public projection stream: servable public posts
    /// ([`crate::db::public_servability::PUBLIC_POST_SERVABLE`] — `post/%`,
    /// with a `content_meta` row, ungated and unflagged by every moderation
    /// axis) + post-delete journal rows, oldest-first by
    /// `(created_at ASC, id ASC)`, strictly after the exclusive `after`
    /// cursor. `limit` is clamped to [`MAX_PUBLIC_PROJECTION_PAGE`].
    ///
    /// `floor_micros` is where the stream STARTS — the history-backfill opt-in
    /// made operative (`atproto-pds-bridge.md` § Projection & backfill: opt-in
    /// → genesis, default → the enable instant). Rows older than it are never
    /// served, so a post predating the user's consent to publish never leaves
    /// this box, whatever the bridge asks for.
    ///
    /// It is applied as a filter on the whole stream, NOT as a clamp on
    /// `after`: a clamp would skip rows between an existing watermark and a
    /// later floor, silently dropping posts the user *did* consent to publish.
    /// Being a filter also makes it idempotent — the bridge's watermark walks
    /// forward from the first row it is allowed to see.
    pub async fn list_public_projection_page(
        &self,
        author: &[u8; 32],
        after: Option<(i64, [u8; 32])>,
        limit: u32,
        floor_micros: i64,
    ) -> Result<Vec<PublicProjectionRow>> {
        let author = author.to_vec();
        let limit = limit.clamp(1, MAX_PUBLIC_PROJECTION_PAGE) as i64;
        let conn = self.conn.lock().await;

        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<PublicProjectionRow> {
            let id: Vec<u8> = row.get(0)?;
            let schema: String = row.get(2)?;
            Ok(PublicProjectionRow {
                id: blob_col_to_array(id, 0, "id")?,
                created_at: row.get(1)?,
                is_tombstone: schema == POST_TOMBSTONE_SCHEMA,
                inline_payload: row.get(3)?,
            })
        };

        // Two prepared shapes rather than one dynamic string with NULL
        // params — same filter, the only difference is the keyset predicate.
        // The floor rides in `base_filter` so neither shape can forget it.
        //
        // The post arm is the shared off-box servability predicate; the
        // tombstone arm deliberately bypasses it, because a retraction must
        // reach the network for a post that is no longer servable — that is
        // the entire point of the journal row.
        let base_filter = format!(
            "c.author = :author
               AND (({servable})
                    OR c.schema = 'tombstone/post')
               AND c.created_at >= :floor",
            servable = super::public_servability::PUBLIC_POST_SERVABLE.as_str(),
        );
        let base_filter = base_filter.as_str();
        let rows = match after {
            None => {
                let mut stmt = conn.prepare(&format!(
                    "SELECT c.id, c.created_at, c.schema, c.payload
                       FROM content c
                       LEFT JOIN content_meta cm ON cm.content_id = c.id
                      WHERE {base_filter}
                      ORDER BY c.created_at ASC, c.id ASC
                      LIMIT :limit"
                ))?;
                let rows = stmt.query_map(
                    rusqlite::named_params! {
                        ":author": author, ":limit": limit, ":floor": floor_micros,
                    },
                    map_row,
                )?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            }
            Some((cursor_ts, cursor_id)) => {
                let mut stmt = conn.prepare(&format!(
                    "SELECT c.id, c.created_at, c.schema, c.payload
                       FROM content c
                       LEFT JOIN content_meta cm ON cm.content_id = c.id
                      WHERE {base_filter}
                        AND (c.created_at > :cursor_ts
                             OR (c.created_at = :cursor_ts AND c.id > :cursor_id))
                      ORDER BY c.created_at ASC, c.id ASC
                      LIMIT :limit"
                ))?;
                let rows = stmt.query_map(
                    rusqlite::named_params! {
                        ":author": author,
                        ":cursor_ts": cursor_ts,
                        ":cursor_id": cursor_id.as_slice(),
                        ":limit": limit,
                        ":floor": floor_micros,
                    },
                    map_row,
                )?;
                rows.collect::<rusqlite::Result<Vec<_>>>()
            }
        }
        .context("list public projection page")?;
        Ok(rows)
    }

    /// The author's latest stored profile payload (the `fauna.profile.set`
    /// at-rest bytes) — the same read `profile_handlers::profile_get_handler`
    /// performs, exposed for the bridge-class `fetch_profile` kind.
    pub async fn get_latest_profile_payload(&self, author: &[u8; 32]) -> Result<Option<Vec<u8>>> {
        let author = author.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT payload FROM content
              WHERE author = ?1 AND schema = 'profile'
              ORDER BY created_at DESC LIMIT 1",
            rusqlite::params![author],
            |row| row.get(0),
        )
        .optional()
        .context("get latest profile payload")
    }

    /// The author a post's `tombstone/post` journal row names — who
    /// retracted it — or `None` when no such row exists.
    ///
    /// The witness doubles as the durable record that a delete *ran*: the
    /// projection rows it replaces are gone, so it is the one row left that
    /// says whose post this was. That is what lets a re-drive of a gone post's
    /// outward delete legs (`routes::delete_post_core`'s settled path, the
    /// `post_delete_redrive` sweep) act for the author the delete ran for and
    /// no one else. A legal takedown writes the same row over a still-live
    /// post (`post_legal_takedown_txn`), so a caller asking "was this post
    /// deleted, and by whom" must check the post is gone as well.
    pub async fn post_delete_witness_author(&self, post_id: &[u8; 32]) -> Result<Option<[u8; 32]>> {
        let id = super::content_id_for_document(POST_TOMBSTONE_SCHEMA, &hex::encode(post_id));
        let conn = self.conn.lock().await;
        let author: Option<Vec<u8>> = conn
            .query_row(
                "SELECT author FROM content WHERE id = ?1 AND schema = ?2",
                rusqlite::params![id.as_slice(), POST_TOMBSTONE_SCHEMA],
                |row| row.get(0),
            )
            .optional()
            .context("get post-delete witness author")?;
        Ok(author.and_then(|a| a.try_into().ok()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::content;

    async fn fresh_db() -> CacheDb {
        CacheDb::open_in_memory().expect("in-memory db")
    }

    const AUTHOR: [u8; 32] = [0x0Au8; 32];

    /// The `content_meta` row a seeded post gets. Every axis the servability
    /// predicate reads is expressible here — including, via
    /// [`seed_post_without_meta`], the row's *absence*, which the old seeder
    /// could not express at all (so the gated-post assertion below passed
    /// vacuously on the one shape that actually leaked).
    #[derive(Default, Clone, Copy)]
    struct Meta {
        tier: Option<&'static str>,
        legal_takedown_ref: Option<&'static str>,
        quarantined: bool,
        suppressed: bool,
    }

    impl Meta {
        /// A servable public post: no tier, no flag set.
        fn public() -> Self {
            Self::default()
        }
        fn gated(tier: &'static str) -> Self {
            Self {
                tier: Some(tier),
                ..Self::default()
            }
        }
        fn taken_down(reference: &'static str) -> Self {
            Self {
                legal_takedown_ref: Some(reference),
                ..Self::default()
            }
        }
        fn quarantined() -> Self {
            Self {
                quarantined: true,
                ..Self::default()
            }
        }
        fn suppressed() -> Self {
            Self {
                suppressed: true,
                ..Self::default()
            }
        }
    }

    /// Seed a `post/text` content row (empty payload, the post-cutover
    /// projection shape) plus its `content_meta` row.
    async fn seed_post(db: &CacheDb, id: [u8; 32], created_at: i64, meta: Meta) {
        seed_post_row(db, id, created_at).await;
        let conn = db.conn().await;
        conn.execute(
            "INSERT INTO content_meta (content_id, gated_tier, legal_takedown_ref, quarantined, suppressed)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                id.as_slice(),
                meta.tier,
                meta.legal_takedown_ref,
                meta.quarantined as i64,
                meta.suppressed as i64,
            ],
        )
        .unwrap();
    }

    /// Seed a post with **no** `content_meta` row — the shape a swallowed
    /// `write_post_index` error leaves behind (`db/moderation.rs:304-307`),
    /// and the one the `LEFT JOIN` used to serve as public.
    async fn seed_post_without_meta(db: &CacheDb, id: [u8; 32], created_at: i64) {
        seed_post_row(db, id, created_at).await;
    }

    async fn seed_post_row(db: &CacheDb, id: [u8; 32], created_at: i64) {
        let conn = db.conn().await;
        content::insert_content(
            &conn,
            &id,
            "post/text",
            &AUTHOR,
            created_at,
            &[],
            None,
            "fauna",
            None,
        )
        .unwrap();
    }

    #[tokio::test]
    async fn public_filter_excludes_gated_and_other_authors() {
        let db = fresh_db().await;
        seed_post(&db, [1u8; 32], 1_000, Meta::public()).await;
        seed_post(&db, [2u8; 32], 2_000, Meta::gated("premium")).await;
        // Another author's public post must not leak into this stream.
        {
            let conn = db.conn().await;
            content::insert_content(
                &conn,
                &[3u8; 32],
                "post/text",
                &[0x0Bu8; 32],
                1_500,
                &[],
                None,
                "fauna",
                None,
            )
            .unwrap();
        }

        let page = db
            .list_public_projection_page(&AUTHOR, None, 50, 0)
            .await
            .unwrap();
        assert_eq!(page.len(), 1, "gated + foreign posts excluded");
        assert_eq!(page[0].id, [1u8; 32]);
        assert!(!page[0].is_tombstone);
    }

    /// A legally compelled takedown must stop **this** serve path too.
    ///
    /// `moderation.md` § Legal takedown makes the withholding nest-wide and
    /// non-discretionary; this stream publishes to a third-party public
    /// network, so serving a flagged post here silently defeats the one
    /// mechanism that is not the admin's to decline. The feed read has gated
    /// on all three flags since 2026-07-05 (`db/feeds.rs`) — this path had
    /// drifted from it.
    #[tokio::test]
    async fn public_filter_excludes_every_moderation_flag() {
        let db = fresh_db().await;
        seed_post(&db, [1u8; 32], 1_000, Meta::public()).await;
        seed_post(&db, [2u8; 32], 2_000, Meta::taken_down("court-order-1")).await;
        seed_post(&db, [3u8; 32], 3_000, Meta::quarantined()).await;
        seed_post(&db, [4u8; 32], 4_000, Meta::suppressed()).await;

        let page = db
            .list_public_projection_page(&AUTHOR, None, 50, 0)
            .await
            .unwrap();
        let ids: Vec<_> = page.iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            vec![[1u8; 32]],
            "a taken-down / quarantined / suppressed post must never project"
        );
    }

    /// The join-shape hole, independent of the flags above: a post with **no**
    /// `content_meta` row must not read as public.
    ///
    /// `cm.gated_tier IS NULL` is vacuously true for an unmatched `LEFT JOIN`
    /// right side, so absence used to mean permission. The state is reachable
    /// — every post-write site swallows the `write_post_index` error that
    /// carries `gated_tier` into the index (`db/moderation.rs:304-307` records
    /// the same reachability) — which means a **gated** post whose index write
    /// failed was published to Bluesky as public.
    #[tokio::test]
    async fn public_filter_excludes_a_post_with_no_content_meta_row() {
        let db = fresh_db().await;
        seed_post(&db, [1u8; 32], 1_000, Meta::public()).await;
        seed_post_without_meta(&db, [2u8; 32], 2_000).await;

        let page = db
            .list_public_projection_page(&AUTHOR, None, 50, 0)
            .await
            .unwrap();
        let ids: Vec<_> = page.iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            vec![[1u8; 32]],
            "a missing content_meta row must not read as public and unflagged"
        );
    }

    /// The retraction arm must survive the tightened predicate: a tombstone
    /// journal row carries no `content_meta` row **by construction**, so an
    /// `INNER` join — or asserting `cm.content_id IS NOT NULL` across the whole
    /// filter rather than inside the post arm — would silently drop every
    /// delete. That failure is invisible in the happy path and permanent on the
    /// network, so it gets its own pin.
    #[tokio::test]
    async fn tightened_predicate_still_serves_tombstone_rows() {
        let db = fresh_db().await;
        seed_post(&db, [1u8; 32], 1_000, Meta::public()).await;
        db.delete_post_projection_with_witness(
            &[1u8; 32],
            Some((&AUTHOR, b"tombstone-bytes", 2_000)),
        )
        .await
        .unwrap();

        let page = db
            .list_public_projection_page(&AUTHOR, None, 50, 0)
            .await
            .unwrap();
        assert_eq!(page.len(), 1, "the retraction still reaches the bridge");
        assert!(page[0].is_tombstone);
    }

    #[tokio::test]
    async fn tombstones_interleave_at_delete_time() {
        let db = fresh_db().await;
        seed_post(&db, [1u8; 32], 1_000, Meta::public()).await;
        seed_post(&db, [2u8; 32], 2_000, Meta::public()).await;
        // Post 1 deleted at t=3000: its projection row goes away, the journal
        // row lands at the DELETE instant — after post 2 in stream order.
        // Driven through the production path, which writes both ATOMICALLY.
        let existed = db
            .delete_post_projection_with_witness(
                &[1u8; 32],
                Some((&AUTHOR, b"tombstone-bytes", 3_000)),
            )
            .await
            .unwrap();
        assert!(existed, "the post row was there to remove");

        let page = db
            .list_public_projection_page(&AUTHOR, None, 50, 0)
            .await
            .unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].id, [2u8; 32]);
        assert!(!page[0].is_tombstone);
        assert!(page[1].is_tombstone, "journal row last, at delete time");
        assert_eq!(page[1].created_at, 3_000);
        assert_eq!(page[1].inline_payload, b"tombstone-bytes");

        // Idempotent: a delete retry converges on the same single row (the
        // `AlreadyGone` arm — the post is gone, the witness re-converges).
        let existed = db
            .delete_post_projection_with_witness(
                &[1u8; 32],
                Some((&AUTHOR, b"tombstone-bytes", 3_000)),
            )
            .await
            .unwrap();
        assert!(!existed, "retry finds the post already gone");
        let page = db
            .list_public_projection_page(&AUTHOR, None, 50, 0)
            .await
            .unwrap();
        assert_eq!(page.len(), 2, "retry did not duplicate the journal row");
    }

    #[tokio::test]
    async fn cursor_pages_are_exclusive_and_ordered() {
        let db = fresh_db().await;
        // Two rows share created_at so the id tie-break is exercised.
        seed_post(&db, [1u8; 32], 1_000, Meta::public()).await;
        seed_post(&db, [2u8; 32], 1_000, Meta::public()).await;
        seed_post(&db, [3u8; 32], 2_000, Meta::public()).await;

        let first = db
            .list_public_projection_page(&AUTHOR, None, 2, 0)
            .await
            .unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].id, [1u8; 32]);
        assert_eq!(first[1].id, [2u8; 32], "same-instant tie broken by id");

        let last = first.last().unwrap();
        let second = db
            .list_public_projection_page(&AUTHOR, Some((last.created_at, last.id)), 2, 0)
            .await
            .unwrap();
        assert_eq!(second.len(), 1, "cursor is exclusive");
        assert_eq!(second[0].id, [3u8; 32]);

        let last = second.last().unwrap();
        let third = db
            .list_public_projection_page(&AUTHOR, Some((last.created_at, last.id)), 2, 0)
            .await
            .unwrap();
        assert!(third.is_empty(), "stream exhausted");
    }

    /// The ratified forward-only default (`atproto-pds-bridge.md` § Projection
    /// & backfill, `:92`): a floor at the enable instant serves "nothing
    /// historical" — posts predating consent never leave this box — while
    /// posts from the instant onward flow normally.
    #[tokio::test]
    async fn forward_only_floor_excludes_history_and_admits_the_enable_instant() {
        let db = fresh_db().await;
        const ENABLE_AT: i64 = 2_000;
        seed_post(&db, [1u8; 32], 1_000, Meta::public()).await; // years of back-catalogue
        seed_post(&db, [2u8; 32], 1_999, Meta::public()).await; // one microsecond too early
        seed_post(&db, [3u8; 32], ENABLE_AT, Meta::public()).await; // exactly at consent
        seed_post(&db, [4u8; 32], 3_000, Meta::public()).await; // after

        let page = db
            .list_public_projection_page(&AUTHOR, None, 50, ENABLE_AT)
            .await
            .unwrap();
        let ids: Vec<_> = page.iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            vec![[3u8; 32], [4u8; 32]],
            "forward-only publishes from the enable instant onward, never history"
        );
    }

    /// The history opt-in (`:93`): floor 0 = genesis, the whole stream.
    #[tokio::test]
    async fn history_opt_in_floor_publishes_from_genesis() {
        let db = fresh_db().await;
        seed_post(&db, [1u8; 32], 1_000, Meta::public()).await;
        seed_post(&db, [2u8; 32], 2_000, Meta::public()).await;

        let page = db
            .list_public_projection_page(&AUTHOR, None, 50, 0)
            .await
            .unwrap();
        assert_eq!(page.len(), 2, "opt-in publishes the back-catalogue");
        assert_eq!(page[0].id, [1u8; 32]);
    }

    /// The floor filters the stream; it never clamps the cursor. A watermark
    /// BELOW the floor must still resume correctly (and skip nothing above the
    /// floor), and a page whose rows all predate the floor must not wedge —
    /// this is the shape that would silently drop consented posts if the floor
    /// were applied as `max(watermark, floor)`.
    #[tokio::test]
    async fn floor_is_a_filter_not_a_cursor_clamp() {
        let db = fresh_db().await;
        const FLOOR: i64 = 2_000;
        seed_post(&db, [1u8; 32], 1_000, Meta::public()).await;
        seed_post(&db, [2u8; 32], 2_500, Meta::public()).await;
        seed_post(&db, [3u8; 32], 3_000, Meta::public()).await;

        // Resume from a watermark that predates the floor: the floor decides
        // what is visible, the cursor only decides where we resume.
        let page = db
            .list_public_projection_page(&AUTHOR, Some((1_000, [1u8; 32])), 50, FLOOR)
            .await
            .unwrap();
        let ids: Vec<_> = page.iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            vec![[2u8; 32], [3u8; 32]],
            "nothing above the floor lost"
        );

        // And a floor past everything yields an empty, non-wedging stream.
        let none = db
            .list_public_projection_page(&AUTHOR, None, 50, 9_999)
            .await
            .unwrap();
        assert!(none.is_empty(), "a floor past the tail serves nothing");
    }

    /// A tombstone journal row carries the DELETE instant, so deleting an
    /// old (pre-floor) post still reaches the bridge — which is correct: the
    /// bridge maps it against its own PostId→AT-URI map and no-ops when the
    /// post was never projected. The floor must not suppress deletions of
    /// posts that WERE published.
    #[tokio::test]
    async fn floor_admits_a_tombstone_for_a_pre_floor_post() {
        let db = fresh_db().await;
        seed_post(&db, [1u8; 32], 1_000, Meta::public()).await;
        let existed = db
            .delete_post_projection_with_witness(
                &[1u8; 32],
                Some((&AUTHOR, b"tombstone-bytes", 5_000)),
            )
            .await
            .unwrap();
        assert!(existed);

        let page = db
            .list_public_projection_page(&AUTHOR, None, 50, 2_000)
            .await
            .unwrap();
        assert_eq!(page.len(), 1, "the pre-floor post itself stays hidden");
        assert!(page[0].is_tombstone);
        assert_eq!(
            page[0].created_at, 5_000,
            "journal row at the delete instant"
        );
    }

    /// Ruling 1: the ATProto projection stream skips an archive-imported post
    /// while serving the native post beside it.
    #[tokio::test]
    async fn an_archive_imported_post_is_never_projected() {
        use fauna_core::data::{Post, PostBody, PostOrigin, Timestamp};
        use fauna_core::identity::ActorId;
        let db = CacheDb::open_in_memory().unwrap();
        let author = [21u8; 32];
        let store = async |content: &str, origin: Option<PostOrigin>| {
            let post = Post {
                author: ActorId(author),
                created_at: Timestamp(1_000_000_000_000),
                body: PostBody::Text {
                    content: content.into(),
                    facets: vec![],
                },
                references: vec![],
                expires_at: None,
                gated: None,
                content_warning: None,
                origin,
            };
            let payload = fauna_core::encoding::canonical_encode(&post).unwrap();
            let id = *blake3::hash(&payload).as_bytes();
            db.put_post(&id, &payload, None).await.unwrap();
            id
        };
        let imported = store(
            "imported",
            Some(PostOrigin {
                platform: fauna_core::source::INSTAGRAM.into(),
                url: None,
            }),
        )
        .await;
        let native = store("native", None).await;
        let page = db
            .list_public_projection_page(&author, None, 10, 0)
            .await
            .unwrap();
        let ids: Vec<[u8; 32]> = page.iter().map(|r| r.id).collect();
        assert!(ids.contains(&native));
        assert!(!ids.contains(&imported));
    }

    #[tokio::test]
    async fn latest_profile_payload_present_and_absent() {
        let db = fresh_db().await;
        assert_eq!(db.get_latest_profile_payload(&AUTHOR).await.unwrap(), None);
        {
            let conn = db.conn().await;
            content::insert_content(
                &conn,
                &[9u8; 32],
                "profile",
                &AUTHOR,
                5_000,
                b"old-profile",
                None,
                "fauna",
                None,
            )
            .unwrap();
            content::insert_content(
                &conn,
                &[10u8; 32],
                "profile",
                &AUTHOR,
                6_000,
                b"new-profile",
                None,
                "fauna",
                None,
            )
            .unwrap();
        }
        assert_eq!(
            db.get_latest_profile_payload(&AUTHOR).await.unwrap(),
            Some(b"new-profile".to_vec())
        );
    }
}
