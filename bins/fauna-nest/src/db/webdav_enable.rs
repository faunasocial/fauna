//! Deployment-wide WebDAV-enable toggle — the persisted, settable-both-ways
//! `webdav_enabled` singleton. The files twin of [`super::carddav_enable`].
//!
//! This is the DB half of `webdav.enabled`. The flag-file + supervisor-socket
//! half lives in `crate::mail_enable` (the lifecycle module, shared with the
//! mail + CalDAV + CardDAV toggles); this row is the authoritative state the
//! `/data/webdav-enabled` flag file mirrors. The admin's
//! `fauna.bridges.set_webdav_enabled(bool)` upserts it; `fetch_config` reads it
//! for the bridge's `webdav_enabled`, **falling back to `mail_enabled`** when
//! unset (a fresh real-domain deployment gets a files surface out of the box,
//! the same default posture as CalDAV/CardDAV); the 60 s reconciliation tick
//! re-asserts the flag from it.
//!
//! Why a separate toggle at all: WebDAV needs only the HTTPS surface (and rides
//! the SAME DAV listener as CalDAV/CardDAV — no separate port), whereas email
//! needs MX + DKIM + SPF/DMARC + port-25 reachability + deliverability
//! reputation. So files can run without email or calendar or contacts (and vice
//! versa) — all four ride the one MDA bridge and share the one credential, but
//! gate independently. `webdav_enabled` is the *deployment* gate; nothing is
//! served until a set is individually flagged (`folders.webdav_enabled`), so
//! defaulting it on is harmless. Spec: `docs/goal/behavior/webdav-server.md`
//! § Independent enablement.

use anyhow::Result;

impl crate::db::CacheDb {
    /// Read the persisted deployment-wide WebDAV-enable state, or `None` if the
    /// admin has never toggled it. `fetch_config` supplies the derived fallback
    /// (follow `mail_enabled`) for the `None` case.
    pub async fn get_webdav_enabled(&self) -> Result<Option<bool>> {
        let raw: Option<i64> = self
            .get_singleton_column("webdav_enabled", "enabled")
            .await?;
        Ok(raw.map(|v| v != 0))
    }

    /// Upsert the deployment-wide WebDAV-enable toggle. Settable both ways
    /// (`true` ⇒ enabled, `false` ⇒ disabled); the latest write wins.
    pub async fn set_webdav_enabled(&self, enabled: bool) -> Result<()> {
        self.set_singleton_column("webdav_enabled", "enabled", enabled as i64)
            .await
    }
}

#[cfg(test)]
mod tests {
    use crate::db::CacheDb;

    #[tokio::test]
    async fn unset_reads_none() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(db.get_webdav_enabled().await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_is_settable_both_ways() {
        let db = CacheDb::open_in_memory().unwrap();

        db.set_webdav_enabled(true).await.unwrap();
        assert_eq!(db.get_webdav_enabled().await.unwrap(), Some(true));

        // Settable both ways.
        db.set_webdav_enabled(false).await.unwrap();
        assert_eq!(db.get_webdav_enabled().await.unwrap(), Some(false));

        db.set_webdav_enabled(true).await.unwrap();
        assert_eq!(db.get_webdav_enabled().await.unwrap(), Some(true));
    }

    /// The WebDAV toggle is independent of the mail + CalDAV + CardDAV toggles —
    /// flipping one does not perturb the others (they are separate singleton
    /// rows).
    #[tokio::test]
    async fn webdav_mail_caldav_and_carddav_toggles_are_independent() {
        let db = CacheDb::open_in_memory().unwrap();
        db.set_mail_enabled(true).await.unwrap();
        db.set_caldav_enabled(true).await.unwrap();
        db.set_carddav_enabled(true).await.unwrap();
        db.set_webdav_enabled(false).await.unwrap();
        assert_eq!(db.get_mail_enabled().await.unwrap(), Some(true));
        assert_eq!(db.get_caldav_enabled().await.unwrap(), Some(true));
        assert_eq!(db.get_carddav_enabled().await.unwrap(), Some(true));
        assert_eq!(db.get_webdav_enabled().await.unwrap(), Some(false));
    }
}
