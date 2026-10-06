//! The append-only deployment-seed rotation log, and the **one transaction** the
//! ceremony commits.
//!
//! Owner: `docs/goal/architecture/nest/box-recovery.md` § Deployment-seed
//! rotation → *The ceremony*. This module owns steps 1–4 of that section's
//! numbered transaction; everything outside the transaction (minting the seed,
//! custodying it, rewriting `nest_deployment.key`, re-publishing DNS) is the
//! caller's.

use anyhow::{Context, Result, bail};
use ed25519_dalek::SigningKey;
use fauna_protocol::nest_rotation::{NestRotation, SignedNestRotation};
use rusqlite::{Connection, OptionalExtension};
use zeroize::Zeroizing;

use super::{CacheDb, now_epoch_secs};

/// Why a rotation was refused. Each variant is a *different* refusal the caller
/// must render differently — collapsing them into one error is how an idempotent
/// retry ends up looking like a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationRefusal {
    /// The successor seed derives to the identity the box already serves. An
    /// **idempotent ack**, not a failure: the client mints its seed before
    /// dispatch, so a retry after a lost reply lands here by construction.
    AlreadyRotated,
    /// The successor seed derives to an identity this box has already
    /// superseded. A revoked identity never returns.
    SupersededAncestor,
    /// An admin-remove pending action is in flight. Rotating now would hand the
    /// successor seed to the party being evicted, via the self-healing capture
    /// (`box-recovery.md` § Ordering rule — rotate only into a clean roster).
    PendingAdminRemove,
}

impl core::fmt::Display for RotationRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::AlreadyRotated => write!(f, "this seed is already the box's current identity"),
            Self::SupersededAncestor => {
                write!(
                    f,
                    "this seed is a superseded identity; a revoked identity never returns"
                )
            }
            Self::PendingAdminRemove => write!(
                f,
                "an admin removal is still pending — it must take effect before rotating, or the \
                 successor seed reaches the admin being removed"
            ),
        }
    }
}

/// The outcome of a committed rotation.
#[derive(Debug)]
pub struct RotationOutcome {
    /// The statement appended to the log, ready to serve on the chain.
    pub statement: SignedNestRotation,
    /// How many satellite ciphertexts were re-keyed (audit detail).
    pub satellites_rekeyed: usize,
}

/// Read the full ordered rotation chain (seq 1..head).
///
/// Served verbatim by `fauna.auth.rotation_chain`. Rotations are
/// deployment-rare, so the chain stays trivially small and needs no paging.
pub fn read_chain_tx(conn: &Connection) -> Result<Vec<SignedNestRotation>> {
    let mut stmt = conn
        .prepare("SELECT statement FROM nest_rotation_log ORDER BY seq ASC")
        .context("prepare rotation chain read")?;
    let rows = stmt
        .query_map([], |r| r.get::<_, Vec<u8>>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("read rotation chain")?;
    rows.iter()
        .map(|bytes| {
            fauna_cbor::decode_strict::<SignedNestRotation>(bytes)
                .context("decode a stored rotation statement")
        })
        .collect()
}

/// Every identity this box has superseded, oldest first.
///
/// Both the rotate handler (to refuse a return to a revoked identity) and the
/// boot reconcile (to recognise a stale on-disk key the DB already moved past)
/// read this.
pub fn superseded_identities_tx(conn: &Connection) -> Result<Vec<[u8; 32]>> {
    let mut stmt = conn
        .prepare("SELECT old_actor_id FROM nest_rotation_log ORDER BY seq ASC")
        .context("prepare superseded read")?;
    let rows = stmt
        .query_map([], |r| r.get::<_, Vec<u8>>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("read superseded identities")?;
    Ok(rows
        .into_iter()
        .filter_map(|v| <[u8; 32]>::try_from(v.as_slice()).ok())
        .collect())
}

impl CacheDb {
    /// The full ordered rotation chain — the `fauna.auth.rotation_chain` reply.
    pub async fn nest_rotation_chain(&self) -> Result<Vec<SignedNestRotation>> {
        let conn = self.conn.lock().await;
        read_chain_tx(&conn)
    }

    /// Every deployment identity this box has superseded.
    pub async fn superseded_nest_identities(&self) -> Result<Vec<[u8; 32]>> {
        let conn = self.conn.lock().await;
        superseded_identities_tx(&conn)
    }

    /// Commit a deployment-seed rotation — **the atomic decision point**
    /// (`box-recovery.md` § The ceremony).
    ///
    /// In one transaction: verify the successor is fresh, append the signed
    /// statement at `head + 1`, swap the `nest_keypair` row, and re-encrypt every
    /// nest-internal KEK satellite under the successor-derived keys. The box
    /// holds both roots for exactly the span of this call and never again — which
    /// is why the satellites cannot be deferred to a later pass.
    ///
    /// The caller must have durably custodied `new_seed` **before** calling
    /// (the CR-1 discipline): a tear anywhere here rolls back to the old identity
    /// with at worst an orphan custody entry off-box, never a live identity
    /// nobody holds.
    ///
    /// `Ok(Err(refusal))` is a *rule* refusal the caller renders to the admin;
    /// `Err(_)` is an internal failure.
    pub async fn rotate_deployment_seed(
        &self,
        old_seed: &Zeroizing<[u8; 32]>,
        new_seed: &Zeroizing<[u8; 32]>,
    ) -> Result<std::result::Result<RotationOutcome, RotationRefusal>> {
        let old_key = SigningKey::from_bytes(old_seed);
        let new_key = SigningKey::from_bytes(new_seed);
        let old_id = old_key.verifying_key().to_bytes();
        let new_id = new_key.verifying_key().to_bytes();
        let now = now_epoch_secs();

        let conn = self.conn.lock().await;
        let tx = conn.unchecked_transaction().context("begin rotation tx")?;

        // ── 1. Verify ────────────────────────────────────────────────────────
        if new_id == old_id {
            return Ok(Err(RotationRefusal::AlreadyRotated));
        }
        if superseded_identities_tx(&tx)?.contains(&new_id) {
            return Ok(Err(RotationRefusal::SupersededAncestor));
        }
        if admin_remove_in_flight_tx(&tx)? {
            return Ok(Err(RotationRefusal::PendingAdminRemove));
        }
        // The seed the caller believes is current must actually be current, or
        // two concurrent rotations could both append from the same head.
        let current: Option<Vec<u8>> = tx
            .query_row(
                "SELECT public_key FROM nest_keypair WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .optional()
            .context("read current nest identity")?;
        match current {
            Some(pk) if pk.as_slice() == old_id.as_slice() => {}
            Some(_) => bail!(
                "the deployment identity changed under this rotation — refusing to append a \
                 statement that would not link"
            ),
            None => bail!("no nest_keypair row to rotate"),
        }

        // ── 2. Append the statement ─────────────────────────────────────────
        let head_seq: Option<i64> = tx
            .query_row("SELECT MAX(seq) FROM nest_rotation_log", [], |r| r.get(0))
            .optional()
            .context("read rotation head")?
            .flatten();
        let seq = head_seq.unwrap_or(0) + 1;
        let statement = NestRotation {
            old_nest_actor_id: old_id,
            new_nest_actor_id: new_id,
            seq: seq as u64,
            rotated_at: now,
        }
        .sign(&old_key, &new_key)
        .map_err(|e| anyhow::anyhow!("sign rotation statement: {e}"))?;
        let encoded =
            fauna_cbor::encode_canonical(&statement).context("encode rotation statement")?;
        tx.execute(
            "INSERT INTO nest_rotation_log (seq, old_actor_id, new_actor_id, statement, rotated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![seq, old_id.as_slice(), new_id.as_slice(), encoded, now],
        )
        .context("append rotation statement")?;

        // ── 3. Swap the keypair ─────────────────────────────────────────────
        tx.execute(
            "INSERT OR REPLACE INTO nest_keypair (id, secret_key, public_key, created_at)
             VALUES (1, ?1, ?2, ?3)",
            rusqlite::params![new_seed.as_slice(), new_id.as_slice(), now],
        )
        .context("swap nest keypair")?;

        // ── 4. Re-key the nest-internal KEK satellites ──────────────────────
        // Inside the same transaction, deliberately: this is the only instant
        // the box holds both roots. A failure here rolls back the swap too.
        let satellites_rekeyed = crate::nest_kek::reencrypt_satellites(&tx, old_seed, new_seed)
            .context("re-encrypt nest-internal KEK satellites")?;

        tx.commit().context("commit rotation")?;
        Ok(Ok(RotationOutcome {
            statement,
            satellites_rekeyed,
        }))
    }
}

/// Is an admin-removal pending action still in flight?
///
/// "In flight" means `status = 'pending'` — a scheduled removal that has not yet
/// executed or been cancelled. `admin.md` § Admin continuity makes the removal a
/// delayed action precisely so it can be reviewed; rotating during that window
/// would hand the successor seed to the admin on their way out.
fn admin_remove_in_flight_tx(conn: &Connection) -> Result<bool> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pending_actions
             WHERE action_type = 'admin.remove' AND status = 'pending'",
            [],
            |r| r.get(0),
        )
        .context("count pending admin removals")?;
    Ok(n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::nest_rotation::verify_chain;

    fn seed(b: u8) -> Zeroizing<[u8; 32]> {
        Zeroizing::new([b; 32])
    }

    fn id(s: &Zeroizing<[u8; 32]>) -> [u8; 32] {
        SigningKey::from_bytes(s).verifying_key().to_bytes()
    }

    async fn db_at(seed_bytes: &Zeroizing<[u8; 32]>) -> (tempfile::TempDir, CacheDb) {
        let dir = tempfile::TempDir::new().unwrap();
        let db = CacheDb::open(dir.path().join("nest.db")).unwrap();
        db.set_nest_keypair(seed_bytes.as_slice(), id(seed_bytes).as_slice())
            .await
            .unwrap();
        (dir, db)
    }

    /// The whole ceremony, end to end: the box serves the successor, and a client
    /// pinned to the predecessor can walk the chain to it.
    #[tokio::test]
    async fn a_rotation_swaps_the_identity_and_leaves_a_walkable_chain() {
        let (a, b) = (seed(1), seed(2));
        let (_dir, db) = db_at(&a).await;

        let out = db.rotate_deployment_seed(&a, &b).await.unwrap().unwrap();
        assert_eq!(out.statement.statement.seq, 1);

        let (secret, public) = db.get_nest_keypair().await.unwrap().unwrap();
        assert_eq!(secret.as_slice(), b.as_slice(), "the box now holds B");
        assert_eq!(public.as_slice(), id(&b).as_slice());

        let chain = db.nest_rotation_chain().await.unwrap();
        assert_eq!(verify_chain(&chain, &id(&a), &id(&b)).unwrap(), 1);
    }

    /// Two rotations must chain, and a client pinned at the original must reach
    /// the head in one walk — the property the whole log exists for.
    #[tokio::test]
    async fn two_rotations_chain_and_an_old_pin_still_converges() {
        let (a, b, c) = (seed(1), seed(2), seed(3));
        let (_dir, db) = db_at(&a).await;
        db.rotate_deployment_seed(&a, &b).await.unwrap().unwrap();
        db.rotate_deployment_seed(&b, &c).await.unwrap().unwrap();

        let chain = db.nest_rotation_chain().await.unwrap();
        assert_eq!(chain.len(), 2);
        assert_eq!(verify_chain(&chain, &id(&a), &id(&c)).unwrap(), 2);
    }

    /// The client mints its seed before dispatch, so a retry after a lost reply
    /// re-sends the *same* seed. That must be an idempotent ack, never an error
    /// — otherwise the safe retry looks like a failed rotation.
    #[tokio::test]
    async fn re_dispatching_the_committed_seed_is_an_idempotent_ack() {
        let (a, b) = (seed(1), seed(2));
        let (_dir, db) = db_at(&a).await;
        db.rotate_deployment_seed(&a, &b).await.unwrap().unwrap();

        assert_eq!(
            db.rotate_deployment_seed(&b, &b)
                .await
                .unwrap()
                .unwrap_err(),
            RotationRefusal::AlreadyRotated
        );
        assert_eq!(
            db.nest_rotation_chain().await.unwrap().len(),
            1,
            "an idempotent ack must not append a second statement"
        );
    }

    /// A revoked identity never returns: rotating *back* to a superseded seed
    /// would resurrect exactly the key the ceremony evicted.
    #[tokio::test]
    async fn rotating_back_to_a_superseded_identity_is_refused() {
        let (a, b) = (seed(1), seed(2));
        let (_dir, db) = db_at(&a).await;
        db.rotate_deployment_seed(&a, &b).await.unwrap().unwrap();

        assert_eq!(
            db.rotate_deployment_seed(&b, &a)
                .await
                .unwrap()
                .unwrap_err(),
            RotationRefusal::SupersededAncestor
        );
        let (_, public) = db.get_nest_keypair().await.unwrap().unwrap();
        assert_eq!(public.as_slice(), id(&b).as_slice(), "still on B");
    }

    /// The ordering rule: rotating while an admin removal is in flight hands the
    /// successor seed to the party being evicted, so it achieves nothing.
    #[tokio::test]
    async fn a_pending_admin_removal_blocks_the_rotation() {
        let (a, b) = (seed(1), seed(2));
        let (_dir, db) = db_at(&a).await;
        db.create_pending_action(
            &crate::pending_actions::ActionType::AdminRemove,
            &[9u8; 32],
            Some("target"),
            None,
            None,
        )
        .await
        .unwrap();

        assert_eq!(
            db.rotate_deployment_seed(&a, &b)
                .await
                .unwrap()
                .unwrap_err(),
            RotationRefusal::PendingAdminRemove
        );
        assert!(
            db.nest_rotation_chain().await.unwrap().is_empty(),
            "a refused rotation appends nothing"
        );
        let (_, public) = db.get_nest_keypair().await.unwrap().unwrap();
        assert_eq!(public.as_slice(), id(&a).as_slice(), "identity untouched");
    }

    /// A rotation dispatched against a stale view of the current identity must
    /// not append a statement that does not link.
    #[tokio::test]
    async fn a_rotation_from_a_non_current_identity_is_rejected() {
        let (a, b, c) = (seed(1), seed(2), seed(3));
        let (_dir, db) = db_at(&a).await;
        db.rotate_deployment_seed(&a, &b).await.unwrap().unwrap();
        // A second caller still believes A is current.
        assert!(db.rotate_deployment_seed(&a, &c).await.is_err());
        assert_eq!(db.nest_rotation_chain().await.unwrap().len(), 1);
    }
}
