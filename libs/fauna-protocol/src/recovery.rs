//! `fauna.recovery.registration.{submit,chain}` — the RecoveryKey registration
//! chain plane (identity-succession slice 2;
//! `docs/goal/behavior/identity-succession.md` § The RecoveryKey).
//!
//! The RecoveryKey is the offline root that outranks the identity seed. A
//! client registers its public half with the home nest so that (a) the nest can
//! later validate a succession statement against a chain it has actually seen,
//! and (b) any third party verifying a succession can fetch the binding from
//! the old identity's home nest (`identity-succession.md:56`).
//!
//! **The nest is enforcer and distributor, never authorizer**
//! (`identity-succession.md:104`): `submit` only ever *verifies* a record the
//! client signed with keys the nest does not hold, and `chain` only ever
//! replays what it stored. There is no nest-side path that mints a
//! registration.
//!
//! ## Why the records ride as bytes
//!
//! Both kinds carry a [`fauna_core::recovery::SignedRecoveryKeyRegistration`]
//! as **canonical DAG-CBOR bytes**, not as a re-encoded nested map — the
//! embed-as-bytes rule for signed payloads (`transport.md` § SignedEnvelope and
//! embed-as-bytes: "the raw canonical dag-cbor bytes the publisher signed
//! travel as a CBOR byte string ... never as a re-encoded nested map").
//!
//! The nest stores the submitted bytes **verbatim** and serves those same bytes
//! back, so the serve path never re-encodes a record it did not author. A
//! canonicalization drift would therefore be unable to invalidate a chain
//! already on disk.
//!
//! `submit` is a USER-class kind (the owner registers its own key over its own
//! authenticated connection). `chain` is **pre-identity** — a peer verifying a
//! succession holds no account here — and is rate-limited as the public
//! `actor_id → recovery_pubkey` oracle it is.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;

use fauna_cbor::Value;

/// `fauna.recovery.registration.submit` request — register (or replace) the
/// authenticated actor's RecoveryKey.
///
/// The account is always the authenticated connection's actor, never a wire
/// param; a record whose `actor_id` disagrees is refused, so this kind cannot
/// be used to write into another identity's chain.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RegistrationSubmitRequest {
    /// Canonical DAG-CBOR of a `SignedRecoveryKeyRegistration`.
    pub registration: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RegistrationSubmitReply {
    /// The `seq` now at the head of this identity's chain.
    pub seq: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.recovery.registration.chain` request — fetch an identity's
/// registration chain.
///
/// Pre-identity: the caller is typically a federation peer or another user's
/// client verifying a succession statement, and holds no account on this nest.
/// The reply is public by construction — the same `recovery_pubkey` rides the
/// actor's signed `Profile` (`identity-succession.md:34`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RegistrationChainRequest {
    /// The 32-byte actor id whose chain is being fetched.
    pub actor_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RegistrationChainReply {
    /// Every registration this nest holds for the actor, **oldest first** — the
    /// verbatim bytes each was submitted as. Empty when the identity has
    /// registered no RecoveryKey (the honest "no succession capability"
    /// answer, `identity-succession.md:46`), never an error.
    pub registrations: Vec<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Seed escrow (`identity-succession.md` § Seed escrow)
// ─────────────────────────────────────────────────────────────────────────────

/// `fauna.recovery.escrow.put` request — store the actor's opaque seed-escrow
/// blob (USER class).
///
/// `blob` is canonical DAG-CBOR of a `fauna_mls::wrapped_blob::SeedEscrowBlob`
/// — the identity seed HPKE-sealed to the RecoveryKey's escrow public half. The
/// nest **never decodes it**: it holds ciphertext it has no key for
/// (`identity-succession.md:42`, key-material rule #4). It is typed as opaque
/// bytes here for exactly that reason — a structured wire type would invite a
/// nest-side parse that must never exist.
///
/// One row per actor: a re-put replaces, since the blob is rewritten only when
/// the sealed value or the sealing key changes (kit creation, succession).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EscrowPutRequest {
    /// The opaque sealed blob.
    pub blob: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EscrowPutReply {
    /// Unix seconds the nest recorded the blob at — the client's confirmation
    /// that the *new* value landed rather than an earlier one being kept.
    pub updated_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.recovery.escrow.challenge` request — ask for a nonce to sign.
///
/// **Pre-identity**: the caller has lost every device and holds only the
/// recovery phrase, so no session exists to authenticate with
/// (`identity-succession.md:44`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EscrowChallengeRequest {
    /// The 32-byte actor id being recovered.
    pub actor_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EscrowChallengeReply {
    /// The 32-byte single-use nonce to sign with the RecoveryKey.
    pub nonce: ByteBuf,
    /// Unix seconds the nonce stops being accepted.
    pub expires_at: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.recovery.escrow.fetch` request — redeem a signed challenge for the
/// blob.
///
/// `signature` is over a `fauna_core::recovery::EscrowChallenge { actor_id,
/// nonce }` under `TAG_ESCROW_CHALLENGE`, verified against the head of the
/// actor's registration chain. Unlike the registration kinds this carries no
/// signed *record* — the signature authorizes one call and is never stored — so
/// the two covered fields ride as ordinary wire params and the nest rebuilds the
/// record it verifies from them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EscrowFetchRequest {
    /// The 32-byte actor id being recovered.
    pub actor_id: ByteBuf,
    /// The nonce this nest issued.
    pub nonce: ByteBuf,
    /// Ed25519 signature by the registered RecoveryKey.
    pub signature: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EscrowFetchReply {
    /// The opaque blob, byte-identical to what was put.
    pub blob: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.recovery.escrow.status` request — does a blob rest for the
/// authenticated actor?
///
/// **USER class, and deliberately not pre-identity**, unlike the rest of the
/// escrow family. The three kinds above serve the phrase holder who has no
/// session; this one serves the opposite party — a signed-in device asking
/// about its *own* account — and that difference is load-bearing in both
/// directions:
///
/// - It **must** exist, because the surface that owns the "registered, no
///   escrow" state is a signed-in one (`ui/settings.md` § Recovery kit), and
///   that device cannot use `escrow.fetch` to find out: fetch is gated on a
///   RecoveryKey signature, and the RecoveryKey is offline-only by iron-clad
///   rule (`identity-succession.md` § The RecoveryKey — *Custody*). Without
///   this kind the state is unobservable by the only device that can repair it.
/// - It **must not** be pre-identity, for the reason `escrow.challenge` issues
///   its nonce unconditionally: an unauthenticated presence answer would be a
///   directory oracle mapping actor ids to "holds an escrow blob". Taking the
///   actor from the authenticated connection keeps the answer caller-scoped, so
///   it reveals only what the caller already owns.
///
/// It answers presence, never bytes — the blob is opened by the RecoveryKey
/// alone, and this path holds no key material at all.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EscrowStatusRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EscrowStatusReply {
    /// Whether a blob currently rests for the authenticated actor.
    ///
    /// `false` is the honest "a re-put is owed" signal: the nest deletes the
    /// row whenever the key it is sealed to retires (§ Seed escrow —
    /// *Lifecycle on the nest*), so a registered kit with no blob is a real
    /// loss-protection gap rather than an error.
    pub present: bool,
    /// Unix seconds the resting blob was recorded at; `None` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Seed-initiated replacement — the pending window
// (`identity-succession.md:37`: 30 days, loudly notified, vetoable instantly)
// ─────────────────────────────────────────────────────────────────────────────

/// `fauna.recovery.replacement.request` — the **seed-alone** replacement arm
/// (USER class: the requester holds the identity seed, so a session exists).
///
/// `registration` is canonical DAG-CBOR of a `SignedRecoveryKeyRegistration`
/// **without** `prior_recovery_sig` — the prior key is what was lost. The nest
/// verifies it under `verify_seed_alone` and parks it in the pending store for
/// `RECOVERY_REPLACE_GRACE_SECS`; it does NOT touch the chain. Only an
/// uncontested window lands it (the landing sweep re-verifies against the
/// then-current head), and the current RecoveryKey vetoes instantly.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplacementRequestRequest {
    /// Canonical DAG-CBOR of a `SignedRecoveryKeyRegistration` (no prior sig).
    pub registration: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplacementRequestReply {
    /// Unix seconds the pending replacement lands if uncontested.
    pub lands_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.recovery.replacement.challenge` — ask for a nonce to sign a veto.
///
/// **Pre-identity**: the veto scenario is a seed thief who revoked every
/// session and invoked the seed-signed lockout, leaving the real owner holding
/// only the recovery phrase. Issued unconditionally (whether or not anything
/// is pending) for the same no-probe reason as the escrow challenge.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplacementChallengeRequest {
    /// The 32-byte actor id whose pending replacement is being contested.
    pub actor_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplacementChallengeReply {
    /// The 32-byte single-use nonce to sign with the RecoveryKey.
    pub nonce: ByteBuf,
    /// Unix seconds the nonce stops being accepted.
    pub expires_at: u64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.recovery.replacement.veto` — cancel whatever replacement currently
/// pends, on proof of the **current** RecoveryKey (pre-identity).
///
/// `signature` is over a `fauna_core::recovery::ReplacementVeto { actor_id,
/// nonce }` under `TAG_REPLACEMENT_VETO`, verified against the **head** of the
/// registration chain. The nonce is single-use, so a captured veto can never
/// be replayed to cancel a future honest replacement — and a live veto needs
/// no reference to the pending record: it contests whatever pends now.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplacementVetoRequest {
    /// The 32-byte actor id whose pending replacement is being contested.
    pub actor_id: ByteBuf,
    /// The nonce this nest issued.
    pub nonce: ByteBuf,
    /// Ed25519 signature by the registered RecoveryKey.
    pub signature: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplacementVetoReply {
    /// `true` if a pending replacement was cancelled; `false` if nothing was
    /// pending (an idempotent success — the vetoer's goal state holds either
    /// way, and the caller has already proven they are the key holder, so this
    /// is an honest answer to the owner, not a probe).
    pub cancelled: bool,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.recovery.replacement.status` — the authenticated actor's own pending
/// replacement, if any (USER class).
///
/// The read every app's standing "a replacement is pending — veto it if
/// this wasn't you" banner derives from, for the whole window
/// (`identity-succession.md:37`'s loud-on-every-device requirement; the
/// one-shot inbox/push/email alarm rides `SecurityNotifier` separately).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplacementStatusRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplacementStatusReply {
    /// The pending replacement, or `None` when nothing pends. Absent — never
    /// `null` — when `None`, so the encoding stays single-valued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending: Option<ReplacementPendingInfo>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One pending seed-initiated replacement.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplacementPendingInfo {
    /// The Ed25519 public key of the RecoveryKey that would be registered.
    pub new_recovery_pubkey: ByteBuf,
    /// Unix seconds the request was accepted.
    pub requested_at: i64,
    /// Unix seconds it lands if uncontested.
    pub lands_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Succession (`identity-succession.md` § Enforcement on the home nest)
// ─────────────────────────────────────────────────────────────────────────────

/// `fauna.recovery.succession.submit` request — re-point an account to its
/// successor identity.
///
/// **Pre-identity, and necessarily so** (`identity-succession.md:66`): the whole
/// scenario is an owner whose seed was stolen, and a thief holding that seed can
/// revoke every session and invoke the seed-signed emergency lockout. If this
/// kind needed a bearer, the attack would disable its own remedy. Authorization
/// is the RecoveryKey signature inside the statement — a credential the thief
/// cannot hold, because it never touched a device.
///
/// Unlike `registration.submit`, there is therefore **no authenticated actor to
/// bind to**; the account is named by the statement's own `old_actor_id`, which
/// is safe precisely because `recovery_sig` is checked against the chain that
/// identity registered.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SuccessionSubmitRequest {
    /// Canonical DAG-CBOR of a `fauna_core::recovery::SignedIdentitySuccession`
    /// — carried as bytes and stored verbatim, per the embed-as-bytes rule.
    pub statement: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SuccessionSubmitReply {
    /// The successor actor id the account now belongs to — echoed so a client
    /// that submitted on someone else's behalf (a helper device) can confirm
    /// which identity landed.
    pub new_actor_id: ByteBuf,
    /// Unix seconds the nest applied the succession.
    pub succeeded_at: i64,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// `fauna.recovery.succession.lookup` request — "has this identity been
/// succeeded, and by whom?"
///
/// Pre-identity for the same reason `registration.chain` is: peers hold *old*
/// actor ids and discovery must work **from** them
/// (`identity-succession.md:72`), and a peer holds no account here.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SuccessionLookupRequest {
    /// The 32-byte actor id to look up — normally one the caller last saw.
    pub actor_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SuccessionLookupReply {
    /// Every succession from `actor_id` forward, **oldest first**, each the
    /// verbatim statement bytes its submitter signed. Empty when the identity
    /// was never succeeded — the honest answer, never an error.
    ///
    /// Multiple entries mean the caller was several hops behind; it verifies
    /// them in order, each against the RecoveryKey registered for the identity
    /// *that link* succeeds. The nest's say-so is never what makes a link valid
    /// (`identity-succession.md:74`), so a truncated or reordered reply can
    /// only fail closed.
    pub statements: Vec<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The kind name of [`SuccessionStatusRequest`], as a shared symbol.
///
/// A constant rather than a literal — unlike its `submit`/`lookup` siblings —
/// because this one is spelled in two *crates*: the nest registers it and
/// `fauna_client_config::raise_succession_filter_marks` calls it. A literal in
/// the client would fail silently (an unknown kind is a refusal, and this
/// caller's whole contract is to degrade quietly on a refusal), which is
/// exactly the class of typo a shared symbol makes unrepresentable.
pub const SUCCESSION_STATUS_KIND: &str = "fauna.recovery.succession.status";

/// `fauna.recovery.succession.status` request — "when did the succession that
/// produced *me* commit?"
///
/// **Authenticated and self-scoped, deliberately taking no parameters.** The
/// value it serves is `actor_successions.succeeded_at`, a *server-observed*
/// stamp, and the reason it is not on `succession.lookup` — the natural-looking
/// home — is that `lookup` is pre-identity and answers for whatever actor id it
/// is handed. Serving a commit stamp there would build an anonymous oracle for
/// the wall-clock second at which **any** account's recovery-from-compromise
/// committed, plus a correlation handle (two accounts succeeding in the same
/// second ⇒ one incident). The statements `lookup` serves are signed artifacts
/// a peer must be able to fetch; a server-observed stamp is not part of the
/// artifact and does not inherit its disclosure licence
/// (`succession-aftermath.md` § Adjudicating what the aftermath carries across).
///
/// Having **no request field at all** is what makes "actor-scoped" structural
/// rather than a check a future edit could forget: there is no id to pass, so
/// the only answer this kind can give is the caller's own.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SuccessionStatusRequest {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SuccessionStatusReply {
    /// Unix **seconds** the nest committed the succession that made the caller
    /// the successor — the same value [`SuccessionSubmitReply::succeeded_at`]
    /// carries, from the row rather than from a fresh clock read.
    ///
    /// `None` when the caller is not a successor at all: the honest answer, and
    /// never an error, for `SuccessionLookupReply::statements`' reason.
    ///
    /// ⚠ **Seconds, not milliseconds** — `email_filters.created_at`, the value
    /// this is compared against, is in milliseconds. The one classifier
    /// (`fauna_client_config::SuccessionTime`) owns the conversion and the
    /// deliberate round *up*; nothing else may compare this raw.
    pub succeeded_at: Option<i64>,
    /// The nests the caller's predecessors were linked to and that have not
    /// been told of the succession yet — the **owed nests**
    /// (`identity-succession.md` § Enforcement on the home nest → *Every nest
    /// the identity is linked to*). One entry per pairing destination the
    /// succession transaction burned, for **every** hop on the caller's
    /// predecessor path, so an entry a first successor never settled is still
    /// served after a second succession. Within a hop the order is the kept
    /// one: oldest pairing first.
    ///
    /// Empty — and absent from the wire — for a caller that is not a successor,
    /// and for one whose owed nests are all settled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owed_nests: Vec<OwedNest>,
    /// The landed succession statements that end at the caller, verbatim and
    /// **oldest first** — the statement path from the caller's first
    /// predecessor forward, what `fauna.recovery.succession.lookup` serves for
    /// that predecessor. The link action submits it at the nest it is about to
    /// link before it signs in there (`identity-succession.md` § Enforcement on
    /// the home nest → *Every nest the identity is linked to*, **The road**'s
    /// last sentence), so a nest no owed list names is retired too.
    ///
    /// Additive: empty — and absent from the wire — for a caller that
    /// succeeded nobody, which the link reads as "nothing to deliver".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub predecessor_statements: Vec<ByteBuf>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// One owed nest: a nest a retired identity was paired with when its
/// succession was applied, which the successor's devices still have to carry
/// the statement to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OwedNest {
    /// The retired identity whose pairing named this nest — the statement to
    /// deliver is the one that succeeded *this* id. It is also half of the key
    /// [`SuccessionOwedSettleRequest`] takes.
    pub old_actor_id: ByteBuf,
    /// The owed nest's id.
    pub nest_id: ByteBuf,
    /// The owed nest's address, when the burned pairing row carried one.
    pub nest_url: Option<String>,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

/// The kind name of [`SuccessionOwedSettleRequest`], as a shared symbol — for
/// [`SUCCESSION_STATUS_KIND`]'s reason: the nest registers it and a client
/// crate calls it.
pub const SUCCESSION_OWED_SETTLE_KIND: &str = "fauna.recovery.succession.owed_settle";

/// `fauna.recovery.succession.owed_settle` request — "this owed nest is
/// settled; stop serving it to me."
///
/// **Authenticated, and only a successor on `old_actor_id`'s path may settle
/// its entries**: the caller is the connection's actor, and the nest refuses a
/// caller the path from `old_actor_id` does not reach. Settling an entry that
/// is not there is a success, so a retry — or a second device settling the same
/// nest — is harmless.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SuccessionOwedSettleRequest {
    /// The retired identity the entry belongs to ([`OwedNest::old_actor_id`]).
    pub old_actor_id: ByteBuf,
    /// The settled nest ([`OwedNest::nest_id`]).
    pub nest_id: ByteBuf,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SuccessionOwedSettleReply {
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode_strict;

    fn roundtrip<T>(value: &T) -> T
    where
        T: Serialize + serde::de::DeserializeOwned,
    {
        let bytes = fauna_core::encoding::canonical_encode(value).expect("encode");
        decode_strict(&bytes).expect("decode")
    }

    #[test]
    fn submit_request_roundtrips() {
        let req = RegistrationSubmitRequest {
            registration: ByteBuf::from(vec![1, 2, 3, 4]),
            extra: BTreeMap::new(),
        };
        assert_eq!(roundtrip(&req), req);
    }

    #[test]
    fn chain_reply_roundtrips_and_preserves_order() {
        let reply = RegistrationChainReply {
            registrations: vec![
                ByteBuf::from(vec![0xaa]),
                ByteBuf::from(vec![0xbb]),
                ByteBuf::from(vec![0xcc]),
            ],
            extra: BTreeMap::new(),
        };
        let back = roundtrip(&reply);
        assert_eq!(back, reply);
        // Oldest-first ordering is part of the contract: a consumer walks the
        // chain forward to find the head, so a reordering reply would change
        // which key it believes is current.
        assert_eq!(back.registrations[0].as_ref(), &[0xaa]);
        assert_eq!(back.registrations[2].as_ref(), &[0xcc]);
    }

    #[test]
    fn an_empty_chain_is_a_valid_reply() {
        let reply = RegistrationChainReply::default();
        assert!(roundtrip(&reply).registrations.is_empty());
    }

    #[test]
    fn escrow_put_roundtrips_the_blob_byte_for_byte() {
        // The blob is opaque ciphertext; a single flipped byte makes it
        // unopenable, so the wire type must not normalize it in any way.
        let req = EscrowPutRequest {
            blob: ByteBuf::from(vec![0x00, 0xff, 0x7f, 0x80, 0x00]),
            extra: BTreeMap::new(),
        };
        assert_eq!(roundtrip(&req), req);
    }

    #[test]
    fn replacement_status_roundtrips_both_arms() {
        // `None` must encode as an ABSENT key (single-valued encoding), and the
        // populated arm must round-trip — the one nested-struct Option in this
        // file, so pin it explicitly (the dag-cbor trap is nested `Option`,
        // which this deliberately is not).
        let empty = ReplacementStatusReply::default();
        let empty_bytes = fauna_core::encoding::canonical_encode(&empty).expect("encode");
        assert_eq!(roundtrip(&empty), empty);

        let populated = ReplacementStatusReply {
            pending: Some(ReplacementPendingInfo {
                new_recovery_pubkey: ByteBuf::from(vec![0x33; 32]),
                requested_at: 1_753_000_000,
                lands_at: 1_755_592_000,
                extra: BTreeMap::new(),
            }),
            extra: BTreeMap::new(),
        };
        let populated_bytes = fauna_core::encoding::canonical_encode(&populated).expect("encode");
        assert_eq!(roundtrip(&populated), populated);
        assert!(
            populated_bytes.len() > empty_bytes.len(),
            "the empty arm must omit the key entirely"
        );
    }

    #[test]
    fn succession_status_roundtrips_both_arms_and_carries_no_actor_id() {
        // The request is deliberately field-less: "actor-scoped" is a property
        // of the wire shape here, not of a check in the handler. Pin that it
        // stays that way — adding an `actor_id` would silently turn a
        // self-scoped read into the anonymous-oracle shape the kind exists to
        // avoid, and nothing else in the tree would notice.
        let req = SuccessionStatusRequest::default();
        assert_eq!(roundtrip(&req), req);
        let req_bytes = fauna_core::encoding::canonical_encode(&req).expect("encode");
        assert_eq!(
            req_bytes.len(),
            1,
            "the request must encode as a bare empty map — an added field is a \
             scope change, not a compatible extension"
        );

        let unknown = SuccessionStatusReply::default();
        assert_eq!(unknown.succeeded_at, None);
        let unknown_bytes = fauna_core::encoding::canonical_encode(&unknown).expect("encode");
        assert_eq!(roundtrip(&unknown), unknown);

        let known = SuccessionStatusReply {
            succeeded_at: Some(1_753_000_000),
            ..Default::default()
        };
        let known_bytes = fauna_core::encoding::canonical_encode(&known).expect("encode");
        assert_eq!(roundtrip(&known), known);
        assert!(
            known_bytes.len() > unknown_bytes.len(),
            "the not-a-successor arm must omit the key entirely"
        );
    }

    #[test]
    fn succession_status_owed_nests_are_additive_and_roundtrip() {
        // A reply with no `owed_nests` key is the settled-successor shape, and
        // a settled successor's reply must encode exactly as that one does —
        // the field is additive in both directions.
        let settled = SuccessionStatusReply {
            succeeded_at: Some(1_753_000_000),
            ..Default::default()
        };
        let settled_bytes = fauna_core::encoding::canonical_encode(&settled).expect("encode");
        assert!(
            !settled_bytes
                .windows(b"owed_nests".len())
                .any(|w| w == b"owed_nests"),
            "an empty list must stay off the wire"
        );

        // Both address arms: a pairing row may carry no `nest_url`.
        let owing = SuccessionStatusReply {
            succeeded_at: Some(1_753_000_000),
            owed_nests: vec![
                OwedNest {
                    old_actor_id: ByteBuf::from(vec![0x11; 32]),
                    nest_id: ByteBuf::from(vec![0xa1; 32]),
                    nest_url: Some("https://x.example".into()),
                    extra: BTreeMap::new(),
                },
                OwedNest {
                    old_actor_id: ByteBuf::from(vec![0x11; 32]),
                    nest_id: ByteBuf::from(vec![0xa2; 32]),
                    nest_url: None,
                    extra: BTreeMap::new(),
                },
            ],
            ..Default::default()
        };
        let back = roundtrip(&owing);
        assert_eq!(back, owing);
        // Order is part of the contract: oldest pairing first.
        assert_eq!(back.owed_nests[0].nest_id.as_ref(), &[0xa1; 32]);

        let settle = SuccessionOwedSettleRequest {
            old_actor_id: ByteBuf::from(vec![0x11; 32]),
            nest_id: ByteBuf::from(vec![0xa1; 32]),
            extra: BTreeMap::new(),
        };
        assert_eq!(roundtrip(&settle), settle);
        assert_eq!(
            roundtrip(&SuccessionOwedSettleReply::default()),
            SuccessionOwedSettleReply::default()
        );
    }

    #[test]
    fn succession_status_predecessor_statements_are_additive_and_ordered() {
        // A reply naming no predecessor encodes without the key, so a caller
        // who succeeded nobody reads as "nothing to deliver".
        let none = SuccessionStatusReply {
            succeeded_at: Some(1_753_000_000),
            ..Default::default()
        };
        let none_bytes = fauna_core::encoding::canonical_encode(&none).expect("encode");
        assert!(
            !none_bytes
                .windows(b"predecessor_statements".len())
                .any(|w| w == b"predecessor_statements"),
            "an empty list must stay off the wire"
        );

        // Order is the contract: the first predecessor's hop first.
        let two_hops = SuccessionStatusReply {
            succeeded_at: Some(1_753_000_000),
            predecessor_statements: vec![ByteBuf::from(vec![1, 1]), ByteBuf::from(vec![2, 2])],
            ..Default::default()
        };
        let back = roundtrip(&two_hops);
        assert_eq!(back, two_hops);
        assert_eq!(back.predecessor_statements[0].as_ref(), &[1, 1]);
    }

    #[test]
    fn replacement_request_and_veto_roundtrip() {
        let req = ReplacementRequestRequest {
            registration: ByteBuf::from(vec![9, 8, 7]),
            extra: BTreeMap::new(),
        };
        assert_eq!(roundtrip(&req), req);

        let veto = ReplacementVetoRequest {
            actor_id: ByteBuf::from(vec![0x42; 32]),
            nonce: ByteBuf::from(vec![0x5c; 32]),
            signature: ByteBuf::from(vec![0x9c; 64]),
            extra: BTreeMap::new(),
        };
        assert_eq!(roundtrip(&veto), veto);
    }

    #[test]
    fn escrow_challenge_and_fetch_roundtrip() {
        let challenge = EscrowChallengeReply {
            nonce: ByteBuf::from(vec![0x7a; 32]),
            expires_at: 1_753_000_300,
            extra: BTreeMap::new(),
        };
        assert_eq!(roundtrip(&challenge), challenge);

        let fetch = EscrowFetchRequest {
            actor_id: ByteBuf::from(vec![0x42; 32]),
            nonce: ByteBuf::from(vec![0x7a; 32]),
            signature: ByteBuf::from(vec![0x9c; 64]),
            extra: BTreeMap::new(),
        };
        assert_eq!(roundtrip(&fetch), fetch);
    }

    #[test]
    fn succession_submit_and_lookup_roundtrip() {
        let submit = SuccessionSubmitRequest {
            statement: ByteBuf::from(vec![0xd1; 96]),
            extra: BTreeMap::new(),
        };
        assert_eq!(roundtrip(&submit), submit);

        let reply = SuccessionSubmitReply {
            new_actor_id: ByteBuf::from(vec![0xb2; 32]),
            succeeded_at: 1_753_400_000,
            extra: BTreeMap::new(),
        };
        assert_eq!(roundtrip(&reply), reply);

        let lookup = SuccessionLookupRequest {
            actor_id: ByteBuf::from(vec![0xa1; 32]),
            extra: BTreeMap::new(),
        };
        assert_eq!(roundtrip(&lookup), lookup);
    }

    #[test]
    fn a_never_succeeded_identity_encodes_as_an_empty_list_not_a_missing_field() {
        // The lookup's "no" answer must survive the round trip as an empty
        // list: a consumer distinguishes "never succeeded" from "the nest
        // withheld it" only by getting a well-formed empty reply.
        let empty = SuccessionLookupReply {
            statements: Vec::new(),
            extra: BTreeMap::new(),
        };
        assert_eq!(roundtrip(&empty), empty);

        let two = SuccessionLookupReply {
            statements: vec![ByteBuf::from(vec![0x01; 8]), ByteBuf::from(vec![0x02; 8])],
            extra: BTreeMap::new(),
        };
        // Order is load-bearing: a consumer walks the hops forward.
        let back = roundtrip(&two);
        assert_eq!(back.statements[0], ByteBuf::from(vec![0x01; 8]));
        assert_eq!(back.statements[1], ByteBuf::from(vec![0x02; 8]));
    }
}
