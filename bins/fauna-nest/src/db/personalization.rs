//! `personalization_models` accessors — the at-rest home of the user's sealed
//! personalization-model blobs (`docs/goal/behavior/topic-factors.md` § At
//! rest — seal + home): one mutable row per (actor, factor), v1 factor
//! namespace `topic:<hex>`.
//!
//! The blob is sealed client-side under the BackupKey and **nest-opaque from
//! birth** — unlike `spam_models` (`db/moderation.rs`) there is no
//! server-side train path, no seal-vs-server-write race machinery, and no
//! plaintext/sealed dual state: these accessors store and return bytes
//! verbatim. `sample_count` is the client's ADVISORY training-event count
//! (adopt-if-larger cross-device reconcile hint), never validated against
//! the blob. Owner-scoping (a caller only ever touches its own rows) is by
//! construction: every accessor keys on the caller's `actor_id`, supplied by
//! the WS-RPC handler from the authenticated connection
//! (`personalization_handlers.rs`).

use super::{CacheDb, now_epoch_secs};
use anyhow::{Context, Result};
use rusqlite::OptionalExtension;

impl CacheDb {
    /// Get the sealed model row for `(actor, factor)` —
    /// `(sealed_blob, sample_count, updated_at)` — or `None` when the actor
    /// has never put one (or deleted it).
    pub async fn get_personalization_model(
        &self,
        actor_id: &[u8; 32],
        factor: &str,
    ) -> Result<Option<(Vec<u8>, i64, i64)>> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT sealed_blob, sample_count, updated_at FROM personalization_models
             WHERE actor_id = ?1 AND factor = ?2",
            rusqlite::params![actor_id.as_slice(), factor],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .context("get personalization model")
    }

    /// Store (INSERT OR REPLACE) the sealed model row for `(actor, factor)`,
    /// stamping `updated_at = now`. Enforces the per-actor factor cap
    /// **atomically** (count + write under the one `conn` lock, so two
    /// concurrent puts can't race past it): creating a NEW row when the actor
    /// already has `max_factors` rows returns `Ok(false)` and writes nothing;
    /// an overwrite of an existing row is always allowed. Returns `Ok(true)`
    /// when the row was stored.
    ///
    /// Content validation (factor namespace, blob size/emptiness) is the
    /// handler's job — this accessor stores the caller's bytes verbatim.
    pub async fn put_personalization_model(
        &self,
        actor_id: &[u8; 32],
        factor: &str,
        sealed_blob: &[u8],
        sample_count: i64,
        max_factors: i64,
    ) -> Result<bool> {
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let exists: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM personalization_models WHERE actor_id = ?1 AND factor = ?2",
                rusqlite::params![actor_id.as_slice(), factor],
                |row| row.get(0),
            )
            .optional()
            .context("check personalization model exists")?;
        if exists.is_none() {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM personalization_models WHERE actor_id = ?1",
                    rusqlite::params![actor_id.as_slice()],
                    |row| row.get(0),
                )
                .context("count personalization models")?;
            if count >= max_factors {
                return Ok(false);
            }
        }
        conn.execute(
            "INSERT OR REPLACE INTO personalization_models
                 (actor_id, factor, sealed_blob, sample_count, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![actor_id.as_slice(), factor, sealed_blob, sample_count, now],
        )
        .context("put personalization model")?;
        Ok(true)
    }

    /// Delete the sealed model row for `(actor, factor)`. Idempotent —
    /// returns `true` if a row was deleted, `false` if none existed.
    pub async fn delete_personalization_model(
        &self,
        actor_id: &[u8; 32],
        factor: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        let changed = conn
            .execute(
                "DELETE FROM personalization_models WHERE actor_id = ?1 AND factor = ?2",
                rusqlite::params![actor_id.as_slice(), factor],
            )
            .context("delete personalization model")?;
        Ok(changed > 0)
    }

    /// The actor's current trained-factor row count (the
    /// `TRAINED_FACTORS_MAX` cap denominator).
    pub async fn count_personalization_models(&self, actor_id: &[u8; 32]) -> Result<i64> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT COUNT(*) FROM personalization_models WHERE actor_id = ?1",
            rusqlite::params![actor_id.as_slice()],
            |row| row.get(0),
        )
        .context("count personalization models")
    }
}
