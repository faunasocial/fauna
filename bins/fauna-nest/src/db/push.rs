//! Push subscription storage: upsert, list, delete.

use super::CacheDb;
use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

/// A single push subscription row.
pub struct PushSubscription {
    pub id: i64,
    pub actor_id: Vec<u8>,
    pub device_id: String,
    pub transport: String,
    pub endpoint: String,
    pub key_p256dh: Option<String>,
    pub key_auth: Option<String>,
    pub created_at: i64,
}

impl CacheDb {
    /// The persisted VAPID keypair PEM (PKCS8, EC P-256), if this nest has
    /// generated one yet. `None` before the first call to
    /// `crate::push::ensure_vapid_pem`, which fills it in.
    pub async fn get_vapid_pem(&self) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let result = conn
            .query_row("SELECT pem FROM vapid_keypair WHERE id = 1", [], |row| {
                row.get(0)
            })
            .optional()
            .context("get vapid keypair")?;
        Ok(result)
    }

    /// Persist the singleton `vapid_keypair` row (`id = 1`). Called once, the
    /// first time a nest boots with no persisted key
    /// (`crate::push::ensure_vapid_pem`) — the keypair is pure nest
    /// infrastructure, never rotated by this path.
    pub async fn set_vapid_pem(&self, pem: &[u8]) -> Result<()> {
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO vapid_keypair (id, pem, created_at) VALUES (1, ?1, ?2)",
            rusqlite::params![pem, now],
        )
        .context("set vapid keypair")?;
        Ok(())
    }

    /// Insert or update a push subscription for an (actor_id, device_id) pair.
    /// If a subscription already exists for that pair, the transport, endpoint,
    /// and keys are updated in place.
    pub async fn upsert_push_subscription(
        &self,
        actor_id: &[u8],
        device_id: &str,
        transport: &str,
        endpoint: &str,
        key_p256dh: Option<&str>,
        key_auth: Option<&str>,
    ) -> Result<()> {
        let actor_id = actor_id.to_vec();
        let device_id = device_id.to_string();
        let transport = transport.to_string();
        let endpoint = endpoint.to_string();
        let key_p256dh = key_p256dh.map(|s| s.to_string());
        let key_auth = key_auth.map(|s| s.to_string());
        let conn = self.conn.lock().await;
        let now = super::now_epoch_secs();

        conn.execute(
            "INSERT INTO push_subscriptions
                (actor_id, device_id, transport, endpoint, key_p256dh, key_auth, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(actor_id, device_id) DO UPDATE SET
                transport   = excluded.transport,
                endpoint    = excluded.endpoint,
                key_p256dh  = excluded.key_p256dh,
                key_auth    = excluded.key_auth",
            rusqlite::params![
                actor_id, device_id, transport, endpoint, key_p256dh, key_auth, now,
            ],
        )
        .context("upsert push subscription")?;

        Ok(())
    }

    /// List all push subscriptions for a given actor.
    pub async fn list_push_subscriptions(&self, actor_id: &[u8]) -> Result<Vec<PushSubscription>> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;

        let mut stmt = conn
            .prepare(
                "SELECT id, actor_id, device_id, transport, endpoint,
                        key_p256dh, key_auth, created_at
                 FROM push_subscriptions
                 WHERE actor_id = ?1
                 ORDER BY created_at",
            )
            .context("prepare list_push_subscriptions")?;

        let rows = stmt
            .query_map(rusqlite::params![actor_id], |row| {
                Ok(PushSubscription {
                    id: row.get(0)?,
                    actor_id: row.get(1)?,
                    device_id: row.get(2)?,
                    transport: row.get(3)?,
                    endpoint: row.get(4)?,
                    key_p256dh: row.get(5)?,
                    key_auth: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })
            .context("query push_subscriptions")?;

        let mut results = Vec::new();
        for row in rows {
            results.push(row.context("read push_subscription row")?);
        }
        Ok(results)
    }

    /// Delete a push subscription for a specific (actor_id, device_id) pair.
    pub async fn delete_push_subscription(&self, actor_id: &[u8], device_id: &str) -> Result<()> {
        let actor_id = actor_id.to_vec();
        let device_id = device_id.to_string();
        let conn = self.conn.lock().await;

        conn.execute(
            "DELETE FROM push_subscriptions WHERE actor_id = ?1 AND device_id = ?2",
            rusqlite::params![actor_id, device_id],
        )
        .context("delete push subscription")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscribe_and_list() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [1u8; 32];
        db.upsert_push_subscription(
            &actor,
            "device-1",
            "web-push",
            "https://fcm.googleapis.com/wp/abc",
            Some("p256dh-key"),
            Some("auth-key"),
        )
        .await
        .unwrap();
        let subs = db.list_push_subscriptions(&actor).await.unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].device_id, "device-1");
    }

    #[tokio::test]
    async fn unsubscribe_removes() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [1u8; 32];
        db.upsert_push_subscription(
            &actor,
            "device-1",
            "web-push",
            "https://example.com/push",
            Some("p"),
            Some("a"),
        )
        .await
        .unwrap();
        db.delete_push_subscription(&actor, "device-1")
            .await
            .unwrap();
        let subs = db.list_push_subscriptions(&actor).await.unwrap();
        assert!(subs.is_empty());
    }

    /// The property the client-side succession-aftermath push fix
    /// (`apps/fauna-web/src/lib/push-actor.ts`'s `needsReconcile`,
    /// `docs/goal/behavior/succession-aftermath.md` § Implementation status
    /// today) leans on: the row key is `(actor_id, device_id)`, not
    /// `device_id` alone, so a browser that already holds a subscription for
    /// one actor and later signs in as another (a succession completing on
    /// this device, or an ordinary account switch) gets an INDEPENDENT row
    /// for the new actor when it replays `subscribe` — nothing needs to move,
    /// and nothing needs the old row's actor_id to change.
    #[tokio::test]
    async fn subscribing_a_different_actor_under_the_same_device_id_is_independent() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor_a = [1u8; 32];
        let actor_b = [2u8; 32];
        db.upsert_push_subscription(
            &actor_a,
            "device-1",
            "web-push",
            "https://example.com/a",
            Some("pa"),
            Some("aa"),
        )
        .await
        .unwrap();
        db.upsert_push_subscription(
            &actor_b,
            "device-1",
            "web-push",
            "https://example.com/b",
            Some("pb"),
            Some("ab"),
        )
        .await
        .unwrap();

        let subs_a = db.list_push_subscriptions(&actor_a).await.unwrap();
        let subs_b = db.list_push_subscriptions(&actor_b).await.unwrap();
        assert_eq!(
            subs_a.len(),
            1,
            "actor A's row survives actor B's own subscribe"
        );
        assert_eq!(subs_a[0].endpoint, "https://example.com/a");
        assert_eq!(
            subs_b.len(),
            1,
            "actor B gets its own row under the shared device_id"
        );
        assert_eq!(subs_b[0].endpoint, "https://example.com/b");

        // The old actor's row is never touched by the new actor's traffic — this
        // is the "burn" verdict (succession-aftermath.md): nothing moves it, and
        // nothing deletes it either.
        db.delete_push_subscription(&actor_b, "device-1")
            .await
            .unwrap();
        let subs_a_after = db.list_push_subscriptions(&actor_a).await.unwrap();
        assert_eq!(
            subs_a_after.len(),
            1,
            "deleting actor B's subscription must not touch actor A's row"
        );
    }

    #[tokio::test]
    async fn upsert_updates_existing() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [1u8; 32];
        db.upsert_push_subscription(
            &actor,
            "device-1",
            "web-push",
            "https://old-endpoint.com",
            Some("p1"),
            Some("a1"),
        )
        .await
        .unwrap();
        db.upsert_push_subscription(
            &actor,
            "device-1",
            "web-push",
            "https://new-endpoint.com",
            Some("p2"),
            Some("a2"),
        )
        .await
        .unwrap();
        let subs = db.list_push_subscriptions(&actor).await.unwrap();
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].endpoint, "https://new-endpoint.com");
    }
}
