//! Sender behavioral event log — Layer 2 of the anti-spam pipeline.
//!
//! Records per-actor DM events so that behavioral anomaly scores can be
//! computed at delivery time without hitting external services. Events are
//! pruned after 7 days.
//!
//! Two event types, and **both have production writers** — a rule this module
//! learned the hard way. `channel_created` was a third, read by a profile field
//! (`channel_create_rate_1h`) that the scorer never consulted and written by
//! nothing but a test; finding retired both ends of it. Before adding an
//! event type here, wire its writer and its reader in the same change.

use anyhow::{Context, Result};
use fauna_core::behavioral::BehavioralProfile;

use super::CacheDb;

/// Time window constants in microseconds.
const ONE_HOUR_US: i64 = 3_600_000_000;
const ONE_DAY_US: i64 = 86_400_000_000;
const SEVEN_DAYS_US: i64 = 604_800_000_000;

impl CacheDb {
    /// Record a single sender behavioral event.
    ///
    /// - `actor_id`    — 32-byte actor key of the sender.
    /// - `event_type`  — `"dm_sent"`; `"dm_replied"` rows go through
    ///   [`CacheDb::record_dm_reply`], which enforces their preconditions.
    /// - `target_actor`— for DM events, the recipient's 32-byte actor key.
    /// - `timestamp`   — microseconds since UNIX epoch.
    pub async fn record_sender_event(
        &self,
        actor_id: &[u8; 32],
        event_type: &str,
        target_actor: Option<&[u8; 32]>,
        timestamp: i64,
    ) -> Result<()> {
        let actor_id = actor_id.to_vec();
        let event_type = event_type.to_string();
        let target_actor = target_actor.map(|t| t.to_vec());
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO sender_behavior (actor_id, event_type, target_actor, timestamp)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![actor_id, event_type, target_actor, timestamp],
        )
        .context("record sender event")?;
        Ok(())
    }

    /// Record that `replier` answered a DM from `original_sender` — the feed
    /// for `BehavioralProfile::dm_response_rate`.
    ///
    /// The event is attributed to **`original_sender`**, not the replier: the
    /// rate measures *"did the people you messaged write back?"*, so the row
    /// belongs to the actor whose outreach was answered.
    ///
    /// Two conditions, both enforced in SQL so the whole thing is one
    /// statement under the connection lock:
    ///
    /// - `original_sender` must actually have DMed `replier` inside the 7-day
    ///   window. Without this, the *first* message of any conversation would
    ///   count as a reply to a message that was never sent.
    /// - at most one reply row per correspondent pair per window, which is what
    ///   makes the rate a pair-ratio rather than a message-count ratio (see
    ///   `get_behavioral_profile`). Re-replying does not inflate the rate.
    ///
    /// A pair that falls out of the window and resumes records again, which is
    /// correct: the windows are the whole point.
    pub async fn record_dm_reply(
        &self,
        original_sender: &[u8; 32],
        replier: &[u8; 32],
        timestamp: i64,
    ) -> Result<()> {
        let original_sender = original_sender.to_vec();
        let replier = replier.to_vec();
        let cutoff = timestamp - SEVEN_DAYS_US;
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO sender_behavior (actor_id, event_type, target_actor, timestamp)
             SELECT ?1, 'dm_replied', ?2, ?3
             WHERE EXISTS (
                     SELECT 1 FROM sender_behavior
                     WHERE actor_id = ?1 AND event_type = 'dm_sent'
                       AND target_actor = ?2 AND timestamp >= ?4
                   )
               AND NOT EXISTS (
                     SELECT 1 FROM sender_behavior
                     WHERE actor_id = ?1 AND event_type = 'dm_replied'
                       AND target_actor = ?2 AND timestamp >= ?4
                   )",
            rusqlite::params![original_sender, replier, timestamp, cutoff],
        )
        .context("record dm reply")?;
        Ok(())
    }

    /// Count raw `dm_replied` rows for one correspondent pair in the 7-day
    /// window.
    ///
    /// Exists because [`CacheDb::get_behavioral_profile`] deliberately reads
    /// `COUNT(DISTINCT target_actor)`, which collapses duplicates before any
    /// caller can observe them — so the profile cannot witness whether
    /// [`CacheDb::record_dm_reply`]'s one-row-per-pair guard actually held.
    /// Without this read, deleting that guard leaves every rate-level test
    /// green (mutation-verified) and the
    /// invariant rests on the reader alone.
    pub async fn count_dm_replied_rows(
        &self,
        actor_id: &[u8; 32],
        target_actor: &[u8; 32],
        now_us: i64,
    ) -> Result<i64> {
        let actor_id = actor_id.to_vec();
        let target_actor = target_actor.to_vec();
        let cutoff = now_us - SEVEN_DAYS_US;
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT COUNT(*)
             FROM sender_behavior
             WHERE actor_id = ?1
               AND event_type = 'dm_replied'
               AND target_actor = ?2
               AND timestamp >= ?3",
            rusqlite::params![actor_id, target_actor, cutoff],
            |row| row.get::<_, i64>(0),
        )
        .context("count dm_replied rows")
    }

    /// Build a [`BehavioralProfile`] for `sender` with respect to `recipient`.
    ///
    /// Queries the `sender_behavior` table for DM fanout windows (1 h / 24 h / 7 d)
    /// and the DM response rate over 7 days. Social-graph fields are computed
    /// from the `contacts`, `users`, `content`, and `content_meta` tables.
    pub async fn get_behavioral_profile(
        &self,
        sender: &[u8; 32],
        recipient: &[u8; 32],
        now_us: i64,
    ) -> Result<BehavioralProfile> {
        let sender_id = sender.to_vec();
        let recipient_id = recipient.to_vec();
        let conn = self.conn.lock().await;

        // --- Unique DM recipients in 1 h ---
        let cutoff_1h = now_us - ONE_HOUR_US;
        let unique_dm_recipients_1h: u32 =
            conn.query_row(
                "SELECT COUNT(DISTINCT target_actor)
                 FROM sender_behavior
                 WHERE actor_id = ?1
                   AND event_type = 'dm_sent'
                   AND target_actor IS NOT NULL
                   AND timestamp >= ?2",
                rusqlite::params![sender_id, cutoff_1h],
                |row| row.get::<_, i64>(0),
            )
            .context("query unique dm recipients 1h")? as u32;

        // --- Unique DM recipients in 24 h ---
        let cutoff_24h = now_us - ONE_DAY_US;
        let unique_dm_recipients_24h: u32 =
            conn.query_row(
                "SELECT COUNT(DISTINCT target_actor)
                 FROM sender_behavior
                 WHERE actor_id = ?1
                   AND event_type = 'dm_sent'
                   AND target_actor IS NOT NULL
                   AND timestamp >= ?2",
                rusqlite::params![sender_id, cutoff_24h],
                |row| row.get::<_, i64>(0),
            )
            .context("query unique dm recipients 24h")? as u32;

        // --- Unique DM recipients in 7 d ---
        let cutoff_7d = now_us - SEVEN_DAYS_US;
        let unique_dm_recipients_7d: u32 =
            conn.query_row(
                "SELECT COUNT(DISTINCT target_actor)
                 FROM sender_behavior
                 WHERE actor_id = ?1
                   AND event_type = 'dm_sent'
                   AND target_actor IS NOT NULL
                   AND timestamp >= ?2",
                rusqlite::params![sender_id, cutoff_7d],
                |row| row.get::<_, i64>(0),
            )
            .context("query unique dm recipients 7d")? as u32;

        // --- Total DM sends over 7 d (for `dm_to_post_ratio`) ---
        let dm_sent_7d: i64 = conn
            .query_row(
                "SELECT COUNT(*)
                 FROM sender_behavior
                 WHERE actor_id = ?1
                   AND event_type = 'dm_sent'
                   AND timestamp >= ?2",
                rusqlite::params![sender_id, cutoff_7d],
                |row| row.get::<_, i64>(0),
            )
            .context("query dm_sent 7d")?;

        // --- DM response rate over 7 d ---
        //
        // Distinct correspondents who replied ÷ distinct correspondents
        // messaged — deliberately NOT `dm_replied / dm_sent` message counts.
        // `record_dm_reply` writes one row per correspondent pair per window,
        // so a pair-basis numerator over a message-count denominator would
        // understate the rate for anyone who sends several messages per
        // conversation — biasing the spam rule toward firing on the chattiest
        // honest users. Pair ÷ pair is bounded by construction and answers the
        // question the rule actually asks: *did the people you messaged write
        // back?*
        let replied_recipients_7d: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT target_actor)
                 FROM sender_behavior
                 WHERE actor_id = ?1
                   AND event_type = 'dm_replied'
                   AND target_actor IS NOT NULL
                   AND timestamp >= ?2",
                rusqlite::params![sender_id, cutoff_7d],
                |row| row.get::<_, i64>(0),
            )
            .context("query dm_replied recipients 7d")?;

        // A sender with no resolved correspondents scores 0.0 here, which reads
        // as "nobody replied". That is unreachable by the only rule consuming
        // this field (it guards on `unique_dm_recipients_7d > 10` first), and
        // any other reading would have to invent a response rate from no data.
        let dm_response_rate = if unique_dm_recipients_7d > 0 {
            replied_recipients_7d as f64 / f64::from(unique_dm_recipients_7d)
        } else {
            0.0
        };

        // --- Social graph: contact relationships ---
        let is_followed_by_recipient: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM contacts
                 WHERE actor_id = ?1 AND peer_id = ?2 AND status = 'accepted'",
                rusqlite::params![recipient_id, sender_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0;

        let is_following_recipient: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM contacts
                 WHERE actor_id = ?1 AND peer_id = ?2 AND status = 'accepted'",
                rusqlite::params![sender_id, recipient_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0;

        let mutual_contacts: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM contacts c1
                 INNER JOIN contacts c2 ON c1.peer_id = c2.peer_id
                 WHERE c1.actor_id = ?1 AND c1.status = 'accepted'
                   AND c2.actor_id = ?2 AND c2.status = 'accepted'",
                rusqlite::params![sender_id, recipient_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0) as u32;

        let social_distance: Option<u8> = if is_followed_by_recipient || is_following_recipient {
            Some(1)
        } else if mutual_contacts > 0 {
            Some(2)
        } else {
            None
        };

        // --- Account age and content stats ---
        let now_secs = now_us / 1_000_000;
        let account_age_days: u64 = conn
            .query_row(
                "SELECT created_at FROM users WHERE actor_id = ?1",
                rusqlite::params![sender_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|created_at| ((now_secs - created_at).max(0) / 86400) as u64)
            .unwrap_or(0);

        let total_public_posts: u64 = conn
            .query_row(
                "SELECT COUNT(*) FROM content
                 WHERE author = ?1 AND schema LIKE 'post/%'",
                rusqlite::params![sender_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0) as u64;

        let total_received_replies: u64 = conn
            .query_row(
                "SELECT COALESCE(SUM(cm.reply_count), 0) FROM content_meta cm
                 JOIN content c ON c.id = cm.content_id
                 WHERE c.author = ?1 AND c.schema LIKE 'post/%'",
                rusqlite::params![sender_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap_or(0) as u64;

        let dm_to_post_ratio = if dm_sent_7d > 0 {
            dm_sent_7d as f64 / total_public_posts.max(1) as f64
        } else {
            0.0
        };

        Ok(BehavioralProfile {
            unique_dm_recipients_1h,
            unique_dm_recipients_24h,
            unique_dm_recipients_7d,
            social_distance,
            mutual_contacts,
            is_followed_by_recipient,
            is_following_recipient,
            account_age_days,
            total_public_posts,
            total_received_replies,
            dm_to_post_ratio,
            dm_response_rate,
        })
    }

    /// Delete sender behavior events older than 7 days.
    ///
    /// Returns the number of rows deleted.
    pub async fn prune_sender_behavior(&self, now_us: i64) -> Result<u64> {
        let cutoff = now_us - SEVEN_DAYS_US;
        let conn = self.conn.lock().await;
        let deleted = conn
            .execute(
                "DELETE FROM sender_behavior WHERE timestamp < ?1",
                rusqlite::params![cutoff],
            )
            .context("prune sender_behavior")?;
        Ok(deleted as u64)
    }
}
