//! Per-actor IMAP/CalDAV-serving opt-out (Slice 2 of the home-with-public-relay
//! deployment shape).
//!
//! This is a **per-actor, user-set** flag, orthogonal to the deployment-wide
//! `mail_enabled` / `caldav_enabled` singletons in [`crate::db::mail_enable`] /
//! [`crate::db::caldav_enable`]. Those are admin-set and gate whether the MDA
//! binds its listeners at all; this flag says whether *this nest's MDA* serves
//! *one particular actor's* mail/calendar to external MUAs/CalDAV clients. In
//! the public→private relay deployment the user reads their mail on the paired
//! residential nest over LAN, so they turn their own row OFF on the public nest;
//! every other actor on the same public nest is unaffected.
//!
//! **Default ON / absent ⇒ ON.** A missing row means "serve" — the common case,
//! and back-compatible (no row exists until the user flips serving off). Only
//! the deployment user writes a `false` row. Settable both ways via
//! `fauna.bridges.set_mail_serving_enabled` (User-class, caller-scoped); the
//! nest-side serving gates (`require_local_mail_serving` for IMAP, the
//! `BridgeMda`-path check in the CalDAV handlers) read it per request, so a flip
//! takes effect on the MUA's next operation with no bridge round-trip — the Go
//! MDA serves every IMAP/CalDAV operation via a fresh nest WS-RPC and holds no
//! local mail/calendar cache, so the nest-side gate alone fully enforces it (no
//! `fetch_config` threading, no `config_changed` push needed).
//!
//! Spec: `docs/goal/architecture/nest/deployment-home-with-public-relay.md`
//! § MUA reach.

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

impl crate::db::CacheDb {
    /// Read an actor's IMAP/CalDAV-serving flag, or `None` if the actor has
    /// never set it (the default-ON case — the serving gates treat `None` as
    /// "serve").
    pub async fn get_actor_mail_serving_enabled(&self, actor: &[u8; 32]) -> Result<Option<bool>> {
        let conn = self.conn.lock().await;
        let raw: Option<i64> = conn
            .query_row(
                "SELECT enabled FROM actor_mail_serving WHERE actor_id = ?1",
                rusqlite::params![&actor[..]],
                |row| row.get(0),
            )
            .optional()
            .context("get actor_mail_serving")?;
        Ok(raw.map(|v| v != 0))
    }

    /// Read **every** actor's explicit IMAP/CalDAV-serving override in one
    /// query, keyed by raw 32-byte actor id. Only actors who have ever set the
    /// flag have a row (the common case — never-touched ⇒ absent ⇒ on), so this
    /// table is tiny; the admin users-list projection resolves the per-row
    /// audit indicator from this map with `.get(actor).copied().unwrap_or(true)`,
    /// avoiding an N-query fan-out across the page of users.
    pub async fn list_mail_serving_overrides(
        &self,
    ) -> Result<std::collections::HashMap<[u8; 32], bool>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare("SELECT actor_id, enabled FROM actor_mail_serving")
            .context("prepare list actor_mail_serving")?;
        let rows = stmt
            .query_map([], |row| {
                let actor: Vec<u8> = row.get(0)?;
                let enabled: i64 = row.get(1)?;
                Ok((actor, enabled != 0))
            })
            .context("query list actor_mail_serving")?;
        let mut map = std::collections::HashMap::new();
        for r in rows {
            let (actor, enabled) = r.context("row list actor_mail_serving")?;
            if let Ok(id) = <[u8; 32]>::try_from(actor.as_slice()) {
                map.insert(id, enabled);
            }
        }
        Ok(map)
    }

    /// Upsert an actor's IMAP/CalDAV-serving flag. Settable both ways
    /// (`true` ⇒ serve, `false` ⇒ stop serving this actor on this nest); the
    /// latest write wins.
    pub async fn set_actor_mail_serving_enabled(
        &self,
        actor: &[u8; 32],
        enabled: bool,
    ) -> Result<()> {
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO actor_mail_serving (actor_id, enabled, set_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(actor_id) DO UPDATE SET enabled = ?2, set_at = ?3",
            rusqlite::params![&actor[..], enabled as i64, now],
        )
        .context("upsert actor_mail_serving")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::db::CacheDb;

    #[tokio::test]
    async fn absent_reads_none() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(
            db.get_actor_mail_serving_enabled(&[0x11u8; 32])
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn set_is_settable_both_ways() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [0x22u8; 32];

        db.set_actor_mail_serving_enabled(&actor, false)
            .await
            .unwrap();
        assert_eq!(
            db.get_actor_mail_serving_enabled(&actor).await.unwrap(),
            Some(false)
        );

        db.set_actor_mail_serving_enabled(&actor, true)
            .await
            .unwrap();
        assert_eq!(
            db.get_actor_mail_serving_enabled(&actor).await.unwrap(),
            Some(true)
        );
    }

    #[tokio::test]
    async fn isolates_by_actor() {
        let db = CacheDb::open_in_memory().unwrap();
        let a = [0xA0u8; 32];
        let b = [0xB0u8; 32];

        // A opts out; B never sets a row.
        db.set_actor_mail_serving_enabled(&a, false).await.unwrap();
        assert_eq!(
            db.get_actor_mail_serving_enabled(&a).await.unwrap(),
            Some(false)
        );
        // B is untouched (absent ⇒ default-on).
        assert_eq!(db.get_actor_mail_serving_enabled(&b).await.unwrap(), None);
    }

    #[tokio::test]
    async fn list_overrides_returns_only_explicit_rows() {
        let db = CacheDb::open_in_memory().unwrap();
        let off = [0xCCu8; 32];
        let on = [0xDDu8; 32];
        // `never` sets no row → absent from the map (the projection defaults it on).
        let never = [0xEEu8; 32];

        db.set_actor_mail_serving_enabled(&off, false)
            .await
            .unwrap();
        db.set_actor_mail_serving_enabled(&on, true).await.unwrap();

        let map = db.list_mail_serving_overrides().await.unwrap();
        assert_eq!(map.get(&off).copied(), Some(false));
        assert_eq!(map.get(&on).copied(), Some(true));
        assert_eq!(map.get(&never).copied(), None);
        assert_eq!(map.len(), 2);
    }
}
