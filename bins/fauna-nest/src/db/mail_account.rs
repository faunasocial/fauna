//! Per-account mail settings (`mail_account_settings`), one row per actor.
//!
//! Sibling of the `spam_preferences` accessors in `moderation.rs`. Today this
//! holds the per-account "forward all incoming mail to" address (PLAINTEXT
//! mode — `mail-forwarding.md` § Per-account "forward all") and the user's own
//! hourly forward cap `forward_per_hour`, which the N5 rate cap reads. In encrypted mode
//! the user-private `forward_all_to` is sealed wrapped-to-the-bridge in a
//! separate fan-out table (N1b), so this column stays NULL there.

use super::{CacheDb, now_epoch_millis};
use anyhow::Result;
use rusqlite::OptionalExtension;

impl CacheDb {
    // ── Per-account mail settings ──────────────────────────────────────

    /// The actor's plaintext-mode `forward_all_to` address, or `None` when
    /// forwarding is disabled (no row, or a NULL/empty column).
    pub async fn get_forward_all_to(&self, actor_id: &[u8; 32]) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        let value: Option<Option<String>> = conn
            .query_row(
                "SELECT forward_all_to FROM mail_account_settings WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        // Flatten "no row" and "NULL column" to the same disabled state, and
        // treat a blank string as disabled too.
        Ok(value.flatten().filter(|s| !s.trim().is_empty()))
    }

    /// The actor's per-account forward rate cap (`mail.account.forward_per_hour`,
    /// `mail-forwarding.md:174`). Returns the column value, or
    /// [`fauna_mail::FORWARD_PER_HOUR_DEFAULT`] when the actor has no settings
    /// row yet. The effective cap is `min(this, admin-ceiling)` — see N5's
    /// rate-cap in `forward_message_handler`.
    pub async fn get_forward_per_hour(&self, actor_id: &[u8; 32]) -> Result<u32> {
        let conn = self.conn.lock().await;
        let value: Option<u32> = conn
            .query_row(
                "SELECT forward_per_hour FROM mail_account_settings WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
                |row| row.get::<_, u32>(0),
            )
            .optional()?;
        Ok(value.unwrap_or(fauna_mail::FORWARD_PER_HOUR_DEFAULT))
    }

    /// Overwrite the actor's per-account forward rate cap. Upserts the per-actor
    /// row, leaving `forward_all_to` / the spam override untouched. The caller
    /// validates the value against the admin ceiling first
    /// (`fauna_mail::validate_forward_per_hour`).
    pub async fn set_forward_per_hour(
        &self,
        actor_id: &[u8; 32],
        forward_per_hour: u32,
    ) -> Result<()> {
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO mail_account_settings (actor_id, forward_per_hour, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(actor_id) DO UPDATE SET
                forward_per_hour = excluded.forward_per_hour,
                updated_at = excluded.updated_at",
            rusqlite::params![actor_id.as_slice(), forward_per_hour, now],
        )?;
        Ok(())
    }

    /// Set (or, with `None`, clear) the actor's plaintext-mode `forward_all_to`.
    /// Upserts the per-actor row, leaving `forward_per_hour` at its default on
    /// insert. The caller is responsible for storage-mode gating + address
    /// validation (`fauna_mail::validate_forward_target`) before calling this.
    pub async fn set_forward_all_to(
        &self,
        actor_id: &[u8; 32],
        forward_all_to: Option<&str>,
    ) -> Result<()> {
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO mail_account_settings (actor_id, forward_all_to, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(actor_id) DO UPDATE SET
                forward_all_to = excluded.forward_all_to,
                updated_at = excluded.updated_at",
            rusqlite::params![actor_id.as_slice(), forward_all_to, now],
        )?;
        Ok(())
    }

    /// The actor's per-account spam-threshold override
    /// (`mail.account.spam_threshold_override`, `mail-aliases.md`
    /// § Spam-threshold override / `mail-policy-config.md` § Tier 3), or `None`
    /// when the actor inherits the admin default. **`Some(0)` is not `None`**:
    /// zero is the user disabling auto-Junk filing for their whole account,
    /// which must outrank the admin tier — see
    /// [`fauna_mail::aliases::resolve_delivery_spam_threshold`], the one place
    /// the three tiers collapse.
    pub async fn get_spam_threshold_override(&self, actor_id: &[u8; 32]) -> Result<Option<u32>> {
        let conn = self.conn.lock().await;
        let value: Option<Option<u32>> = conn
            .query_row(
                "SELECT spam_threshold_override FROM mail_account_settings WHERE actor_id = ?1",
                rusqlite::params![actor_id.as_slice()],
                |row| row.get::<_, Option<u32>>(0),
            )
            .optional()?;
        // "No row" and "NULL column" are the same inherit-the-default state.
        Ok(value.flatten())
    }

    /// Set (or, with `None`, clear) the actor's per-account spam-threshold
    /// override. Upserts the per-actor row, leaving the forwarding columns at
    /// their defaults on insert (the mirror of [`Self::set_forward_all_to`]).
    pub async fn set_spam_threshold_override(
        &self,
        actor_id: &[u8; 32],
        spam_threshold_override: Option<u32>,
    ) -> Result<()> {
        let now = now_epoch_millis();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO mail_account_settings (actor_id, spam_threshold_override, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(actor_id) DO UPDATE SET
                spam_threshold_override = excluded.spam_threshold_override,
                updated_at = excluded.updated_at",
            rusqlite::params![actor_id.as_slice(), spam_threshold_override, now],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::db::CacheDb;

    #[tokio::test]
    async fn forward_per_hour_defaults_when_no_row_and_reads_seeded_value() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [7u8; 32];
        // No settings row yet → the spec default (100).
        assert_eq!(
            db.get_forward_per_hour(&actor).await.unwrap(),
            fauna_mail::FORWARD_PER_HOUR_DEFAULT
        );
        // Seed an explicit per-account cap (mirrors a future write path).
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO mail_account_settings (actor_id, forward_per_hour, updated_at)
                 VALUES (?1, ?2, ?3)",
                rusqlite::params![actor.as_slice(), 5i64, 0i64],
            )
            .unwrap();
        }
        assert_eq!(db.get_forward_per_hour(&actor).await.unwrap(), 5);
    }

    #[tokio::test]
    async fn spam_threshold_override_round_trips_including_the_meaningful_zero() {
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [9u8; 32];
        // No row at all ⇒ inherit.
        assert_eq!(db.get_spam_threshold_override(&actor).await.unwrap(), None);
        // Set, read back.
        db.set_spam_threshold_override(&actor, Some(3))
            .await
            .unwrap();
        assert_eq!(
            db.get_spam_threshold_override(&actor).await.unwrap(),
            Some(3)
        );
        // Zero is a stored CHOICE (auto-Junk off for this account), never
        // conflated with "unset" — the distinction the fold depends on.
        db.set_spam_threshold_override(&actor, Some(0))
            .await
            .unwrap();
        assert_eq!(
            db.get_spam_threshold_override(&actor).await.unwrap(),
            Some(0)
        );
        // Clearing goes back to inherit.
        db.set_spam_threshold_override(&actor, None).await.unwrap();
        assert_eq!(db.get_spam_threshold_override(&actor).await.unwrap(), None);
    }

    #[tokio::test]
    async fn the_two_settings_upserts_do_not_clobber_each_other() {
        // Both writers upsert the same one-row-per-actor table; each must leave
        // the other's column alone (they are independent user preferences, and
        // an app sets them from different screens).
        let db = CacheDb::open_in_memory().unwrap();
        let actor = [11u8; 32];
        db.set_forward_all_to(&actor, Some("elsewhere@example.test"))
            .await
            .unwrap();
        db.set_spam_threshold_override(&actor, Some(2))
            .await
            .unwrap();
        assert_eq!(
            db.get_forward_all_to(&actor).await.unwrap().as_deref(),
            Some("elsewhere@example.test")
        );
        assert_eq!(
            db.get_spam_threshold_override(&actor).await.unwrap(),
            Some(2)
        );
        db.set_forward_all_to(&actor, None).await.unwrap();
        assert_eq!(
            db.get_spam_threshold_override(&actor).await.unwrap(),
            Some(2),
            "clearing the forward target must not clear the spam override"
        );
    }
}
