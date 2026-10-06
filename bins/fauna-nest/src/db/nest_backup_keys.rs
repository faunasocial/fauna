//! Nest-side `NestBackupKey` grant store (nest-side segment backup slice 2,
//! design tracked internally;
//! `key-material-hierarchy.md` § Path A-sibling-0).
//!
//! One row per `owner_actor_id` in `nest_backup_keys` (table defined in
//! `migrations::MIGRATIONS_NEST_BACKUP_KEYS`). The user's own client derives
//! `NestBackupKey` from its identity seed and grants the 32-byte key to its
//! source nest at destination-enroll; the nest's in-process backup coordinator
//! seals that owner's segment files under it before upload to blind
//! destinations.
//!
//! **Contrast with `capability_grants`:** that store holds opaque HPKE-sealed
//! ciphertext the nest cannot open. This key is stored **plaintext and
//! nest-readable by design** — the coordinator must open it to seal, and it
//! reveals nothing the nest does not already host (the sealed segment files
//! rest plaintext-framed in the same data dir; design record § Trust-domain
//! analysis). It sits beside the deployment key material nest.db already holds
//! (the reconciled `nest_keypair` row). Revoke = delete the row (idempotent);
//! it is always re-derivable from the seed, so a revoke loses nothing
//! unrecoverable.
//!
//! CRUD only — the `fauna.backup.nest_key.{grant,revoke}` + `fauna.backup.status`
//! handlers and their per-class gate are the nest handler slice
//! (`backup_handlers.rs`); this module never interprets segment or key bytes
//! beyond the length check.

use anyhow::{Result, anyhow};
use rusqlite::OptionalExtension;

use super::{CacheDb, now_epoch_secs};

/// A `NestBackupKey` is exactly 32 bytes (`fauna_core::crypto::NestBackupKey`).
/// The store refuses any other length so a malformed grant can never rest in
/// the table and later feed the coordinator a wrong-sized seal key.
pub const NEST_BACKUP_KEY_LEN: usize = 32;

impl CacheDb {
    /// Store (grant) an owner's `NestBackupKey`. `INSERT OR REPLACE` keyed on
    /// `owner_actor_id`, so re-granting (e.g. after an identity succession
    /// re-derives the key under the successor seed) is an idempotent replace,
    /// not a duplicate row. `granted_at` is refreshed on each grant.
    pub async fn put_nest_backup_key(
        &self,
        owner_actor_id: &[u8],
        backup_key: &[u8],
    ) -> Result<()> {
        if backup_key.len() != NEST_BACKUP_KEY_LEN {
            return Err(anyhow!(
                "nest backup key must be {NEST_BACKUP_KEY_LEN} bytes, got {}",
                backup_key.len()
            ));
        }
        let owner_actor_id = owner_actor_id.to_vec();
        let backup_key = backup_key.to_vec();
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO nest_backup_keys
                (owner_actor_id, backup_key, granted_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![owner_actor_id, backup_key, now],
        )
        .map_err(|e| anyhow!("put nest backup key: {e}"))?;
        Ok(())
    }

    /// Read one owner's granted `NestBackupKey`, or `None` if the owner has not
    /// granted one (not enrolled). The coordinator calls this to build the
    /// owner's `OwnerSealKey::SourceNest`; the status handler calls it to report
    /// the enrolled flag.
    pub async fn get_nest_backup_key(&self, owner_actor_id: &[u8]) -> Result<Option<Vec<u8>>> {
        let owner_actor_id = owner_actor_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT backup_key FROM nest_backup_keys WHERE owner_actor_id = ?1",
            rusqlite::params![owner_actor_id],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(|e| anyhow!("get nest backup key: {e}"))
    }

    /// Revoke an owner's grant: delete the row (the freeze-the-backup affordance
    /// surfaced in the nests-page trust facet). Returns whether a row existed —
    /// idempotent, so a double revoke is not an error.
    pub async fn delete_nest_backup_key(&self, owner_actor_id: &[u8]) -> Result<bool> {
        let owner_actor_id = owner_actor_id.to_vec();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM nest_backup_keys WHERE owner_actor_id = ?1",
                rusqlite::params![owner_actor_id],
            )
            .map_err(|e| anyhow!("delete nest backup key: {e}"))?;
        Ok(n > 0)
    }

    /// Every owner actor that has granted a `NestBackupKey`, for the in-process
    /// coordinator's per-user scheduling enumeration (slice 3 joins this against
    /// configured destinations to decide which owners to back up). Oldest grant
    /// first, deterministic.
    pub async fn list_nest_backup_key_owners(&self) -> Result<Vec<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT owner_actor_id FROM nest_backup_keys ORDER BY granted_at, owner_actor_id",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> [u8; NEST_BACKUP_KEY_LEN] {
        [b; NEST_BACKUP_KEY_LEN]
    }

    #[tokio::test]
    async fn round_trip_and_revoke() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let k = key(0xAB);

        assert!(db.get_nest_backup_key(&owner).await.unwrap().is_none());
        db.put_nest_backup_key(&owner, &k).await.unwrap();
        assert_eq!(
            db.get_nest_backup_key(&owner).await.unwrap().as_deref(),
            Some(&k[..])
        );
        assert_eq!(
            db.list_nest_backup_key_owners().await.unwrap(),
            vec![owner.to_vec()]
        );
        // revoke → gone, and a second revoke is a no-op (idempotent).
        assert!(db.delete_nest_backup_key(&owner).await.unwrap());
        assert!(!db.delete_nest_backup_key(&owner).await.unwrap());
        assert!(db.get_nest_backup_key(&owner).await.unwrap().is_none());
        assert!(db.list_nest_backup_key_owners().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn regrant_replaces_key_not_appends() {
        // A re-grant (e.g. identity succession re-derives the key) replaces the
        // row rather than duplicating it — one key per owner.
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [1u8; 32];
        db.put_nest_backup_key(&owner, &key(0x01)).await.unwrap();
        db.put_nest_backup_key(&owner, &key(0x02)).await.unwrap();
        assert_eq!(
            db.get_nest_backup_key(&owner).await.unwrap().as_deref(),
            Some(&key(0x02)[..])
        );
        assert_eq!(db.list_nest_backup_key_owners().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn keys_are_per_owner_isolated() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner_a = [0xAAu8; 32];
        let owner_b = [0xBBu8; 32];
        db.put_nest_backup_key(&owner_a, &key(0x0A)).await.unwrap();
        db.put_nest_backup_key(&owner_b, &key(0x0B)).await.unwrap();
        assert_eq!(
            db.get_nest_backup_key(&owner_a).await.unwrap().as_deref(),
            Some(&key(0x0A)[..])
        );
        assert_eq!(
            db.get_nest_backup_key(&owner_b).await.unwrap().as_deref(),
            Some(&key(0x0B)[..])
        );
        // revoking one leaves the other intact.
        db.delete_nest_backup_key(&owner_a).await.unwrap();
        assert!(db.get_nest_backup_key(&owner_a).await.unwrap().is_none());
        assert_eq!(
            db.get_nest_backup_key(&owner_b).await.unwrap().as_deref(),
            Some(&key(0x0B)[..])
        );
    }

    #[tokio::test]
    async fn put_rejects_wrong_length_key() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [1u8; 32];
        let err = db
            .put_nest_backup_key(&owner, &[0u8; 31])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("32 bytes"));
        // nothing stored on rejection
        assert!(db.get_nest_backup_key(&owner).await.unwrap().is_none());
    }
}
