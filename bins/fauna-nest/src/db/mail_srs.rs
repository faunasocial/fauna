//! Per-deployment SRS secret(s) (`mail_srs_secrets`).
//!
//! Holds the random 32-byte HMAC key(s) behind the SRS envelope rewrite
//! (`mail-forwarding.md` § SRS secret). Auto-seeded with one row on first DB
//! open (see `migrations::run_migrations`); the admin only *rotates* it via
//! `rotate_srs_secret` (N5), never sets or reads its bytes (`:273`).
//!
//! Two accessors, one per SRS direction:
//! - [`CacheDb::get_active_srs_secret`] — the **newest** secret, used by
//!   `srs_forward` at queue-out to rewrite a forwarded envelope.
//! - [`CacheDb::list_srs_secrets`] — **every** secret, newest-first, used by
//!   `decode_srs_bounce` so an in-flight bounce issued under the prior secret
//!   still verifies during the N5 rotation overlap (`:97,:256`). N3 holds
//!   exactly one row, so both return the same single secret.

use super::{CacheDb, now_epoch_secs};
use anyhow::Result;
use rusqlite::OptionalExtension;

/// Number of SRS secrets retained across a rotation — the 2-secret overlap
/// window (`mail-forwarding.md:97,:262`): the freshly-minted secret signs new
/// forwards, and the immediately-prior one still verifies in-flight bounces
/// minted before the rotation.
pub const SRS_SECRET_OVERLAP: usize = 2;

impl CacheDb {
    /// Rotate the SRS secret (`rotate_srs_secret`, N5 — `mail-forwarding.md`
    /// § Wire shapes `:245`, § Architectural rules `:262`). Generates a fresh
    /// random 32-byte secret **nest-side** (the admin triggers the action but
    /// never supplies or sees the bytes — `:279`), inserts it as the new active
    /// secret, then prunes to the newest [`SRS_SECRET_OVERLAP`] rows so exactly
    /// the 2-secret overlap is held. `decode_srs_bounce` tries every retained
    /// secret, so a bounce minted under the prior secret still verifies during
    /// the overlap; older secrets are dropped (their in-flight bounces, if any,
    /// have expired by the SRS TT age floor).
    pub async fn rotate_srs_secret(&self) -> Result<()> {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|e| anyhow::anyhow!("getrandom failed: {e:?}"))?;
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO mail_srs_secrets (secret, created_at) VALUES (?1, ?2)",
            rusqlite::params![secret.as_slice(), now],
        )?;
        // Keep only the newest SRS_SECRET_OVERLAP rows.
        conn.execute(
            "DELETE FROM mail_srs_secrets WHERE id NOT IN (
                SELECT id FROM mail_srs_secrets ORDER BY id DESC LIMIT ?1
            )",
            rusqlite::params![SRS_SECRET_OVERLAP as i64],
        )?;
        Ok(())
    }

    /// The active (newest) SRS secret — the one `srs_forward` signs with.
    /// `None` only if the auto-seed somehow didn't run (callers treat that as
    /// "SRS not available" and skip the rewrite).
    pub async fn get_active_srs_secret(&self) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let secret: Option<Vec<u8>> = conn
            .query_row(
                "SELECT secret FROM mail_srs_secrets ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;
        Ok(secret)
    }

    /// Every stored SRS secret, newest-first. `decode_srs_bounce` tries each in
    /// turn so a bounce minted under a now-superseded secret still verifies
    /// during the rotation overlap window (N5). N3 returns a single secret.
    pub async fn list_srs_secrets(&self) -> Result<Vec<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT secret FROM mail_srs_secrets ORDER BY id DESC")?;
        let rows = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use crate::db::CacheDb;

    #[tokio::test]
    async fn auto_seeds_exactly_one_32_byte_secret() {
        let db = CacheDb::open_in_memory().unwrap();
        let secrets = db.list_srs_secrets().await.unwrap();
        assert_eq!(secrets.len(), 1, "first open seeds exactly one secret");
        assert_eq!(secrets[0].len(), 32, "SRS secret is 32 bytes");
        let active = db.get_active_srs_secret().await.unwrap().unwrap();
        assert_eq!(active, secrets[0], "active secret == the only row");
    }

    #[tokio::test]
    async fn active_is_newest_and_list_is_newest_first() {
        let db = CacheDb::open_in_memory().unwrap();
        // Simulate an N5 rotation: insert a second, newer secret.
        let newer = vec![0xABu8; 32];
        {
            let conn = db.conn.lock().await;
            conn.execute(
                "INSERT INTO mail_srs_secrets (secret, created_at) VALUES (?1, ?2)",
                rusqlite::params![newer.as_slice(), 9_999_999_999i64],
            )
            .unwrap();
        }
        let active = db.get_active_srs_secret().await.unwrap().unwrap();
        assert_eq!(active, newer, "active secret is the newest row");
        let secrets = db.list_srs_secrets().await.unwrap();
        assert_eq!(secrets.len(), 2);
        assert_eq!(secrets[0], newer, "list is newest-first");
    }

    #[tokio::test]
    async fn rotate_holds_a_two_secret_overlap_and_prunes_older() {
        let db = CacheDb::open_in_memory().unwrap();
        let seed = db.get_active_srs_secret().await.unwrap().unwrap();

        // First rotation: a new active secret, the seed retained as the overlap.
        db.rotate_srs_secret().await.unwrap();
        let after_first = db.list_srs_secrets().await.unwrap();
        assert_eq!(after_first.len(), 2, "exactly the 2-secret overlap");
        let active1 = db.get_active_srs_secret().await.unwrap().unwrap();
        assert_ne!(active1, seed, "rotation minted a fresh active secret");
        assert!(
            after_first.contains(&seed),
            "the prior (seed) secret still verifies in-flight bounces"
        );

        // Second rotation: a newer active secret; the seed is now pruned.
        db.rotate_srs_secret().await.unwrap();
        let after_second = db.list_srs_secrets().await.unwrap();
        assert_eq!(after_second.len(), 2, "still bounded at 2");
        let active2 = db.get_active_srs_secret().await.unwrap().unwrap();
        assert_ne!(active2, active1);
        assert!(
            after_second.contains(&active1),
            "the immediately-prior secret is retained"
        );
        assert!(
            !after_second.contains(&seed),
            "the twice-superseded seed is dropped"
        );
    }
}
