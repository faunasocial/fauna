//! Deployment-wide mail-enable toggle (Phase E) — the persisted, settable-
//! both-ways `mail_enabled` singleton.
//!
//! This is the DB half of `mail.enabled`. The flag-file + supervisor-socket
//! half lives in `crate::mail_enable` (the lifecycle module); this row is the
//! authoritative state that the flag file mirrors. The admin's
//! `fauna.bridges.set_mail_enabled(bool)` upserts it; `fetch_config` reads it
//! for the bridge's `mail_enabled`; the 60 s reconciliation tick re-asserts the
//! flag file from it.
//!
//! This toggle flips both ways for the life of the deployment —
//! `INSERT … ON CONFLICT DO UPDATE`.
//!
//! This module also hosts the sibling `mail_auto_enable_new_users` singleton
//! (the deployment-wide "auto-enable mail for new users" policy default —
//! `get/set_auto_enable_mail_for_new_users`): same shape, but a *client-read*
//! deployment default surfaced on `fauna.setup.status` rather than a bridge
//! signal (the nest can't mint a user's mailbox — the MSEK is client-held).
//!
//! Spec: `docs/goal/behavior/mail-bridge-lifecycle.md` § Default-off on first
//! claim + § Implementation status today (Phase E);
//! `docs/goal/behavior/mail-policy-config.md` § Tier-2 *Auto-enable mail for new
//! users* + `docs/goal/behavior/mail-credentials.md` § Auto-enable for new users.

use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

impl crate::db::CacheDb {
    /// Read the persisted deployment-wide mail-enable state, or `None` if the
    /// admin has never toggled it (a freshly-claimed nest). `fetch_config`
    /// supplies the derived fallback for the `None` case.
    pub async fn get_mail_enabled(&self) -> Result<Option<bool>> {
        let raw: Option<i64> = self.get_singleton_column("mail_enabled", "enabled").await?;
        Ok(raw.map(|v| v != 0))
    }

    /// The **effective** deployment-wide mail-enable state — the single owner
    /// of the unset default (Stage-5 default-off, `mail-policy-config.md`
    /// § Default-off on first claim): a toggle the admin has never written
    /// reads **off**. Enablement is always an explicit act — the launched
    /// client's claim-time § 3b glue (`onboarding.md` § 3b) or the admin-mail
    /// toggle — never a fallback. Every site needing an effective bool (not the tri-state) must route
    /// through here so the default lives in exactly one place.
    pub async fn effective_mail_enabled(&self) -> Result<bool> {
        Ok(self.get_mail_enabled().await?.unwrap_or(false))
    }

    /// Upsert the deployment-wide mail-enable toggle. Settable both ways
    /// (`true` ⇒ enabled, `false` ⇒ disabled); the latest write wins.
    pub async fn set_mail_enabled(&self, enabled: bool) -> Result<()> {
        self.set_singleton_column("mail_enabled", "enabled", enabled as i64)
            .await
    }

    /// Read the persisted deployment-wide "auto-enable mail for new users"
    /// policy, or `None` if the admin has never toggled it. The effective
    /// default for the `None` case is **ON** — `setup_status` supplies it via
    /// `.unwrap_or(true)`, so a freshly-claimed nest auto-enables mail for new
    /// users out of the box (the works-out-of-box invariant, extended to every
    /// user). Distinct from `mail_enabled` (whether the subsystem runs at all):
    /// this gates whether a *new user's* client auto-provisions its mailbox,
    /// and is only consulted client-side together with `email_enabled`.
    pub async fn get_auto_enable_mail_for_new_users(&self) -> Result<Option<bool>> {
        let raw: Option<i64> = self
            .get_singleton_column("mail_auto_enable_new_users", "enabled")
            .await?;
        Ok(raw.map(|v| v != 0))
    }

    /// Upsert the deployment-wide "auto-enable mail for new users" policy.
    /// Settable both ways; the latest write wins. Admin-only at the RPC layer
    /// (`fauna.bridges.set_auto_enable_mail_for_new_users`); a deployment-wide
    /// default, never a per-user control (`admin.md` § Don't do these).
    pub async fn set_auto_enable_mail_for_new_users(&self, enabled: bool) -> Result<()> {
        self.set_singleton_column("mail_auto_enable_new_users", "enabled", enabled as i64)
            .await
    }

    /// The deployment's own Microsoft app registration for the Outlook import
    /// as `(client_id, tenant)`, or `None` when the admin has set none — the
    /// project's registration, the Rust constant
    /// `fauna_mail::imap_client::MICROSOFT`, is then in force
    /// (`mailbox-migration.md` § Microsoft sign-in, *An admin-supplied
    /// registration*). Surfaced on `fauna.setup.status`; the bridge never reads
    /// it.
    pub async fn get_microsoft_registration(&self) -> Result<Option<(String, String)>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT client_id, tenant FROM mail_microsoft_registration WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .context("get mail_microsoft_registration")
    }

    /// Replace the registration with an already-normalised pair
    /// (`fauna_protocol::microsoft_registration::normalize`), or clear it back
    /// to the project's with `None`. One statement either way — the single
    /// atomic decision point. The clear deletes the row: the admin chose the
    /// project default, and the two public identifiers are re-enterable.
    pub async fn set_microsoft_registration(
        &self,
        registration: Option<(&str, &str)>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        match registration {
            Some((client_id, tenant)) => conn
                .execute(
                    "INSERT INTO mail_microsoft_registration (id, client_id, tenant, set_at)
                     VALUES (1, ?1, ?2, ?3)
                     ON CONFLICT(id) DO UPDATE SET
                         client_id = excluded.client_id,
                         tenant = excluded.tenant,
                         set_at = excluded.set_at",
                    rusqlite::params![client_id, tenant, super::now_epoch_secs()],
                )
                .context("upsert mail_microsoft_registration")?,
            // The admin's own clear of a setting back to its default: two
            // public identifiers they can re-enter, no user data.
            None => conn
                .execute("DELETE FROM mail_microsoft_registration WHERE id = 1", [])
                .context("clear mail_microsoft_registration")?,
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::db::CacheDb;

    #[tokio::test]
    async fn unset_reads_none() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(db.get_mail_enabled().await.unwrap(), None);
    }

    #[tokio::test]
    async fn set_is_settable_both_ways() {
        let db = CacheDb::open_in_memory().unwrap();

        db.set_mail_enabled(true).await.unwrap();
        assert_eq!(db.get_mail_enabled().await.unwrap(), Some(true));

        // Settable both ways: flipping back down sticks.
        db.set_mail_enabled(false).await.unwrap();
        assert_eq!(db.get_mail_enabled().await.unwrap(), Some(false));

        db.set_mail_enabled(true).await.unwrap();
        assert_eq!(db.get_mail_enabled().await.unwrap(), Some(true));
    }

    #[tokio::test]
    async fn auto_enable_new_users_unset_reads_none() {
        // Unset ⇒ None; the effective default-ON lives at the read site
        // (`setup_status` `.unwrap_or(true)`), not in the DB row.
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(db.get_auto_enable_mail_for_new_users().await.unwrap(), None);
    }

    #[tokio::test]
    async fn auto_enable_new_users_settable_both_ways() {
        let db = CacheDb::open_in_memory().unwrap();

        db.set_auto_enable_mail_for_new_users(false).await.unwrap();
        assert_eq!(
            db.get_auto_enable_mail_for_new_users().await.unwrap(),
            Some(false)
        );

        db.set_auto_enable_mail_for_new_users(true).await.unwrap();
        assert_eq!(
            db.get_auto_enable_mail_for_new_users().await.unwrap(),
            Some(true)
        );
    }

    #[tokio::test]
    async fn microsoft_registration_sets_replaces_and_clears() {
        let db = CacheDb::open_in_memory().unwrap();
        assert_eq!(db.get_microsoft_registration().await.unwrap(), None);

        db.set_microsoft_registration(Some(("id-1", "common")))
            .await
            .unwrap();
        assert_eq!(
            db.get_microsoft_registration().await.unwrap(),
            Some(("id-1".into(), "common".into()))
        );

        db.set_microsoft_registration(Some(("id-2", "contoso.example")))
            .await
            .unwrap();
        assert_eq!(
            db.get_microsoft_registration().await.unwrap(),
            Some(("id-2".into(), "contoso.example".into()))
        );

        db.set_microsoft_registration(None).await.unwrap();
        assert_eq!(db.get_microsoft_registration().await.unwrap(), None);
    }
}
