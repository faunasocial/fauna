//! Deployment-wide client-facing API serving port — the persisted,
//! settable-both-ways `serving_port` singleton. The admin's chosen port for the
//! nest's **own** client-facing HTTPS listener (the WS-RPC transport + the
//! served SPA) on a router-less / desktop-native / bare-IP / domainless box that
//! serves clients **directly** at `<host>:<port>` (no SNI router). The symmetric
//! twin of [`super::caldav_port`].
//!
//! This is an admin *choice* (a product invariant — the client UI is the
//! one user-config surface, so a port a human picks is client UI + nest
//! state, never a config file/env), set via `fauna.admin.set_serving_port`.
//! Boot-resolve reads it for the nest's listener bind, **falling back to
//! [`fauna_protocol::node_policy::DEFAULT_SERVING_PORT`] (443)** when the admin
//! has never set it; the `--bind`/`listen` seed supplies the interface/host (and
//! the port pre-claim). A router-fronted *domain* box keeps its `:443` SNI
//! router + `FAUNA_PORT` internal port (artifact-wiring), which wins over this
//! value — the singleton is inert there.
//!
//! Unlike the CORS/registration `[nest]`-policy singletons (live `ArcSwap`,
//! applied without a reboot — see [`super::node_policy`]) the nest **cannot
//! hot-rebind its own TCP listener**, so a change applies on the next nest
//! (re)start. It drives a **value** flag-file for the desktop-supervisor path
//! (which can't reach a live AppState reload): `set_serving_port` +
//! [`crate::mail_enable::reconcile_serving_port_flag_once`] materialize
//! `/data/serving-port` (decimal text) so the supervisor restarts the nest on a
//! port change. The Docker entrypoint ignores the flag (the external port is the
//! SNI router + compose port-map). Spec: `docs/goal/architecture/nest/common.md`
//! § Serving ports.

use anyhow::Result;

impl crate::db::CacheDb {
    /// Read the persisted deployment-wide client-facing serving port, or `None`
    /// if the admin has never set it. Boot-resolve supplies the derived fallback
    /// ([`fauna_protocol::node_policy::DEFAULT_SERVING_PORT`]) for the `None`
    /// case.
    pub async fn get_serving_port(&self) -> Result<Option<u16>> {
        let raw: Option<i64> = self.get_singleton_column("serving_port", "port").await?;
        // The CHECK constraint guarantees `0 < port < 65536`, so the cast is
        // lossless; clamp defensively rather than panic on a corrupt row.
        Ok(raw.map(|v| v.clamp(1, u16::MAX as i64) as u16))
    }

    /// Upsert the deployment-wide client-facing serving port. The latest write
    /// wins. `port` must be a valid bind port (1–65535); the table's CHECK
    /// rejects 0 (callers validate the range before this for a clean error).
    pub async fn set_serving_port(&self, port: u16) -> Result<()> {
        self.set_singleton_column("serving_port", "port", port as i64)
            .await
    }
}

#[cfg(test)]
mod tests {
    use crate::db::CacheDb;

    #[tokio::test]
    async fn unset_reads_none() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(db.get_serving_port().await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_is_settable_and_overwrites() {
        let db = CacheDb::open_in_memory().unwrap();

        db.set_serving_port(443).await.unwrap();
        assert_eq!(db.get_serving_port().await.unwrap(), Some(443));

        // Latest write wins (settable both ways).
        db.set_serving_port(3443).await.unwrap();
        assert_eq!(db.get_serving_port().await.unwrap(), Some(3443));

        // Boundary values round-trip.
        db.set_serving_port(1).await.unwrap();
        assert_eq!(db.get_serving_port().await.unwrap(), Some(1));
        db.set_serving_port(u16::MAX).await.unwrap();
        assert_eq!(db.get_serving_port().await.unwrap(), Some(u16::MAX));
    }

    /// The CHECK constraint rejects port 0 (not a bindable port) at the DB layer
    /// — a defence-in-depth backstop behind the handler's range validation.
    #[tokio::test]
    async fn port_zero_is_rejected_by_check() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.lock().await;
        let err = conn.execute(
            "INSERT INTO serving_port (id, port, set_at) VALUES (1, 0, 0)",
            [],
        );
        assert!(err.is_err(), "port 0 must violate the CHECK constraint");
    }

    /// The serving port and the CalDAV port are independent singleton rows.
    #[tokio::test]
    async fn serving_and_caldav_ports_are_independent() {
        let db = CacheDb::open_in_memory().unwrap();
        db.set_caldav_port(8443).await.unwrap();
        db.set_serving_port(3443).await.unwrap();
        assert_eq!(db.get_caldav_port().await.unwrap(), Some(8443));
        assert_eq!(db.get_serving_port().await.unwrap(), Some(3443));
    }
}
