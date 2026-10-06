//! Deployment-wide CardDAV-enable toggle — the persisted, settable-both-ways
//! `carddav_enabled` singleton. The contacts twin of [`super::caldav_enable`].
//!
//! This is the DB half of `carddav.enabled`. The flag-file + supervisor-socket
//! half lives in `crate::mail_enable` (the lifecycle module, shared with the
//! mail + CalDAV toggles); this row is the authoritative state the
//! `/data/carddav-enabled` flag file mirrors. The admin's
//! `fauna.bridges.set_carddav_enabled(bool)` upserts it; `fetch_config` reads it
//! for the bridge's `carddav_enabled`, **falling back to `mail_enabled`** when
//! unset (a fresh real-domain deployment gets a contacts surface out of the box,
//! the same default posture as CalDAV); the 60 s reconciliation tick re-asserts
//! the flag from it.
//!
//! Why a separate toggle at all: CardDAV needs only the HTTPS surface (and rides
//! the SAME DAV listener as CalDAV — no separate port), whereas email needs MX +
//! DKIM + SPF/DMARC + port-25 reachability + deliverability reputation. So
//! contacts can run without email or calendar (and vice versa) — all three ride
//! the one MDA bridge and share the one credential, but gate independently.
//! Part of the CardDAV server design (tracked internally), § 5.

use anyhow::Result;

impl crate::db::CacheDb {
    /// Read the persisted deployment-wide CardDAV-enable state, or `None` if the
    /// admin has never toggled it. `fetch_config` supplies the derived fallback
    /// (follow `mail_enabled`) for the `None` case.
    pub async fn get_carddav_enabled(&self) -> Result<Option<bool>> {
        let raw: Option<i64> = self
            .get_singleton_column("carddav_enabled", "enabled")
            .await?;
        Ok(raw.map(|v| v != 0))
    }

    /// Upsert the deployment-wide CardDAV-enable toggle. Settable both ways
    /// (`true` ⇒ enabled, `false` ⇒ disabled); the latest write wins.
    pub async fn set_carddav_enabled(&self, enabled: bool) -> Result<()> {
        self.set_singleton_column("carddav_enabled", "enabled", enabled as i64)
            .await
    }
}

#[cfg(test)]
mod tests {
    use crate::db::CacheDb;

    #[tokio::test]
    async fn unset_reads_none() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(db.get_carddav_enabled().await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_is_settable_both_ways() {
        let db = CacheDb::open_in_memory().unwrap();

        db.set_carddav_enabled(true).await.unwrap();
        assert_eq!(db.get_carddav_enabled().await.unwrap(), Some(true));

        // Settable both ways.
        db.set_carddav_enabled(false).await.unwrap();
        assert_eq!(db.get_carddav_enabled().await.unwrap(), Some(false));

        db.set_carddav_enabled(true).await.unwrap();
        assert_eq!(db.get_carddav_enabled().await.unwrap(), Some(true));
    }

    /// The CardDAV toggle is independent of the mail + CalDAV toggles — flipping
    /// one does not perturb the others (they are separate singleton rows).
    #[tokio::test]
    async fn carddav_mail_and_caldav_toggles_are_independent() {
        let db = CacheDb::open_in_memory().unwrap();
        db.set_mail_enabled(true).await.unwrap();
        db.set_caldav_enabled(true).await.unwrap();
        db.set_carddav_enabled(false).await.unwrap();
        assert_eq!(db.get_mail_enabled().await.unwrap(), Some(true));
        assert_eq!(db.get_caldav_enabled().await.unwrap(), Some(true));
        assert_eq!(db.get_carddav_enabled().await.unwrap(), Some(false));
    }
}
