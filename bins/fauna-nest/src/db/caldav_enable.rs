//! Deployment-wide CalDAV-enable toggle — the persisted, settable-both-ways
//! `caldav_enabled` singleton. The calendar twin of [`super::mail_enable`].
//!
//! This is the DB half of `caldav.enabled`. The flag-file + supervisor-socket
//! half lives in `crate::mail_enable` (the lifecycle module, shared with the
//! mail toggle); this row is the authoritative state the `/data/caldav-enabled`
//! flag file mirrors. The admin's `fauna.bridges.set_caldav_enabled(bool)`
//! upserts it; `fetch_config` reads it for the bridge's `caldav_enabled`,
//! **falling back to `mail_enabled`** when unset (the out-of-the-box default —
//! "enabling email also enables CalDAV" until the admin sets the toggle
//! independently); the 60 s reconciliation tick re-asserts the flag from it.
//!
//! Why a separate toggle at all: CalDAV needs only the HTTPS surface (a cert for
//! `mail.<domain>` + the SNI-router), whereas email needs MX + DKIM + SPF/DMARC +
//! port-25 reachability + deliverability reputation. So calendar can run
//! without email (and vice versa) — both ride the one MDA bridge and share the
//! one credential, but gate independently. Spec:
//! `docs/goal/behavior/caldav-server.md` § Independent enablement.

use anyhow::Result;

impl crate::db::CacheDb {
    /// Read the persisted deployment-wide CalDAV-enable state, or `None` if the
    /// admin has never toggled it. `fetch_config` supplies the derived fallback
    /// (follow `mail_enabled`) for the `None` case.
    pub async fn get_caldav_enabled(&self) -> Result<Option<bool>> {
        let raw: Option<i64> = self
            .get_singleton_column("caldav_enabled", "enabled")
            .await?;
        Ok(raw.map(|v| v != 0))
    }

    /// Upsert the deployment-wide CalDAV-enable toggle. Settable both ways
    /// (`true` ⇒ enabled, `false` ⇒ disabled); the latest write wins.
    pub async fn set_caldav_enabled(&self, enabled: bool) -> Result<()> {
        self.set_singleton_column("caldav_enabled", "enabled", enabled as i64)
            .await
    }
}

#[cfg(test)]
mod tests {
    use crate::db::CacheDb;

    #[tokio::test]
    async fn unset_reads_none() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(db.get_caldav_enabled().await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_is_settable_both_ways() {
        let db = CacheDb::open_in_memory().unwrap();

        db.set_caldav_enabled(true).await.unwrap();
        assert_eq!(db.get_caldav_enabled().await.unwrap(), Some(true));

        // Settable both ways.
        db.set_caldav_enabled(false).await.unwrap();
        assert_eq!(db.get_caldav_enabled().await.unwrap(), Some(false));

        db.set_caldav_enabled(true).await.unwrap();
        assert_eq!(db.get_caldav_enabled().await.unwrap(), Some(true));
    }

    /// The CalDAV toggle is independent of the mail toggle — flipping one does
    /// not perturb the other (they are separate singleton rows).
    #[tokio::test]
    async fn caldav_and_mail_toggles_are_independent() {
        let db = CacheDb::open_in_memory().unwrap();
        db.set_mail_enabled(true).await.unwrap();
        db.set_caldav_enabled(false).await.unwrap();
        assert_eq!(db.get_mail_enabled().await.unwrap(), Some(true));
        assert_eq!(db.get_caldav_enabled().await.unwrap(), Some(false));
    }
}
