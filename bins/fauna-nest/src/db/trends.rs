//! The nest trend scorer — the metadata-only `trending` bus factor.
//!
//! Scans the explicit-act event log (`engagement_events`) for one public post,
//! computes the decayed local velocity (`fauna_core::scoring::trends`), and
//! upserts / withdraws its `content_scores` row `{factor: "trending", tier: 3,
//! actor_id: NULL}`. Two recompute choke points, matched to the two ways the
//! value moves:
//!   - **the engagement transition** — inside `db::engagement::insert` /
//!     `delete_engagement_event`, so a new act is reflected at once (the event
//!     log is this factor's input, exactly as the counter columns are the
//!     `engagement` scalar's, so each materialized score recomputes at the choke
//!     point of *its own* input);
//!   - **the ~15-min sweep** ([`CacheDb::sweep_trend_scores`], driven by
//!     `trend_sweeper`), which re-decays live rows against wall-clock and
//!     withdraws those fallen to 0‰ — the wall-clock decay a transition can't
//!     catch.
//!
//! Metadata only (event log + timestamps, never content) and public posts only,
//! so the nest is legitimately the scorer. Like `report:spam`, the factor is
//! deliberately OUT of `model_versions` (no capability-holder drain): a curve
//! change bumps [`fauna_core::scoring::scorer_version::TREND`] and the sweep
//! re-scores nest-side. Owner doc: `docs/goal/behavior/trending.md`.

use super::CacheDb;
use anyhow::{Context, Result};
use fauna_core::scoring::{TIER_COMMUNITY, factor, scorer_version, trends};
use rusqlite::{Connection, OptionalExtension};

impl CacheDb {
    /// Recompute the `trending` row for one post from the current event log, on
    /// the connection already held by the caller. `now_us` is wall-clock
    /// microseconds (the unit of `engagement_events.created_at`); it decays every
    /// event's weight to now.
    ///
    /// **Public posts only** (`trending.md` § Public posts only): a gated post
    /// (`content_meta.gated_tier IS NOT NULL`) or an unseen post (no
    /// `content_meta` row at all — never a blind row for content this nest lacks)
    /// scores 0, which withdraws any stale row. **Withdraw is a DELETE** of the
    /// `(content_id, "trending")` row, never a `score = 0` row (the report-factor
    /// precedent, `db/reports.rs`): the composed feed then simply finds no
    /// trending term for the post.
    pub(crate) fn recompute_trend_score_locked(
        conn: &Connection,
        content_id: &[u8],
        now_us: i64,
    ) -> Result<()> {
        // NULL gated_tier = public. `.optional()` → None when the post is unseen
        // (no content_meta row); either non-public case scores 0 below.
        let is_public = conn
            .query_row(
                "SELECT gated_tier IS NULL FROM content_meta WHERE content_id = ?1",
                rusqlite::params![content_id],
                |r| r.get::<_, i64>(0).map(|v| v != 0),
            )
            .optional()
            .context("probe post audience for trend recompute")?
            == Some(true);

        // The whole score — local velocity AND the peer ramp — is gated behind
        // public+seen: a gated or unseen post scores 0 and any stale row is
        // withdrawn. Critically the peer ramp lives INSIDE this gate too, so peer
        // corroboration never mints a blind `trending` row for content this nest
        // lacks (`trending.md` § Import-triggered fetch — "never a blind row for
        // unseen content"); the Slice-4 `post.get` fetch is what surfaces a
        // peer-only post, after which recompute picks up its ramp.
        let score = if is_public {
            let mut stmt = conn
                .prepare(
                    "SELECT event_type, created_at FROM engagement_events
                     WHERE content_id = ?1",
                )
                .context("prepare trend velocity scan")?;
            let events: Vec<(String, i64)> = stmt
                .query_map(rusqlite::params![content_id], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .context("scan engagement events")?
                .filter_map(|r| r.ok())
                .collect();
            // The shared weight table (`event_weight_milli`) filters unknown and
            // deliberately-excluded kinds (views) — don't duplicate the vocabulary
            // in SQL (priority #2: one source of truth for the weights).
            let v = trends::local_velocity(
                events
                    .iter()
                    .map(|(kind, at_us)| (kind.as_str(), (now_us - at_us) / 1_000_000)),
            );
            let local_pm = trends::local_trend_pm(v);
            // The bounded distinct-peer ramp: COUNT(DISTINCT peer_nest_id) over
            // LIVE (un-expired) peer rows — presence-only, never the claimed
            // magnitude (`peer_ramp_pm`; hostile-signer invariant). A row past the
            // TTL is filtered at read here (the sweep purges it physically).
            let cutoff_ms = now_us / 1000 - TREND_PEER_TTL_HOURS * 3_600_000;
            let distinct_peers: u32 = conn
                .query_row(
                    "SELECT COUNT(DISTINCT peer_nest_id) FROM peer_content_trends
                     WHERE content_id = ?1 AND updated_at >= ?2",
                    rusqlite::params![content_id, cutoff_ms],
                    |r| r.get::<_, i64>(0).map(|c| c.max(0) as u32),
                )
                .context("count live distinct trend peers")?;
            trends::trend_score_pm(local_pm, distinct_peers)
        } else {
            0
        };

        if score > 0 {
            conn.execute(
                "INSERT OR REPLACE INTO content_scores
                     (content_id, content_kind, factor, score, tier,
                      scorer_version, scored_at, actor_id)
                 VALUES (?1, 'post', ?2, ?3, ?4, ?5, ?6, NULL)",
                rusqlite::params![
                    content_id,
                    factor::TRENDING,
                    score,
                    TIER_COMMUNITY as i64,
                    scorer_version::TREND as i64,
                    now_us / 1000, // scored_at in epoch-millis (reports precedent)
                ],
            )
            .context("write trending bus row")?;
        } else {
            conn.execute(
                "DELETE FROM content_scores WHERE content_id = ?1 AND factor = ?2",
                rusqlite::params![content_id, factor::TRENDING],
            )
            .context("withdraw trending bus row")?;
        }
        Ok(())
    }

    /// Async wrapper over [`recompute_trend_score_locked`] that takes the lock —
    /// for the sweep's per-row recompute and any external caller.
    pub async fn recompute_trend_score(&self, content_id: &[u8], now_us: i64) -> Result<()> {
        let conn = self.conn.lock().await;
        Self::recompute_trend_score_locked(&conn, content_id, now_us)
    }

    /// The ~15-min sweep core: re-decay every live `trending` row against the
    /// current wall-clock and withdraw those fallen to 0‰. The live rows ARE the
    /// state (`trending.md` § The factor), so the sweep set is exactly the
    /// existing `trending` rows — a post with no row needs no decay, and a post
    /// gaining its first act is picked up by the transition hook, not here.
    /// Returns the number of rows swept. Driven by
    /// `trend_sweeper::spawn_trend_sweeper`.
    pub async fn sweep_trend_scores(&self, now_us: i64) -> Result<usize> {
        let conn = self.conn.lock().await;
        // Purge peer trend rows past the TTL first (storage reclaim + the
        // "expiry withdraws the presence bit" half of `trending.md` § Peer
        // storage is bounded). The recompute below already TTL-filters at read,
        // so this bulk delete is what keeps orphan peer rows (a post whose
        // trending row was never minted — imported while unseen) from lingering
        // past the per-peer cap. Content whose only signal was now-expired peers
        // is re-scored to 0 by the loop that follows (its row is in the set).
        let cutoff_ms = now_us / 1000 - TREND_PEER_TTL_HOURS * 3_600_000;
        conn.execute(
            "DELETE FROM peer_content_trends WHERE updated_at < ?1",
            rusqlite::params![cutoff_ms],
        )
        .context("purge expired peer trend rows")?;
        let ids: Vec<Vec<u8>> = {
            let mut stmt = conn
                .prepare("SELECT content_id FROM content_scores WHERE factor = ?1")
                .context("prepare trend sweep scan")?;
            stmt.query_map(rusqlite::params![factor::TRENDING], |r| r.get(0))
                .context("scan live trend rows")?
                .filter_map(|r| r.ok())
                .collect()
        };
        for id in &ids {
            Self::recompute_trend_score_locked(&conn, id, now_us)?;
        }
        Ok(ids.len())
    }

    /// Import one peer's ≥k trend entry (`trending.md` § Federation exchange):
    /// latest-epoch-wins per `(content_id, peer)`; a stale epoch is a harmless
    /// replay (ignored). Enforces the per-peer stored-row cap
    /// ([`MAX_PEER_TREND_ROWS`]) by evicting that peer's oldest-updated rows — the
    /// id universe is unbounded, so a hostile peer must not grow the DB without
    /// bound. `score_pm`/`engager_count` are stored verbatim as a fetch-priority
    /// hint + the ramp's re-validation input; they are NEVER summed and NEVER
    /// re-exported (the ramp counts *distinct peers*, never magnitudes). Returns
    /// `true` when the row landed (the caller then recomputes the aggregate).
    pub async fn upsert_peer_trend(
        &self,
        peer_nest_id: &[u8; 32],
        content_id: &[u8; 32],
        score_pm: u16,
        engager_count: u32,
        epoch: i64,
        now_ms: i64,
    ) -> Result<bool> {
        let peer = *peer_nest_id;
        let id = *content_id;
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "INSERT INTO peer_content_trends
                    (content_id, peer_nest_id, score_pm, engager_count, epoch, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(content_id, peer_nest_id) DO UPDATE SET
                    score_pm = excluded.score_pm,
                    engager_count = excluded.engager_count,
                    epoch = excluded.epoch,
                    updated_at = excluded.updated_at
                 WHERE excluded.epoch >= peer_content_trends.epoch",
                rusqlite::params![
                    &id[..],
                    &peer[..],
                    score_pm as i64,
                    engager_count as i64,
                    epoch,
                    now_ms
                ],
            )
            .context("upsert peer trend")?;
        // Per-peer cap: evict this peer's oldest-updated rows past the cap.
        conn.execute(
            "DELETE FROM peer_content_trends
             WHERE peer_nest_id = ?1
               AND rowid NOT IN (
                   SELECT rowid FROM peer_content_trends
                   WHERE peer_nest_id = ?1
                   ORDER BY updated_at DESC
                   LIMIT ?2)",
            rusqlite::params![&peer[..], MAX_PEER_TREND_ROWS],
        )
        .context("enforce per-peer trend cap")?;
        Ok(changed > 0)
    }

    /// The federation-export view (`trending.md` § Federation exchange): this
    /// nest's LOCAL trend entries only, each re-derived fresh from the event log
    /// at `now_us` so the wire `score_pm` is the exporter's un-composed `local_pm`
    /// — NEVER the peer-ramped `content_scores.score` (no laundering: a hostile
    /// aggregate cannot wash through an honest intermediary). Gated to `local_pm >
    /// 0` AND ≥ [`trends::TREND_MIN_ENGAGERS`] distinct local engagers (the
    /// `exposed_trend_entry` k-gate — the ONLY export path, so a below-k pattern
    /// never crosses any wire). Highest `local_pm` first, bounded to
    /// [`MAX_TREND_EXCHANGE_ENTRIES`] so an export is always importable whole.
    ///
    /// Candidate set = the live `trending` rows: `local_pm > 0` implies a row
    /// exists (recompute wrote it), and a row implies public+seen, so the set is
    /// an exact superset — the `local_pm > 0` filter drops peer-only rows.
    pub async fn export_trend_entries(&self, now_us: i64) -> Result<Vec<([u8; 32], u16, u32)>> {
        let conn = self.conn.lock().await;
        let candidates: Vec<Vec<u8>> = {
            let mut stmt = conn
                .prepare("SELECT content_id FROM content_scores WHERE factor = ?1")
                .context("prepare trend export candidate scan")?;
            stmt.query_map(rusqlite::params![factor::TRENDING], |r| r.get(0))
                .context("scan trend export candidates")?
                .filter_map(|r| r.ok())
                .collect()
        };
        let mut entries: Vec<([u8; 32], u16, u32)> = Vec::new();
        for id in &candidates {
            let Ok(content_id) = <[u8; 32]>::try_from(id.as_slice()) else {
                continue;
            };
            // Re-derive local_pm fresh (un-laundered) from the event log at now.
            let events: Vec<(String, i64)> = {
                let mut stmt = conn
                    .prepare(
                        "SELECT event_type, created_at FROM engagement_events
                         WHERE content_id = ?1",
                    )
                    .context("prepare export velocity scan")?;
                stmt.query_map(rusqlite::params![id], |r| Ok((r.get(0)?, r.get(1)?)))
                    .context("scan export events")?
                    .filter_map(|r| r.ok())
                    .collect()
            };
            let v = trends::local_velocity(
                events
                    .iter()
                    .map(|(kind, at_us)| (kind.as_str(), (now_us - at_us) / 1_000_000)),
            );
            let local_pm = trends::local_trend_pm(v);
            if local_pm <= 0 {
                continue;
            }
            // The k-gate input: distinct local engagers (NULL actors — anonymous
            // views — are ignored by COUNT DISTINCT, which is correct).
            let distinct: u32 = conn
                .query_row(
                    "SELECT COUNT(DISTINCT actor_id) FROM engagement_events
                     WHERE content_id = ?1",
                    rusqlite::params![id],
                    |r| r.get::<_, i64>(0).map(|c| c.max(0) as u32),
                )
                .context("count distinct local engagers")?;
            // THE k-gate — a below-k entry never leaves the nest.
            if trends::exposed_trend_entry(distinct).is_none() {
                continue;
            }
            entries.push((content_id, local_pm.min(u16::MAX as i64) as u16, distinct));
        }
        // Highest local_pm first (the fetch-priority order the wire hint mirrors).
        entries.sort_by_key(|e| std::cmp::Reverse(e.1));
        entries.truncate(MAX_TREND_EXCHANGE_ENTRIES);
        Ok(entries)
    }

    /// Has this nest seen `content_id` (a `content_meta` row exists)? The
    /// existence probe the Slice-4 import-triggered fetch runs to decide which
    /// pulled peer-trend ids are *unseen* here and therefore worth a `post.get`
    /// (`trending.md` § Import-triggered fetch) — a seen post already scores via
    /// the ramp inside [`recompute_trend_score_locked`], so only the unseen head
    /// is fetched. Mirrors `db::reports`' post-existence probe.
    pub async fn content_meta_exists(&self, content_id: &[u8; 32]) -> Result<bool> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM content_meta WHERE content_id = ?1)",
            rusqlite::params![content_id.as_slice()],
            |row| row.get::<_, i64>(0).map(|v| v != 0),
        )
        .context("probe content_meta existence")
    }
}

/// Hard cap on entries in a single `trends.exchange` request AND the export view
/// size (`trending.md` § Federation exchange) — one peer cannot exhaust the DB
/// (or make its export un-importable) in one call. Smaller than the reports cap:
/// trending is a "what's hot right now" head, not the full report universe.
pub const MAX_TREND_EXCHANGE_ENTRIES: usize = 256;

/// Per-peer stored-row cap on `peer_content_trends` (oldest-updated evicted). The
/// content-id universe is unbounded, so a hostile peer must not grow the DB
/// without bound (`trending.md` § Peer storage is bounded).
pub const MAX_PEER_TREND_ROWS: i64 = 1024;

/// Age TTL on a `peer_content_trends` row: a trend entry older than the decay
/// horizon is dead weight (`trending.md` § Peer storage is bounded). Expiry
/// withdraws the peer's presence bit — the recompute filters by TTL at read and
/// the ~15-min sweep purges rows physically.
pub const TREND_PEER_TTL_HOURS: i64 = 48;

/// Hard cap on `post.get` fetches triggered by ONE `trends.export` pull
/// (`trending.md` § Import-triggered fetch): a peer's trend head can name up to
/// [`MAX_TREND_EXCHANGE_ENTRIES`] unseen ids, but each fetch is a full cross-nest
/// round-trip + verify + ingest, so we bound the work per exchange and let the
/// remaining ids ride the next cycle (their presence bit is already stored, so
/// the ramp lands as soon as they're fetched). Ordered by the `score_pm` hint so
/// the hottest unseen posts surface first.
pub const MAX_TREND_FETCHES_PER_EXCHANGE: usize = 64;

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR_US: i64 = 3_600 * 1_000_000;

    /// Read the `trending` per-mille for a post, or `None` if no row exists.
    async fn trend_pm(db: &CacheDb, id: &[u8; 32]) -> Option<i64> {
        db.get_content_scores(id)
            .await
            .unwrap()
            .into_iter()
            .find(|e| e.factor == factor::TRENDING)
            .map(|e| e.score)
    }

    /// Seed a public `content_meta` row (NULL gated_tier).
    async fn seed_public(db: &CacheDb, id: &[u8; 32]) {
        let conn = db.conn.lock().await;
        crate::db::meta::upsert_meta(&conn, id, 0.0, false, false, None, None, None).unwrap();
    }

    /// Insert a `like` event for `id` by a distinct actor at `at_us`, writing the
    /// engagement_events row WITHOUT going through the counter path.
    async fn like_at(db: &CacheDb, id: &[u8; 32], actor: u8, at_us: i64) -> [u8; 32] {
        // event_id folds in BOTH the post and the actor: the same actor liking two
        // different posts must not collide on the `engagement_events` primary key.
        let mut event_id = *id;
        event_id[0] = 0xE0; // keep event ids distinct from content ids
        event_id[1] = actor; // distinct per actor within a post
        db.insert_engagement_event(&event_id, id, Some(&[actor; 32]), "like", None, at_us)
            .await
            .unwrap();
        event_id
    }

    /// Count a peer's stored `peer_content_trends` rows.
    async fn peer_trend_count(db: &CacheDb, peer: &[u8; 32]) -> i64 {
        let conn = db.conn.lock().await;
        conn.query_row(
            "SELECT COUNT(*) FROM peer_content_trends WHERE peer_nest_id = ?1",
            rusqlite::params![&peer[..]],
            |r| r.get(0),
        )
        .unwrap()
    }

    /// Read one peer's stored `(score_pm, engager_count, epoch)` for a content id.
    async fn peer_trend_row(
        db: &CacheDb,
        id: &[u8; 32],
        peer: &[u8; 32],
    ) -> Option<(i64, i64, i64)> {
        let conn = db.conn.lock().await;
        conn.query_row(
            "SELECT score_pm, engager_count, epoch FROM peer_content_trends
             WHERE content_id = ?1 AND peer_nest_id = ?2",
            rusqlite::params![&id[..], &peer[..]],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .unwrap()
    }

    /// The bounded distinct-peer ramp lifts a seen+public post's trending score
    /// (presence-only: the claimed `score_pm`/`engager_count` never enter the
    /// arithmetic — the ramp is `100·log2(1+distinct_peers)`).
    #[tokio::test]
    async fn peer_ramp_lifts_seen_public_post() {
        let db = CacheDb::open_in_memory().unwrap();
        let pid = [0xEE; 32];
        seed_public(&db, &pid).await;
        let now = 100 * HOUR_US;
        for actor in [1u8, 2, 3] {
            like_at(&db, &pid, actor, now).await;
        }
        // No peers yet → local only: v = 3 → local_pm = round(3000/23) = 130.
        db.recompute_trend_score(&pid, now).await.unwrap();
        assert_eq!(trend_pm(&db, &pid).await, Some(130));

        // One peer → +floor(100·log2(2)) = +100 (its claimed 999‰ is ignored).
        db.upsert_peer_trend(&[0x01; 32], &pid, 999, 8, 1, now / 1000)
            .await
            .unwrap();
        db.recompute_trend_score(&pid, now).await.unwrap();
        assert_eq!(trend_pm(&db, &pid).await, Some(230));

        // A second distinct peer → +floor(100·log2(3)) = +158.
        db.upsert_peer_trend(&[0x02; 32], &pid, 100, 4, 1, now / 1000)
            .await
            .unwrap();
        db.recompute_trend_score(&pid, now).await.unwrap();
        assert_eq!(trend_pm(&db, &pid).await, Some(288));
    }

    /// Peer corroboration NEVER mints a blind row for a post this nest has not
    /// seen — the ramp lives inside the public+seen gate (`trending.md`
    /// § Import-triggered fetch); the Slice-4 fetch is what surfaces such a post.
    #[tokio::test]
    async fn unseen_post_with_peers_never_scores() {
        let db = CacheDb::open_in_memory().unwrap();
        let pid = [0xAB; 32]; // no content_meta row → unseen
        let now = 100 * HOUR_US;
        db.upsert_peer_trend(&[0x01; 32], &pid, 200, 5, 1, now / 1000)
            .await
            .unwrap();
        db.recompute_trend_score(&pid, now).await.unwrap();
        assert_eq!(trend_pm(&db, &pid).await, None);
    }

    /// `upsert_peer_trend` is latest-epoch-wins per (content, peer): a stale epoch
    /// is a harmless replay that overwrites nothing and reports no change.
    #[tokio::test]
    async fn upsert_peer_trend_latest_epoch_wins() {
        let db = CacheDb::open_in_memory().unwrap();
        let pid = [0xCD; 32];
        let peer = [0x07; 32];
        assert!(
            db.upsert_peer_trend(&peer, &pid, 100, 5, 10, 1_000)
                .await
                .unwrap()
        );
        assert_eq!(peer_trend_row(&db, &pid, &peer).await, Some((100, 5, 10)));

        // Stale epoch (5 < 10) → no change, no overwrite.
        assert!(
            !db.upsert_peer_trend(&peer, &pid, 999, 9, 5, 2_000)
                .await
                .unwrap()
        );
        assert_eq!(peer_trend_row(&db, &pid, &peer).await, Some((100, 5, 10)));

        // Equal/newer epoch → overwrite.
        assert!(
            db.upsert_peer_trend(&peer, &pid, 300, 7, 11, 3_000)
                .await
                .unwrap()
        );
        assert_eq!(peer_trend_row(&db, &pid, &peer).await, Some((300, 7, 11)));
    }

    /// The per-peer row cap evicts that peer's oldest-updated rows so an
    /// unbounded id universe cannot grow the DB without bound.
    #[tokio::test]
    async fn upsert_peer_trend_enforces_per_peer_cap() {
        let db = CacheDb::open_in_memory().unwrap();
        let peer = [0x08; 32];
        let overflow = (MAX_PEER_TREND_ROWS + 2) as u64;
        for i in 0..overflow {
            let mut id = [0u8; 32];
            id[..8].copy_from_slice(&i.to_be_bytes());
            // updated_at = i so the lowest-i rows are the oldest-updated.
            db.upsert_peer_trend(&peer, &id, 100, 5, 1, i as i64)
                .await
                .unwrap();
        }
        assert_eq!(peer_trend_count(&db, &peer).await, MAX_PEER_TREND_ROWS);
        // The two oldest (i = 0, 1) were evicted; the newest survives.
        let mut oldest = [0u8; 32];
        oldest[..8].copy_from_slice(&0u64.to_be_bytes());
        assert_eq!(peer_trend_row(&db, &oldest, &peer).await, None);
        let mut newest = [0u8; 32];
        newest[..8].copy_from_slice(&(overflow - 1).to_be_bytes());
        assert!(peer_trend_row(&db, &newest, &peer).await.is_some());
    }

    /// A peer row past the 48 h TTL neither counts toward the ramp (filtered at
    /// read) nor survives the sweep (purged physically); a fresh peer does both.
    #[tokio::test]
    async fn expired_peer_row_neither_counts_nor_survives_sweep() {
        let db = CacheDb::open_in_memory().unwrap();
        let pid = [0xDE; 32];
        seed_public(&db, &pid).await;
        let now = 1_000 * HOUR_US;
        for actor in [1u8, 2, 3] {
            like_at(&db, &pid, actor, now).await; // local_pm = 130
        }
        // Peer updated 49 h ago → past the 48 h TTL.
        let expired_ms = now / 1000 - 49 * 3_600_000;
        db.upsert_peer_trend(&[0x01; 32], &pid, 200, 5, 1, expired_ms)
            .await
            .unwrap();
        db.recompute_trend_score(&pid, now).await.unwrap();
        assert_eq!(trend_pm(&db, &pid).await, Some(130)); // expired peer ignored

        // A fresh peer (updated now) counts → +100.
        db.upsert_peer_trend(&[0x02; 32], &pid, 200, 5, 1, now / 1000)
            .await
            .unwrap();
        db.recompute_trend_score(&pid, now).await.unwrap();
        assert_eq!(trend_pm(&db, &pid).await, Some(230));

        // The sweep purges the expired row but keeps the fresh one.
        assert_eq!(peer_trend_count(&db, &[0x01; 32]).await, 1);
        db.sweep_trend_scores(now).await.unwrap();
        assert_eq!(peer_trend_count(&db, &[0x01; 32]).await, 0);
        assert_eq!(peer_trend_count(&db, &[0x02; 32]).await, 1);
    }

    /// The export serves the exporter's UN-composed `local_pm` (no laundering),
    /// only for entries ≥ TREND_MIN_ENGAGERS distinct engagers AND live
    /// `local_pm > 0`, highest `local_pm` first, bounded to the cap.
    #[tokio::test]
    async fn export_serves_kgated_local_pm_no_laundering() {
        let db = CacheDb::open_in_memory().unwrap();
        let now = 100 * HOUR_US;

        // A: 3 distinct fresh likers → local_pm 130; a peer lifts the LOCAL row to
        // 230, but the export must still report 130 (no laundering).
        let a = [0xA0; 32];
        seed_public(&db, &a).await;
        for actor in [1u8, 2, 3] {
            like_at(&db, &a, actor, now).await;
        }
        db.upsert_peer_trend(&[0x09; 32], &a, 500, 8, 1, now / 1000)
            .await
            .unwrap();
        db.recompute_trend_score(&a, now).await.unwrap();
        assert_eq!(trend_pm(&db, &a).await, Some(230)); // composed locally

        // B: only 2 distinct likers → local row exists but BELOW the k-gate.
        let b = [0xB0; 32];
        seed_public(&db, &b).await;
        for actor in [1u8, 2] {
            like_at(&db, &b, actor, now).await;
        }
        db.recompute_trend_score(&b, now).await.unwrap();
        assert!(trend_pm(&db, &b).await.is_some());

        // D: 6 distinct fresh likers → local_pm 231, sorts ahead of A.
        let d = [0xD0; 32];
        seed_public(&db, &d).await;
        for actor in [1u8, 2, 3, 4, 5, 6] {
            like_at(&db, &d, actor, now).await;
        }
        db.recompute_trend_score(&d, now).await.unwrap();

        let entries = db.export_trend_entries(now).await.unwrap();
        assert_eq!(
            entries.len(),
            2,
            "only A and D pass the k-gate (B is below k)"
        );
        assert_eq!(entries[0], (d, 231, 6)); // highest local_pm first
        assert_eq!(entries[1], (a, 130, 3)); // un-laundered local_pm, NOT 230
    }

    /// Core: a public post with recent likes gets a `trending` row carrying the
    /// ratified curve value, the community tier, and the trend scorer version;
    /// decaying past the horizon withdraws it (DELETE, not a zero row).
    #[tokio::test]
    async fn core_scores_then_withdraws_via_wrapper() {
        let db = CacheDb::open_in_memory().unwrap();
        let pid = [0xAA; 32];
        seed_public(&db, &pid).await;
        let now = 100 * HOUR_US;
        for actor in [1u8, 2, 3] {
            like_at(&db, &pid, actor, now).await;
        }
        db.recompute_trend_score(&pid, now).await.unwrap();

        // v = 3 × 1.0 (three fresh likes); local_pm = round(1000·3/(3+20)) = 130.
        let entry = db
            .get_content_scores(&pid)
            .await
            .unwrap()
            .into_iter()
            .find(|e| e.factor == factor::TRENDING)
            .expect("trending row present");
        assert_eq!(entry.score, 130);
        assert_eq!(entry.tier, TIER_COMMUNITY);
        assert_eq!(entry.scorer_version, scorer_version::TREND);

        // Same events, but the clock advanced ~10 half-lives → velocity ≈ 0 →
        // the row is withdrawn (a DELETE — trend_pm now finds nothing).
        db.recompute_trend_score(&pid, now + 60 * HOUR_US)
            .await
            .unwrap();
        assert_eq!(trend_pm(&db, &pid).await, None);
    }

    /// A gated (restricted) post never gets a trending row — its engagement is
    /// not the network's business (`trending.md` § Public posts only).
    #[tokio::test]
    async fn gated_post_never_scores() {
        let db = CacheDb::open_in_memory().unwrap();
        let pid = [0xBB; 32];
        {
            let conn = db.conn.lock().await;
            crate::db::meta::upsert_meta(
                &conn,
                &pid,
                0.0,
                false,
                false,
                Some("premium"),
                None,
                None,
            )
            .unwrap();
        }
        let now = 100 * HOUR_US;
        for actor in [1u8, 2, 3] {
            like_at(&db, &pid, actor, now).await;
        }
        db.recompute_trend_score(&pid, now).await.unwrap();
        assert_eq!(trend_pm(&db, &pid).await, None);
    }

    /// An unseen post (engagement events but no content_meta row) never gets a
    /// blind trending row.
    #[tokio::test]
    async fn unseen_post_never_scores() {
        let db = CacheDb::open_in_memory().unwrap();
        let pid = [0xCC; 32];
        let now = 100 * HOUR_US;
        like_at(&db, &pid, 1, now).await;
        db.recompute_trend_score(&pid, now).await.unwrap();
        assert_eq!(trend_pm(&db, &pid).await, None);
    }

    /// The sweep re-decays live rows on wall-clock and withdraws the fully-decayed
    /// ones — the drift a transition recompute can never catch.
    #[tokio::test]
    async fn sweep_re_decays_and_withdraws() {
        let db = CacheDb::open_in_memory().unwrap();
        let pid = [0xDD; 32];
        seed_public(&db, &pid).await;
        let t0 = 100 * HOUR_US;
        for actor in [1u8, 2, 3] {
            like_at(&db, &pid, actor, t0).await;
        }
        db.recompute_trend_score(&pid, t0).await.unwrap();
        assert_eq!(trend_pm(&db, &pid).await, Some(130));

        // Sweep at t0 (no real time passed) leaves the row unchanged, and reports
        // it as one live row swept.
        assert_eq!(db.sweep_trend_scores(t0).await.unwrap(), 1);
        assert_eq!(trend_pm(&db, &pid).await, Some(130));

        // Sweep far past the decay horizon → velocity ≈ 0 → the row is withdrawn,
        // and the next sweep finds nothing to do.
        db.sweep_trend_scores(t0 + 60 * HOUR_US).await.unwrap();
        assert_eq!(trend_pm(&db, &pid).await, None);
        assert_eq!(db.sweep_trend_scores(t0 + 60 * HOUR_US).await.unwrap(), 0);
    }

    /// INTEGRATION (red until the transition hook is wired): a `like` written
    /// through `insert_engagement_event` recomputes the trend row with NO explicit
    /// recompute call — the choke point does it in the same lock.
    #[tokio::test]
    async fn like_event_triggers_recompute() {
        let db = CacheDb::open_in_memory().unwrap();
        let pid = [0x11; 32];
        seed_public(&db, &pid).await;
        let now = 100 * HOUR_US;
        // One like → v = 1.0 → local_pm = round(1000/21) = 48.
        like_at(&db, &pid, 1, now).await;
        assert_eq!(trend_pm(&db, &pid).await, Some(48));
    }

    /// INTEGRATION (red until the hook is wired): deleting the like event
    /// (`unlike`) recomputes and withdraws the row.
    #[tokio::test]
    async fn unlike_event_withdraws_row() {
        let db = CacheDb::open_in_memory().unwrap();
        let pid = [0x22; 32];
        seed_public(&db, &pid).await;
        let now = 100 * HOUR_US;
        let ev = like_at(&db, &pid, 1, now).await;
        assert_eq!(trend_pm(&db, &pid).await, Some(48));

        db.delete_engagement_event(&ev).await.unwrap();
        assert_eq!(trend_pm(&db, &pid).await, None);
    }
}
