//! Deployment-wide apex web-content actor designation — the persisted,
//! settable/clearable `web_apex_actor` singleton. The web analogue of the
//! per-domain catch-all *mail* actor (`mail_domains.catch_all_actor_id`,
//! [`super::mail_domains`]), but a **nest-wide singleton**: the apex is the
//! single node domain, not a per-`mail_domains` row. Presence of the row ⇒ an
//! actor is designated (`Some`); its absence ⇒ no apex actor (`None`, the apex
//! serves the built-in nest info page).
//!
//! The admin's `fauna.web.set_apex_actor(Some/None)` upserts/clears this row;
//! `start_server` reads it at boot to seed the `HostResolver`'s
//! node-domain → apex-actor mapping, and updates the live resolver on set/clear.
//! Spec: `docs/goal/behavior/web-content-hosting.md` § Admin apex hosting.

use anyhow::{Context, Result};

impl crate::db::CacheDb {
    /// Read the designated apex web-content actor, or `None` if no actor is
    /// designated (the apex serves the built-in nest info page).
    pub async fn get_apex_actor(&self) -> Result<Option<[u8; 32]>> {
        let raw: Option<Vec<u8>> = self
            .get_singleton_column("web_apex_actor", "actor_id")
            .await?;
        match raw {
            None => Ok(None),
            Some(bytes) => {
                let arr: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("web_apex_actor.actor_id must be 32 bytes"))?;
                Ok(Some(arr))
            }
        }
    }

    /// Designate the apex web-content actor (upsert the singleton row).
    pub async fn set_apex_actor(&self, actor_id: &[u8; 32]) -> Result<()> {
        self.set_singleton_column("web_apex_actor", "actor_id", actor_id.as_slice())
            .await
    }

    /// Clear the apex designation (delete the singleton row) — the apex reverts
    /// to the built-in nest info page.
    pub async fn clear_apex_actor(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute("DELETE FROM web_apex_actor WHERE id = 1", [])
            .context("clear web_apex_actor")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::db::CacheDb;

    #[tokio::test]
    async fn unset_reads_none() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(db.get_apex_actor().await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_then_get_round_trips_and_clears() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0xABu8; 32];

        db.set_apex_actor(&actor).await.unwrap();
        assert_eq!(db.get_apex_actor().await.unwrap(), Some(actor));

        // Re-designating a different actor overwrites (singleton, latest wins).
        let actor2 = [0x07u8; 32];
        db.set_apex_actor(&actor2).await.unwrap();
        assert_eq!(db.get_apex_actor().await.unwrap(), Some(actor2));

        // Clearing reverts to None (apex → info page).
        db.clear_apex_actor().await.unwrap();
        assert_eq!(db.get_apex_actor().await.unwrap(), None);
    }

    /// Clearing an already-empty designation is a harmless no-op (idempotent).
    #[tokio::test]
    async fn clear_when_unset_is_noop() {
        let db = CacheDb::open_in_memory().unwrap();
        db.clear_apex_actor().await.unwrap();
        assert_eq!(db.get_apex_actor().await.unwrap(), None);
    }
}
