use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};

/// Insert or update feed metadata for a content row.
///
/// `gated_tier` is the tier name of a gated-to-tier post (`Post.gated.tier`,
/// `ui/feed.md` § Encryption at rest) — a plaintext-floor attribute projected
/// so `query_feed` can surface the `gated-post-badge` without reading the
/// body; `None` for public posts (and every row written before the column).
/// `gated_room` is the channel id of the room a room-restricted post
/// addresses (`KeyAccess::Room.group_id`, the same floor), projected so a
/// member's card can name the room; `None` for every other post.
/// `preview` is the list-card text (`content_meta.preview`, `ui/feed.md` § The
/// read model → *The list-card preview*) — `write_post_index` always passes
/// the post's; `None` only from fixtures that seed a bare feed coordinate.
#[allow(clippy::too_many_arguments)]
pub fn upsert_meta(
    conn: &Connection,
    content_id: &[u8; 32],
    score: f64,
    has_media: bool,
    is_reply: bool,
    gated_tier: Option<&str>,
    gated_room: Option<&[u8; 32]>,
    preview: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO content_meta (content_id, score, has_media, is_reply, gated_tier, gated_room, preview)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            content_id.as_slice(),
            score,
            has_media as i64,
            is_reply as i64,
            gated_tier,
            gated_room.map(|r| r.as_slice()),
            preview,
        ],
    )
    .context("upsert content_meta")?;
    // Post-arrival List-labeler join (labeler-registry design Block A, D12d):
    // content arriving after a List subscription gets its bus row now. Cheap
    // (indexed lookup, usually empty) + idempotent; best-effort — never fails
    // the content write (the ingest report-aggregate join posture).
    if let Err(e) = super::labelers::join_list_labeler_scores_for_content(conn, content_id) {
        tracing::warn!("list-labeler post-arrival join failed (content write succeeded): {e}");
    }
    Ok(())
}

/// Get the score of a content row.
pub fn get_score(conn: &Connection, content_id: &[u8; 32]) -> Result<Option<f64>> {
    conn.query_row(
        "SELECT score FROM content_meta WHERE content_id = ?1",
        rusqlite::params![content_id.as_slice()],
        |row| row.get(0),
    )
    .optional()
    .context("get score")
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
    fn upsert_and_get_meta() {
        let conn = setup();
        let id = [1u8; 32];
        upsert_meta(&conn, &id, 0.0, true, false, None, None, None).unwrap();
        assert_eq!(get_score(&conn, &id).unwrap(), Some(0.0));
    }

    /// What a `content_meta` row-minting fn does about the list-card text.
    enum Preview {
        /// Writes `preview` from the post's own text.
        Carries(&'static str),
        /// Mints a row with no text to carry, and says why the card is right
        /// to be empty (or who fills it).
        NoText(&'static str),
    }

    /// The SQL that mints a `content_meta` row.
    const CONTENT_META_WRITERS: &[&str] = &["INTO content_meta"];

    /// Every production fn that mints a `content_meta` row, classified.
    const CONTENT_META_CENSUS: &[(&str, &str, Preview)] = &[
        (
            "db/meta.rs",
            "upsert_meta",
            Preview::Carries(
                "the funnel: `write_post_index` passes `PostMetadata::preview`, which \
                 `extract_post_metadata` derives from `Post::body_text()`",
            ),
        ),
        (
            "db/feeds.rs",
            "insert_post_index_entry_with_origin",
            Preview::NoText(
                "a feed coordinate for a discovery candidate whose `content` row is a \
                 payload-less stub: the nest holds no text for it (the FTS lookup this \
                 column replaced returned none either). `INSERT OR IGNORE`, so a row \
                 `write_post_index` already wrote keeps its preview",
            ),
        ),
    ];

    /// **Every production writer of a `content_meta` row is classified** against
    /// the list-card preview (`ui/feed.md` § The read model → *The list-card
    /// preview*: a writer can no more mint a card without its text than without
    /// its `content_meta` row). A card's text is the `preview` column, so a new
    /// row-minting path that forgets it paints an empty card on every app — the
    /// defect the column was built to end for bridged posts. A new writer fails
    /// here until someone decides whether it carries the text; a removed one
    /// fails until its entry goes.
    #[test]
    fn every_content_meta_writer_decides_the_preview() {
        use std::collections::BTreeSet;
        let found = crate::partition_scan::callers_of(CONTENT_META_WRITERS);
        let table: BTreeSet<(String, String)> = CONTENT_META_CENSUS
            .iter()
            .map(|(file, func, _)| (file.to_string(), func.to_string()))
            .collect();
        assert_eq!(
            table.len(),
            CONTENT_META_CENSUS.len(),
            "a pair is listed twice"
        );
        for (file, func, class) in CONTENT_META_CENSUS {
            let (Preview::Carries(why) | Preview::NoText(why)) = class;
            assert!(!why.trim().is_empty(), "{file}::{func} states its reason");
        }
        crate::partition_scan::assert_partitioned(
            &found,
            &table,
            "This fn mints a `content_meta` row — a feed card. Decide what the card's text is \
             and add it to CONTENT_META_CENSUS: a post write goes through `write_post_index` \
             (which carries `content_meta.preview`); anything else says why it has no text.",
        );
    }

    /// The funnel carries the card text, cut on a character boundary at the
    /// same 500 characters SQLite's `substr` took.
    #[test]
    fn post_preview_cuts_on_a_character_boundary() {
        let long: String = "é".repeat(600);
        let p = crate::db::post_preview(&long);
        assert_eq!(p.chars().count(), crate::db::PREVIEW_CHARS);
        assert!(p.chars().all(|c| c == 'é'));
        assert_eq!(crate::db::post_preview("short"), "short");
    }
}
