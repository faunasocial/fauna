//! **The chain follows the link** — reconciling an identity's RecoveryKey
//! registration chain between two nests that both hold an account for it
//! (`docs/goal/behavior/identity-succession.md` § Enforcement on the home nest
//! → *Every nest the identity is linked to*).
//!
//! A nest applies a succession only against the registration chain it holds
//! itself, so a linked nest that holds none can verify nothing — and a seed
//! thief can register a RecoveryKey of their own there first. The records name
//! no nest and are signed end to end, so the client carries them: it reads
//! `fauna.recovery.registration.chain` at both nests, compares the verbatim
//! record lists, and submits what the shorter one lacks at that nest, oldest
//! first, through the ordinary `fauna.recovery.registration.submit` — which
//! verifies each link against the chain that nest already holds. Nothing here
//! verifies a signature: the receiving nest is the verifier, and a record it
//! refuses is an error the caller reports.
//!
//! Two callers, one body: the Nests page's both-ends link runs it before it
//! writes a pairing row (`fauna-client-pair`), and every full pass of a
//! seed-holding runtime runs it against each linked nest it reaches
//! (`fauna-account-plane`'s secondary leg).
//!
//! **Why this crate.** The ceremonies' home is `fauna-client-recovery`, which
//! re-exports this module as `chain_reconcile`. It cannot hold the body: it
//! sits above both callers in the dependency graph (its aftermath reaches
//! `fauna-client-mail-settings`, which reaches `fauna-client-pair`).

use fauna_core::encoding::canonical_decode;
use fauna_core::recovery::SignedRecoveryKeyRegistration;
use fauna_protocol::recovery as wire;
use fauna_protocol::{ByteBuf, RpcRequester};

/// `fauna.recovery.registration.chain` — pre-identity, so it is callable on
/// the authenticated connections the reconcile runs over.
pub const REGISTRATION_CHAIN_KIND: &str = "fauna.recovery.registration.chain";
/// `fauna.recovery.registration.submit` — USER class: the account is the
/// connection's actor.
pub const REGISTRATION_SUBMIT_KIND: &str = "fauna.recovery.registration.submit";
/// `fauna.recovery.replacement.request` — USER class: parks a seed-alone
/// replacement for its own window at the nest it is sent to.
pub const REPLACEMENT_REQUEST_KIND: &str = "fauna.recovery.replacement.request";
/// `fauna.recovery.replacement.status` — USER class: the caller's own pending
/// replacement at the nest it is sent to.
pub const REPLACEMENT_STATUS_KIND: &str = "fauna.recovery.replacement.status";

/// Which of the two nests an outcome or an error names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainSide {
    /// The nest the runtime is bound to (for the link action: the connected
    /// nest).
    Bound,
    /// The linked nest (for the link action: the nest being linked).
    Linked,
}

impl core::fmt::Display for ChainSide {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Bound => "the bound nest",
            Self::Linked => "the linked nest",
        })
    }
}

/// What a reconcile found, and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainReconcile {
    /// Both nests hold the same chain (none at either is the same chain).
    InStep,
    /// `side` held a prefix of the other's chain and now holds all of it:
    /// `records` were submitted there.
    Extended { side: ChainSide, records: usize },
    /// Neither chain is a prefix of the other: the two nests hold different
    /// recovery keys for this identity. Nothing was submitted.
    Forked,
    /// `side` is behind and the next link it lacks is a **seed-alone** link —
    /// one a nest lands itself after an uncontested window, which carries no
    /// prior-key signature and so cannot be replayed through the strict
    /// submit. The `submitted` records before it landed; the rest stays owed
    /// (a seed-alone replacement is requested at every linked nest, each
    /// running its own window). `requested`: the seed-alone link itself was
    /// sent there as a replacement request (clause (c)), so that nest runs
    /// its own window for it — a replayed identical request keeps the clock
    /// the first one started. A door that cannot request
    /// ([`RegistrationChainDoor::request_replacement`]'s default) leaves it
    /// `false`.
    Owed {
        side: ChainSide,
        submitted: usize,
        requested: bool,
    },
}

/// What the two record lists call for, before anything is sent
/// ([`plan_chain_reconcile`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainPlan {
    InStep,
    Forked,
    /// `side` lacks `records` (verbatim, oldest first). `owed`: the link after
    /// the last of them is seed-alone, so `side` stays behind once they land.
    Extend {
        side: ChainSide,
        records: Vec<Vec<u8>>,
        /// The seed-alone link the replay stopped at, verbatim — what the
        /// lagging side is asked to run its own window for.
        owed: Option<Vec<u8>>,
    },
}

/// Why a reconcile could not finish. Diagnostic text, never shown as a page's
/// own wording: the one outcome a user must read — [`ChainReconcile::Forked`]
/// — is an outcome, not an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainReconcileError {
    /// The chain could not be read at `side`.
    Read { side: ChainSide, message: String },
    /// `side` refused, or never answered, a record's submit. Records before it
    /// may have landed; the next reconcile resumes from what each nest holds.
    Submit { side: ChainSide, message: String },
    /// A record the longer chain serves does not decode as a registration.
    Malformed(String),
}

impl core::fmt::Display for ChainReconcileError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Read { side, message } => {
                write!(f, "reading the recovery-key chain at {side}: {message}")
            }
            Self::Submit { side, message } => {
                write!(f, "carrying a recovery-key record to {side}: {message}")
            }
            Self::Malformed(message) => write!(f, "a recovery-key record is malformed: {message}"),
        }
    }
}

impl std::error::Error for ChainReconcileError {}

/// One nest's two registration doors, as the reconcile uses them. Implemented
/// for every [`RpcRequester`] through [`RpcChainDoor`]; a caller whose
/// connection is a seam object rather than a requester (`fauna-client-pair`'s
/// `LinkedNestsNest`) implements it over that seam.
#[allow(async_fn_in_trait)] // static dispatch, per-impl `Send` — as `RpcRequester`
pub trait RegistrationChainDoor {
    /// The connection's own account's chain as this nest serves it: the
    /// verbatim records, oldest first; empty when it holds none.
    async fn registration_chain(&self) -> Result<Vec<Vec<u8>>, String>;
    /// Submit one verbatim record for the connection's own account.
    async fn submit_registration(&self, record: &[u8]) -> Result<(), String>;
    /// Send a seed-alone link as a replacement request, so this nest parks it
    /// for its own window. `Ok(false)`: this door cannot request (the
    /// default) — the link stays owed, and nothing was sent.
    async fn request_replacement(&self, record: &[u8]) -> Result<bool, String> {
        let _ = record;
        Ok(false)
    }
}

/// [`RegistrationChainDoor`] over a connection's [`RpcRequester`], for the
/// account that connection is authenticated as.
pub struct RpcChainDoor<'a, R> {
    pub rpc: &'a R,
    /// The account both connections of a reconcile are authenticated as.
    pub actor_id: [u8; 32],
}

impl<R: RpcRequester> RegistrationChainDoor for RpcChainDoor<'_, R> {
    async fn registration_chain(&self) -> Result<Vec<Vec<u8>>, String> {
        fetch_registration_chain(self.rpc, &self.actor_id)
            .await
            .map_err(|e| e.to_string())
    }

    async fn submit_registration(&self, record: &[u8]) -> Result<(), String> {
        submit_registration_record(self.rpc, record)
            .await
            .map_err(|e| e.to_string())
    }

    async fn request_replacement(&self, record: &[u8]) -> Result<bool, String> {
        request_replacement_record(self.rpc, record)
            .await
            .map(|_| true)
            .map_err(|e| e.to_string())
    }
}

/// `fauna.recovery.replacement.request` with a verbatim seed-alone record —
/// the record names no nest, so the same bytes open a window at any nest the
/// identity holds an account on. Answers when that window lands.
pub async fn request_replacement_record<R: RpcRequester>(
    rpc: &R,
    record: &[u8],
) -> Result<i64, R::Error> {
    let reply: wire::ReplacementRequestReply = rpc
        .request(
            REPLACEMENT_REQUEST_KIND,
            wire::ReplacementRequestRequest {
                registration: ByteBuf::from(record.to_vec()),
                ..Default::default()
            },
        )
        .await?;
    Ok(reply.lands_at)
}

/// `fauna.recovery.replacement.status` — the connection's own account's
/// pending replacement at that nest, if any.
pub async fn fetch_replacement_status<R: RpcRequester>(
    rpc: &R,
) -> Result<Option<wire::ReplacementPendingInfo>, R::Error> {
    let reply: wire::ReplacementStatusReply = rpc
        .request(
            REPLACEMENT_STATUS_KIND,
            wire::ReplacementStatusRequest::default(),
        )
        .await?;
    Ok(reply.pending)
}

/// `fauna.recovery.registration.chain` for `actor_id`, as verbatim records.
///
/// # Errors
///
/// The request failed.
pub async fn fetch_registration_chain<R: RpcRequester>(
    rpc: &R,
    actor_id: &[u8; 32],
) -> Result<Vec<Vec<u8>>, R::Error> {
    let reply: wire::RegistrationChainReply = rpc
        .request(
            REGISTRATION_CHAIN_KIND,
            wire::RegistrationChainRequest {
                actor_id: ByteBuf::from(actor_id.to_vec()),
                ..Default::default()
            },
        )
        .await?;
    Ok(reply
        .registrations
        .into_iter()
        .map(ByteBuf::into_vec)
        .collect())
}

/// `fauna.recovery.registration.submit` of one verbatim record.
///
/// # Errors
///
/// The nest refused the record or the request failed.
pub async fn submit_registration_record<R: RpcRequester>(
    rpc: &R,
    record: &[u8],
) -> Result<(), R::Error> {
    let _: wire::RegistrationSubmitReply = rpc
        .request(
            REGISTRATION_SUBMIT_KIND,
            wire::RegistrationSubmitRequest {
                registration: ByteBuf::from(record.to_vec()),
                ..Default::default()
            },
        )
        .await?;
    Ok(())
}

/// Compare the two nests' verbatim record lists.
///
/// The comparison is over bytes, never over decoded fields: a nest stores and
/// replays what was submitted, so two nests that hold the same link hold the
/// same bytes, and anything else is a different record.
///
/// # Errors
///
/// A record the shorter side lacks does not decode
/// ([`ChainReconcileError::Malformed`]) — it could not be told from a
/// seed-alone link, and the receiving nest would refuse it anyway.
pub fn plan_chain_reconcile(
    bound: &[Vec<u8>],
    linked: &[Vec<u8>],
) -> Result<ChainPlan, ChainReconcileError> {
    let (side, shorter, longer) = if bound.len() <= linked.len() {
        (ChainSide::Bound, bound, linked)
    } else {
        (ChainSide::Linked, linked, bound)
    };
    if longer[..shorter.len()] != *shorter {
        return Ok(ChainPlan::Forked);
    }
    if shorter.len() == longer.len() {
        return Ok(ChainPlan::InStep);
    }
    let mut records = Vec::new();
    let mut owed = None;
    for (index, record) in longer.iter().enumerate().skip(shorter.len()) {
        let signed: SignedRecoveryKeyRegistration = canonical_decode(record)
            .map_err(|e| ChainReconcileError::Malformed(format!("record {index}: {e}")))?;
        // A first registration carries no prior-key signature either, and the
        // strict rule takes it; every later link without one was landed by a
        // nest after its window, and no nest takes that on a submit.
        if index > 0 && signed.prior_recovery_sig.is_none() {
            owed = Some(record.clone());
            break;
        }
        records.push(record.clone());
    }
    Ok(ChainPlan::Extend {
        side,
        records,
        owed,
    })
}

/// Reconcile one account's registration chain between the bound nest and a
/// linked one, both over that account's own authenticated connections:
/// whichever holds the longer chain extends the other.
///
/// # Errors
///
/// A chain could not be read, a record is malformed, or a nest refused a
/// record ([`ChainReconcileError`]). Records submitted before a refusal have
/// landed; running it again resumes from what each nest then holds.
pub async fn reconcile_registration_chains<A, B>(
    bound: &A,
    linked: &B,
) -> Result<ChainReconcile, ChainReconcileError>
where
    A: RegistrationChainDoor,
    B: RegistrationChainDoor,
{
    let read = |side| move |message| ChainReconcileError::Read { side, message };
    let bound_chain = bound
        .registration_chain()
        .await
        .map_err(read(ChainSide::Bound))?;
    let linked_chain = linked
        .registration_chain()
        .await
        .map_err(read(ChainSide::Linked))?;
    let (side, records, owed) = match plan_chain_reconcile(&bound_chain, &linked_chain)? {
        ChainPlan::InStep => return Ok(ChainReconcile::InStep),
        ChainPlan::Forked => return Ok(ChainReconcile::Forked),
        ChainPlan::Extend {
            side,
            records,
            owed,
        } => (side, records, owed),
    };
    for record in &records {
        match side {
            ChainSide::Bound => bound.submit_registration(record).await,
            ChainSide::Linked => linked.submit_registration(record).await,
        }
        .map_err(|message| ChainReconcileError::Submit { side, message })?;
    }
    Ok(if let Some(owed) = owed {
        // Clause (c): the link no submit carries is requested there instead,
        // and that nest runs its own window for it.
        let requested = match side {
            ChainSide::Bound => bound.request_replacement(&owed).await,
            ChainSide::Linked => linked.request_replacement(&owed).await,
        }
        .map_err(|message| ChainReconcileError::Submit { side, message })?;
        ChainReconcile::Owed {
            side,
            submitted: records.len(),
            requested,
        }
    } else {
        ChainReconcile::Extended {
            side,
            records: records.len(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_core::data::Timestamp;
    use fauna_core::encoding::canonical_encode;
    use fauna_core::identity::ActorKeypair;
    use fauna_core::recovery::{RecoveryKey, RecoveryKeyRegistration};

    fn record(
        identity: &ActorKeypair,
        key: &RecoveryKey,
        prior: Option<&RecoveryKey>,
        seq: u64,
    ) -> Vec<u8> {
        let signed = RecoveryKeyRegistration {
            actor_id: identity.actor_id(),
            recovery_pubkey: key.public(),
            seq,
            created_at: Timestamp(1_700_000_000 + seq),
        }
        .sign(identity.signing_key(), key, prior)
        .unwrap();
        canonical_encode(&signed).unwrap()
    }

    fn keys() -> (ActorKeypair, RecoveryKey, RecoveryKey, RecoveryKey) {
        (
            ActorKeypair::from_secret([0x11; 32]),
            RecoveryKey::from_bytes([0x21; 32]),
            RecoveryKey::from_bytes([0x22; 32]),
            RecoveryKey::from_bytes([0x23; 32]),
        )
    }

    #[test]
    fn equal_chains_are_in_step_and_so_are_two_empty_ones() {
        let (id, k1, _, _) = keys();
        let first = record(&id, &k1, None, 1);
        assert_eq!(plan_chain_reconcile(&[], &[]), Ok(ChainPlan::InStep));
        assert_eq!(
            plan_chain_reconcile(std::slice::from_ref(&first), std::slice::from_ref(&first)),
            Ok(ChainPlan::InStep)
        );
    }

    #[test]
    fn the_shorter_side_is_extended_with_what_it_lacks_oldest_first() {
        let (id, k1, k2, _) = keys();
        let first = record(&id, &k1, None, 1);
        let second = record(&id, &k2, Some(&k1), 2);
        let both = vec![first.clone(), second.clone()];
        assert_eq!(
            plan_chain_reconcile(&both, &[]),
            Ok(ChainPlan::Extend {
                side: ChainSide::Linked,
                records: both.clone(),
                owed: None,
            })
        );
        assert_eq!(
            plan_chain_reconcile(std::slice::from_ref(&first), &both),
            Ok(ChainPlan::Extend {
                side: ChainSide::Bound,
                records: vec![second],
                owed: None,
            })
        );
    }

    #[test]
    fn two_chains_of_which_neither_extends_the_other_are_forked() {
        let (id, k1, k2, k3) = keys();
        let ours = record(&id, &k1, None, 1);
        let theirs = record(&id, &k2, None, 1);
        assert_eq!(
            plan_chain_reconcile(std::slice::from_ref(&ours), std::slice::from_ref(&theirs)),
            Ok(ChainPlan::Forked)
        );
        // A shared first link does not make a later divergence a prefix.
        let a = vec![ours.clone(), record(&id, &k2, Some(&k1), 2)];
        let b = vec![
            ours.clone(),
            record(&id, &k3, Some(&k1), 2),
            record(&id, &k2, Some(&k3), 3),
        ];
        assert_eq!(plan_chain_reconcile(&a, &b), Ok(ChainPlan::Forked));
    }

    #[test]
    fn a_seed_alone_link_stops_the_replay_and_is_owed() {
        let (id, k1, k2, k3) = keys();
        let first = record(&id, &k1, None, 1);
        // Landed by a nest after its window: no prior-key signature.
        let seed_alone = record(&id, &k2, None, 2);
        let after = record(&id, &k3, Some(&k2), 3);
        let full = vec![first.clone(), seed_alone.clone(), after];
        assert_eq!(
            plan_chain_reconcile(&full, &[]),
            Ok(ChainPlan::Extend {
                side: ChainSide::Linked,
                records: vec![first.clone()],
                owed: Some(seed_alone.clone()),
            })
        );
        assert_eq!(
            plan_chain_reconcile(std::slice::from_ref(&first), &full),
            Ok(ChainPlan::Extend {
                side: ChainSide::Bound,
                records: vec![],
                owed: Some(seed_alone),
            })
        );
    }

    #[test]
    fn a_record_that_does_not_decode_is_malformed_not_forwarded() {
        assert!(matches!(
            plan_chain_reconcile(&[vec![0xff, 0x00]], &[]),
            Err(ChainReconcileError::Malformed(_))
        ));
    }
}
