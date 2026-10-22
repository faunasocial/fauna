//! The federated follow's two stores (`docs/goal/behavior/federated-follow.md`
//! § Decision 2): on a **follower's** nest, `remote_follows` — this nest's
//! record of a local reader's follow of an author homed elsewhere; on an
//! **author's** nest, the home columns of `subscribe_requests`/`subscribers`
//! and `follow_intents_seen` — the monotonic intent rule.

use super::{CacheDb, now_epoch_secs};
use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

/// A follow's state as the follower's nest last heard it from the home.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteFollowState {
    Pending,
    Approved,
    Rejected,
    Removed,
}

impl RemoteFollowState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Removed => "removed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            "removed" => Some(Self::Removed),
            _ => None,
        }
    }
}

/// One `remote_follows` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteFollowRow {
    pub subscriber_id: [u8; 32],
    pub author_id: [u8; 32],
    pub tier: String,
    pub home_nest_id: [u8; 32],
    pub home_nest_url: String,
    pub state: RemoteFollowState,
    pub request_id: Option<i64>,
    /// Epoch seconds.
    pub updated_at: i64,
}

fn id32(v: Vec<u8>) -> rusqlite::Result<[u8; 32]> {
    v.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Blob,
            "actor or nest id is not 32 bytes".into(),
        )
    })
}

fn row_to_remote_follow(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteFollowRow> {
    let state: String = row.get(5)?;
    Ok(RemoteFollowRow {
        subscriber_id: id32(row.get(0)?)?,
        author_id: id32(row.get(1)?)?,
        tier: row.get(2)?,
        home_nest_id: id32(row.get(3)?)?,
        home_nest_url: row.get(4)?,
        state: RemoteFollowState::parse(&state).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                format!("unknown remote follow state {state:?}").into(),
            )
        })?,
        request_id: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

const REMOTE_FOLLOW_COLUMNS: &str = "subscriber_id, author_id, tier, home_nest_id, home_nest_url, \
     state, request_id, updated_at";

impl CacheDb {
    // ── Follower side: remote_follows ─────────────────────────────────

    /// Record what the home answered a relayed subscribe or unsubscribe: the
    /// row is created on first contact and its home, state, request id and
    /// intent stamp follow the latest answer.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_remote_follow(
        &self,
        subscriber_id: &[u8; 32],
        author_id: &[u8; 32],
        tier: &str,
        home_nest_id: &[u8; 32],
        home_nest_url: &str,
        state: RemoteFollowState,
        request_id: Option<i64>,
        intent_at: i64,
    ) -> Result<()> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO remote_follows
                 (subscriber_id, author_id, tier, home_nest_id, home_nest_url, state,
                  request_id, latest_intent_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9)
             ON CONFLICT(subscriber_id, author_id, tier) DO UPDATE SET
                 home_nest_id = excluded.home_nest_id,
                 home_nest_url = excluded.home_nest_url,
                 state = excluded.state,
                 request_id = excluded.request_id,
                 latest_intent_at = excluded.latest_intent_at,
                 updated_at = excluded.updated_at",
            rusqlite::params![
                subscriber_id.as_slice(),
                author_id.as_slice(),
                tier,
                home_nest_id.as_slice(),
                home_nest_url,
                state.as_str(),
                request_id,
                intent_at,
                now,
            ],
        )
        .context("upsert remote follow")?;
        Ok(())
    }

    /// Every remote follow `subscriber_id` holds of `author_id`, any tier.
    pub async fn remote_follows_of(
        &self,
        subscriber_id: &[u8; 32],
        author_id: &[u8; 32],
    ) -> Result<Vec<RemoteFollowRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&format!(
            "SELECT {REMOTE_FOLLOW_COLUMNS} FROM remote_follows
              WHERE subscriber_id = ?1 AND author_id = ?2 ORDER BY tier"
        ))?;
        let rows = stmt
            .query_map(
                rusqlite::params![subscriber_id.as_slice(), author_id.as_slice()],
                row_to_remote_follow,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("remote_follows_of")?;
        Ok(rows)
    }

    /// Every remote follow `subscriber_id` holds, for `mine.list`.
    pub async fn remote_follows_for_subscriber(
        &self,
        subscriber_id: &[u8; 32],
    ) -> Result<Vec<RemoteFollowRow>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(&format!(
            "SELECT {REMOTE_FOLLOW_COLUMNS} FROM remote_follows
              WHERE subscriber_id = ?1 ORDER BY updated_at DESC"
        ))?;
        let rows = stmt
            .query_map(
                rusqlite::params![subscriber_id.as_slice()],
                row_to_remote_follow,
            )?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("remote_follows_for_subscriber")?;
        Ok(rows)
    }

    /// Apply a home's `follow.state` push: moves the row only when the pushing
    /// nest is the row's home (`origin_nest_id`, verified by the channel).
    /// Returns whether a row moved.
    pub async fn set_remote_follow_state_from(
        &self,
        subscriber_id: &[u8; 32],
        author_id: &[u8; 32],
        tier: &str,
        origin_nest_id: &[u8; 32],
        state: RemoteFollowState,
    ) -> Result<bool> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "UPDATE remote_follows SET state = ?5, updated_at = ?6
                  WHERE subscriber_id = ?1 AND author_id = ?2 AND tier = ?3
                    AND home_nest_id = ?4",
                rusqlite::params![
                    subscriber_id.as_slice(),
                    author_id.as_slice(),
                    tier,
                    origin_nest_id.as_slice(),
                    state.as_str(),
                    now,
                ],
            )
            .context("set remote follow state")?;
        Ok(changed > 0)
    }

    // ── Author side: home columns + the monotonic intent rule ─────────

    /// Admit an intent under the monotonic rule: returns `true` and records
    /// `created_at` when it is newer than every intent seen for this
    /// `(author, subscriber, tier)`, or is the very one already seen (a
    /// replay of the latest is answered as it was); `false` for an older one,
    /// which must change nothing.
    pub async fn admit_follow_intent(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        tier: &str,
        created_at: i64,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let seen: Option<i64> = conn
            .query_row(
                "SELECT latest_created_at FROM follow_intents_seen
                  WHERE author_id = ?1 AND subscriber_id = ?2 AND tier = ?3",
                rusqlite::params![author_id.as_slice(), subscriber_id.as_slice(), tier],
                |row| row.get(0),
            )
            .optional()
            .context("read follow intent stamp")?;
        if seen.is_some_and(|s| created_at < s) {
            return Ok(false);
        }
        conn.execute(
            "INSERT INTO follow_intents_seen (author_id, subscriber_id, tier, latest_created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(author_id, subscriber_id, tier) DO UPDATE SET
                 latest_created_at = MAX(latest_created_at, excluded.latest_created_at)",
            rusqlite::params![
                author_id.as_slice(),
                subscriber_id.as_slice(),
                tier,
                created_at
            ],
        )
        .context("record follow intent stamp")?;
        Ok(true)
    }

    /// [`Self::admit_follow_intent`] for an act that spans every tier of the
    /// author — a leave: admitted only when no intent seen from `subscriber_id`
    /// for `author_id`, under any tier, is newer; then stamped under `tier`.
    pub async fn admit_follow_intent_over_author(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        tier: &str,
        created_at: i64,
    ) -> Result<bool> {
        let newest: Option<i64> = {
            let conn = self.conn.lock().await;
            conn.query_row(
                "SELECT MAX(latest_created_at) FROM follow_intents_seen
                  WHERE author_id = ?1 AND subscriber_id = ?2",
                rusqlite::params![author_id.as_slice(), subscriber_id.as_slice()],
                |row| row.get(0),
            )
            .context("read newest follow intent stamp")?
        };
        if newest.is_some_and(|n| created_at < n) {
            return Ok(false);
        }
        self.admit_follow_intent(author_id, subscriber_id, tier, created_at)
            .await
    }

    /// Whether any intent from `subscriber_id` about `author_id` was ever
    /// admitted — the follow twin's line between a subscriber already known
    /// and a stranger, which spends the author's budget
    /// (`follow_federation::admit_stranger_intent`).
    pub async fn follow_intent_seen(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM follow_intents_seen
                            WHERE author_id = ?1 AND subscriber_id = ?2)",
            rusqlite::params![author_id.as_slice(), subscriber_id.as_slice()],
            |row| row.get::<_, i64>(0).map(|v| v != 0),
        )
        .context("probe follow intent seen")
    }

    /// Whether `subscriber_id` holds a pending subscribe request or an approved
    /// roster row with `author_id` — the real bound on what a known subscriber
    /// adds to the author's queue, which a seen-stamp is not (a leave that ended
    /// nothing stamps too).
    pub async fn holds_follow_row(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM subscribe_requests
                            WHERE author_id = ?1 AND subscriber_id = ?2 AND kind = 'subscribe')
                 OR EXISTS(SELECT 1 FROM subscribers
                            WHERE author_id = ?1 AND subscriber_id = ?2)",
            rusqlite::params![author_id.as_slice(), subscriber_id.as_slice()],
            |row| row.get::<_, i64>(0).map(|v| v != 0),
        )
        .context("probe follow row held")
    }

    /// Pending subscribe requests from remote subscribers (a home nest
    /// recorded) one author holds.
    pub async fn count_remote_pending_subscribe_requests(
        &self,
        author_id: &[u8; 32],
    ) -> Result<usize> {
        let conn = self.conn.lock().await;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM subscribe_requests
                  WHERE author_id = ?1 AND kind = 'subscribe' AND home_nest_id IS NOT NULL",
                rusqlite::params![author_id.as_slice()],
                |row| row.get(0),
            )
            .context("count remote pending subscribe requests")?;
        Ok(n.max(0) as usize)
    }

    /// Stamp a subscribe/unsubscribe request with the subscriber's home nest.
    /// The URL is `None` when the origin has neither a proven address nor a
    /// declared one; the outbox resolves the push target by the id at send
    /// time either way.
    pub async fn set_subscribe_request_home(
        &self,
        request_id: i64,
        home_nest_id: &[u8; 32],
        home_nest_url: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE subscribe_requests SET home_nest_id = ?2, home_nest_url = ?3 WHERE id = ?1",
            rusqlite::params![request_id, home_nest_id.as_slice(), home_nest_url],
        )
        .context("set subscribe request home")?;
        Ok(())
    }

    /// The home nest id a request carries; `None` for a local subscriber's.
    pub async fn subscribe_request_home(&self, request_id: i64) -> Result<Option<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let home: Option<Option<Vec<u8>>> = conn
            .query_row(
                "SELECT home_nest_id FROM subscribe_requests WHERE id = ?1",
                rusqlite::params![request_id],
                |row| row.get(0),
            )
            .optional()
            .context("read subscribe request home")?;
        Ok(home.flatten().and_then(|v| v.try_into().ok()))
    }

    /// Copy a request's home columns onto the subscriber row its approval just
    /// wrote — the approval half of "the request row carries the subscriber's
    /// home". A local request (NULL home) copies nothing.
    pub async fn copy_subscribe_request_home(
        &self,
        request_id: i64,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        tier_name: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE subscribers
                SET home_nest_id = r.home_nest_id, home_nest_url = r.home_nest_url
               FROM (SELECT home_nest_id, home_nest_url FROM subscribe_requests
                      WHERE id = ?1 AND home_nest_id IS NOT NULL) AS r
              WHERE subscribers.author_id = ?2 AND subscribers.subscriber_id = ?3
                AND subscribers.tier_name = ?4",
            rusqlite::params![
                request_id,
                author_id.as_slice(),
                subscriber_id.as_slice(),
                tier_name
            ],
        )
        .context("copy subscribe request home")?;
        Ok(())
    }

    /// The home nest id on a roster row; `None` when there is no such row or
    /// the subscriber is local.
    pub async fn subscriber_home(
        &self,
        author_id: &[u8; 32],
        subscriber_id: &[u8; 32],
        tier_name: &str,
    ) -> Result<Option<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let home: Option<Option<Vec<u8>>> = conn
            .query_row(
                "SELECT home_nest_id FROM subscribers
                  WHERE author_id = ?1 AND subscriber_id = ?2 AND tier_name = ?3",
                rusqlite::params![author_id.as_slice(), subscriber_id.as_slice(), tier_name],
                |row| row.get(0),
            )
            .optional()
            .context("read subscriber home")?;
        Ok(home.flatten().and_then(|v| v.try_into().ok()))
    }

    // ── Delivery (`federated-follow.md` § Decision 1, § Decision 3) ──────

    /// On an author's nest: the distinct home nests of the author's remote
    /// subscribers, whatever the tier — where each of the author's posts is
    /// delivered.
    pub async fn subscriber_homes(&self, author_id: &[u8; 32]) -> Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT home_nest_id FROM subscribers
              WHERE author_id = ?1 AND home_nest_id IS NOT NULL",
        )?;
        let homes = stmt
            .query_map([author_id.as_slice()], |row| row.get::<_, Vec<u8>>(0))?
            .filter_map(|r| r.ok().and_then(|v| v.try_into().ok()))
            .collect();
        Ok(homes)
    }

    /// On an author's nest: how many of the author's roster rows (any tier)
    /// are homed at `home`.
    pub async fn subscribers_homed_at(&self, author_id: &[u8; 32], home: &[u8; 32]) -> Result<i64> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT COUNT(*) FROM subscribers WHERE author_id = ?1 AND home_nest_id = ?2",
            rusqlite::params![author_id.as_slice(), home.as_slice()],
            |row| row.get(0),
        )
        .context("count subscribers homed at a nest")
    }

    /// Queue one follow-delivery entry to `peer` and keep at most `cap` of the
    /// author's pending delivery entries for that peer, the oldest dropped
    /// first — a dead or hostile peer never grows the queue without bound
    /// (§ Decision 1). One transaction, so the bound holds after every write.
    pub async fn outbox_enqueue_follow_delivery(
        &self,
        author: &[u8; 32],
        payload: &[u8],
        entry_type: &str,
        peer_nest_id: &[u8; 32],
        cap: i64,
    ) -> Result<()> {
        let now = fauna_core::data::Timestamp::now().as_i64();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO outbox (payload, entry_type, created_at, author_id, peer_nest_id)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                payload,
                entry_type,
                now,
                author.as_slice(),
                peer_nest_id.as_slice()
            ],
        )?;
        tx.execute(
            "DELETE FROM outbox WHERE id IN (
                 SELECT id FROM outbox
                  WHERE author_id = ?1 AND peer_nest_id = ?2
                    AND entry_type IN ('follow_deliver', 'follow_delete')
                  ORDER BY id DESC LIMIT -1 OFFSET ?3)",
            rusqlite::params![author.as_slice(), peer_nest_id.as_slice(), cap],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Drop the author's pending delivery entries for `peer` — the peer holds
    /// no subscriber of the author any more (§ Decision 1's discard). The
    /// `follow_state` push that says so stays queued.
    pub async fn outbox_discard_follow_deliveries(
        &self,
        author: &[u8; 32],
        peer_nest_id: &[u8; 32],
    ) -> Result<usize> {
        let conn = self.conn.lock().await;
        Ok(conn.execute(
            "DELETE FROM outbox WHERE author_id = ?1 AND peer_nest_id = ?2
               AND entry_type IN ('follow_deliver', 'follow_delete')",
            rusqlite::params![author.as_slice(), peer_nest_id.as_slice()],
        )?)
    }

    /// On an author's nest: the author's newest `limit` posts a non-author may
    /// read — this nest's own (never a delivered copy), moderation-servable and
    /// not deleted — the approval backfill's set, oldest first.
    pub async fn author_backfill_post_ids(
        &self,
        author_id: &[u8; 32],
        limit: i64,
    ) -> Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let sql = format!(
            "SELECT id FROM (
                 SELECT c.id, c.created_at FROM content c
                   JOIN content_meta cm ON cm.content_id = c.id
                  WHERE c.author = ?1 AND c.schema LIKE 'post/%'
                    AND c.follow_delivered = 0 AND c.origin_nest_url IS NULL
                    AND {}
                  ORDER BY c.created_at DESC LIMIT ?2)
              ORDER BY created_at ASC",
            super::public_servability::MODERATION_SERVABLE
        );
        let mut stmt = conn.prepare(&sql)?;
        let ids = stmt
            .query_map(rusqlite::params![author_id.as_slice(), limit], |row| {
                row.get::<_, Vec<u8>>(0)
            })?
            .filter_map(|r| r.ok().and_then(|v| v.try_into().ok()))
            .collect();
        Ok(ids)
    }

    /// On a follower's nest: whether a local reader holds an `approved` follow
    /// of `author` homed at `home` — the follow gate a delivery passes
    /// (§ Decision 3).
    pub async fn has_approved_remote_follow_from(
        &self,
        author_id: &[u8; 32],
        home: &[u8; 32],
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT home_nest_url FROM remote_follows
              WHERE author_id = ?1 AND home_nest_id = ?2 AND state = 'approved' LIMIT 1",
            rusqlite::params![author_id.as_slice(), home.as_slice()],
            |row| row.get(0),
        )
        .optional()
        .context("read the follow gate")
    }

    /// A stored post's delivery custody: `Some(home_url)` iff a federated
    /// follow delivered it here from that home (`follow_delivered = 1`).
    pub async fn delivered_post_home(&self, post_id: &[u8; 32]) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        let row: Option<Option<String>> = conn
            .query_row(
                "SELECT origin_nest_url FROM content WHERE id = ?1 AND follow_delivered = 1",
                [post_id.as_slice()],
                |row| row.get(0),
            )
            .optional()
            .context("read a delivered post's home")?;
        Ok(row.flatten())
    }

    /// On a follower's nest: up to `limit` delivered envelopes no local reader
    /// follows the author of any more — no `pending` or `approved`
    /// `remote_follows` row for the author, nor for an identity the author's
    /// own succession maps it onto (§ Decision 3's purge).
    pub async fn follow_delivered_orphans(&self, limit: i64) -> Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT c.id FROM content c
              WHERE c.follow_delivered = 1
                AND c.author NOT IN (
                    SELECT author_id FROM remote_follows WHERE state IN ('pending', 'approved'))
                AND c.author NOT IN (
                    SELECT s.old_actor_id FROM actor_successions s
                      JOIN remote_follows rf ON rf.author_id = s.new_actor_id
                     WHERE rf.state IN ('pending', 'approved'))
              LIMIT ?1",
        )?;
        let ids = stmt
            .query_map([limit], |row| row.get::<_, Vec<u8>>(0))?
            .filter_map(|r| r.ok().and_then(|v| v.try_into().ok()))
            .collect();
        Ok(ids)
    }
}
