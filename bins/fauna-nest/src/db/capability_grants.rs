//! User-minted capability-grant storage (capability-mediated content
//! processing, design spec § Phase 2 Step 2 § 2.4).
//!
//! One row per `(owner_actor_id, grant_id)` in `capability_grants` (table
//! defined in `migrations::MIGRATIONS_CAPABILITY_GRANTS`). The `blob` is a
//! canonical-dag-cbor `fauna-mls::wrapped_blob::GrantBlob`, HPKE-sealed to the
//! `holder_pubkey` — **the nest stores opaque ciphertext it cannot open**
//! (`encryption-at-rest.md` § nest holds no content key;
//! `key-material-hierarchy.md` rule #4). The `holder_pubkey` and `epoch_end`
//! columns are the *only* fields the nest reads without opening the blob: the
//! former scopes `fauna.capabilities.fetch` to one holder, the latter is the
//! expiry filter (the honest-box "lasts-until" bound — § Phase 2 Step 1).
//!
//! CRUD only — the `fauna.capabilities.*` handlers, gate, and blob parsing
//! (extracting `holder`/`grant_id`/`epoch_end` from the `GrantBlob`) are the
//! nest handler slice; this module never interprets blob bytes.

use anyhow::{Context, Result, anyhow};
use rusqlite::OptionalExtension;

use super::{CacheDb, now_epoch_secs};

/// Maximum bytes for a stored capability-grant blob. A grant carries one
/// `WrappedScopeKey` per key-bearing scope tuple × epoch. Sized from the
/// bounded mail grant, the only kind whose wrap count follows the calendar
/// (`encryption-at-rest.md` § Capability tiering → *Content-sealing epochs*,
/// the retention ruling): its window slides and is never wider than twice the
/// mint-time length — at most 27 weekly epochs for the ~90-day standard grant
/// — and one wrap is ~2.5 KB to a classical holder or ~3.6 KB to a
/// post-quantum one, plus 2432 bytes per extra MSEK generation at a
/// rotation-boundary epoch. That is ~100 KB in the ordinary case and ~150 KB
/// with several rotations inside the window; 256 KiB leaves headroom without
/// loosening the storage bound (`MAX_GRANTS_PER_OWNER` × this = 64 MiB per
/// owner, the same order as before). The pre-ruling 64 KiB was reached after
/// a few months of weekly renewals once nothing pruned the old wraps.
pub const MAX_CAPABILITY_GRANT_BYTES: usize = 256 * 1024;

/// Maximum number of distinct grants (rows) one owner may hold — **defined on
/// the wire plane** ([`fauna_protocol::wrapped_blob::MAX_GRANTS_PER_OWNER`]),
/// because the reconcile sweep's client half bounds a hostile reply by the same
/// number and two definitions could drift. Re-exported here so every nest-side
/// call site keeps resolving `capability_grants::MAX_GRANTS_PER_OWNER`; the
/// storage rationale (and the follow-on note that tightening the *aggregate*
/// fetch across owners would need fetch pagination — a wire change) lives on
/// the definition.
pub use fauna_protocol::wrapped_blob::MAX_GRANTS_PER_OWNER;

/// Typed quota rejection so the mint handler can map it to a client-actionable
/// error class instead of the generic `fauna.protocol.internal` — a client at
/// its grant quota must be able to tell "revoke something first" from a server
/// bug it would blindly retry.
#[derive(Debug, thiserror::Error)]
#[error("owner has too many capability grants: {count} (max {max})")]
pub struct GrantQuotaExceeded {
    pub count: i64,
    pub max: usize,
}

impl CacheDb {
    /// Store (mint) a capability grant. `INSERT OR REPLACE` keyed on
    /// `(owner_actor_id, grant_id)`, so re-minting the same grant id replaces
    /// it (also the storage primitive a renew rides — the handler rebuilds the
    /// blob with the appended keys and re-stores here). `epoch_end` and
    /// `holder_pubkey` are extracted from the blob by the handler and passed in
    /// so the nest can filter/scope without opening the blob.
    pub async fn put_capability_grant(
        &self,
        owner_actor_id: &[u8],
        grant_id: &[u8],
        holder_pubkey: &[u8],
        epoch_end: i64,
        blob: &[u8],
    ) -> Result<()> {
        if blob.len() > MAX_CAPABILITY_GRANT_BYTES {
            return Err(anyhow!(
                "capability grant blob too large: {} bytes (max {})",
                blob.len(),
                MAX_CAPABILITY_GRANT_BYTES
            ));
        }
        let owner_actor_id = owner_actor_id.to_vec();
        let grant_id = grant_id.to_vec();
        let holder_pubkey = holder_pubkey.to_vec();
        let blob = blob.to_vec();
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        // Per-owner grant quota. Enforced
        // under the connection lock so the count-then-insert is atomic (no
        // TOCTOU). A *replace* of an existing `(owner, grant_id)` — a re-mint /
        // the renew storage primitive — is not a new row, so it is allowed even
        // at the cap; only a brand-new `grant_id` past `MAX_GRANTS_PER_OWNER` is
        // rejected.
        let is_replace = conn
            .query_row(
                "SELECT 1 FROM capability_grants
                  WHERE owner_actor_id = ?1 AND grant_id = ?2",
                rusqlite::params![owner_actor_id, grant_id],
                |_| Ok(()),
            )
            .optional()
            .context("check capability grant existence")?
            .is_some();
        if !is_replace {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM capability_grants WHERE owner_actor_id = ?1",
                    rusqlite::params![owner_actor_id],
                    |row| row.get(0),
                )
                .context("count owner capability grants")?;
            if count as usize >= MAX_GRANTS_PER_OWNER {
                return Err(GrantQuotaExceeded {
                    count,
                    max: MAX_GRANTS_PER_OWNER,
                }
                .into());
            }
        }
        conn.execute(
            "INSERT OR REPLACE INTO capability_grants
                (owner_actor_id, grant_id, holder_pubkey, blob, epoch_end, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                owner_actor_id,
                grant_id,
                holder_pubkey,
                blob,
                epoch_end,
                now
            ],
        )
        .context("put capability grant")?;
        Ok(())
    }

    /// Fetch one grant blob by its `(owner_actor_id, grant_id)` key — the
    /// read half of a renew's read-modify-write. Returns `None` if absent.
    pub async fn get_capability_grant(
        &self,
        owner_actor_id: &[u8],
        grant_id: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        let owner_actor_id = owner_actor_id.to_vec();
        let grant_id = grant_id.to_vec();
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT blob FROM capability_grants
              WHERE owner_actor_id = ?1 AND grant_id = ?2",
            rusqlite::params![owner_actor_id, grant_id],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .context("get capability grant")
    }

    /// Serve `fauna.capabilities.fetch`: every non-expired grant sealed to
    /// `holder_pubkey`, oldest-first. **Expired grants (`epoch_end < now`) are
    /// omitted** — the honest-box expiry mechanism (design § 2.3/§ 2.5);
    /// revoked grants are already gone (revoke deletes the row). `now_epoch` is
    /// passed in (not read here) so callers and tests control the clock.
    pub async fn fetch_capability_grants_for_holder(
        &self,
        holder_pubkey: &[u8],
        now_epoch: i64,
    ) -> Result<Vec<Vec<u8>>> {
        let holder_pubkey = holder_pubkey.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT blob FROM capability_grants
              WHERE holder_pubkey = ?1 AND epoch_end >= ?2
              ORDER BY created_at, grant_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![holder_pubkey, now_epoch], |row| {
            row.get::<_, Vec<u8>>(0)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// [`Self::fetch_capability_grants_for_holder`] narrowed to one OWNER — a
    /// third-party principal's `fauna.capabilities.fetch`. A principal is one
    /// account's, and two accounts' principals for the same app may attest the
    /// same key, so a session opened for one account reads only the grants
    /// that account minted.
    pub async fn fetch_capability_grants_for_holder_owned_by(
        &self,
        owner_actor_id: &[u8; 32],
        holder_pubkey: &[u8],
        now_epoch: i64,
    ) -> Result<Vec<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT blob FROM capability_grants
              WHERE owner_actor_id = ?1 AND holder_pubkey = ?2 AND epoch_end >= ?3
              ORDER BY created_at, grant_id",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![&owner_actor_id[..], holder_pubkey, now_epoch],
            |row| row.get::<_, Vec<u8>>(0),
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every live (non-expired) grant blob whose holder is an **approved,
    /// enrolled** bridge service user of this box — the sufficiency scan the
    /// task-delegation lease runner uses to decide which `(owner, kind)`
    /// leases this nest should heartbeat (participants.md § Dispatch by kind:
    /// holding the grant IS the assignment). The join keeps a grant minted to
    /// a since-revoked or never-approved holder from counting as "this box can
    /// run the kind".
    pub async fn fetch_live_enrolled_capability_grant_blobs(
        &self,
        now_epoch: i64,
    ) -> Result<Vec<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT cg.blob FROM capability_grants cg
              JOIN bridge_service_users bsu ON bsu.x25519_pubkey = cg.holder_pubkey
             WHERE cg.epoch_end >= ?1 AND bsu.status = 'approved'
             ORDER BY cg.created_at, cg.grant_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![now_epoch], |row| row.get::<_, Vec<u8>>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Serve `fauna.capabilities.reconcile`: every `grant_id` this owner
    /// holds on this nest — ALL rows, expired included (`ui/nests.md` §
    /// Trust facet — grants → *Reconcile*, ratified 2026-08-15). Reconcile is
    /// about which ROWS exist, not which are live, so this deliberately skips
    /// `fetch_capability_grants_for_holder`'s expiry filter and returns
    /// `grant_id` only, never the blob. Bounded to `MAX_GRANTS_PER_OWNER` by
    /// construction — `put_capability_grant` already caps one owner's row
    /// count at mint time.
    pub async fn fetch_capability_grant_ids_for_owner(
        &self,
        owner_actor_id: &[u8],
    ) -> Result<Vec<Vec<u8>>> {
        let owner_actor_id = owner_actor_id.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT grant_id FROM capability_grants
              WHERE owner_actor_id = ?1
              ORDER BY created_at, grant_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![owner_actor_id], |row| {
            row.get::<_, Vec<u8>>(0)
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Revoke a grant: delete the `(owner_actor_id, grant_id)` row so the
    /// holder's next fetch omits it (honest-box revocation). Returns whether a
    /// row existed.
    pub async fn delete_capability_grant(
        &self,
        owner_actor_id: &[u8],
        grant_id: &[u8],
    ) -> Result<bool> {
        let owner_actor_id = owner_actor_id.to_vec();
        let grant_id = grant_id.to_vec();
        let conn = self.conn.lock().await;
        let n = conn
            .execute(
                "DELETE FROM capability_grants
                  WHERE owner_actor_id = ?1 AND grant_id = ?2",
                rusqlite::params![owner_actor_id, grant_id],
            )
            .context("delete capability grant")?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Far-future / far-past epoch_end sentinels for the expiry filter tests.
    const NEVER: i64 = i64::MAX;
    const NOW: i64 = 1_800_000_000;

    #[tokio::test]
    async fn round_trip_capability_grant() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let grant = [0x22u8; 16];
        let holder = [0x33u8; 32];
        let blob = vec![0xABu8; 256];
        db.put_capability_grant(&owner, &grant, &holder, NEVER, &blob)
            .await
            .unwrap();

        // get by PK
        let got = db.get_capability_grant(&owner, &grant).await.unwrap();
        assert_eq!(got.as_deref(), Some(&blob[..]));
        // fetch by holder
        let for_holder = db
            .fetch_capability_grants_for_holder(&holder, NOW)
            .await
            .unwrap();
        assert_eq!(for_holder, vec![blob.clone()]);
        // revoke → gone from both reads
        assert!(db.delete_capability_grant(&owner, &grant).await.unwrap());
        assert!(!db.delete_capability_grant(&owner, &grant).await.unwrap());
        assert!(
            db.get_capability_grant(&owner, &grant)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db.fetch_capability_grants_for_holder(&holder, NOW)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn fetch_is_holder_scoped() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [1u8; 32];
        let holder_a = [0xAAu8; 32];
        let holder_b = [0xBBu8; 32];
        db.put_capability_grant(&owner, &[1u8; 16], &holder_a, NEVER, b"grant-a")
            .await
            .unwrap();
        db.put_capability_grant(&owner, &[2u8; 16], &holder_b, NEVER, b"grant-b")
            .await
            .unwrap();

        let a = db
            .fetch_capability_grants_for_holder(&holder_a, NOW)
            .await
            .unwrap();
        assert_eq!(a, vec![b"grant-a".to_vec()]);
        let b = db
            .fetch_capability_grants_for_holder(&holder_b, NOW)
            .await
            .unwrap();
        assert_eq!(b, vec![b"grant-b".to_vec()]);
    }

    #[tokio::test]
    async fn fetch_omits_expired() {
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [1u8; 32];
        let holder = [0xCCu8; 32];
        // live grant (epoch_end == now → still valid, boundary inclusive)
        db.put_capability_grant(&owner, &[1u8; 16], &holder, NOW, b"live")
            .await
            .unwrap();
        // expired grant (epoch_end < now)
        db.put_capability_grant(&owner, &[2u8; 16], &holder, NOW - 1, b"expired")
            .await
            .unwrap();

        let served = db
            .fetch_capability_grants_for_holder(&holder, NOW)
            .await
            .unwrap();
        assert_eq!(served, vec![b"live".to_vec()]);
        // get-by-PK still returns the expired row (renew can revive it).
        assert_eq!(
            db.get_capability_grant(&owner, &[2u8; 16])
                .await
                .unwrap()
                .as_deref(),
            Some(&b"expired"[..])
        );
    }

    #[tokio::test]
    async fn reconcile_ids_include_expired_and_are_owner_scoped() {
        // Reconcile is about which ROWS exist, not which are live — unlike
        // `fetch_capability_grants_for_holder`, an expired row's id is still
        // returned. And it is owner-scoped, not holder-scoped: two owners'
        // grants to the SAME holder never bleed into each other's list.
        let db = CacheDb::open_in_memory().unwrap();
        let owner_a = [1u8; 32];
        let owner_b = [2u8; 32];
        let holder = [0xEEu8; 32];
        db.put_capability_grant(&owner_a, &[1u8; 16], &holder, NOW, b"live")
            .await
            .unwrap();
        db.put_capability_grant(&owner_a, &[2u8; 16], &holder, NOW - 1, b"expired")
            .await
            .unwrap();
        db.put_capability_grant(&owner_b, &[3u8; 16], &holder, NEVER, b"other-owner")
            .await
            .unwrap();

        let mut ids = db
            .fetch_capability_grant_ids_for_owner(&owner_a)
            .await
            .unwrap();
        ids.sort();
        assert_eq!(ids, vec![vec![1u8; 16], vec![2u8; 16]]);
    }

    #[tokio::test]
    async fn put_replaces_on_same_pk() {
        // Re-minting the same (owner, grant_id) replaces the blob + epoch_end
        // (mint idempotency / the renew storage primitive).
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [1u8; 32];
        let grant = [7u8; 16];
        let holder = [0xDDu8; 32];
        db.put_capability_grant(&owner, &grant, &holder, NOW - 100, b"old")
            .await
            .unwrap();
        db.put_capability_grant(&owner, &grant, &holder, NEVER, b"new")
            .await
            .unwrap();
        assert_eq!(
            db.get_capability_grant(&owner, &grant)
                .await
                .unwrap()
                .as_deref(),
            Some(&b"new"[..])
        );
        // the refreshed epoch_end makes it live again
        assert_eq!(
            db.fetch_capability_grants_for_holder(&holder, NOW)
                .await
                .unwrap(),
            vec![b"new".to_vec()]
        );
    }

    #[tokio::test]
    async fn put_rejects_oversize_blob() {
        let db = CacheDb::open_in_memory().unwrap();
        let too_big = vec![0u8; MAX_CAPABILITY_GRANT_BYTES + 1];
        let err = db
            .put_capability_grant(&[1u8; 32], &[1u8; 16], &[1u8; 32], NEVER, &too_big)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too large"));
    }

    // grant_id #n as a 16-byte key (little-endian counter, zero-padded).
    fn grant_id_n(n: usize) -> [u8; 16] {
        let mut id = [0u8; 16];
        id[..8].copy_from_slice(&(n as u64).to_le_bytes());
        id
    }

    #[tokio::test]
    async fn put_rejects_over_owner_quota() {
        // A single owner may mint up to MAX_GRANTS_PER_OWNER distinct grants;
        // the next NEW grant_id is rejected (cross-user storage/fetch-bloat DoS
        // bound). The bound is per-owner, so
        // one abusive owner can't bloat the shared co-resident holder's fetch.
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let holder = [0x33u8; 32];
        for n in 0..MAX_GRANTS_PER_OWNER {
            db.put_capability_grant(&owner, &grant_id_n(n), &holder, NEVER, b"g")
                .await
                .unwrap();
        }
        let err = db
            .put_capability_grant(
                &owner,
                &grant_id_n(MAX_GRANTS_PER_OWNER),
                &holder,
                NEVER,
                b"g",
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too many"));
    }

    #[tokio::test]
    async fn put_over_quota_still_allows_replace_and_other_owners() {
        // At the quota, re-minting an EXISTING grant_id must still succeed (it is
        // a replace / the renew storage primitive, not a new grant), and a
        // DIFFERENT owner is unaffected (the quota is per-owner).
        let db = CacheDb::open_in_memory().unwrap();
        let owner = [0x11u8; 32];
        let holder = [0x33u8; 32];
        for n in 0..MAX_GRANTS_PER_OWNER {
            db.put_capability_grant(&owner, &grant_id_n(n), &holder, NEVER, b"g")
                .await
                .unwrap();
        }
        // replace an existing grant_id (renew) — allowed at the cap.
        db.put_capability_grant(&owner, &grant_id_n(0), &holder, NEVER, b"renewed")
            .await
            .expect("replacing an existing grant at the quota must succeed");
        assert_eq!(
            db.get_capability_grant(&owner, &grant_id_n(0))
                .await
                .unwrap()
                .as_deref(),
            Some(&b"renewed"[..])
        );
        // a different owner still gets their own full quota.
        let owner2 = [0x22u8; 32];
        db.put_capability_grant(&owner2, &grant_id_n(0), &holder, NEVER, b"g")
            .await
            .expect("a different owner's quota is independent");
    }
}
