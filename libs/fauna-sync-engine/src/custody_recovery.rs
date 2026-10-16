//! The custody leg's **recovery-material** half — the custodian copy of the
//! owner's seed escrow (`docs/goal/behavior/identity-succession.md` § Seed
//! escrow → *The custodian copy*, rulings 1, 2 and 5).
//!
//! A kit-only user whose every device AND home nest are lost needs the blob
//! their kit opens, and the home nest was its only holder. So the custodian
//! that already holds the owner's planes also keeps the owner's **recovery
//! material** in its pair store: the RecoveryKey registration chain and any
//! succession record (read through the public pre-identity kinds every caller
//! may read) and the seed-escrow blob with its write stamp (read through the
//! one custodian-class kind, [`ESCROW_PULL_KIND`]). The custodian then answers
//! the owner's ordinary phrase restore from that copy (the nest's
//! `escrow_fetch_handler`, custodied arm).
//!
//! Two halves, so the lifecycle is one pure function every form shares:
//!
//! - [`next_custodied_recovery`] — **the copy follows the chain, never a bare
//!   answer** (ruling 2): a newer stamp replaces the blob; the chain's head
//!   advancing past the head the blob was pulled under, or a succession
//!   record appearing, deletes it; `no_escrow`, `not_registered` (an empty
//!   chain), a chain that does not extend the custodied one, a refused
//!   handshake or an unreachable nest delete nothing. A rebuilt or lost owner
//!   nest is a replica behind, not a retirement — and is the one case the
//!   copy exists for.
//! - [`pull_recovery_material`] — the three reads over one custody session,
//!   folded through the function above and persisted whole
//!   ([`AccountStore::put_custodied_recovery`]).
//!
//! The nest's custody-hosting pump drives the pull; a device custodian may
//! adopt it later over its own session — nothing here is nest-specific.
//!
//! **Order is load-bearing: the chain is read BEFORE the blob.** The owner's
//! nest deletes its blob in the same transaction that advances the chain
//! (`identity-succession.md` § Seed escrow → *Lifecycle on the nest*), so a
//! blob read after the chain is sealed to that chain's head or to a newer one
//! — never to an older one. Recording it under the head read first can at
//! worst under-state its head, which deletes it one pass early and re-pulls it
//! the next; the reverse order could keep a retired kit's blob under a head it
//! was never sealed to.
//!
//! The material is **outside the relay plane** (ruling 5): never metered,
//! never evicted by the budget pass, never served on the sync doors.

use anyhow::{Context, Result};
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_account_store::types::{CustodiedEscrow, CustodiedRecovery};
use fauna_core::custody_grant::{CustodyScopeSet, verified_custody_grant};
use fauna_core::encoding::{EmbedAsBytes, canonical_decode};
use fauna_core::identity::ActorId;
use fauna_core::recovery::{
    ChainHead, SignedIdentitySuccession, SignedRecoveryKeyRegistration, verify_registration_chain,
    verify_succession_against_chain,
};
use fauna_protocol::recovery::{
    ESCROW_PULL_KIND, EscrowPullReply, EscrowPullRequest, RegistrationChainReply,
    RegistrationChainRequest, SuccessionLookupReply, SuccessionLookupRequest,
};
use fauna_protocol::{ByteBuf, RpcErrorClass, RpcRequester};

/// The pre-identity chain read (public by construction).
const REGISTRATION_CHAIN_KIND: &str = "fauna.recovery.registration.chain";
/// The pre-identity succession read (public by construction).
const SUCCESSION_LOOKUP_KIND: &str = "fauna.recovery.succession.lookup";
/// The owner's nest's honest "no blob rests" answer to [`ESCROW_PULL_KIND`].
const NO_ESCROW_CODE: &str = "fauna.recovery.no_escrow";

/// Does this custody witness cover the owner's recovery material?
///
/// Only the `Account` form (ruling 1): the recovery material is of the
/// account, never of a scope, so an explicit scope list — however wide —
/// does not reach it, and the owner's nest refuses the pull for it. An
/// unreadable witness covers nothing.
pub fn recovery_material_covered(witness: &EmbedAsBytes) -> bool {
    verified_custody_grant(witness)
        .is_ok_and(|grant| matches!(grant.scopes, CustodyScopeSet::Account))
}

/// What one read of the owner's escrow answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscrowPulled {
    /// A blob rests, with the owner's nest's write stamp.
    Blob { blob: Vec<u8>, updated_at: i64 },
    /// The owner's nest says no blob rests — never grounds to delete the copy.
    NoEscrow,
    /// Refused, unreachable or unreadable — never grounds to delete either.
    Failed,
}

/// The head a custodied chain establishes for `owner`, verified link by link
/// ([`verify_registration_chain`]) — `None` for an empty or unverifiable
/// chain, which authorizes nothing. The serve side's head comes from here too,
/// so the pump and the door cannot read one chain two ways.
pub fn custodied_chain_head(owner: ActorId, material: &CustodiedRecovery) -> Option<ChainHead> {
    let chain = decode_chain(&material.chain)?;
    verify_registration_chain(owner, &chain, None).ok()
}

/// The identity that succeeded `owner`, when the custodied material records a
/// succession its custodied chain authorizes — the serve side's `superseded`
/// answer. `None` when no statement is held or the first does not verify.
pub fn custodied_successor(owner: ActorId, material: &CustodiedRecovery) -> Option<ActorId> {
    let chain = decode_chain(&material.chain)?;
    verified_first_succession(owner, &chain, &material.successions)
}

/// **The lifecycle (ruling 2)** — the custodied material after one pass's
/// reads, from what was held and what the owner's nest answered. A read that
/// failed is `None` (chain, successions) or [`EscrowPulled::Failed`] (blob);
/// none of them deletes anything.
///
/// - **Chain:** the pulled chain replaces the held one only when it verifies
///   for `owner` AND visits the held head — so it extends what was custodied,
///   never rewrites or truncates it. An empty or shorter chain keeps the held
///   one.
/// - **Successions:** once held, kept; first adopted when the owner's first
///   statement verifies against the (new) custodied chain.
/// - **Blob:** gone once a succession is held, or once the custodied head
///   advanced past the head it was pulled under; replaced by a pulled blob
///   with a newer stamp, recorded under the current head. Never adopted
///   without a chain to verify a fetch against.
pub fn next_custodied_recovery(
    owner: ActorId,
    held: &CustodiedRecovery,
    pulled_chain: Option<Vec<Vec<u8>>>,
    pulled_successions: Option<Vec<Vec<u8>>>,
    escrow: EscrowPulled,
) -> CustodiedRecovery {
    let held_head = custodied_chain_head(owner, held);

    let mut chain = held.chain.clone();
    if let Some(pulled) = pulled_chain
        && let Some(decoded) = decode_chain(&pulled)
        && verify_registration_chain(owner, &decoded, held_head.as_ref()).is_ok()
    {
        chain = pulled;
    }
    let decoded = decode_chain(&chain);
    let head = decoded
        .as_ref()
        .and_then(|c| verify_registration_chain(owner, c, None).ok());

    let mut successions = held.successions.clone();
    if successions.is_empty()
        && let (Some(pulled), Some(decoded)) = (pulled_successions, decoded.as_ref())
        && verified_first_succession(owner, decoded, &pulled).is_some()
    {
        successions = pulled;
    }

    let escrow = if !successions.is_empty() {
        // The kit the blob is sealed to retired with the old identity.
        None
    } else if let Some(head) = head {
        let kept = held.escrow.clone().filter(|held| held.head_seq >= head.seq);
        match escrow {
            EscrowPulled::Blob { blob, updated_at }
                if kept.as_ref().is_none_or(|k| updated_at > k.updated_at) =>
            {
                Some(CustodiedEscrow {
                    blob,
                    updated_at,
                    head_seq: head.seq,
                })
            }
            _ => kept,
        }
    } else {
        // No verifiable chain: nothing could ever open a fetch against it, so
        // nothing new is adopted — and a held blob stays with the held chain
        // it was kept under (an unverifiable *held* chain is a corrupt row,
        // never a retirement).
        held.escrow.clone()
    };

    CustodiedRecovery {
        chain,
        successions,
        escrow,
    }
}

/// What one [`pull_recovery_material`] pass did — latency-independent state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryMaterialTally {
    /// The material changed and was written back.
    pub written: bool,
    /// A blob is custodied after the pass.
    pub escrow_held: bool,
    /// A read of the three failed (refused, unreachable, unreadable).
    pub failed_reads: usize,
}

/// **The pull** (ruling 1): read the owner's chain, then its succession
/// record, then its blob, over `nest` (the custody session — the two public
/// reads ride it as any authenticated connection may), fold them through
/// [`next_custodied_recovery`] and persist the answer whole when it changed.
/// A failed read is tallied, never an error: the pass's other legs proceed,
/// and the copy stays as it was.
pub async fn pull_recovery_material<B, R>(
    store: &AccountStore<B>,
    nest: &R,
    owner: &[u8; 32],
) -> Result<RecoveryMaterialTally>
where
    B: StoreBackend,
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let mut tally = RecoveryMaterialTally::default();
    let actor = ByteBuf::from(owner.to_vec());

    // Chain FIRST — see the module doc.
    let chain: Option<Vec<Vec<u8>>> = nest
        .request::<_, RegistrationChainReply>(
            REGISTRATION_CHAIN_KIND,
            RegistrationChainRequest {
                actor_id: actor.clone(),
                ..Default::default()
            },
        )
        .await
        .ok()
        .map(|reply| {
            reply
                .registrations
                .into_iter()
                .map(|b| b.into_vec())
                .collect()
        });
    let successions: Option<Vec<Vec<u8>>> = nest
        .request::<_, SuccessionLookupReply>(
            SUCCESSION_LOOKUP_KIND,
            SuccessionLookupRequest {
                actor_id: actor.clone(),
                ..Default::default()
            },
        )
        .await
        .ok()
        .map(|reply| reply.statements.into_iter().map(|b| b.into_vec()).collect());
    let escrow = match nest
        .request::<_, EscrowPullReply>(
            ESCROW_PULL_KIND,
            EscrowPullRequest {
                owner_actor_id: actor,
                ..Default::default()
            },
        )
        .await
    {
        Ok(reply) => EscrowPulled::Blob {
            blob: reply.blob.into_vec(),
            updated_at: reply.updated_at,
        },
        Err(e) if e.as_rpc_error().is_some_and(|r| r.code == NO_ESCROW_CODE) => {
            EscrowPulled::NoEscrow
        }
        Err(_) => EscrowPulled::Failed,
    };
    tally.failed_reads = usize::from(chain.is_none())
        + usize::from(successions.is_none())
        + usize::from(escrow == EscrowPulled::Failed);

    let held = store
        .custodied_recovery()
        .await
        .context("read the custodied recovery material")?;
    let next = next_custodied_recovery(ActorId(*owner), &held, chain, successions, escrow);
    tally.escrow_held = next.escrow.is_some();
    if next != held {
        store
            .put_custodied_recovery(&next)
            .await
            .context("write the custodied recovery material")?;
        tally.written = true;
    }
    Ok(tally)
}

/// Decode a verbatim chain; `None` when empty or any link is unreadable.
fn decode_chain(chain: &[Vec<u8>]) -> Option<Vec<SignedRecoveryKeyRegistration>> {
    if chain.is_empty() {
        return None;
    }
    chain.iter().map(|b| canonical_decode(b).ok()).collect()
}

/// The successor the FIRST statement names, when it is `owner`'s and verifies
/// against `chain` ([`verify_succession_against_chain`]). Later statements are
/// further hops, each of its own identity's chain — held verbatim, judged by
/// whoever follows them.
fn verified_first_succession(
    owner: ActorId,
    chain: &[SignedRecoveryKeyRegistration],
    statements: &[Vec<u8>],
) -> Option<ActorId> {
    let first: SignedIdentitySuccession = canonical_decode(statements.first()?).ok()?;
    if first.statement.old_actor_id != owner {
        return None;
    }
    verify_succession_against_chain(&first, chain, None)
        .ok()
        .map(|v| v.new_actor_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use fauna_core::data::Timestamp;
    use fauna_core::encoding::canonical_encode;
    use fauna_core::recovery::{IdentitySuccession, RecoveryKey, RecoveryKeyRegistration};

    fn identity() -> SigningKey {
        SigningKey::from_bytes(&[0x51; 32])
    }

    fn owner() -> ActorId {
        ActorId(identity().verifying_key().to_bytes())
    }

    fn kit(n: u8) -> RecoveryKey {
        RecoveryKey::from_bytes([n; 32])
    }

    /// A chain of `len` links, each RecoveryKey-authorized by the previous kit.
    fn chain(len: u8) -> Vec<Vec<u8>> {
        (1..=len)
            .map(|seq| {
                let reg = RecoveryKeyRegistration {
                    actor_id: owner(),
                    recovery_pubkey: kit(seq).public(),
                    seq: u64::from(seq),
                    created_at: Timestamp(1),
                };
                let prior = (seq > 1).then(|| kit(seq - 1));
                let signed = reg.sign(&identity(), &kit(seq), prior.as_ref()).unwrap();
                canonical_encode(&signed).unwrap().to_vec()
            })
            .collect()
    }

    /// A succession of `owner` authorized by kit `seq` (the head of `chain(seq)`).
    fn succession(seq: u8) -> Vec<Vec<u8>> {
        let next = SigningKey::from_bytes(&[0x52; 32]);
        let statement = IdentitySuccession {
            old_actor_id: owner(),
            new_actor_id: ActorId(next.verifying_key().to_bytes()),
            recovery_pubkey: kit(seq).public(),
            seq: u64::from(seq) + 1,
            created_at: Timestamp(2),
        };
        let signed = statement.sign(&kit(seq), &next, None).unwrap();
        vec![canonical_encode(&signed).unwrap().to_vec()]
    }

    fn blob(updated_at: i64) -> EscrowPulled {
        EscrowPulled::Blob {
            blob: vec![updated_at as u8; 8],
            updated_at,
        }
    }

    fn held(chain_len: u8, updated_at: i64, head_seq: u64) -> CustodiedRecovery {
        CustodiedRecovery {
            chain: chain(chain_len),
            successions: Vec::new(),
            escrow: Some(CustodiedEscrow {
                blob: vec![updated_at as u8; 8],
                updated_at,
                head_seq,
            }),
        }
    }

    #[test]
    fn a_first_pass_custodies_the_chain_and_the_blob_under_its_head() {
        let next = next_custodied_recovery(
            owner(),
            &CustodiedRecovery::default(),
            Some(chain(2)),
            Some(Vec::new()),
            blob(10),
        );
        assert_eq!(next, held(2, 10, 2));
        assert_eq!(
            custodied_chain_head(owner(), &next).map(|h| h.recovery_pubkey),
            Some(kit(2).public())
        );
    }

    #[test]
    fn a_newer_stamp_replaces_and_an_older_or_equal_one_does_not() {
        let at = held(1, 10, 1);
        let newer = next_custodied_recovery(owner(), &at, Some(chain(1)), None, blob(11));
        assert_eq!(newer, held(1, 11, 1));
        for stale in [9, 10] {
            let kept = next_custodied_recovery(owner(), &at, Some(chain(1)), None, blob(stale));
            assert_eq!(kept, at, "a stamp of {stale} must not replace 10");
        }
    }

    #[test]
    fn nothing_but_the_chain_or_a_succession_deletes_the_blob() {
        let at = held(2, 10, 2);
        // `no_escrow`, a failed read, `not_registered` (an empty chain), a
        // shorter chain, an unreachable nest (every read failed): each keeps it.
        for (chain_read, escrow) in [
            (Some(chain(2)), EscrowPulled::NoEscrow),
            (Some(chain(2)), EscrowPulled::Failed),
            (Some(Vec::new()), EscrowPulled::NoEscrow),
            (Some(chain(1)), EscrowPulled::NoEscrow),
            (None, EscrowPulled::Failed),
        ] {
            let next = next_custodied_recovery(owner(), &at, chain_read, None, escrow);
            assert_eq!(next, at, "the copy must survive a bare answer");
        }
    }

    #[test]
    fn a_rewritten_chain_is_refused_and_keeps_what_was_custodied() {
        let at = held(2, 10, 2);
        // A chain that does not visit the custodied head (a different kit at
        // seq 2) is a rewrite, never an extension.
        let mut forged = chain(1);
        let reg = RecoveryKeyRegistration {
            actor_id: owner(),
            recovery_pubkey: kit(9).public(),
            seq: 2,
            created_at: Timestamp(1),
        };
        let signed = reg.sign(&identity(), &kit(9), Some(&kit(1))).unwrap();
        forged.push(canonical_encode(&signed).unwrap().to_vec());
        let next = next_custodied_recovery(owner(), &at, Some(forged), None, blob(99));
        assert_eq!(next.chain, at.chain, "the custodied chain stands");
        assert_eq!(
            next.escrow.as_ref().map(|e| e.updated_at),
            Some(99),
            "a newer blob under the custodied head is still adopted"
        );
    }

    #[test]
    fn a_replaced_kit_retires_the_blob_and_a_re_put_is_adopted_under_the_new_head() {
        let at = held(1, 10, 1);
        // The owner replaced the kit; its nest deleted the blob in the same
        // transaction, so the pass reads the new chain and `no_escrow`.
        let gone =
            next_custodied_recovery(owner(), &at, Some(chain(2)), None, EscrowPulled::NoEscrow);
        assert_eq!(gone.chain, chain(2));
        assert_eq!(gone.escrow, None, "the retired kit's blob is deleted");
        // A later pass, after the re-put.
        let back = next_custodied_recovery(owner(), &gone, Some(chain(2)), None, blob(20));
        assert_eq!(back, held(2, 20, 2));
    }

    #[test]
    fn a_succession_deletes_the_blob_and_names_the_successor() {
        let at = held(1, 10, 1);
        let next =
            next_custodied_recovery(owner(), &at, Some(chain(1)), Some(succession(1)), blob(30));
        assert_eq!(next.escrow, None);
        assert_eq!(next.successions, succession(1));
        assert_eq!(
            custodied_successor(owner(), &next),
            Some(ActorId(
                SigningKey::from_bytes(&[0x52; 32])
                    .verifying_key()
                    .to_bytes()
            ))
        );
        // An empty lookup on a later pass does not un-succeed the owner.
        let later =
            next_custodied_recovery(owner(), &next, Some(chain(1)), Some(Vec::new()), blob(31));
        assert_eq!(later, next);
    }

    #[test]
    fn an_unverifiable_succession_is_not_adopted() {
        let at = held(2, 10, 2);
        // Signed by kit 1, which the chain's head (kit 2) retired.
        let next = next_custodied_recovery(
            owner(),
            &at,
            Some(chain(2)),
            Some(succession(1)),
            EscrowPulled::NoEscrow,
        );
        assert_eq!(next, at);
        assert_eq!(custodied_successor(owner(), &next), None);
    }

    #[test]
    fn no_blob_is_adopted_without_a_chain() {
        let next = next_custodied_recovery(
            owner(),
            &CustodiedRecovery::default(),
            Some(Vec::new()),
            Some(Vec::new()),
            blob(10),
        );
        assert_eq!(next, CustodiedRecovery::default());
    }
}
