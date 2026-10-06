//! Engagement event storage and count tracking.
//!
//! Stores discrete engagement events (likes, reposts, replies, views) and
//! maintains per-content aggregate counters on `content_meta`.

use super::CacheDb;
use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

/// A single engagement event row.
pub struct EngagementEventRow {
    pub event_id: Vec<u8>,
    pub content_id: Vec<u8>,
    pub actor_id: Option<Vec<u8>>,
    pub event_type: String,
    pub event_data: Option<Vec<u8>>,
    pub created_at: i64,
}

/// Aggregate engagement counts for a piece of content.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngagementCounts {
    pub like_count: i64,
    pub reply_count: i64,
    pub repost_count: i64,
    pub quote_count: i64,
}

impl CacheDb {
    /// Insert an engagement event. Uses INSERT OR IGNORE so duplicate
    /// event_ids are silently skipped. Returns true if a row was inserted.
    pub async fn insert_engagement_event(
        &self,
        event_id: &[u8],
        content_id: &[u8],
        actor_id: Option<&[u8]>,
        event_type: &str,
        event_data: Option<&[u8]>,
        created_at: i64,
    ) -> Result<bool> {
        let event_id = event_id.to_vec();
        let content_id = content_id.to_vec();
        let actor_id = actor_id.map(|a| a.to_vec());
        let event_type = event_type.to_string();
        let event_data = event_data.map(|d| d.to_vec());
        let conn = self.conn.lock().await;
        let rows = conn
            .execute(
                "INSERT OR IGNORE INTO engagement_events
                    (event_id, content_id, actor_id, event_type, event_data, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    event_id, content_id, actor_id, event_type, event_data, created_at,
                ],
            )
            .context("insert engagement event")?;
        // The `trending` factor rides this same event log — recompute the post's
        // decayed-velocity bus row in the same lock so a new act shows up at once
        // (each materialized score recomputes at the choke point of its own input:
        // the counters drive `engagement`, the event rows drive `trending`). Only
        // on a real insert — a duplicate (INSERT OR IGNORE no-op) changes nothing;
        // the ~15-min sweep owns the between-act wall-clock decay.
        if rows > 0 {
            Self::recompute_trend_score_locked(&conn, &content_id, created_at)?;
        }
        Ok(rows > 0)
    }

    /// Recompute and persist the `engagement` factor scalar (`content_meta.score`)
    /// from the current explicit-act counters, on the connection already held by
    /// the caller. Called at the single counter choke point
    /// ([`increment_engagement_count`] / [`decrement_engagement_count`]) so the
    /// materialized scalar can never drift from the counts — you cannot move a
    /// count without moving the score in the same lock.
    ///
    /// The formula is the frame-decomposed cumulative-engagement saturation
    /// ([`fauna_core::scoring::engagement::engagement_score`]): a decay-free
    /// `[0, 1)` value over the weighted like/reply/repost/quote counts. No time
    /// decay (that is the `trending` factor) and no moderation terms (label /
    /// trust are separate composition bus factors). Rationale + the retirement of
    /// the pre-frame `compute_score` monolith it replaces:
    /// `docs/goal/behavior/trending.md` § The engagement factor.
    fn recompute_engagement_score_locked(
        conn: &rusqlite::Connection,
        content_id: &[u8],
    ) -> Result<()> {
        let (like, reply, repost, quote): (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT like_count, reply_count, repost_count, quote_count
                 FROM content_meta WHERE content_id = ?1",
                rusqlite::params![content_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .context("read counts for engagement recompute")?;
        let score = fauna_core::scoring::engagement::engagement_score(like, reply, repost, quote);
        conn.execute(
            "UPDATE content_meta SET score = ?1 WHERE content_id = ?2",
            rusqlite::params![score, content_id],
        )
        .context("update engagement score")?;
        Ok(())
    }

    /// Map a public engagement-type tag to its `content_meta` counter column.
    /// The single allow-list shared by [`increment_engagement_count`] and
    /// [`decrement_engagement_count`] so the two never drift on which tags are
    /// countable (`quote` is the interaction-bar addition, ratified 2026-06-27).
    fn engagement_counter_column(engagement_type: &str) -> Result<&'static str> {
        Ok(match engagement_type {
            "like" => "like_count",
            "reply" => "reply_count",
            "repost" => "repost_count",
            "quote" => "quote_count",
            other => anyhow::bail!("unknown engagement type: {other}"),
        })
    }

    /// Atomically increment one of the engagement count columns on content_meta.
    /// `engagement_type` must be one of: "like", "reply", "repost", "quote".
    /// The single increment core behind the live **explicit-act** paths
    /// (`fauna.posts.interact` like, `fauna.posts.create` reply/repost/quote) —
    /// priority #4, one increment path (`api-layers.md` § Labels & Engagement).
    pub async fn increment_engagement_count(
        &self,
        content_id: &[u8],
        engagement_type: &str,
    ) -> Result<()> {
        let column = Self::engagement_counter_column(engagement_type)?;
        let content_id = content_id.to_vec();
        let conn = self.conn.lock().await;
        // Use format! for column name (safe — validated above from fixed set)
        let sql = format!(
            "UPDATE content_meta SET {col} = {col} + 1 WHERE content_id = ?1",
            col = column,
        );
        let rows = conn
            .execute(&sql, rusqlite::params![content_id])
            .context("increment engagement count")?;
        // The materialized `engagement` scalar rides the same counters — recompute
        // it in this lock so it can't drift. Skip when the row is absent (a no-op
        // UPDATE): nothing to score, and the recompute SELECT would find no row.
        if rows > 0 {
            Self::recompute_engagement_score_locked(&conn, &content_id)?;
        }
        Ok(())
    }

    /// Atomically decrement one of the engagement count columns on content_meta,
    /// clamped at 0 (a counter never goes negative even if a stray un-action
    /// outruns its action). The mirror of [`increment_engagement_count`], used by
    /// the reversible social-action toggles (`unlike` decrements `like_count`;
    /// `unrepost` / reference removal decrement their counters via
    /// `reverse_reference_engagements`, called from the post-delete path).
    /// Same fixed-set column allow-list.
    pub async fn decrement_engagement_count(
        &self,
        content_id: &[u8],
        engagement_type: &str,
    ) -> Result<()> {
        let column = Self::engagement_counter_column(engagement_type)?;
        let content_id = content_id.to_vec();
        let conn = self.conn.lock().await;
        let sql = format!(
            "UPDATE content_meta SET {col} = MAX({col} - 1, 0) WHERE content_id = ?1",
            col = column,
        );
        let rows = conn
            .execute(&sql, rusqlite::params![content_id])
            .context("decrement engagement count")?;
        // Mirror of the increment path: keep the materialized `engagement` scalar
        // in step with the counters within the same lock.
        if rows > 0 {
            Self::recompute_engagement_score_locked(&conn, &content_id)?;
        }
        Ok(())
    }

    /// Delete an engagement event by its id. Returns true if a row was removed.
    /// Pairs with [`insert_engagement_event`](Self::insert_engagement_event)
    /// (`INSERT OR IGNORE`) to make the like/unlike toggle reversible: `like`
    /// inserts a stable-keyed event (first-insert ⇒ bump), `unlike` deletes that
    /// same event (removed ⇒ decrement). The stable key is
    /// [`fauna_core::engagement::compute_toggle_event_id`].
    pub async fn delete_engagement_event(&self, event_id: &[u8]) -> Result<bool> {
        let event_id = event_id.to_vec();
        let conn = self.conn.lock().await;
        // Capture the target content before the DELETE so the trend recompute
        // below can re-score it (a removed act — an `unlike` — lowers, or
        // withdraws, the post's `trending` row). No such event → nothing to do.
        let Some(content_id) = conn
            .query_row(
                "SELECT content_id FROM engagement_events WHERE event_id = ?1",
                rusqlite::params![event_id],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()
            .context("look up content for trend recompute")?
        else {
            return Ok(false);
        };
        let rows = conn
            .execute(
                "DELETE FROM engagement_events WHERE event_id = ?1",
                rusqlite::params![event_id],
            )
            .context("delete engagement event")?;
        // The event existed (same lock throughout), so the DELETE removed it —
        // recompute the target's `trending` row (mirror of the insert hook).
        if rows > 0 {
            let now_us = fauna_core::data::Timestamp::now().as_i64();
            Self::recompute_trend_score_locked(&conn, &content_id, now_us)?;
        }
        Ok(rows > 0)
    }

    /// Read the aggregate engagement counts for a piece of content.
    pub async fn get_engagement_counts(&self, content_id: &[u8]) -> Result<EngagementCounts> {
        let content_id = content_id.to_vec();
        let conn = self.conn.lock().await;
        let counts = conn
            .query_row(
                "SELECT like_count, reply_count, repost_count, quote_count
             FROM content_meta WHERE content_id = ?1",
                rusqlite::params![content_id],
                |row| {
                    Ok(EngagementCounts {
                        like_count: row.get(0)?,
                        reply_count: row.get(1)?,
                        repost_count: row.get(2)?,
                        quote_count: row.get(3)?,
                    })
                },
            )
            .context("get engagement counts")?;
        Ok(counts)
    }

    /// Read the materialized `engagement` factor scalar (`content_meta.score`)
    /// for a piece of content — the value the composed feed reads as
    /// [`fauna_core::scoring::factor::ENGAGEMENT`]. Kept in step with the counters
    /// by [`recompute_engagement_score_locked`](Self::recompute_engagement_score_locked).
    pub async fn get_content_score(&self, content_id: &[u8; 32]) -> Result<f64> {
        let content_id = *content_id;
        let conn = self.conn.lock().await;
        let score: f64 = conn
            .query_row(
                "SELECT score FROM content_meta WHERE content_id = ?1",
                rusqlite::params![content_id.as_slice()],
                |row| row.get(0),
            )
            .context("get content score")?;
        Ok(score)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The counter choke point recomputes the materialized engagement scalar, so
    /// the score tracks the counts with no separate recompute call — and a
    /// decrement lowers it again (the coverage the retired `score_recompute.rs` /
    /// `trust_weighted_scoring.rs` formula tests carried, on the surviving path).
    #[tokio::test]
    async fn counters_drive_the_engagement_scalar() {
        let db = CacheDb::open_in_memory().unwrap();
        let author = [1u8; 32];
        let pid = [0xAAu8; 32];
        db.insert_post_index_entry(&pid, &author, 1_000_000, false, false, "fauna", &[])
            .await
            .unwrap();

        // Fresh post: no engagement → score 0.
        assert_eq!(db.get_content_score(&pid).await.unwrap(), 0.0);

        // Likes push the score up monotonically, bounded below 1.
        db.increment_engagement_count(&pid, "like").await.unwrap();
        let one_like = db.get_content_score(&pid).await.unwrap();
        assert!(one_like > 0.0 && one_like < 1.0);

        db.increment_engagement_count(&pid, "repost").await.unwrap();
        let plus_repost = db.get_content_score(&pid).await.unwrap();
        assert!(
            plus_repost > one_like,
            "a repost (weight 2.0) raises engagement above one like: {plus_repost} vs {one_like}"
        );

        // The independently-computed formula value matches the materialized score.
        let expected = fauna_core::scoring::engagement::engagement_score(1, 0, 1, 0);
        assert!((plus_repost - expected).abs() < 1e-12);

        // Decrement lowers it back.
        db.decrement_engagement_count(&pid, "repost").await.unwrap();
        assert!((db.get_content_score(&pid).await.unwrap() - one_like).abs() < 1e-12);
    }

    /// A no-op count UPDATE on an absent row must not error (the recompute SELECT
    /// is skipped when the row is missing).
    #[tokio::test]
    async fn missing_row_is_a_quiet_no_op() {
        let db = CacheDb::open_in_memory().unwrap();
        db.increment_engagement_count(&[0x99u8; 32], "like")
            .await
            .unwrap();
        db.decrement_engagement_count(&[0x99u8; 32], "like")
            .await
            .unwrap();
    }
}
