use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

use super::{CacheDb, blob_col_to_array};

/// The schema family a `content` row belongs to: the segment before the first
/// `/` (`post/text` → `post`, `inbox/message` → `inbox`), or the whole schema
/// when it has none (`profile`).
pub fn schema_plane(schema: &str) -> &str {
    schema.split_once('/').map_or(schema, |(plane, _)| plane)
}

/// Whether a `content` row is a post — the Rust spelling of
/// `schema LIKE 'post/%'`, which covers every `PostBody` variant.
pub fn is_post_schema(schema: &str) -> bool {
    schema.starts_with("post/")
}

/// Insert a content row. The caller must compute the id (BLAKE3 hash of payload)
/// and extract denormalized fields (schema, author, created_at) before calling.
///
/// **A replace stays inside one plane.** The table is shared — posts, profiles,
/// inbox rows, and every other plane's rows — and some planes key rows by
/// digests that can coincide (the retired group plane keyed a message by its
/// signed post's CID digest, the very id a signed wire of that post names). So
/// an id already holding a row of another schema family ([`schema_plane`]) is
/// refused rather than replaced; replacing it would hand one plane's writer
/// another plane's row (an import-triggered trend fetch once emptied a stored
/// group message this way). Within a family the write stays the
/// content-addressed upsert every re-ingest relies on.
#[allow(clippy::too_many_arguments)]
pub fn insert_content(
    conn: &Connection,
    id: &[u8; 32],
    schema: &str,
    author: &[u8; 32],
    created_at: i64,
    payload: &[u8],
    expires_at: Option<i64>,
    source: &str,
    blob_hash: Option<&[u8; 32]>,
) -> Result<()> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT schema FROM content WHERE id = ?1",
            rusqlite::params![id.as_slice()],
            |row| row.get(0),
        )
        .optional()
        .context("read existing content schema")?;
    if let Some(existing) = existing.filter(|e| schema_plane(e) != schema_plane(schema)) {
        anyhow::bail!(
            "content id {} already holds a `{existing}` row; refusing to replace it with `{schema}`",
            hex::encode(id)
        );
    }
    conn.execute(
        "INSERT OR REPLACE INTO content (id, schema, author, created_at, payload, expires_at, source, blob_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            id.as_slice(), schema, author.as_slice(), created_at,
            payload, expires_at, source,
            blob_hash.map(|h| h.as_slice()),
        ],
    ).context("insert content")?;
    Ok(())
}

/// Insert content and index it for full-text search in one call.
#[allow(clippy::too_many_arguments)]
pub fn insert_and_index(
    conn: &Connection,
    id: &[u8; 32],
    schema: &str,
    author: &[u8; 32],
    created_at: i64,
    payload: &[u8],
    expires_at: Option<i64>,
    source: &str,
    blob_hash: Option<&[u8; 32]>,
    // FTS fields:
    title: &str,
    body: &str,
    author_name: &str,
    tags: &str,
) -> Result<()> {
    insert_content(
        conn, id, schema, author, created_at, payload, expires_at, source, blob_hash,
    )?;
    let _ =
        super::search::index_content(conn, id, schema, title, body, author_name, tags, created_at);
    Ok(())
}

/// Get a content row by id. Returns the raw payload bytes and optional blob_hash.
pub fn get_content(conn: &Connection, id: &[u8; 32]) -> Result<Option<(Vec<u8>, Option<Vec<u8>>)>> {
    conn.query_row(
        "SELECT payload, blob_hash FROM content WHERE id = ?1",
        rusqlite::params![id.as_slice()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .context("get content")
}

/// [`get_content`] for the post plane: `None` unless the row is a post
/// ([`is_post_schema`]). The post read keys on caller-supplied ids over a shared
/// table, so without the filter another plane's row (a group message's, once)
/// read back its members-only payload through a post's id.
pub fn get_post_content(
    conn: &Connection,
    id: &[u8; 32],
) -> Result<Option<(Vec<u8>, Option<Vec<u8>>)>> {
    conn.query_row(
        "SELECT payload, blob_hash FROM content WHERE id = ?1 AND schema LIKE 'post/%'",
        rusqlite::params![id.as_slice()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .context("get post content")
}

/// Every content row of `source` whose `expires_at` has passed — the
/// expiry-sweep query.
///
/// `content.expires_at` is written only by writers that carry a real
/// source-side expiry (today: the nostr inbound sweep, honouring NIP-40), and
/// **no serving query filters on it** — so this sweep, not a read-side clause,
/// is what makes an expiry take effect. Callers must tear the row down through
/// the bridge's own withdrawal path rather than deleting here, so the
/// projection, the segment record and the bridge Search corpus go with it.
pub fn list_expired_by_source(conn: &Connection, source: &str, now: i64) -> Result<Vec<[u8; 32]>> {
    let mut stmt = conn
        .prepare(
            "SELECT id FROM content
              WHERE source = ?1 AND expires_at IS NOT NULL AND expires_at <= ?2",
        )
        .context("prepare list_expired_by_source")?;
    let rows = stmt
        .query_map(rusqlite::params![source, now], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .context("query list_expired_by_source")?;
    let mut out = Vec::new();
    for row in rows {
        let bytes = row.context("read expired content id")?;
        if let Ok(id) = <[u8; 32]>::try_from(bytes.as_slice()) {
            out.push(id);
        }
    }
    Ok(out)
}

/// Delete a content row by id.
pub fn delete_content(conn: &Connection, id: &[u8; 32]) -> Result<bool> {
    let rows = conn
        .execute(
            "DELETE FROM content WHERE id = ?1",
            rusqlite::params![id.as_slice()],
        )
        .context("delete content")?;
    Ok(rows > 0)
}

/// One row of an author's own-post enumeration — see [`list_authored_posts_page`].
#[derive(Debug, Clone, PartialEq)]
pub struct AuthoredPost {
    /// The 32-byte content digest, which for a post *is* its `post_id`.
    pub post_id: [u8; 32],
    /// Epoch microseconds (this table's one unit for every writer).
    pub created_at: i64,
    /// The plain body text from the FTS corpus, or empty when the body is
    /// withheld or was never extracted — see the withholding note below.
    pub body: String,
}

/// A page of the given author's **own posts**, newest-first, on a two-half
/// keyset cursor — the store behind `fauna.posts.list` (`content-index.md`
/// § Ingest triggers, v1, ruled 2026-08-05).
///
/// ## The cursor is `(created_at, post_id)`, and the second half is load-bearing
///
/// `ORDER BY created_at DESC` alone is **not a total order** — posts written in
/// the same microsecond tie — so a key-only `created_at < cursor` predicate
/// drops every row sharing the boundary timestamp, and the page contents are
/// not even stable between identical calls. `id` is the table's primary key, so
/// `(created_at, id)` is total, and the strict-successor predicate below is
/// exact across a tie: it neither skips nor duplicates a row at a page
/// boundary. (This is the same defect `feed.rs`'s `score_cursor_created_at`
/// documents and fixed for the scored feed branch; the chronological branch
/// there still carries the key-only shape, so **that** one is not the prior art
/// to copy.)
///
/// ## Posts only, and the caller's own
///
/// `schema LIKE 'post/%'` is the established spelling for "is a post"
/// (`db::public_servability::PUBLIC_POST_SERVABLE` uses the same one), and it
/// covers all five `PostBody` variants — an exact-match schema filter would
/// silently return only one of them. `author = ?` is what makes the corpus
/// self-scoped; the caller's actor id is the author column's value for their
/// own posts, the same equality `check_post_delete_authorization` enforces.
///
/// ## Body text: the FTS corpus, not a second extraction
///
/// The text comes from `content_fts.body`, which `put_post` populated with
/// `Post::body_text()`. Reusing it (rather than re-decoding the payload here)
/// is deliberate: it guarantees the client-built index and the nest's own
/// search corpus agree on what a post's text is, and it works uniformly for
/// post-cutover posts whose `content.payload` is empty because the body lives
/// in a segment. The join is **LEFT**: `insert_and_index` discards the FTS
/// insert's error (`let _ = …`), so a post can have a `content` row and no FTS
/// row — an INNER join would silently drop it from the enumeration, which is
/// precisely the permanently-partial-corpus failure this kind exists to close.
///
/// ## Legal takedown withholds the body — this is a body-serving path
///
/// A legally-taken-down post's body is withheld from **every** viewer, its
/// author included (`routes::get_post_core`; `moderation.md` § Categories &
/// enforcement item 1). This function serves body text, so it is bound by that
/// rule exactly as `fauna.posts.get` is — otherwise `list` would hand the
/// author (and their index) the very bytes `get` refuses them. The row is still
/// returned, with an empty `body`: withholding the text is the rule, dropping
/// the row would make the enumeration lie about the corpus.
///
/// The other two moderation flags are **not** applied: quarantine is
/// author-and-admin-visible by design, and `gated_tier` marks the author's own
/// paid content. `PUBLIC_POST_SERVABLE` is not the predicate here — its module
/// doc scopes it to an *anonymous, off-box* audience, which this is not.
pub fn list_authored_posts_page(
    conn: &Connection,
    author: &[u8; 32],
    cursor: Option<(i64, [u8; 32])>,
    limit: u32,
) -> Result<Vec<AuthoredPost>> {
    let mut sql = String::from(
        "SELECT c.id, c.created_at,
                CASE WHEN cm.legal_takedown_ref IS NOT NULL THEN ''
                     ELSE COALESCE(f.body, '') END
           FROM content c
           LEFT JOIN content_fts_map m ON m.content_id = c.id
           LEFT JOIN content_fts f ON f.rowid = m.fts_rowid
           LEFT JOIN content_meta cm ON cm.content_id = c.id
          WHERE c.author = ?1 AND c.schema LIKE 'post/%'",
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(author.to_vec())];

    if let Some((cursor_ts, cursor_id)) = cursor {
        // Strict successor in `(created_at DESC, id DESC)` order.
        sql.push_str(" AND (c.created_at < ?2 OR (c.created_at = ?2 AND c.id < ?3))");
        params.push(Box::new(cursor_ts));
        params.push(Box::new(cursor_id.to_vec()));
        sql.push_str(" ORDER BY c.created_at DESC, c.id DESC LIMIT ?4");
    } else {
        sql.push_str(" ORDER BY c.created_at DESC, c.id DESC LIMIT ?2");
    }
    params.push(Box::new(limit));

    let mut stmt = conn
        .prepare(&sql)
        .context("prepare list_authored_posts_page")?;
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt
        .query_map(param_refs.as_slice(), |row| {
            Ok(AuthoredPost {
                post_id: blob_col_to_array(row.get(0)?, 0, "post_id")?,
                created_at: row.get(1)?,
                body: row.get(2)?,
            })
        })
        .context("query list_authored_posts_page")?;

    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

impl CacheDb {
    /// The newest `schema = 'profile'` row's payload per author — exactly the
    /// rows `fauna.profile.get` serves (`profile_handlers::latest_profile_bytes`
    /// reads keep-latest-1 by `created_at`). The blob GC's record walk
    /// (`backup::gc` step 2f) pins the avatar/banner these name, so a replaced
    /// profile's old images reclaim while the current ones never do.
    pub async fn list_latest_profile_payloads(&self) -> Result<Vec<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT c.payload FROM content c
                 WHERE c.schema = 'profile'
                   AND c.created_at = (SELECT MAX(c2.created_at) FROM content c2
                                       WHERE c2.author = c.author AND c2.schema = 'profile')",
            )
            .context("prepare list_latest_profile_payloads")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .context("query list_latest_profile_payloads")?;
        rows.collect::<Result<Vec<_>, _>>()
            .context("read latest profile payloads")
    }

    /// Every blob a live `content` row rests its payload in — the row's
    /// `blob_hash`, set when [`crate::payload_store::PayloadStore`] spilled a
    /// payload over its threshold to the blob store (an inbox envelope). The blob GC's
    /// step 2i pins these: while the row lives, its payload IS that blob
    /// (`backup-restore.md` § 9 step 2i).
    pub async fn list_content_payload_blob_hashes(&self) -> Result<Vec<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT DISTINCT blob_hash FROM content WHERE blob_hash IS NOT NULL")
            .context("prepare list_content_payload_blob_hashes")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .context("query list_content_payload_blob_hashes")?;
        rows.collect::<Result<Vec<_>, _>>()
            .context("read content payload blob hashes")
    }

    /// Look up the author (actor ID) of a content row by its content ID.
    ///
    /// Returns `None` if the content row does not exist.
    pub async fn get_content_author(&self, content_id: &[u8; 32]) -> Result<Option<[u8; 32]>> {
        let cid = content_id.to_vec();
        let conn = self.conn.lock().await;
        let result: Option<Vec<u8>> = conn
            .query_row(
                "SELECT author FROM content WHERE id = ?1",
                rusqlite::params![cid],
                |row| row.get(0),
            )
            .optional()
            .context("get content author")?;
        Ok(result.and_then(|v| v.try_into().ok()))
    }

    /// A newest-first page of `author`'s own posts — the store behind
    /// `fauna.posts.list`. Cursor + withholding semantics are documented on
    /// [`list_authored_posts_page`], which this only wraps for the async lock.
    pub async fn list_authored_posts_page(
        &self,
        author: &[u8; 32],
        cursor: Option<(i64, [u8; 32])>,
        limit: u32,
    ) -> Result<Vec<AuthoredPost>> {
        let author = *author;
        let conn = self.conn.lock().await;
        list_authored_posts_page(&conn, &author, cursor, limit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::apply_unified_schema;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        apply_unified_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn insert_and_get_content() {
        let conn = setup();
        let id = [1u8; 32];
        let author = [2u8; 32];
        insert_content(
            &conn,
            &id,
            "post/text",
            &author,
            1000,
            b"hello",
            None,
            "fauna",
            None,
        )
        .unwrap();

        let result = get_content(&conn, &id).unwrap();
        assert_eq!(result.map(|(p, _)| p), Some(b"hello".to_vec()));
    }

    #[test]
    fn get_missing_content_returns_none() {
        let conn = setup();
        let id = [99u8; 32];
        assert_eq!(
            get_content(&conn, &id).unwrap().map(|(p, _)| p),
            None::<Vec<u8>>
        );
    }

    #[test]
    fn delete_content_returns_true_if_existed() {
        let conn = setup();
        let id = [1u8; 32];
        let author = [2u8; 32];
        insert_content(
            &conn,
            &id,
            "post/text",
            &author,
            1000,
            b"data",
            None,
            "fauna",
            None,
        )
        .unwrap();
        assert!(delete_content(&conn, &id).unwrap());
        assert!(!delete_content(&conn, &id).unwrap());
    }

    /// Seed one post with its FTS body, the way `put_post` does.
    fn seed_post(conn: &Connection, id: &[u8; 32], author: &[u8; 32], created_at: i64, body: &str) {
        insert_and_index(
            conn,
            id,
            "post/text",
            author,
            created_at,
            b"payload",
            None,
            "fauna",
            None,
            "",
            body,
            "",
            "",
        )
        .unwrap();
    }

    /// The fixture the post-listing tests run on: `setup()`'s unified schema
    /// already carries `content_meta.legal_takedown_ref`.
    fn setup_posts() -> Connection {
        setup()
    }

    #[test]
    fn list_authored_posts_page_is_self_scoped_and_posts_only() {
        let conn = setup_posts();
        let me = [2u8; 32];
        let other = [9u8; 32];

        seed_post(&conn, &[1u8; 32], &me, 1000, "mine one");
        seed_post(&conn, &[3u8; 32], &me, 3000, "mine two");
        seed_post(&conn, &[4u8; 32], &other, 4000, "not mine");
        // A non-post row of my own must not appear.
        insert_content(
            &conn, &[5u8; 32], "email/v1", &me, 5000, b"m", None, "email", None,
        )
        .unwrap();

        let page = list_authored_posts_page(&conn, &me, None, 10).unwrap();
        let ids: Vec<[u8; 32]> = page.iter().map(|p| p.post_id).collect();
        assert_eq!(ids, vec![[3u8; 32], [1u8; 32]], "newest first, mine only");
        assert_eq!(page[0].body, "mine two", "body comes from the FTS corpus");
    }

    #[test]
    fn list_authored_posts_page_paginates_exactly_across_a_created_at_tie() {
        let conn = setup_posts();
        let me = [2u8; 32];
        // Three posts sharing one microsecond — the case a key-only cursor
        // drops wholesale — plus one strictly older row beneath them.
        seed_post(&conn, &[1u8; 32], &me, 5000, "tie a");
        seed_post(&conn, &[2u8; 32], &me, 5000, "tie b");
        seed_post(&conn, &[3u8; 32], &me, 5000, "tie c");
        seed_post(&conn, &[4u8; 32], &me, 4000, "older");

        // Walk the whole corpus two rows at a time, exactly as a client would.
        let mut seen: Vec<[u8; 32]> = Vec::new();
        let mut cursor: Option<(i64, [u8; 32])> = None;
        loop {
            let page = list_authored_posts_page(&conn, &me, cursor, 2).unwrap();
            if page.is_empty() {
                break;
            }
            let last = page.last().unwrap();
            cursor = Some((last.created_at, last.post_id));
            seen.extend(page.iter().map(|p| p.post_id));
        }

        assert_eq!(
            seen,
            vec![[3u8; 32], [2u8; 32], [1u8; 32], [4u8; 32]],
            "every row exactly once, in (created_at DESC, id DESC) order — no \
             row skipped at the mid-tie page boundary and none duplicated"
        );
    }

    #[test]
    fn list_authored_posts_page_omits_a_deleted_post() {
        let conn = setup_posts();
        let me = [2u8; 32];
        seed_post(&conn, &[1u8; 32], &me, 1000, "kept");
        seed_post(&conn, &[2u8; 32], &me, 2000, "doomed");

        assert!(delete_content(&conn, &[2u8; 32]).unwrap());

        let page = list_authored_posts_page(&conn, &me, None, 10).unwrap();
        let ids: Vec<[u8; 32]> = page.iter().map(|p| p.post_id).collect();
        assert_eq!(ids, vec![[1u8; 32]], "a deleted post is gone from the page");
    }

    #[test]
    fn list_authored_posts_page_withholds_a_legally_taken_down_body_but_keeps_the_row() {
        let conn = setup_posts();
        let me = [2u8; 32];
        seed_post(&conn, &[1u8; 32], &me, 1000, "ordinary text");
        seed_post(&conn, &[2u8; 32], &me, 2000, "unlawful text");
        conn.execute(
            "INSERT INTO content_meta (content_id, legal_takedown_ref) VALUES (?1, ?2)",
            rusqlite::params![[2u8; 32].as_slice(), "court-order-1"],
        )
        .unwrap();

        let page = list_authored_posts_page(&conn, &me, None, 10).unwrap();
        assert_eq!(page.len(), 2, "the row is still enumerated");
        assert_eq!(page[0].post_id, [2u8; 32]);
        assert_eq!(
            page[0].body, "",
            "a taken-down body is withheld from every viewer, the author \
             included — `list` must not serve what `get` refuses"
        );
        assert_eq!(page[1].body, "ordinary text", "beside-control");
    }

    #[test]
    fn list_authored_posts_page_keeps_a_post_whose_fts_row_is_missing() {
        let conn = setup_posts();
        let me = [2u8; 32];
        // `insert_and_index` discards the FTS insert's error, so this state is
        // reachable in production; an INNER join would hide the post forever.
        insert_content(
            &conn,
            &[1u8; 32],
            "post/text",
            &me,
            1000,
            b"payload",
            None,
            "fauna",
            None,
        )
        .unwrap();

        let page = list_authored_posts_page(&conn, &me, None, 10).unwrap();
        assert_eq!(page.len(), 1, "an un-indexed post is still enumerated");
        assert_eq!(page[0].body, "", "with an empty body rather than a hole");
    }

    /// One id, one plane. Posts and other planes' rows (an inbox message here)
    /// share this table and their ids can coincide (a row keyed by a signed
    /// post's CID digest names the very id a signed wire of that post does), so
    /// a writer of one plane must never replace the other's row.
    #[test]
    fn insert_content_refuses_to_replace_a_row_of_another_plane() {
        let conn = setup();
        let author = [2u8; 32];

        let message_id = [7u8; 32];
        insert_content(
            &conn,
            &message_id,
            "inbox/message",
            &author,
            1000,
            b"signed members-only bytes",
            None,
            "fauna",
            None,
        )
        .unwrap();
        let err = insert_content(
            &conn,
            &message_id,
            "post/structured",
            &author,
            2000,
            b"",
            None,
            "fauna",
            None,
        )
        .expect_err("a post writer must not replace an inbox message row");
        assert!(
            format!("{err:#}").contains("inbox/message"),
            "the refusal names the row it protected: {err:#}"
        );
        assert_eq!(
            get_content(&conn, &message_id).unwrap().map(|(p, _)| p),
            Some(b"signed members-only bytes".to_vec()),
            "the inbox message's payload is untouched"
        );

        let post_id = [8u8; 32];
        insert_content(
            &conn,
            &post_id,
            "post/text",
            &author,
            1000,
            b"post",
            None,
            "fauna",
            None,
        )
        .unwrap();
        insert_content(
            &conn,
            &post_id,
            "inbox/message",
            &author,
            1000,
            b"message",
            None,
            "fauna",
            None,
        )
        .expect_err("an inbox writer must not replace a post row");

        // Within one plane a replace is still the content-addressed upsert every
        // re-ingest relies on — here, a body moving out to the segment store.
        insert_content(
            &conn,
            &post_id,
            "post/text",
            &author,
            1000,
            b"",
            None,
            "fauna",
            None,
        )
        .unwrap();
        assert_eq!(
            get_content(&conn, &post_id).unwrap().map(|(p, _)| p),
            Some(Vec::new())
        );
    }
}
