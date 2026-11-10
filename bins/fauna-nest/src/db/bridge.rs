//! Bridge feed subscription methods.

use super::{BridgeFeedSubscription, CacheDb, now_epoch_millis};
use anyhow::{Context, Result};

// A bridge's own messages never land here: mail lives in the `__mail`
// segment store, and a bridge's PRIVATE content (bridge DMs, gift-wraps, mail
// bodies) is never eligible for `content_fts` (`content-index.md` § Bridge
// content in the Search corpus). The former `insert_bridge_message` writer,
// which indexed a message's subject and body with no private/public split and
// had only test callers, was deleted so no producer can wire it up.

impl CacheDb {
    // ==================== Bridge Feed Subscriptions ====================

    /// Subscribe an actor to a named bridge feed.
    pub async fn subscribe_bridge_feed(
        &self,
        actor_id: &[u8; 32],
        bridge: &str,
        feed_uri: &str,
        name: &str,
    ) -> Result<i64> {
        let actor_id = *actor_id;
        let bridge = bridge.to_string();
        let feed_uri = feed_uri.to_string();
        let name = name.to_string();
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO bridge_feed_subscriptions (actor_id, bridge, feed_uri, name, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![actor_id.as_slice(), bridge, feed_uri, name, now],
        ).context("subscribe bridge feed")?;
        // Retrieve the actual row id (handles both fresh insert and duplicate-ignore cases)
        let id: i64 = conn.query_row(
            "SELECT id FROM bridge_feed_subscriptions WHERE actor_id = ?1 AND bridge = ?2 AND feed_uri = ?3",
            rusqlite::params![actor_id.as_slice(), bridge, feed_uri],
            |row| row.get(0),
        ).context("get bridge feed subscription id")?;
        Ok(id)
    }

    /// List all bridge feed subscriptions for an actor.
    pub async fn list_bridge_feeds(
        &self,
        actor_id: &[u8; 32],
    ) -> Result<Vec<BridgeFeedSubscription>> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, actor_id, bridge, feed_uri, name, created_at
             FROM bridge_feed_subscriptions WHERE actor_id = ?1 ORDER BY created_at DESC",
            )
            .context("prepare list_bridge_feeds")?;
        let rows = stmt
            .query_map(rusqlite::params![actor_id.as_slice()], |row| {
                Ok(BridgeFeedSubscription {
                    id: row.get(0)?,
                    actor_id: row.get(1)?,
                    bridge: row.get(2)?,
                    feed_uri: row.get(3)?,
                    name: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })
            .context("query bridge feeds")?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Remove a bridge feed subscription. Returns true if a row was deleted.
    pub async fn unsubscribe_bridge_feed(&self, id: i64, actor_id: &[u8; 32]) -> Result<bool> {
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let affected = conn
            .execute(
                "DELETE FROM bridge_feed_subscriptions WHERE id = ?1 AND actor_id = ?2",
                rusqlite::params![id, actor_id.as_slice()],
            )
            .context("unsubscribe bridge feed")?;
        Ok(affected > 0)
    }
}
