//! A **community room's labels** — the labeler set its owner or admins sign,
//! and the bus rows the home nest derives with it.
//!
//! Authority: `docs/goal/behavior/conversation-rooms.md` § The three classes →
//! *What the home nest does with its read*, purposes 2 (messages) and 3
//! (room-restricted posts); placement
//! `docs/goal/architecture/content-scoring.md` § The placement matrix (the
//! *capability-holder* row).
//!
//! Two halves with opposite lifetimes:
//!
//! - **The set** (`rooms.labelers_version` / `rooms.labelers_blob`) — the
//!   signed `fauna_mls::room_policy::SignedRoomLabelers`, stored exactly like
//!   the room policy beside it: a compare-and-set on the version, and never
//!   authored here. It is the room's own choice, not something the nest read,
//!   so a revoke leaves it standing — a nest the members rotate back in resumes
//!   labelling under the set the room already chose.
//! - **The bus rows** — `content_labels` (the category plane) and
//!   `content_scores` (the factor plane), keyed to one room message or one
//!   room-restricted post. A **derived view** like the search index:
//!   rebuildable from the sealed bytes by a nest that holds a wrap, and deleted
//!   by [`CacheDb::purge_room_derived_views`] in the same act as the FTS rows.
//!
//! ## The content id
//!
//! A room message's bus id is `room_id ‖ seq` (the seq as 8 big-endian bytes)
//! — the 40 raw bytes in `content_scores.content_id`, their lowercase hex in
//! `content_labels.content_id` (the same hex-id bridge the feed's label
//! predicates use for a post). A room post's is `room_id ‖ post_id` — 64 raw
//! bytes — under its own kind. **Deliberately not a hash, and deliberately
//! room-first.** The revoke has to find a room's rows from the room alone,
//! and a count of them has to see a row even when nothing else points at it
//! any more — a hashed id would make an orphaned label invisible to exactly
//! the check that exists to catch one, and a post id alone would leave the
//! revoke to rediscover which posts the room ever indexed. The pairs are no
//! secret from the nest: it stores the log by `(room, seq)` already, and it
//! wrote the `(room, post)` map itself when it indexed the post.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use super::{CacheDb, now_epoch_millis};
use fauna_core::content_category::ContentLabelEntry;
use fauna_core::scoring::ScoreEntry;

/// The `content_labels.content_type` / `content_scores.content_kind` a
/// community room message's bus rows carry — distinct from every other
/// producer's, so the List plane's lifecycle and the deployment-wide label
/// stats can leave these rows alone by kind.
pub const ROOM_MESSAGE_CONTENT_KIND: &str = "room_message";

/// The kind a **room-restricted post's** bus rows carry — the post twin of
/// [`ROOM_MESSAGE_CONTENT_KIND`], and distinct from the `post` kind the List
/// plane materializes and the feed projection serves to every follower: a
/// room post's verdicts reach a live floor member only.
pub const ROOM_POST_CONTENT_KIND: &str = "room_post";

/// Every kind this module writes — what the revoke purges and the
/// deployment-wide stats leave out. A kind added here must be added to
/// `moderation::get_label_stats`'s exclusion too (pinned by
/// `a_rooms_verdicts_stay_out_of_the_deployment_wide_label_stats`).
pub const ROOM_CONTENT_KINDS: [&str; 2] = [ROOM_MESSAGE_CONTENT_KIND, ROOM_POST_CONTENT_KIND];

/// `content_labels.mechanism_type` for a room labeler's verdict — a published
/// third-party scanner (`fauna_core::label::MechanismType::ExternalScanner`).
const ROOM_LABEL_MECHANISM: u8 = fauna_core::label::MechanismType::ExternalScanner as u8;

/// One category verdict a named labeler produced for one room message or
/// room post.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomLabel {
    /// One of the canonical five — the caller has already dropped anything
    /// else (`moderation.md` § Categories & enforcement).
    pub category: String,
    /// Clamped to `[0, 1]` by the caller; a labeler's output is untrusted.
    pub confidence: f64,
    /// The labeler that produced it (`AlgorithmLabeler::algorithm_id`).
    pub labeler_id: [u8; 32],
    /// The registry version that ran.
    pub labeler_version: u64,
}

/// One room message's or room post's bus rows as a live floor member reads
/// them beside the content: one [`ContentLabelEntry`] per category (the
/// highest-confidence verdict, strongest first — the feed's per-row
/// projection) and every factor row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoomBus {
    pub labels: Vec<ContentLabelEntry>,
    pub scores: Vec<ScoreEntry>,
}

/// The bus id of the message at `seq` in `room_id` (module doc § The content
/// id).
pub fn room_message_content_id(room_id: &[u8; 32], seq: i64) -> [u8; 40] {
    let mut id = [0u8; 40];
    id[..32].copy_from_slice(room_id);
    id[32..].copy_from_slice(&(seq as u64).to_be_bytes());
    id
}

/// The bus id of the room-restricted post `post_id` as `room_id` indexed it
/// (module doc § The content id).
pub fn room_post_content_id(room_id: &[u8; 32], post_id: &[u8; 32]) -> [u8; 64] {
    let mut id = [0u8; 64];
    id[..32].copy_from_slice(room_id);
    id[32..].copy_from_slice(post_id);
    id
}

/// The seq a room message bus id names, when `id` is one.
fn seq_of(id: &[u8]) -> Option<i64> {
    let tail: [u8; 8] = id.get(32..40)?.try_into().ok()?;
    Some(u64::from_be_bytes(tail) as i64)
}

/// The post id a room post bus id names, when `id` is one.
fn post_of(id: &[u8]) -> Option<[u8; 32]> {
    id.get(32..64)?.try_into().ok()
}

/// The inclusive id range holding every message bus row of `room_id` between
/// two seqs — a range rather than a prefix match so both tables' existing
/// indexes (`content_scores`' primary key, `idx_content_labels_ref`) serve it.
fn id_range(room_id: &[u8; 32], lo_seq: i64, hi_seq: i64) -> ([u8; 40], [u8; 40]) {
    (
        room_message_content_id(room_id, lo_seq),
        room_message_content_id(room_id, hi_seq),
    )
}

/// Every bus id of `room_id` at one id width, whatever its tail — `N` is 40
/// for the message kind and 64 for the post kind. Per kind rather than one
/// range over both: the hex form is compared as text, where a longer id
/// whose tail starts `ff…` would sort past a shorter kind's upper bound.
fn whole_room<const N: usize>(room_id: &[u8; 32]) -> ([u8; N], [u8; N]) {
    let mut hi = [0xffu8; N];
    hi[..32].copy_from_slice(room_id);
    let mut lo = [0u8; N];
    lo[..32].copy_from_slice(room_id);
    (lo, hi)
}

/// Delete every bus row of `kind` whose id lies in `[lo, hi]`, on a
/// connection the caller already holds. Returns how many rows went.
fn purge_range(conn: &rusqlite::Connection, kind: &str, lo: &[u8], hi: &[u8]) -> Result<usize> {
    let labels = conn
        .execute(
            "DELETE FROM content_labels
              WHERE content_type = ?1 AND content_id >= ?2 AND content_id <= ?3",
            rusqlite::params![kind, hex::encode(lo), hex::encode(hi)],
        )
        .context("purge a room's category verdicts")?;
    let scores = conn
        .execute(
            "DELETE FROM content_scores
              WHERE content_kind = ?1 AND content_id >= ?2 AND content_id <= ?3",
            rusqlite::params![kind, lo, hi],
        )
        .context("purge a room's factor rows")?;
    Ok(labels + scores)
}

/// Count the bus rows of `kind` whose id lies in `[lo, hi]`.
fn count_range(conn: &rusqlite::Connection, kind: &str, lo: &[u8], hi: &[u8]) -> Result<usize> {
    let labels: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM content_labels
              WHERE content_type = ?1 AND content_id >= ?2 AND content_id <= ?3",
            rusqlite::params![kind, hex::encode(lo), hex::encode(hi)],
            |r| r.get(0),
        )
        .context("count a room's category verdicts")?;
    let scores: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM content_scores
              WHERE content_kind = ?1 AND content_id >= ?2 AND content_id <= ?3",
            rusqlite::params![kind, lo, hi],
            |r| r.get(0),
        )
        .context("count a room's factor rows")?;
    Ok((labels + scores) as usize)
}

/// Delete every bus row derived from `room_id` — its messages' and its
/// posts' alike — on a connection the caller already holds: the half of the
/// revoke this module owns, run inside [`CacheDb::purge_room_derived_views`]
/// so the room's views go in one act.
///
/// Returns how many rows went.
pub(super) fn purge_room_bus(conn: &rusqlite::Connection, room_id: &[u8; 32]) -> Result<usize> {
    let (lo, hi) = whole_room::<40>(room_id);
    let messages = purge_range(conn, ROOM_MESSAGE_CONTENT_KIND, &lo, &hi)?;
    let (lo, hi) = whole_room::<64>(room_id);
    let posts = purge_range(conn, ROOM_POST_CONTENT_KIND, &lo, &hi)?;
    Ok(messages + posts)
}

/// Delete the bus rows of one room-restricted post as `room_id` indexed it —
/// the post's half of [`CacheDb::purge_room_post_views_for_post`]. Returns how
/// many rows went.
pub(super) fn purge_room_post_bus(
    conn: &rusqlite::Connection,
    room_id: &[u8; 32],
    post_id: &[u8; 32],
) -> Result<usize> {
    let id = room_post_content_id(room_id, post_id);
    purge_range(conn, ROOM_POST_CONTENT_KIND, &id, &id)
}

/// Write one piece of room content's bus rows — its category verdicts and
/// its factor rows — in one transaction, under `kind` at `id`.
///
/// `actor_id` stays NULL on every factor row: the bus's actor column is a
/// content-at-rest owner's scope, which room content does not have, and NULL
/// is also what keeps these rows out of every owner-scoped re-score worklist
/// (`content_scores_behind_for_owner`) — nothing drains them; the nest wrote
/// them in the act that opened the content, and only the next such act or
/// the revoke changes them.
fn record_bus(
    conn: &mut rusqlite::Connection,
    kind: &str,
    id: &[u8],
    labels: &[RoomLabel],
    scores: &[ScoreEntry],
    scanner_id: &[u8; 32],
) -> Result<()> {
    if labels.is_empty() && scores.is_empty() {
        return Ok(());
    }
    let id_hex = hex::encode(id);
    let now = now_epoch_millis();
    let tx = conn.transaction().context("begin record room bus")?;
    for label in labels {
        tx.execute(
            "INSERT INTO content_labels
                (content_type, content_id, category, confidence,
                 mechanism_type, classifier_id, classifier_version,
                 attestation_type, attestation_data, obligation_id,
                 created_at, scanner_id, signature)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, NULL, NULL, ?8, ?9, X'')
             ON CONFLICT(content_type, content_id, category, classifier_id)
             DO UPDATE SET confidence = excluded.confidence,
                           classifier_version = excluded.classifier_version,
                           created_at = excluded.created_at",
            rusqlite::params![
                kind,
                id_hex,
                label.category,
                label.confidence,
                ROOM_LABEL_MECHANISM,
                label.labeler_id.as_slice(),
                label.labeler_version as i64,
                now,
                scanner_id.as_slice(),
            ],
        )
        .context("record a room content's category verdict")?;
    }
    for score in scores {
        tx.execute(
            "INSERT OR REPLACE INTO content_scores
                (content_id, content_kind, factor, score, tier,
                 scorer_version, scored_at, actor_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
            rusqlite::params![
                id,
                kind,
                score.factor,
                score.score,
                score.tier as i64,
                score.scorer_version as i64,
                now,
            ],
        )
        .context("record a room content's factor row")?;
    }
    tx.commit().context("commit record room bus")?;
    Ok(())
}

/// Read the bus rows of `kind` in `[lo, hi]`, grouped by whatever `key_of`
/// makes of each id (a seq, a post id). An id `key_of` cannot read, or a key
/// not in `wanted`, is skipped. Labels come strongest first — the feed
/// projection's order, so an app that shows one badge and an app that shows
/// all agree on which leads.
fn read_bus<K: Ord + Copy>(
    conn: &rusqlite::Connection,
    kind: &str,
    lo: &[u8],
    hi: &[u8],
    wanted: &BTreeSet<K>,
    key_of: impl Fn(&[u8]) -> Option<K>,
) -> Result<BTreeMap<K, RoomBus>> {
    let mut out: BTreeMap<K, RoomBus> = BTreeMap::new();

    let mut stmt = conn
        .prepare(
            "SELECT content_id, category, MAX(confidence) FROM content_labels
              WHERE content_type = ?1 AND content_id >= ?2 AND content_id <= ?3
              GROUP BY content_id, category",
        )
        .context("prepare the room label projection")?;
    let rows = stmt
        .query_map(
            rusqlite::params![kind, hex::encode(lo), hex::encode(hi)],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, f64>(2)?,
                ))
            },
        )
        .context("query the room label projection")?;
    for row in rows {
        let (id_hex, category, confidence) = row?;
        let Some(key) = hex::decode(&id_hex).ok().and_then(|id| key_of(&id)) else {
            continue;
        };
        if !wanted.contains(&key) {
            continue;
        }
        out.entry(key).or_default().labels.push(ContentLabelEntry {
            category,
            confidence_per_mille: (confidence * 1000.0).round().clamp(0.0, 1000.0) as u16,
        });
    }
    drop(stmt);

    let mut stmt = conn
        .prepare(
            "SELECT content_id, factor, score, tier, scorer_version FROM content_scores
              WHERE content_kind = ?1 AND content_id >= ?2 AND content_id <= ?3
              ORDER BY content_id, factor",
        )
        .context("prepare the room factor projection")?;
    let rows = stmt
        .query_map(rusqlite::params![kind, lo, hi], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                ScoreEntry {
                    factor: row.get(1)?,
                    score: row.get(2)?,
                    tier: row.get::<_, i64>(3)?.clamp(0, u8::MAX as i64) as u8,
                    scorer_version: row.get::<_, i64>(4)?.clamp(0, u32::MAX as i64) as u32,
                },
            ))
        })
        .context("query the room factor projection")?;
    for row in rows {
        let (id, score) = row?;
        let Some(key) = key_of(&id) else { continue };
        if wanted.contains(&key) {
            out.entry(key).or_default().scores.push(score);
        }
    }

    for bus in out.values_mut() {
        bus.labels
            .sort_by_key(|l| std::cmp::Reverse(l.confidence_per_mille));
    }
    Ok(out)
}

impl CacheDb {
    /// The room's stored labeler set: its version (0 when the room never named
    /// one) and the signed bytes, when there are any.
    pub async fn get_room_labelers(&self, room_id: &[u8; 32]) -> Result<(u64, Option<Vec<u8>>)> {
        let room_id = *room_id;
        let conn = self.conn.lock().await;
        let row = conn
            .query_row(
                "SELECT labelers_version, labelers_blob FROM rooms WHERE room_id = ?1",
                rusqlite::params![room_id.as_slice()],
                |row| {
                    Ok((
                        row.get::<_, Option<i64>>(0)?,
                        row.get::<_, Option<Vec<u8>>>(1)?,
                    ))
                },
            )
            .optional()
            .context("get_room_labelers")?;
        Ok(match row {
            Some((version, blob)) => (version.unwrap_or(0).max(0) as u64, blob),
            None => (0, None),
        })
    }

    /// Store a new labeler set iff the room is still at `expected_version` —
    /// the strict ratchet's compare-and-set, the `set_room_policy` shape.
    ///
    /// `Ok(false)` means the version moved between the caller's read and this
    /// write; nothing was stored.
    pub async fn set_room_labelers(
        &self,
        room_id: &[u8; 32],
        expected_version: u64,
        new_version: u64,
        signed_blob: &[u8],
    ) -> Result<bool> {
        let room_id = *room_id;
        let blob = signed_blob.to_vec();
        let conn = self.conn.lock().await;
        let touched = conn
            .execute(
                "UPDATE rooms SET labelers_version = ?3, labelers_blob = ?4, updated_at = ?5
                  WHERE room_id = ?1 AND COALESCE(labelers_version, 0) = ?2",
                rusqlite::params![
                    room_id.as_slice(),
                    expected_version as i64,
                    new_version as i64,
                    blob,
                    now_epoch_millis(),
                ],
            )
            .context("store the room's labeler set")?;
        Ok(touched == 1)
    }

    /// Write one room message's bus rows — its category verdicts and its
    /// factor rows — in one transaction (`record_bus`'s NULL-actor rule).
    pub async fn record_room_message_bus(
        &self,
        room_id: &[u8; 32],
        seq: i64,
        labels: &[RoomLabel],
        scores: &[ScoreEntry],
        scanner_id: &[u8; 32],
    ) -> Result<()> {
        let id = room_message_content_id(room_id, seq);
        let mut conn = self.conn.lock().await;
        record_bus(
            &mut conn,
            ROOM_MESSAGE_CONTENT_KIND,
            &id,
            labels,
            scores,
            scanner_id,
        )
    }

    /// Write one room-restricted post's bus rows, as `room_id` indexed it —
    /// the post twin of [`Self::record_room_message_bus`].
    pub async fn record_room_post_bus(
        &self,
        room_id: &[u8; 32],
        post_id: &[u8; 32],
        labels: &[RoomLabel],
        scores: &[ScoreEntry],
        scanner_id: &[u8; 32],
    ) -> Result<()> {
        let id = room_post_content_id(room_id, post_id);
        let mut conn = self.conn.lock().await;
        record_bus(
            &mut conn,
            ROOM_POST_CONTENT_KIND,
            &id,
            labels,
            scores,
            scanner_id,
        )
    }

    /// The bus rows of the messages at `seqs` in `room_id`, keyed by seq — the
    /// projection `fauna.conversations.channel.fetch` serves a live floor
    /// member beside each envelope. A seq with no rows has no entry.
    pub async fn room_message_bus(
        &self,
        room_id: &[u8; 32],
        seqs: &[i64],
    ) -> Result<BTreeMap<i64, RoomBus>> {
        let wanted: BTreeSet<i64> = seqs.iter().copied().collect();
        let (Some(&lo_seq), Some(&hi_seq)) = (wanted.first(), wanted.last()) else {
            return Ok(BTreeMap::new());
        };
        let (lo, hi) = id_range(room_id, lo_seq, hi_seq);
        let conn = self.conn.lock().await;
        read_bus(&conn, ROOM_MESSAGE_CONTENT_KIND, &lo, &hi, &wanted, seq_of)
    }

    /// The bus rows of the room-restricted posts `post_ids` as `room_id`
    /// indexed them, keyed by post id — the projection
    /// `fauna.posts.room_labels` serves a live floor member. A post with no
    /// rows has no entry.
    pub async fn room_post_bus(
        &self,
        room_id: &[u8; 32],
        post_ids: &[[u8; 32]],
    ) -> Result<BTreeMap<[u8; 32], RoomBus>> {
        let wanted: BTreeSet<[u8; 32]> = post_ids.iter().copied().collect();
        let (Some(lo_post), Some(hi_post)) = (wanted.first(), wanted.last()) else {
            return Ok(BTreeMap::new());
        };
        let lo = room_post_content_id(room_id, lo_post);
        let hi = room_post_content_id(room_id, hi_post);
        let conn = self.conn.lock().await;
        read_bus(&conn, ROOM_POST_CONTENT_KIND, &lo, &hi, &wanted, post_of)
    }

    /// How many message bus rows — category verdicts plus factor rows — this
    /// room's derived view holds.
    ///
    /// The half of the view no read can see: a member reads labels beside
    /// messages, and after a revoke that read answers empty whether or not
    /// the rows survived it. This counts what the store actually holds.
    pub async fn count_room_message_bus_rows(&self, room_id: &[u8; 32]) -> Result<usize> {
        let (lo, hi) = whole_room::<40>(room_id);
        let conn = self.conn.lock().await;
        count_range(&conn, ROOM_MESSAGE_CONTENT_KIND, &lo, &hi)
    }

    /// How many room-post bus rows this room's derived view holds — the post
    /// twin of [`Self::count_room_message_bus_rows`], for the same reason.
    pub async fn count_room_post_bus_rows(&self, room_id: &[u8; 32]) -> Result<usize> {
        let (lo, hi) = whole_room::<64>(room_id);
        let conn = self.conn.lock().await;
        count_range(&conn, ROOM_POST_CONTENT_KIND, &lo, &hi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(category: &str, confidence: f64) -> RoomLabel {
        RoomLabel {
            category: category.into(),
            confidence,
            labeler_id: [0x11u8; 32],
            labeler_version: 1,
        }
    }

    fn score(factor: &str, score: i64) -> ScoreEntry {
        ScoreEntry {
            factor: factor.into(),
            score,
            tier: fauna_core::scoring::TIER_COMMUNITY,
            scorer_version: 1,
        }
    }

    #[test]
    fn a_room_message_id_is_its_room_then_its_seq() {
        let room = [0x22u8; 32];
        let id = room_message_content_id(&room, 7);
        assert_eq!(&id[..32], &room);
        assert_eq!(seq_of(&id), Some(7));
        // Seqs order the same as their ids, which is what lets a page's rows be
        // one range scan.
        assert!(room_message_content_id(&room, 9) < room_message_content_id(&room, 10));
        assert!(room_message_content_id(&room, 255) < room_message_content_id(&room, 256));
    }

    #[test]
    fn a_room_post_id_is_its_room_then_its_post() {
        let (room, post) = ([0x22u8; 32], [0x77u8; 32]);
        let id = room_post_content_id(&room, &post);
        assert_eq!(&id[..32], &room);
        assert_eq!(post_of(&id), Some(post));
        // A message id is never read as a post id, nor the reverse: the widths
        // differ, and each kind's readers only ever see their own kind.
        assert_eq!(post_of(&room_message_content_id(&room, 7)), None);
    }

    #[tokio::test]
    async fn one_rooms_rows_are_its_own_to_read_count_and_purge() {
        let db = CacheDb::open_in_memory().unwrap();
        let (a, b) = ([0x31u8; 32], [0x32u8; 32]);
        let scanner = [0u8; 32];
        for (room, seq) in [(a, 1), (a, 2), (b, 1)] {
            db.record_room_message_bus(
                &room,
                seq,
                &[label("spam", 0.9), label("nsfw", 0.4)],
                &[score("labeler:ab", 900)],
                &scanner,
            )
            .await
            .unwrap();
        }
        assert_eq!(db.count_room_message_bus_rows(&a).await.unwrap(), 6);

        let page = db.room_message_bus(&a, &[2, 5]).await.unwrap();
        assert_eq!(
            page.len(),
            1,
            "seq 5 holds nothing, and seq 1 was not asked for"
        );
        let bus = &page[&2];
        assert_eq!(
            bus.labels,
            vec![
                ContentLabelEntry {
                    category: "spam".into(),
                    confidence_per_mille: 900
                },
                ContentLabelEntry {
                    category: "nsfw".into(),
                    confidence_per_mille: 400
                },
            ],
            "strongest first"
        );
        assert_eq!(bus.scores, vec![score("labeler:ab", 900)]);

        let conn = db.conn.lock().await;
        assert_eq!(purge_room_bus(&conn, &a).unwrap(), 6);
        drop(conn);
        assert_eq!(db.count_room_message_bus_rows(&a).await.unwrap(), 0);
        assert_eq!(
            db.count_room_message_bus_rows(&b).await.unwrap(),
            3,
            "a revoke is a statement about one room"
        );
    }

    #[tokio::test]
    async fn a_rooms_post_rows_read_by_post_and_purge_with_the_room_or_the_post() {
        let db = CacheDb::open_in_memory().unwrap();
        let (a, b) = ([0x41u8; 32], [0x42u8; 32]);
        let (p1, p2, p3) = ([0x01u8; 32], [0x02u8; 32], [0xffu8; 32]);
        let scanner = [0u8; 32];
        for (room, post) in [(a, p1), (a, p3), (b, p2)] {
            db.record_room_post_bus(
                &room,
                &post,
                &[label("spam", 0.9)],
                &[score("labeler:ab", 900)],
                &scanner,
            )
            .await
            .unwrap();
        }
        // A message row in the same room, so the counts prove the kinds stay
        // apart: the post count never sees it and the message count never
        // sees the posts.
        db.record_room_message_bus(&a, 1, &[label("spam", 0.5)], &[], &scanner)
            .await
            .unwrap();
        assert_eq!(db.count_room_post_bus_rows(&a).await.unwrap(), 4);
        assert_eq!(db.count_room_message_bus_rows(&a).await.unwrap(), 1);

        let read = db.room_post_bus(&a, &[p3, p2, p1]).await.unwrap();
        assert_eq!(
            read.keys().copied().collect::<Vec<_>>(),
            vec![p1, p3],
            "p2 is another room's post — asked for, not answered"
        );
        assert_eq!(read[&p3].labels[0].confidence_per_mille, 900);
        assert_eq!(read[&p3].scores, vec![score("labeler:ab", 900)]);

        // The post-scoped purge: one post's rows, nothing else's.
        let conn = db.conn.lock().await;
        assert_eq!(purge_room_post_bus(&conn, &a, &p1).unwrap(), 2);
        drop(conn);
        assert_eq!(db.count_room_post_bus_rows(&a).await.unwrap(), 2);
        assert_eq!(db.count_room_message_bus_rows(&a).await.unwrap(), 1);

        // The revoke: every kind of the room's rows, and only that room's. The
        // post with the all-0xff id is the one a single range over both
        // widths would have missed.
        let conn = db.conn.lock().await;
        assert_eq!(purge_room_bus(&conn, &a).unwrap(), 3);
        drop(conn);
        assert_eq!(db.count_room_post_bus_rows(&a).await.unwrap(), 0);
        assert_eq!(db.count_room_message_bus_rows(&a).await.unwrap(), 0);
        assert_eq!(db.count_room_post_bus_rows(&b).await.unwrap(), 2);
    }

    #[tokio::test]
    async fn a_list_withdrawal_never_reaches_a_rooms_rows_under_the_same_factor() {
        // A room may name a labeler that users also subscribe to, so its rows
        // and a List's can share one `labeler:<id>` factor. The List plane
        // withdraws the rows it materialized — posts — and nothing else.
        let db = CacheDb::open_in_memory().unwrap();
        let room = [0x33u8; 32];
        db.insert_content_scores(&[0x55u8; 32], "post", None, 1, &[score("labeler:ab", 700)])
            .await
            .unwrap();
        db.record_room_message_bus(&room, 1, &[], &[score("labeler:ab", 900)], &[0u8; 32])
            .await
            .unwrap();
        db.record_room_post_bus(
            &room,
            &[0x56u8; 32],
            &[],
            &[score("labeler:ab", 800)],
            &[0u8; 32],
        )
        .await
        .unwrap();

        assert_eq!(
            db.withdraw_labeler_factor_scores("labeler:ab")
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            db.count_room_message_bus_rows(&room).await.unwrap(),
            1,
            "the room's row is the room's derived view, gone only with the room's revoke"
        );
        assert_eq!(
            db.count_room_post_bus_rows(&room).await.unwrap(),
            1,
            "and a room post's row is a room post's, never the List plane's"
        );
    }

    #[tokio::test]
    async fn a_rooms_verdicts_stay_out_of_the_deployment_wide_label_stats() {
        let db = CacheDb::open_in_memory().unwrap();
        db.record_room_message_bus(&[0x34u8; 32], 1, &[label("spam", 0.9)], &[], &[0u8; 32])
            .await
            .unwrap();
        db.record_room_post_bus(
            &[0x34u8; 32],
            &[0x35u8; 32],
            &[label("spam", 0.9)],
            &[],
            &[0u8; 32],
        )
        .await
        .unwrap();
        assert!(
            db.get_label_stats().await.unwrap().is_empty(),
            "a verdict derived for a room's members is not the deployment's to count"
        );
    }
}
