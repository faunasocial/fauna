//! The RecoveryKey **registration chain** store (identity-succession slice 2;
//! `docs/goal/behavior/identity-succession.md` § The RecoveryKey).
//!
//! One row per link in an identity's chain (table defined in
//! `migrations::MIGRATIONS_RECOVERY_REGISTRATIONS`). The chain is what lets a
//! consumer answer *"which RecoveryKey is current for this actor?"* — the
//! binding a succession statement is later validated against
//! (`identity-succession.md:56`).
//!
//! **This module never verifies a signature and never mints a row from
//! nest-side material.** Verification lives in
//! `fauna_core::recovery::SignedRecoveryKeyRegistration::verify`, driven by the
//! handler; the store's whole job is to hold the head, refuse a non-advancing
//! `seq`, and replay bytes verbatim. That split is what keeps the nest
//! *enforcer and distributor, never authorizer* (`identity-succession.md:104`)
//! honest at the storage layer too: there is no code path here that could
//! fabricate a registration.
//!
//! `seq` is one monotonic sequence **per identity, shared with succession
//! statements** — not a per-table counter.

use anyhow::{Result, anyhow};
use rusqlite::OptionalExtension;

use super::{CacheDb, now_epoch_secs};

/// A RecoveryKey public half is exactly 32 bytes (Ed25519). The store refuses
/// any other length so a malformed row can never rest as a binding a later
/// succession would be validated against.
pub const RECOVERY_PUBKEY_LEN: usize = 32;

/// One link in an identity's registration chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryRegistrationRow {
    /// Monotonic per identity; shared with succession `seq`.
    pub seq: u64,
    /// The Ed25519 public half of the RecoveryKey registered at this link.
    pub recovery_pubkey: Vec<u8>,
    /// The verbatim canonical DAG-CBOR bytes the client submitted — replayed
    /// byte-for-byte by the chain-serve kind, never re-encoded.
    pub record: Vec<u8>,
    /// Unix seconds the nest accepted the registration.
    pub created_at: i64,
}

impl CacheDb {
    /// The current head of an identity's chain, or `None` if it has registered
    /// no RecoveryKey.
    ///
    /// This is the lookup the submit handler gates a replacement on: the head's
    /// `recovery_pubkey` is the `prior` that a replacement's
    /// `prior_recovery_sig` must verify under, and its `seq` is the value a
    /// replacement must advance past.
    pub async fn recovery_registration_head(
        &self,
        actor_id: &[u8],
    ) -> Result<Option<RecoveryRegistrationRow>> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT seq, recovery_pubkey, record, created_at FROM recovery_registrations
             WHERE actor_id = ?1
             ORDER BY seq DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![actor_id], |row| {
            Ok(RecoveryRegistrationRow {
                seq: row.get::<_, i64>(0)? as u64,
                recovery_pubkey: row.get(1)?,
                record: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Append a verified registration to an identity's chain.
    ///
    /// Refuses a `seq` that does not advance past the current head — a plain
    /// `INSERT` on the `(actor_id, seq)` primary key would already refuse an
    /// exact replay, but an explicit check also refuses a *lower* seq and
    /// yields an honest error instead of a constraint violation. The handler
    /// has already verified the record; this is the storage-layer twin of that
    /// gate, so a future caller that forgets to verify still cannot rewrite
    /// history.
    ///
    /// A link that **changes the registered pubkey** also deletes the
    /// identity's `recovery_escrow` row, in the same transaction: the blob is
    /// sealed to the retiring key's X25519 half, so after the change no current
    /// kit can open it — serving it would silently brick the new kit, and
    /// keeping it leaves seed ciphertext resting under a retired key. Deleting
    /// here, rather than at the call sites, covers both replacement arms (the
    /// RecoveryKey-authorized submit and the landed seed-alone window) and any
    /// future arm structurally (`identity-succession.md` § Seed escrow →
    /// *Lifecycle on the nest*). A same-pubkey seq advance keeps the row — the
    /// sealing key did not change.
    pub async fn append_recovery_registration(
        &self,
        actor_id: &[u8],
        seq: u64,
        recovery_pubkey: &[u8],
        record: &[u8],
    ) -> Result<()> {
        if recovery_pubkey.len() != RECOVERY_PUBKEY_LEN {
            return Err(anyhow!(
                "recovery pubkey must be {RECOVERY_PUBKEY_LEN} bytes, got {}",
                recovery_pubkey.len()
            ));
        }
        if record.is_empty() {
            return Err(anyhow!("registration record must not be empty"));
        }
        let actor_id_v = actor_id.to_vec();
        let recovery_pubkey = recovery_pubkey.to_vec();
        let record = record.to_vec();
        let now = now_epoch_secs();
        let conn = self.conn.lock().await;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| anyhow!("begin registration tx: {e}"))?;

        let head: Option<(i64, Vec<u8>)> = tx
            .query_row(
                "SELECT seq, recovery_pubkey FROM recovery_registrations
                 WHERE actor_id = ?1
                 ORDER BY seq DESC
                 LIMIT 1",
                rusqlite::params![actor_id_v],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| anyhow!("read recovery chain head: {e}"))?;
        if let Some((head_seq, _)) = &head
            && seq <= *head_seq as u64
        {
            return Err(anyhow!(
                "registration seq {seq} does not advance the chain head {head_seq}"
            ));
        }

        tx.execute(
            "INSERT INTO recovery_registrations
                (actor_id, seq, recovery_pubkey, record, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![actor_id_v, seq as i64, recovery_pubkey, record, now],
        )
        .map_err(|e| anyhow!("append recovery registration: {e}"))?;

        if let Some((_, head_pubkey)) = &head
            && head_pubkey.as_slice() != recovery_pubkey.as_slice()
        {
            tx.execute(
                "DELETE FROM recovery_escrow WHERE actor_id = ?1",
                rusqlite::params![actor_id_v],
            )
            .map_err(|e| anyhow!("invalidate seed escrow on key change: {e}"))?;
        }

        tx.commit()
            .map_err(|e| anyhow!("commit registration append: {e}"))?;
        Ok(())
    }

    /// The identity's whole chain, **oldest first** — the wire order
    /// `fauna.recovery.registration.chain` promises, so a consumer walks it
    /// forward to the head.
    pub async fn list_recovery_registrations(
        &self,
        actor_id: &[u8],
    ) -> Result<Vec<RecoveryRegistrationRow>> {
        let actor_id = actor_id.to_vec();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT seq, recovery_pubkey, record, created_at FROM recovery_registrations
             WHERE actor_id = ?1
             ORDER BY seq",
        )?;
        let rows = stmt.query_map(rusqlite::params![actor_id], |row| {
            Ok(RecoveryRegistrationRow {
                seq: row.get::<_, i64>(0)? as u64,
                recovery_pubkey: row.get(1)?,
                record: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTOR_A: [u8; 32] = [0xA1; 32];
    const ACTOR_B: [u8; 32] = [0xB2; 32];
    const RK_1: [u8; 32] = [0x11; 32];
    const RK_2: [u8; 32] = [0x22; 32];

    #[tokio::test]
    async fn an_unregistered_identity_has_no_head_and_an_empty_chain() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(
            db.recovery_registration_head(&ACTOR_A)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            db.list_recovery_registrations(&ACTOR_A)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn append_then_head_and_chain_round_trip() {
        let db = CacheDb::open_in_memory().unwrap();
        db.append_recovery_registration(&ACTOR_A, 1, &RK_1, b"record-one")
            .await
            .unwrap();

        let head = db.recovery_registration_head(&ACTOR_A).await.unwrap();
        let head = head.expect("head after first registration");
        assert_eq!(head.seq, 1);
        assert_eq!(head.recovery_pubkey, RK_1.to_vec());
        // The record is replayed verbatim — this is the property the serve path
        // depends on.
        assert_eq!(head.record, b"record-one".to_vec());

        let chain = db.list_recovery_registrations(&ACTOR_A).await.unwrap();
        assert_eq!(chain.len(), 1);
        assert_eq!(chain[0].record, b"record-one".to_vec());
    }

    #[tokio::test]
    async fn a_replacement_advances_the_head_and_the_chain_keeps_both_links() {
        let db = CacheDb::open_in_memory().unwrap();
        db.append_recovery_registration(&ACTOR_A, 1, &RK_1, b"one")
            .await
            .unwrap();
        db.append_recovery_registration(&ACTOR_A, 2, &RK_2, b"two")
            .await
            .unwrap();

        let head = db
            .recovery_registration_head(&ACTOR_A)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(head.seq, 2);
        assert_eq!(head.recovery_pubkey, RK_2.to_vec());

        // History is kept, oldest first: a consumer that last saw seq 1 must be
        // able to walk forward rather than be told only about the head.
        let chain = db.list_recovery_registrations(&ACTOR_A).await.unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(chain[0].seq, 1);
        assert_eq!(chain[1].seq, 2);
    }

    #[tokio::test]
    async fn a_non_advancing_seq_is_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        db.append_recovery_registration(&ACTOR_A, 5, &RK_1, b"five")
            .await
            .unwrap();

        // An exact replay of the head.
        assert!(
            db.append_recovery_registration(&ACTOR_A, 5, &RK_2, b"replay")
                .await
                .is_err()
        );
        // A rewind below the head — the arm a bare PRIMARY KEY would let
        // through, and the one that would let a stale record become the head.
        assert!(
            db.append_recovery_registration(&ACTOR_A, 4, &RK_2, b"rewind")
                .await
                .is_err()
        );

        // The head is untouched by either refusal.
        let head = db
            .recovery_registration_head(&ACTOR_A)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(head.seq, 5);
        assert_eq!(head.recovery_pubkey, RK_1.to_vec());
    }

    #[tokio::test]
    async fn chains_are_per_identity() {
        let db = CacheDb::open_in_memory().unwrap();
        db.append_recovery_registration(&ACTOR_A, 1, &RK_1, b"a")
            .await
            .unwrap();
        // ACTOR_B starting its own chain at 1 is not a replay — the seq space
        // is per identity.
        db.append_recovery_registration(&ACTOR_B, 1, &RK_2, b"b")
            .await
            .unwrap();

        assert_eq!(
            db.recovery_registration_head(&ACTOR_A)
                .await
                .unwrap()
                .unwrap()
                .record,
            b"a".to_vec()
        );
        assert_eq!(
            db.recovery_registration_head(&ACTOR_B)
                .await
                .unwrap()
                .unwrap()
                .record,
            b"b".to_vec()
        );
    }

    #[tokio::test]
    async fn a_pubkey_changing_append_deletes_the_escrow_row() {
        // The blob is sealed to the retiring key — after the change no current
        // kit can open it, so it must not rest or be served
        // (`identity-succession.md` § Seed escrow → *Lifecycle on the nest*).
        let db = CacheDb::open_in_memory().unwrap();
        db.append_recovery_registration(&ACTOR_A, 1, &RK_1, b"one")
            .await
            .unwrap();
        db.put_recovery_escrow(&ACTOR_A, b"sealed-to-rk1")
            .await
            .unwrap();

        db.append_recovery_registration(&ACTOR_A, 2, &RK_2, b"two")
            .await
            .unwrap();

        assert!(
            db.get_recovery_escrow(&ACTOR_A).await.unwrap().is_none(),
            "a replacement must invalidate the escrow blob sealed to the retired key"
        );
    }

    #[tokio::test]
    async fn a_same_pubkey_advance_keeps_the_escrow_row() {
        // A seq advance under the same key changes no sealing key — the blob
        // still opens, so deleting it would destroy working loss protection.
        let db = CacheDb::open_in_memory().unwrap();
        db.append_recovery_registration(&ACTOR_A, 1, &RK_1, b"one")
            .await
            .unwrap();
        db.put_recovery_escrow(&ACTOR_A, b"sealed-to-rk1")
            .await
            .unwrap();

        db.append_recovery_registration(&ACTOR_A, 2, &RK_1, b"re-registration")
            .await
            .unwrap();

        assert_eq!(
            db.get_recovery_escrow(&ACTOR_A)
                .await
                .unwrap()
                .expect("the escrow row survives a same-key advance")
                .blob,
            b"sealed-to-rk1".to_vec()
        );
    }

    #[tokio::test]
    async fn a_refused_append_leaves_the_escrow_row_alone() {
        // The invalidation rides the append's transaction — a refused append
        // (non-advancing seq) must therefore change nothing.
        let db = CacheDb::open_in_memory().unwrap();
        db.append_recovery_registration(&ACTOR_A, 5, &RK_1, b"five")
            .await
            .unwrap();
        db.put_recovery_escrow(&ACTOR_A, b"sealed-to-rk1")
            .await
            .unwrap();

        assert!(
            db.append_recovery_registration(&ACTOR_A, 5, &RK_2, b"replay")
                .await
                .is_err()
        );

        assert!(
            db.get_recovery_escrow(&ACTOR_A).await.unwrap().is_some(),
            "a refused append must not invalidate the escrow"
        );
    }

    #[tokio::test]
    async fn a_malformed_pubkey_or_empty_record_is_refused() {
        let db = CacheDb::open_in_memory().unwrap();
        assert!(
            db.append_recovery_registration(&ACTOR_A, 1, &[0x11; 31], b"short-key")
                .await
                .is_err()
        );
        assert!(
            db.append_recovery_registration(&ACTOR_A, 1, &RK_1, b"")
                .await
                .is_err()
        );
        assert!(
            db.recovery_registration_head(&ACTOR_A)
                .await
                .unwrap()
                .is_none()
        );
    }
}
