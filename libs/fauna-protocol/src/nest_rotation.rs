//! The **deployment-seed rotation statement** and its chain — the one sanctioned
//! way a deployment changes its identity without stranding every TOFU-pinned app.
//!
//! Owner: `docs/goal/architecture/nest/box-recovery.md` § Deployment-seed
//! rotation (the ceremony, the acceptance rule, the honest statement). This
//! module is the *wire + verification* half, shared by the nest (which mints and
//! serves the chain) and every app (which walks it before deciding whether a
//! changed `nest_actor_id` is an impersonation or a licensed rotation).
//!
//! # Why both signatures are mandatory, and why `old_sig` is load-bearing here
//!
//! A statement carries two detached signatures over the *same* canonical bytes
//! under the *same* domain tag ([`sig_domain::NEST_ROTATION_V1`]); they are told
//! apart by which key verifies them:
//!
//! - `old_sig` — by the superseded deployment key. **It is the continuity
//!   license**: it is the only thing that distinguishes "the admin who controls
//!   this box rotated it" from "somebody minted a fresh identity".
//! - `new_sig` — by the successor. Possession proof, so a rotation can never
//!   point at a key the box does not hold.
//!
//! ⚠ **This deliberately inverts the user plane's rule** — [`fauna_core::recovery`]'s
//! `SignedIdentitySuccession` refuses to consult `old_sig` at all, because a
//! thief holds the old key and that shape would be takeover rather than
//! recovery. That refusal rests on the user plane having a *strictly stronger*
//! factor (the offline RecoveryKey) to anchor on. The nest plane has no
//! RecoveryKey analogue; what it has is a **locator** — clients dial the box the
//! admin controls — plus, on public-domain rows, DNS `self=`. See the owner §
//! for the full argument. **Do not "fix" this toward either precedent.**
//!
//! # The acceptance rule is NOT in this module
//!
//! [`verify_chain`] answers exactly one question: *do these statements form a
//! valid signed link from the identity I hold to the head they claim?* That is
//! necessary but **never sufficient** — a harvested chain is public and replayable.
//! Acceptance additionally requires that the live channel binding prove the
//! presenter actually holds the head (`box-recovery.md` § Client acceptance,
//! condition 2), which only the caller holding that binding can check. Keeping
//! the two apart is deliberate: a verifier that could accept on its own would be
//! an offline-forgeable pin move.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use fauna_core::encoding::canonical_encode;

use crate::Value;
use crate::sig_domain::{NEST_ROTATION_V1, domain_separated};

/// The **pre-identity** kind that serves this box's rotation chain
/// (`box-recovery.md` § Client acceptance — re-pin on a verified chain).
///
/// Named `fauna.auth.*` because it rides the anonymous auth door, not because it
/// is an auth ceremony: it mints nothing and authenticates nobody. A client
/// reaches it in exactly one situation — the channel-binding identity it was
/// presented does not match the one it pinned — and it must be reachable *then*,
/// which is precisely when the client has no usable session. Hence pre-identity,
/// and hence throttled as the public directory read it is
/// (`bins/fauna-nest/src/pre_identity_allowlist.rs` carries both entries and the
/// rationale).
pub const ROTATION_CHAIN_KIND: &str = "fauna.auth.rotation_chain";

/// Ask a box for its full rotation chain. No arguments: the chain is served
/// whole (seq 1..head) and the *caller* locates its own starting hop
/// ([`verify_chain`]), so one reply serves every client however far back it is
/// pinned — and the box learns nothing about which identity the asker holds.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RotationChainRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The box's rotation chain, oldest hop first.
///
/// **Empty is the honest answer, never an error** — a box that has never rotated
/// has no chain, and a failed fetch falls through the same way
/// (`box-recovery.md` § Version skew). A caller must therefore read an empty
/// `chain` as "no rotation to verify", i.e. fall through to the ordinary
/// identity-changed surface, and never as a refusal.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RotationChainReply {
    /// Every hop this box has committed, ordered by `seq` ascending.
    pub chain: Vec<SignedNestRotation>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The rotation statement — one hop of the chain (`box-recovery.md`
/// § The rotation statement (wire)).
///
/// Canonical DAG-CBOR is the signed form; `seq` is the position in the box's
/// append-only rotation log, starting at 1 for the first rotation a box ever
/// performs.
///
/// **Exempt from transport.md rule 4's `extra` catch-all by construction**
/// (`tools/check-additive-evolution/catch_all_baseline.txt`): both detached
/// signatures on [`SignedNestRotation`] cover `signing_bytes()` — the
/// canonical encoding of this WHOLE struct — so a flatten field here would
/// change what gets signed, not merely what gets carried. Evolution goes
/// through a new domain-separation tag (`NEST_ROTATION_V2`), the same pattern
/// `nat_mode.rs`'s `SETUP_NAT_MODE_V2` took over its retired `V1`, never a
/// silent field add.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NestRotation {
    /// The superseded deployment identity (`nest_actor_id` ≡ Ed25519 pubkey).
    #[serde(with = "serde_bytes")]
    pub old_nest_actor_id: [u8; 32],
    /// The successor deployment identity.
    #[serde(with = "serde_bytes")]
    pub new_nest_actor_id: [u8; 32],
    /// Position in this box's append-only rotation log; the first rotation is 1.
    pub seq: u64,
    /// Epoch seconds at which the box committed the rotation. Advisory — a
    /// nest-maintained timestamp is never forensics; no verdict reads it.
    pub rotated_at: i64,
}

/// A [`NestRotation`] with its two mandatory detached signatures.
///
/// **Exempt from transport.md rule 4's `extra` catch-all** for the same
/// construction reason as [`NestRotation`] itself: the signatures cover the
/// canonical encoding of the statement they wrap, so evolution is a new
/// domain-separation tag, never a silent field add.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedNestRotation {
    pub statement: NestRotation,
    /// By the **superseded** deployment key, over [`NEST_ROTATION_V1`].
    /// **Mandatory and load-bearing — it is the continuity license** (module docs).
    #[serde(with = "serde_bytes")]
    pub old_sig: Vec<u8>,
    /// By the **successor** deployment key, over [`NEST_ROTATION_V1`].
    /// **Mandatory** — possession proof.
    #[serde(with = "serde_bytes")]
    pub new_sig: Vec<u8>,
}

/// Errors a chain walk can produce. Each names the specific broken property so a
/// caller can tell "this box never rotated" (no chain at all)
/// from "somebody handed me a forgery".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotationChainError {
    /// A hop's `old_sig` or `new_sig` did not verify under the key it names.
    BadSignature { seq: u64 },
    /// A hop does not start where the previous one ended — the chain is not a
    /// chain.
    Discontinuous { seq: u64 },
    /// `seq` did not strictly increase across a hop.
    NonMonotonicSeq { seq: u64 },
    /// The walk never reached the identity the caller was presented with.
    HeadNotReached,
    /// The chain does not start at the identity the caller holds a pin for. This
    /// is the ordinary "not for me" answer, not evidence of an attack.
    StartMismatch,
    /// A statement could not be canonically encoded for verification.
    Encoding,
}

impl core::fmt::Display for RotationChainError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadSignature { seq } => {
                write!(f, "rotation statement seq {seq} has a bad signature")
            }
            Self::Discontinuous { seq } => {
                write!(
                    f,
                    "rotation statement seq {seq} does not extend the previous hop"
                )
            }
            Self::NonMonotonicSeq { seq } => {
                write!(f, "rotation statement seq {seq} does not advance")
            }
            Self::HeadNotReached => write!(
                f,
                "the rotation chain does not reach the presented identity"
            ),
            Self::StartMismatch => {
                write!(f, "the rotation chain does not start at the held identity")
            }
            Self::Encoding => write!(f, "a rotation statement could not be canonically encoded"),
        }
    }
}

impl core::error::Error for RotationChainError {}

impl NestRotation {
    /// The canonical bytes both signatures are made over.
    fn signing_bytes(&self) -> Result<Vec<u8>, RotationChainError> {
        canonical_encode(self).map_err(|_| RotationChainError::Encoding)
    }

    /// Sign this statement with both keys. The box holds both roots at exactly
    /// one instant — inside the rotation transaction — and never again, which is
    /// why the statement must be minted there rather than reconstructed later.
    pub fn sign(
        &self,
        old_key: &ed25519_dalek::SigningKey,
        new_key: &ed25519_dalek::SigningKey,
    ) -> Result<SignedNestRotation, RotationChainError> {
        let canonical = self.signing_bytes()?;
        let message = domain_separated(NEST_ROTATION_V1, &canonical);
        use ed25519_dalek::Signer;
        Ok(SignedNestRotation {
            statement: self.clone(),
            old_sig: old_key.sign(&message).to_bytes().to_vec(),
            new_sig: new_key.sign(&message).to_bytes().to_vec(),
        })
    }
}

impl SignedNestRotation {
    /// Verify one hop in isolation: both signatures under the two identities the
    /// statement itself names.
    ///
    /// A hop verifying says nothing about whether it belongs in *your* chain —
    /// that is [`verify_chain`]'s job.
    pub fn verify_hop(&self) -> Result<(), RotationChainError> {
        let seq = self.statement.seq;
        let canonical = self.statement.signing_bytes()?;
        let message = domain_separated(NEST_ROTATION_V1, &canonical);
        verify_one(&self.statement.old_nest_actor_id, &message, &self.old_sig)
            .map_err(|_| RotationChainError::BadSignature { seq })?;
        verify_one(&self.statement.new_nest_actor_id, &message, &self.new_sig)
            .map_err(|_| RotationChainError::BadSignature { seq })?;
        Ok(())
    }
}

fn verify_one(pubkey: &[u8; 32], message: &[u8], sig: &[u8]) -> Result<(), ()> {
    let vk = ed25519_dalek::VerifyingKey::from_bytes(pubkey).map_err(|_| ())?;
    let sig: [u8; 64] = sig.try_into().map_err(|_| ())?;
    vk.verify_strict(message, &ed25519_dalek::Signature::from_bytes(&sig))
        .map_err(|_| ())
}

/// Walk `chain` from the identity the caller holds (`held`) to the identity it
/// was presented with (`presented`), verifying every hop.
///
/// `chain` is the box's full ordered log; the walk starts at the first hop whose
/// `old_nest_actor_id` is `held` and follows the links from there, so a client
/// pinned several rotations back converges in one fetch.
///
/// On success returns the accepted head's `seq` — the value a client stores
/// alongside the head so it can refuse a later chain that does not extend it.
///
/// ⚠ Returning `Ok` is **not** an instruction to re-pin. See the module docs:
/// acceptance also requires the live channel binding to prove the presenter
/// holds `presented`.
pub fn verify_chain(
    chain: &[SignedNestRotation],
    held: &[u8; 32],
    presented: &[u8; 32],
) -> Result<u64, RotationChainError> {
    // Locating the start by the held identity (rather than assuming the caller
    // is pinned to seq 0) is what lets one fetch serve every client, however far
    // back it is pinned.
    let start = chain
        .iter()
        .position(|hop| &hop.statement.old_nest_actor_id == held)
        .ok_or(RotationChainError::StartMismatch)?;

    let mut current = *held;
    let mut last_seq: Option<u64> = None;

    for hop in &chain[start..] {
        hop.verify_hop()?;
        if hop.statement.old_nest_actor_id != current {
            return Err(RotationChainError::Discontinuous {
                seq: hop.statement.seq,
            });
        }
        if let Some(prev) = last_seq
            && hop.statement.seq <= prev
        {
            return Err(RotationChainError::NonMonotonicSeq {
                seq: hop.statement.seq,
            });
        }
        last_seq = Some(hop.statement.seq);
        current = hop.statement.new_nest_actor_id;
        if &current == presented {
            return Ok(hop.statement.seq);
        }
    }

    Err(RotationChainError::HeadNotReached)
}

/// Every identity `chain` proves to be a superseded ancestor of `head`: each
/// hop's old identity from which [`verify_chain`] walks, hop by verified hop,
/// to `head`. Oldest first; empty when the box never rotated, when `head` is
/// not on the chain, or when no hop verifies.
///
/// What the escrow-recovery pass admits a receipt from beside the pinned
/// identity itself (`account-data-taxonomy.md` § The generation machinery →
/// *A holder change re-receipts and never mints*): the ancestry is the box's
/// own signed statements, verified here, never the box's say-so.
#[must_use]
pub fn verified_ancestors(chain: &[SignedNestRotation], head: &[u8; 32]) -> Vec<[u8; 32]> {
    let mut ancestors = Vec::new();
    for hop in chain {
        let old = hop.statement.old_nest_actor_id;
        if &old != head && !ancestors.contains(&old) && verify_chain(chain, &old, head).is_ok() {
            ancestors.push(old);
        }
    }
    ancestors
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn key(b: u8) -> SigningKey {
        SigningKey::from_bytes(&[b; 32])
    }

    fn id(k: &SigningKey) -> [u8; 32] {
        k.verifying_key().to_bytes()
    }

    fn hop(old: &SigningKey, new: &SigningKey, seq: u64) -> SignedNestRotation {
        NestRotation {
            old_nest_actor_id: id(old),
            new_nest_actor_id: id(new),
            seq,
            rotated_at: 1_800_000_000 + seq as i64,
        }
        .sign(old, new)
        .unwrap()
    }

    #[test]
    fn a_single_hop_moves_a_pin_from_old_to_new() {
        let (a, b) = (key(1), key(2));
        let chain = vec![hop(&a, &b, 1)];
        assert_eq!(verify_chain(&chain, &id(&a), &id(&b)).unwrap(), 1);
    }

    /// The multi-hop case is the reason the walk locates its start rather than
    /// assuming one: a client pinned at A must converge on C in one fetch.
    #[test]
    fn a_client_pinned_two_rotations_back_converges_in_one_walk() {
        let (a, b, c) = (key(1), key(2), key(3));
        let chain = vec![hop(&a, &b, 1), hop(&b, &c, 2)];
        assert_eq!(verify_chain(&chain, &id(&a), &id(&c)).unwrap(), 2);
        // And a client pinned at B skips the hop it has already accepted.
        assert_eq!(verify_chain(&chain, &id(&b), &id(&c)).unwrap(), 2);
    }

    /// Downgrade protection's other half lives client-side (refuse an accepted
    /// ancestor), but the walk must at least refuse to *reach* an ancestor: a
    /// chain never links forward-to-backward.
    #[test]
    fn a_superseded_ancestor_is_not_reachable_as_a_head() {
        let (a, b, c) = (key(1), key(2), key(3));
        let chain = vec![hop(&a, &b, 1), hop(&b, &c, 2)];
        assert_eq!(
            verify_chain(&chain, &id(&b), &id(&a)),
            Err(RotationChainError::HeadNotReached)
        );
    }

    /// `old_sig` is the continuity license — a chain signed only by the successor
    /// is exactly the forgery this plane exists to refuse, and it must fail as a
    /// *signature* error rather than being silently skipped the way the user
    /// plane's optional `old_sig` is.
    #[test]
    fn a_statement_whose_old_sig_is_forged_is_refused() {
        let (a, b, thief) = (key(1), key(2), key(9));
        let mut forged = hop(&a, &b, 1);
        // The thief holds only the successor key: they can produce `new_sig` but
        // must guess `old_sig`. Model that as a signature by the wrong key.
        let canonical = forged.statement.signing_bytes().unwrap();
        let message = domain_separated(NEST_ROTATION_V1, &canonical);
        use ed25519_dalek::Signer;
        forged.old_sig = thief.sign(&message).to_bytes().to_vec();
        assert_eq!(
            verify_chain(&[forged], &id(&a), &id(&b)),
            Err(RotationChainError::BadSignature { seq: 1 })
        );
    }

    #[test]
    fn a_statement_whose_new_sig_is_missing_possession_is_refused() {
        let (a, b, other) = (key(1), key(2), key(9));
        let mut forged = hop(&a, &b, 1);
        let canonical = forged.statement.signing_bytes().unwrap();
        let message = domain_separated(NEST_ROTATION_V1, &canonical);
        use ed25519_dalek::Signer;
        forged.new_sig = other.sign(&message).to_bytes().to_vec();
        assert_eq!(
            verify_chain(&[forged], &id(&a), &id(&b)),
            Err(RotationChainError::BadSignature { seq: 1 })
        );
    }

    /// Two independently valid hops that do not link must not be accepted as a
    /// chain — otherwise a thief could splice their own rotation onto the box's.
    #[test]
    fn two_valid_but_unlinked_hops_do_not_form_a_chain() {
        let (a, b, x, y) = (key(1), key(2), key(7), key(8));
        let chain = vec![hop(&a, &b, 1), hop(&x, &y, 2)];
        assert_eq!(
            verify_chain(&chain, &id(&a), &id(&y)),
            Err(RotationChainError::Discontinuous { seq: 2 })
        );
    }

    /// The ancestors of a head are exactly the identities the chain walks from
    /// to it: every earlier identity of a twice-rotated box, and nothing a
    /// spliced foreign hop or a later identity would add.
    #[test]
    fn the_verified_ancestors_of_a_head_are_the_identities_its_chain_walks_from() {
        let (a, b, c, x, y) = (key(1), key(2), key(3), key(7), key(8));
        let chain = vec![hop(&a, &b, 1), hop(&b, &c, 2)];
        assert_eq!(verified_ancestors(&chain, &id(&c)), vec![id(&a), id(&b)]);
        assert_eq!(verified_ancestors(&chain, &id(&b)), vec![id(&a)]);
        assert!(verified_ancestors(&chain, &id(&a)).is_empty());
        assert!(verified_ancestors(&[], &id(&c)).is_empty());
        let spliced = vec![hop(&a, &b, 1), hop(&x, &y, 2)];
        assert_eq!(
            verified_ancestors(&spliced, &id(&y)),
            vec![id(&x)],
            "only the hop that links to the head proves ancestry — never an identity whose \
             own chain stops short of it"
        );
        let mut forged = hop(&b, &c, 2);
        forged.statement.old_nest_actor_id = id(&x);
        assert!(verified_ancestors(&[hop(&a, &b, 1), forged], &id(&c)).is_empty());
    }

    #[test]
    fn a_chain_that_does_not_start_at_the_held_identity_is_not_mine() {
        let (a, b, stranger) = (key(1), key(2), key(5));
        let chain = vec![hop(&a, &b, 1)];
        assert_eq!(
            verify_chain(&chain, &id(&stranger), &id(&b)),
            Err(RotationChainError::StartMismatch)
        );
    }

    /// The signed message must carry the domain tag, or a rotation signature
    /// could be replayed into another deployment-key context (rule #8).
    #[test]
    fn the_signed_message_is_domain_separated() {
        let (a, b) = (key(1), key(2));
        let signed = hop(&a, &b, 1);
        let canonical = signed.statement.signing_bytes().unwrap();
        // The bare canonical bytes must NOT verify — only the tagged form does.
        assert!(verify_one(&id(&a), &canonical, &signed.old_sig).is_err());
        assert!(
            verify_one(
                &id(&a),
                &domain_separated(NEST_ROTATION_V1, &canonical),
                &signed.old_sig
            )
            .is_ok()
        );
    }

    /// The empty chain is a *value*, not an error condition — the reply a box
    /// that never rotated serves, and the one a failed fetch is
    /// indistinguishable from once the client falls back. If this ever
    /// stopped round-tripping, every un-rotated box would look like a failure.
    #[test]
    fn the_empty_chain_round_trips_as_an_ordinary_reply() {
        use crate::codec::{decode_strict, encode_canonical};
        let reply = RotationChainReply::default();
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: RotationChainReply = decode_strict(&bytes).unwrap();
        assert_eq!(reply, decoded);
        assert!(decoded.chain.is_empty());
    }

    /// The served chain must survive the wire **verifiably** — a decode that
    /// preserved the fields but perturbed the canonical bytes would leave every
    /// signature failing at the client for no visible reason, so this asserts
    /// the round trip through `verify_chain` rather than through `PartialEq`.
    #[test]
    fn a_served_chain_still_verifies_after_a_wire_round_trip() {
        use crate::codec::{decode_strict, encode_canonical};
        let (a, b, c) = (key(1), key(2), key(3));
        let reply = RotationChainReply {
            chain: vec![hop(&a, &b, 1), hop(&b, &c, 2)],
            extra: Default::default(),
        };
        let bytes = encode_canonical(&reply).unwrap();
        let decoded: RotationChainReply = decode_strict(&bytes).unwrap();
        assert_eq!(verify_chain(&decoded.chain, &id(&a), &id(&c)).unwrap(), 2);
    }

    #[test]
    fn the_request_round_trips_from_the_empty_map() {
        use crate::codec::{decode_strict, encode_canonical};
        let req = RotationChainRequest::default();
        let bytes = encode_canonical(&req).unwrap();
        let decoded: RotationChainRequest = decode_strict(&bytes).unwrap();
        assert_eq!(req, decoded);
    }

    /// A replayed seq must not be accepted as an advance — the seq a client
    /// stores is what makes a later non-extending chain detectable as a fork.
    #[test]
    fn a_non_advancing_seq_is_refused() {
        let (a, b, c) = (key(1), key(2), key(3));
        let mut chain = vec![hop(&a, &b, 5), hop(&b, &c, 5)];
        // Re-sign the second hop so only the seq, not a signature, is wrong.
        chain[1] = NestRotation {
            old_nest_actor_id: id(&b),
            new_nest_actor_id: id(&c),
            seq: 5,
            rotated_at: 1,
        }
        .sign(&b, &c)
        .unwrap();
        assert_eq!(
            verify_chain(&chain, &id(&a), &id(&c)),
            Err(RotationChainError::NonMonotonicSeq { seq: 5 })
        );
    }
}
