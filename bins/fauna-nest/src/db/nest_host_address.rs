//! `nest_host_address` singleton accessors — the deployment's own public
//! address(es), per `docs/goal/behavior/dns-management.md` § Records covered +
//! § Implementation status today (Slice 2b).
//!
//! Populated by the `fauna.dns.set_host_address` onboarding hand-off (the
//! orchestrator learns the IP from the VPS provider at provisioning; the admin
//! client relays it once claimed). The address is **public, not a secret** —
//! unlike DNS-provider credentials, which stay client-held — so it is plaintext
//! nest state, upserted (not write-once: a re-provision can change the IP).
//!
//! Two address roles because `mail.<primary>` (the MX target) may resolve to a
//! different IP than the apex `<primary>` (the mail server can be a separate
//! box from the nest); see the memory `mail-host-ip-distinct-from-nest-ip`.

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

use super::CacheDb;

/// The deployment's persisted public address(es). Record *names* are derived
/// from the primary mail domain (`<primary>` apex + `mail.<primary>`); only the
/// IPs live here. `*_ipv6` is `None` until the deployment has an IPv6 address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostAddress {
    /// Nest (apex `<primary>`) IPv4 — the WS-RPC / web endpoint.
    pub nest_ipv4: String,
    pub nest_ipv6: Option<String>,
    /// Mail-host (`mail.<primary>` / MX target) IPv4 — may differ from
    /// `nest_ipv4`.
    pub mail_ipv4: String,
    pub mail_ipv6: Option<String>,
}

impl CacheDb {
    /// Read the persisted host address, or `None` if the hand-off hasn't run
    /// yet (fresh nest) → no host rows are surfaced.
    pub async fn get_host_address(&self) -> Result<Option<HostAddress>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT nest_ipv4, nest_ipv6, mail_ipv4, mail_ipv6
             FROM nest_host_address WHERE id = 1",
            [],
            |row| {
                Ok(HostAddress {
                    nest_ipv4: row.get(0)?,
                    nest_ipv6: row.get(1)?,
                    mail_ipv4: row.get(2)?,
                    mail_ipv6: row.get(3)?,
                })
            },
        )
        .optional()
        .context("get nest host address")
    }

    /// Upsert the singleton host-address row (id = 1).
    pub async fn set_host_address(&self, addr: &HostAddress) -> Result<()> {
        let now = super::now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO nest_host_address (id, nest_ipv4, nest_ipv6, mail_ipv4, mail_ipv6, updated_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                 nest_ipv4 = excluded.nest_ipv4,
                 nest_ipv6 = excluded.nest_ipv6,
                 mail_ipv4 = excluded.mail_ipv4,
                 mail_ipv6 = excluded.mail_ipv6,
                 updated_at = excluded.updated_at",
            rusqlite::params![
                addr.nest_ipv4,
                addr.nest_ipv6,
                addr.mail_ipv4,
                addr.mail_ipv6,
                now,
            ],
        )
        .context("upsert nest host address")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn get_is_none_before_set() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        assert_eq!(db.get_host_address().await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_then_get_roundtrips_and_upserts() {
        let db = Arc::new(CacheDb::open_in_memory().unwrap());
        let addr = HostAddress {
            nest_ipv4: "203.0.113.7".into(),
            nest_ipv6: None,
            mail_ipv4: "198.51.100.9".into(),
            mail_ipv6: Some("2001:db8::9".into()),
        };
        db.set_host_address(&addr).await.unwrap();
        assert_eq!(db.get_host_address().await.unwrap(), Some(addr));

        // Upsert: a second write replaces the row (no write-once lock).
        let updated = HostAddress {
            nest_ipv4: "203.0.113.20".into(),
            nest_ipv6: None,
            mail_ipv4: "203.0.113.20".into(),
            mail_ipv6: None,
        };
        db.set_host_address(&updated).await.unwrap();
        assert_eq!(db.get_host_address().await.unwrap(), Some(updated));
    }
}
