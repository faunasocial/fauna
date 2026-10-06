//! Bridge message and feed subscription methods.

use super::{
    BridgeFeedSubscription, BridgeMessageRow, CacheDb, content, links, now_epoch_millis, search,
};
use crate::domain_hash::{HashField, write_fields};
use anyhow::{Context, Result};

/// Domain tag for [`CacheDb::bridge_content_id`].
const BRIDGE_CONTENT_ID_DST: &[u8] = b"fauna.bridge_content.id.v1";

impl CacheDb {
    /// Compute a deterministic content ID for a bridge message. Framed via
    /// [`write_fields`] (`bridge_type` is variable-length and non-trailing —
    /// unframed, a `bridge_type` suffix could alias into `actor_id`'s fixed
    /// 32 bytes) — see the same class of bug fixed in
    /// [`crate::db::admin::audit_on_conn`] and
    /// [`crate::db::CacheDb::compute_pending_action_hash`]. No caller wires
    /// this into a live RPC path yet, so there is no on-disk v1 preimage to
    /// stay compatible with.
    fn bridge_content_id(bridge_type: &str, actor_id: &[u8; 32], external_id: &str) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        write_fields(
            BRIDGE_CONTENT_ID_DST,
            &[
                HashField::LenPrefixed(bridge_type.as_bytes()),
                HashField::Fixed32(actor_id),
                HashField::Trailing(external_id.as_bytes()),
            ],
            |b| {
                hasher.update(b);
            },
        );
        *hasher.finalize().as_bytes()
    }

    /// Insert or update a bridge message. Also indexes in FTS.
    pub async fn insert_bridge_message(
        &self,
        bridge_type: &str,
        actor_id: &[u8; 32],
        external_id: &str,
        sender: &str,
        recipient: &str,
        subject: &str,
        size_bytes: i64,
        flags: i64,
        received_at: i64,
        body_text: Option<&str>,
        blob_hash: Option<&[u8; 32]>,
    ) -> Result<i64> {
        let bridge_type = bridge_type.to_string();
        let actor_id = *actor_id;
        let external_id = external_id.to_string();
        let sender = sender.to_string();
        let recipient = recipient.to_string();
        let subject = subject.to_string();
        let body_text = body_text.map(|s| s.to_string());
        let blob_hash = blob_hash.copied();
        let conn = self.conn.lock().await;

        // Compute deterministic content ID
        let content_id = Self::bridge_content_id(&bridge_type, &actor_id, &external_id);

        // Build metadata JSON with denormalized fields
        let meta = serde_json::to_vec(&serde_json::json!({
            "external_id": external_id,
            "sender": sender,
            "recipient": recipient,
            "subject": subject,
            "size_bytes": size_bytes,
            "flags": flags,
        }))
        .unwrap();

        // INSERT OR REPLACE content and index in FTS
        let body = body_text.as_deref().unwrap_or("");
        let db_payload = if blob_hash.is_some() {
            &[][..]
        } else {
            &meta[..]
        };
        content::insert_and_index(
            &conn,
            &content_id,
            &bridge_type,
            &actor_id,
            received_at,
            db_payload,
            None,
            &bridge_type,
            blob_hash.as_ref(),
            &subject,
            body,
            &sender,
            "",
        )?;

        // Upsert bridge_delivery link
        let link_id = links::upsert_link(
            &conn,
            "bridge_delivery",
            Some(content_id.as_slice()),
            None,
            Some(actor_id.as_slice()),
            None,
            Some(&meta),
            received_at,
        )?;

        Ok(link_id)
    }

    pub async fn delete_bridge_message(
        &self,
        bridge_type: &str,
        actor_id: &[u8; 32],
        external_id: &str,
    ) -> Result<()> {
        let bridge_type = bridge_type.to_string();
        let actor_id = *actor_id;
        let external_id = external_id.to_string();
        let conn = self.conn.lock().await;

        let content_id = Self::bridge_content_id(&bridge_type, &actor_id, &external_id);

        // Delete bridge_delivery links
        links::delete_links_by_source(&conn, content_id.as_slice(), "bridge_delivery")?;

        // Remove from FTS and content
        search::remove_content(&conn, &content_id)?;
        content::delete_content(&conn, &content_id)?;

        Ok(())
    }

    pub async fn list_bridge_messages(
        &self,
        bridge_type: &str,
        actor_id: &[u8; 32],
        before: Option<i64>,
        limit: i64,
    ) -> Result<Vec<BridgeMessageRow>> {
        let bridge_type = bridge_type.to_string();
        let actor_id = *actor_id;
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT cl.id, c.schema, c.author, \
             COALESCE(json_extract(CAST(cl.metadata AS TEXT), '$.external_id'), '') AS external_id, \
             COALESCE(json_extract(CAST(cl.metadata AS TEXT), '$.sender'), '') AS sender, \
             COALESCE(json_extract(CAST(cl.metadata AS TEXT), '$.recipient'), '') AS recipient, \
             COALESCE(json_extract(CAST(cl.metadata AS TEXT), '$.subject'), '') AS subject, \
             COALESCE(json_extract(CAST(cl.metadata AS TEXT), '$.size_bytes'), 0) AS size_bytes, \
             COALESCE(json_extract(CAST(cl.metadata AS TEXT), '$.flags'), 0) AS flags, \
             c.created_at AS received_at \
             FROM content_links cl \
             JOIN content c ON c.id = cl.source_id \
             WHERE cl.link_type = 'bridge_delivery' AND c.source = ?1 AND cl.actor_id = ?2 \
             AND (?3 IS NULL OR c.created_at < ?3) \
             ORDER BY c.created_at DESC LIMIT ?4"
        ).context("prepare list_bridge_messages")?;
        let rows = stmt
            .query_map(
                rusqlite::params![bridge_type, actor_id.as_slice(), before, limit],
                |row| {
                    Ok(BridgeMessageRow {
                        id: row.get(0)?,
                        bridge_type: row.get(1)?,
                        actor_id: row.get(2)?,
                        external_id: row.get(3)?,
                        sender: row.get(4)?,
                        recipient: row.get(5)?,
                        subject: row.get(6)?,
                        size_bytes: row.get(7)?,
                        flags: row.get(8)?,
                        received_at: row.get(9)?,
                    })
                },
            )
            .context("query list_bridge_messages")?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read bridge_message row")?);
        }
        Ok(results)
    }

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
