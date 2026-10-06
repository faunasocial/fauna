//! Per-actor opt-in for subdomain web hosting (`<handle>.<node-domain>`).
//!
//! The per-user counterpart to the nest-wide [`super::web_apex`] singleton:
//! where the admin designates **one** apex actor for the whole deployment, here
//! **each user** opts their own `web` content into serving at their handle
//! subdomain. Presence-as-flag, like `web_apex_actor` but keyed per actor: a row
//! ⇒ that actor opted in; no row ⇒ default OFF (privacy / user-controls-their-
//! data, `web-content-hosting.md` § Routing + Architectural rule 8).
//!
//! The user's `fauna.web.set_subdomain_enabled(true/false)` upserts/deletes
//! their row (caller-scoped — no `actor_id` param, the connection actor is the
//! subject). Two consumers read it: `start_server` seeds the live
//! [`HostResolver`](crate::web_content::serve::HostResolver)'s `<handle>` → actor
//! map at boot, and the per-subdomain cert lifecycle
//! ([`web_content::cert`](crate::web_content::cert)) reconciles each opted-in
//! actor's `<handle>.<domain>` HTTP-01 cert against [`list_subdomain_enabled`].

use anyhow::{Context, Result};

impl crate::db::CacheDb {
    /// Opt an actor into subdomain hosting (upsert the presence row). Idempotent
    /// — re-opting-in a already-enabled actor just refreshes `set_at`.
    pub async fn set_subdomain_enabled(&self, actor: &[u8; 32]) -> Result<()> {
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO web_subdomain_enabled (actor_id, set_at) VALUES (?1, ?2)
             ON CONFLICT(actor_id) DO UPDATE SET set_at = ?2",
            rusqlite::params![&actor[..], now],
        )
        .context("upsert web_subdomain_enabled")?;
        Ok(())
    }

    /// Opt an actor out of subdomain hosting (delete the presence row).
    /// Idempotent — a no-op if the actor was never opted in.
    pub async fn clear_subdomain_enabled(&self, actor: &[u8; 32]) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM web_subdomain_enabled WHERE actor_id = ?1",
            rusqlite::params![&actor[..]],
        )
        .context("clear web_subdomain_enabled")?;
        Ok(())
    }

    /// Whether an actor has opted into subdomain hosting (row present).
    pub async fn is_subdomain_enabled(&self, actor: &[u8; 32]) -> Result<bool> {
        let conn = self.conn.lock().await;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM web_subdomain_enabled WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .context("query web_subdomain_enabled")?;
        Ok(n > 0)
    }

    /// Every actor currently opted into subdomain hosting (raw 32-byte ids). The
    /// boot HostResolver seed + the per-subdomain cert lifecycle reconcile from
    /// this. Skips any malformed (non-32-byte) row.
    pub async fn list_subdomain_enabled(&self) -> Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT actor_id FROM web_subdomain_enabled")
            .context("prepare list web_subdomain_enabled")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .context("query list web_subdomain_enabled")?;
        let mut out = Vec::new();
        for r in rows {
            let bytes = r.context("row list web_subdomain_enabled")?;
            if let Ok(id) = <[u8; 32]>::try_from(bytes.as_slice()) {
                out.push(id);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::CacheDb;

    #[tokio::test]
    async fn absent_reads_false_and_empty_list() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(!db.is_subdomain_enabled(&[0x11u8; 32]).await.unwrap());
        assert!(db.list_subdomain_enabled().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn opt_in_then_out_round_trips() {
        let db = CacheDb::open_in_memory().unwrap();
        let a = [0xABu8; 32];
        let b = [0x07u8; 32];

        db.set_subdomain_enabled(&a).await.unwrap();
        assert!(db.is_subdomain_enabled(&a).await.unwrap());
        assert!(!db.is_subdomain_enabled(&b).await.unwrap());

        // Re-opting-in is idempotent (still exactly one row for `a`).
        db.set_subdomain_enabled(&a).await.unwrap();
        db.set_subdomain_enabled(&b).await.unwrap();
        let mut list = db.list_subdomain_enabled().await.unwrap();
        list.sort();
        let mut want = vec![a, b];
        want.sort();
        assert_eq!(list, want);

        // Opting out drops only that actor.
        db.clear_subdomain_enabled(&a).await.unwrap();
        assert!(!db.is_subdomain_enabled(&a).await.unwrap());
        assert_eq!(db.list_subdomain_enabled().await.unwrap(), vec![b]);
    }

    /// Opting out an actor that was never opted in is a harmless no-op.
    #[tokio::test]
    async fn clear_when_absent_is_noop() {
        let db = CacheDb::open_in_memory().unwrap();
        db.clear_subdomain_enabled(&[0x22u8; 32]).await.unwrap();
        assert!(db.list_subdomain_enabled().await.unwrap().is_empty());
    }
}
