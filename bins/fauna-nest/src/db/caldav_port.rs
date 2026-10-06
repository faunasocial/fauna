//! Deployment-wide CalDAV listener port — the persisted, settable-both-ways
//! `caldav_port` singleton. The admin's chosen port for the MDA's CalDAV
//! listener on a bare-IP / desktop / domainless box that serves CalDAV
//! **directly** at `<host>:<port>` (no SNI router).
//!
//! This is an admin *choice* (a product invariant — the client UI is the
//! one user-config surface, so a port a human picks is client UI + nest
//! state, never a config file/env), set via
//! `fauna.bridges.set_caldav_port`. `fetch_config` reads it for the bridge's
//! `caldav_port`, **falling back to
//! [`fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT`] (8443)** when the
//! admin has never set it. The MDA binds this port when no operator-hatch pins
//! its CalDAV listener; a router-fronted *domain* box keeps its loopback-IPC
//! hatch (`caldav_listen_https = 127.0.0.1:8444`), which wins over this value.
//!
//! Unlike [`super::caldav_enable`] this is not a listener-*gating* flag the s6
//! run-script reads (it never decides whether the MDA starts); the Docker MDA
//! reads the port as a parameter from `fetch_config`. It *does*, however, drive a
//! **value** flag-file for the desktop-supervisor path, which can't reach
//! `fetch_config` (bridge-enrollment-only): `set_caldav_port` +
//! [`crate::mail_enable::reconcile_caldav_port_flag_once`] materialize
//! `/data/caldav-port` (decimal text) so the Windows `fauna-bridge-service`
//! supervisor re-pins the MDA's `caldav_listen_https` operator-hatch and rebinds
//! on a port change (`apps/fauna-windows/fauna-bridge-service`). The Docker MDA
//! ignores the flag. Spec: `docs/goal/behavior/caldav-server.md` § Network
//! exposure (Desktop / IP deployment).

use anyhow::Result;

impl crate::db::CacheDb {
    /// Read the persisted deployment-wide CalDAV listener port, or `None` if the
    /// admin has never set it. `fetch_config` supplies the derived fallback
    /// ([`fauna_protocol::bridge_routing::DEFAULT_CALDAV_PORT`]) for the `None`
    /// case.
    pub async fn get_caldav_port(&self) -> Result<Option<u16>> {
        let raw: Option<i64> = self.get_singleton_column("caldav_port", "port").await?;
        // The CHECK constraint guarantees `0 < port < 65536`, so the cast is
        // lossless; clamp defensively rather than panic on a corrupt row.
        Ok(raw.map(|v| v.clamp(1, u16::MAX as i64) as u16))
    }

    /// Upsert the deployment-wide CalDAV listener port. The latest write wins.
    /// `port` must be a valid bind port (1–65535); the table's CHECK rejects 0
    /// (callers validate the range before this for a clean error).
    pub async fn set_caldav_port(&self, port: u16) -> Result<()> {
        self.set_singleton_column("caldav_port", "port", port as i64)
            .await
    }
}

#[cfg(test)]
mod tests {
    use crate::db::CacheDb;

    #[tokio::test]
    async fn unset_reads_none() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(db.get_caldav_port().await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_is_settable_and_overwrites() {
        let db = CacheDb::open_in_memory().unwrap();

        db.set_caldav_port(8443).await.unwrap();
        assert_eq!(db.get_caldav_port().await.unwrap(), Some(8443));

        // Latest write wins (settable both ways, like the enable toggle).
        db.set_caldav_port(9443).await.unwrap();
        assert_eq!(db.get_caldav_port().await.unwrap(), Some(9443));

        // Boundary values round-trip.
        db.set_caldav_port(1).await.unwrap();
        assert_eq!(db.get_caldav_port().await.unwrap(), Some(1));
        db.set_caldav_port(u16::MAX).await.unwrap();
        assert_eq!(db.get_caldav_port().await.unwrap(), Some(u16::MAX));
    }

    /// The CHECK constraint rejects port 0 (not a bindable port) at the DB layer
    /// — a defence-in-depth backstop behind the handler's range validation.
    #[tokio::test]
    async fn port_zero_is_rejected_by_check() {
        let db = CacheDb::open_in_memory().unwrap();
        let conn = db.conn.lock().await;
        let err = conn.execute(
            "INSERT INTO caldav_port (id, port, set_at) VALUES (1, 0, 0)",
            [],
        );
        assert!(err.is_err(), "port 0 must violate the CHECK constraint");
    }

    /// The port toggle is independent of the CalDAV-enable toggle — they are
    /// separate singleton rows.
    #[tokio::test]
    async fn port_and_enable_are_independent() {
        let db = CacheDb::open_in_memory().unwrap();
        db.set_caldav_enabled(true).await.unwrap();
        db.set_caldav_port(9443).await.unwrap();
        assert_eq!(db.get_caldav_enabled().await.unwrap(), Some(true));
        assert_eq!(db.get_caldav_port().await.unwrap(), Some(9443));
    }
}
