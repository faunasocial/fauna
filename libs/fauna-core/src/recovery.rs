//! The offline **RecoveryKey** root and the two succession-plane records it
//! authorizes — `docs/goal/behavior/identity-succession.md` (owner) and
//! `docs/goal/architecture/key-material-hierarchy.md` § Roots (the taxonomy row).
//!
//! The identity seed is deliberately powerful: possession *is* the account, and
//! every owner-audience key derives from it. Before this plane existed that power
//! was unbounded in time — no revocation existed at the root, so seed theft was
//! declared unrecoverable. The RecoveryKey bounds it: a 32-byte root held **only
//! offline** outranks the seed, so an identity can be *succeeded* (re-pointed to a
//! fresh keypair, the old key refused) in a way a seed thief cannot forge.
//!
//! Two ceremonies share the root, and this module supplies the primitives for
//! both:
//!
//! - **Succession (theft response)** — [`IdentitySuccession`], the statement whose
//!   `recovery_sig` is the sole load-bearing authorization.
//! - **Escrow restore (loss response)** — [`RecoveryKey::escrow_public`], the
//!   X25519 half the identity seed is HPKE-sealed to before it rests opaque on the
//!   home nest.
//!
//! Both are bound to an account by [`RecoveryKeyRegistration`].
//!
//! # Why detached, domain-tagged signatures rather than the `Signed` envelope
//!
//! Every other signed kind in the tree uses the single-signer sign-over-CID path
//! ([`crate::encoding::sign_envelope`]). These records cannot: each carries **two
//! or three signatures from different roles** (seed, RecoveryKey, prior
//! RecoveryKey; recovery, new, old), and the verdict rules differ per role — one
//! is load-bearing, another is mandatory-but-different-key, a third is explicitly
//! *never* load-bearing. A one-signature envelope cannot express that.
//!
//! So each signature covers `[tag_len] ‖ tag ‖ canonical_dag_cbor(record)` (see
//! [`signing_input`]) under its own **per-role domain tag**. The tag is what stops
//! a signature minted for one role being replayed as another — without it, the
//! seed's registration signature and a succession's `old_sig` would be
//! interchangeable bytes over related payloads.
//!
//! # The one rule a consumer must not get wrong
//!
//! `old_sig` is **never load-bearing** (`identity-succession.md:53`). A thief
//! holds the old key, so its presence must never upgrade trust and its absence
//! must never block a valid succession. That is enforced structurally here:
//! [`SignedIdentitySuccession::verify`] never reads `old_sig` at all — not even to
//! reject a malformed one. Display-only continuity goes through the separately
//! named [`SignedIdentitySuccession::old_sig_verifies`], whose `bool` no verdict
//! consumes.

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret as X25519StaticSecret};
use zeroize::{Zeroize, Zeroizing};

use crate::data::Timestamp;
use crate::encoding::canonical_encode;
use crate::error::{Error, Result};
use crate::identity::ActorId;

/// Domain-separation context deriving the RecoveryKey's **X25519 escrow KEM
/// secret** from the recovery root (`key-material-hierarchy.md` rule #3).
///
/// The seed-escrow blob is HPKE-sealed to the public half of this keypair and
/// rests opaque on the home nest; the phrase holder derives the secret half
/// offline to unseal it. Editing this string silently orphans every escrow blob
/// already written, so it is frozen for the life of the key and pinned by a
/// known-answer test.
pub const RECOVERY_ESCROW_X25519_CONTEXT: &str = "fauna recovery escrow x25519 2026-07-23";

/// Registration signature by the **identity seed** — binds the RecoveryKey to
/// this account.
pub const TAG_REGISTRATION_SEED: &str = "fauna.recovery.registration.seed.v1 2026-07-23";

/// Registration signature by the **RecoveryKey being registered** — proves
/// possession of the root, so a registration cannot name a key the registrant
/// does not hold.
pub const TAG_REGISTRATION_RECOVERY: &str = "fauna.recovery.registration.recovery.v1 2026-07-23";

/// Registration signature by the **prior RecoveryKey** — present only on a
/// RecoveryKey-authorized replacement (`identity-succession.md:35`: a new
/// registration at `seq+1`, co-signed old-R + new-R + seed), absent on a first
/// registration.
pub const TAG_REGISTRATION_PRIOR_RECOVERY: &str =
    "fauna.recovery.registration.prior-recovery.v1 2026-07-23";

/// Succession signature by the **RecoveryKey registered for `old_actor_id`** —
/// the sole load-bearing authorization.
pub const TAG_SUCCESSION_RECOVERY: &str = "fauna.succession.recovery.v1 2026-07-23";

/// Succession signature by the **successor identity** — mandatory; proves the
/// claimant holds the key the account is being pointed at.
pub const TAG_SUCCESSION_NEW: &str = "fauna.succession.new.v1 2026-07-23";

/// Succession signature by the **old identity** — optional, informational, and
/// never load-bearing (a thief can always produce it).
pub const TAG_SUCCESSION_OLD: &str = "fauna.succession.old.v1 2026-07-23";

/// How long a **seed-initiated** RecoveryKey replacement stays pending and
/// vetoable before it lands (`identity-succession.md:37`): 30 days.
///
/// Hard-coded bucket-1 constant (mirroring the backup custody grace) — not
/// config, not a client knob. The window exists because the seed alone is the
/// symmetric credential a thief may also hold; the registered RecoveryKey is
/// the asymmetric factor that can veto instantly, and only an *uncontested*
/// window lands the replacement.
pub const RECOVERY_REPLACE_GRACE_SECS: u64 = 30 * 24 * 60 * 60;

/// The byte string every succession-plane signature covers:
/// `[tag_len: u8] ‖ tag ‖ canonical_dag_cbor(record)`.
///
/// The single-byte length prefix makes the framing injective over the tag set
/// (every tag is far under 256 bytes), so no `(tag, record)` pair can be reframed
/// as a different `(tag', record')` — which is the whole point of tagging.
fn signing_input(tag: &str, canonical: &[u8]) -> Vec<u8> {
    let tag_len = u8::try_from(tag.len()).expect("succession domain tags are < 256 bytes");
    let mut out = Vec::with_capacity(1 + tag.len() + canonical.len());
    out.push(tag_len);
    out.extend_from_slice(tag.as_bytes());
    out.extend_from_slice(canonical);
    out
}

/// Verify one tagged detached signature under `pubkey`.
fn verify_tagged(pubkey: &[u8; 32], tag: &str, canonical: &[u8], sig: &[u8]) -> Result<()> {
    let vk = VerifyingKey::from_bytes(pubkey).map_err(|_| Error::InvalidSignature)?;
    let sig: [u8; 64] = sig.try_into().map_err(|_| Error::InvalidSignature)?;
    vk.verify_strict(&signing_input(tag, canonical), &Signature::from_bytes(&sig))
        .map_err(|_| Error::InvalidSignature)
}

/// The user's offline recovery root — 32 bytes, independent of the identity seed.
///
/// **Custody is offline-only and iron-clad** (`identity-succession.md:31`): the
/// secret is displayed once as 64-hex + QR (the recovery kit) and is **never**
/// stored on any device, in the account plane, or anywhere the identity seed unlocks. A
/// recovery secret the thief of a device can hold is no recovery secret, so there
/// is deliberately no "sync the recovery key" affordance and this type has no
/// serialization — only [`RecoveryKey::to_hex`] for the one-time kit display.
///
/// The root serves two roles under domain separation:
/// - **Ed25519 signer** — the root used *directly* as a seed (uniform with the
///   identity-secret idiom), signing only the succession-plane records.
/// - **X25519 escrow KEM secret** — derived via
///   [`RECOVERY_ESCROW_X25519_CONTEXT`], unsealing the seed-escrow blob.
pub struct RecoveryKey([u8; 32]);

impl RecoveryKey {
    /// Mint a fresh random recovery root.
    ///
    /// Fresh and independent of the identity seed by construction — the root is
    /// *not* derived from the seed, because a root the seed can reproduce is
    /// reachable by whoever stole the seed.
    pub fn generate() -> Self {
        let mut root = [0u8; 32];
        getrandom::fill(&mut root).expect("getrandom failed");
        Self(root)
    }

    /// Reconstruct a recovery root from raw bytes — the phrase-import path.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Reconstruct from the 64-hex kit form. Surrounding whitespace is trimmed.
    pub fn from_hex(hex: &str) -> std::result::Result<Self, crate::hex32::Hex32Error> {
        crate::hex32::decode(hex).map(Self)
    }

    /// Raw bytes of the root.
    pub fn to_bytes(&self) -> [u8; 32] {
        self.0
    }

    /// Lowercase-hex form — the recovery kit's printed/QR payload, displayed
    /// once and never persisted.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    fn signing_key(&self) -> SigningKey {
        SigningKey::from_bytes(&self.0)
    }

    /// The Ed25519 public key this root signs under — the value a
    /// [`RecoveryKeyRegistration`] publishes and every succession consumer
    /// verifies `recovery_sig` against.
    pub fn public(&self) -> [u8; 32] {
        self.signing_key().verifying_key().to_bytes()
    }

    /// The X25519 escrow **secret**, derived per
    /// [`RECOVERY_ESCROW_X25519_CONTEXT`]. Held only while unsealing the escrow
    /// blob; zeroized on drop.
    pub fn escrow_secret(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(blake3::derive_key(RECOVERY_ESCROW_X25519_CONTEXT, &self.0))
    }

    /// The X25519 escrow **public** key — what the identity seed is HPKE-sealed
    /// to before the blob is stored opaque on the home nest.
    pub fn escrow_public(&self) -> [u8; 32] {
        let secret = X25519StaticSecret::from(*self.escrow_secret());
        X25519PublicKey::from(&secret).to_bytes()
    }

    /// Sign `canonical` under `tag` with the recovery root.
    fn sign_tagged(&self, tag: &str, canonical: &[u8]) -> Vec<u8> {
        self.signing_key()
            .sign(&signing_input(tag, canonical))
            .to_bytes()
            .to_vec()
    }
}

impl Drop for RecoveryKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Binds a RecoveryKey to an account (`identity-succession.md:33`).
///
/// The home nest stores and serves these; the user's signed `Profile` carries the
/// same `recovery_pubkey` additively so peers cache the binding with the profile
/// they already hold. `seq` is monotonic per identity — a replacement lands at
/// `seq + 1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryKeyRegistration {
    /// The account this RecoveryKey protects.
    pub actor_id: ActorId,
    /// The Ed25519 public key of the RecoveryKey being registered.
    #[serde(with = "serde_bytes")]
    pub recovery_pubkey: [u8; 32],
    /// Monotonic per identity; a replacement registration lands at `seq + 1`.
    pub seq: u64,
    pub created_at: Timestamp,
}

/// A [`RecoveryKeyRegistration`] with its detached, per-role signatures.
///
/// `seed_sig` binds the key to the account; `recovery_sig` proves possession of
/// the root being registered. `prior_recovery_sig` is present only on a
/// RecoveryKey-authorized replacement — the seed-alone replacement path is a
/// *different* ceremony (30-day pending window, `RECOVERY_REPLACE_GRACE`) that
/// lands its record only after an uncontested window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedRecoveryKeyRegistration {
    pub registration: RecoveryKeyRegistration,
    /// By the identity seed, over [`TAG_REGISTRATION_SEED`].
    #[serde(with = "serde_bytes")]
    pub seed_sig: Vec<u8>,
    /// By the RecoveryKey being registered, over [`TAG_REGISTRATION_RECOVERY`].
    #[serde(with = "serde_bytes")]
    pub recovery_sig: Vec<u8>,
    /// By the prior RecoveryKey, over [`TAG_REGISTRATION_PRIOR_RECOVERY`] —
    /// present only on a RecoveryKey-authorized replacement.
    #[serde(default, with = "serde_bytes", skip_serializing_if = "Option::is_none")]
    pub prior_recovery_sig: Option<Vec<u8>>,
}

/// The stored head of an identity's registration chain, as a verifier knows it.
///
/// **Both halves must come from one read of the chain.** Carrying them as a
/// single value is deliberate: when they were independent `Option` parameters,
/// two dangerous caller states were representable —
///
/// - *pubkey but no seq*, which silently skipped the seq-advance check and
///   re-opened the archived-registration replay window;
/// - *neither, while a chain exists*, which re-admitted the seed-thief first
///   registration this plane exists to prevent.
///
/// The fold eliminates the first: a consumer now either holds a head (both
/// fields) or holds none, so the mixed state cannot be written. **It does
/// not eliminate the second.** `None` is also the honest first-registration
/// case — [`SignedRecoveryKeyRegistration::verify`]'s `None` arm and
/// [`verify_registration_chain`]'s first link both pass it legitimately — so
/// the type cannot distinguish "the store says no chain" from "the caller
/// never looked." What the type buys instead is a caller obligation: build
/// `prior` from a fresh read of the stored chain head on every call, so a
/// `None` reaching `verify` is always the former, never the latter.
/// `bins/fauna-nest/src/recovery_handlers.rs`'s registration handler is the
/// production discharge of that obligation — its one call site reads
/// `recovery_registration_head` and derives this value from that single read
/// before calling `verify`, the same way the succession side's carve-out
/// removal is recorded on [`SignedIdentitySuccession::verify`].
///
/// `seq` is one monotonic sequence per identity, **shared with succession
/// statements** — not a per-record counter.
///
/// **This type is also a wire shape:** the owner's client mirrors it into the
/// signed profile as `Profile.recovery_head` (`crate::data::Profile`), coupled
/// there for the same reason it is coupled here — a parallel pubkey/seq field
/// pair would let an editor that predates the seq half strip it while keeping
/// the pubkey. A field added here is therefore a profile
/// encoding change; the profile round-trip pins in `crate::encoding` will fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainHead {
    /// The RecoveryKey registered at this link.
    #[serde(with = "serde_bytes")]
    pub recovery_pubkey: [u8; 32],
    /// The `seq` this link landed at; a successor must advance past it.
    pub seq: u64,
}

impl ChainHead {
    pub fn new(recovery_pubkey: [u8; 32], seq: u64) -> Self {
        Self {
            recovery_pubkey,
            seq,
        }
    }
}

impl RecoveryKeyRegistration {
    /// Co-sign this registration with the identity seed and the RecoveryKey.
    ///
    /// `prior` co-signs when this registration *replaces* an earlier one under
    /// RecoveryKey authority; pass `None` for a first registration.
    pub fn sign(
        &self,
        identity: &SigningKey,
        recovery: &RecoveryKey,
        prior: Option<&RecoveryKey>,
    ) -> Result<SignedRecoveryKeyRegistration> {
        let canonical = canonical_encode(self)?;
        Ok(SignedRecoveryKeyRegistration {
            registration: self.clone(),
            seed_sig: identity
                .sign(&signing_input(TAG_REGISTRATION_SEED, &canonical))
                .to_bytes()
                .to_vec(),
            recovery_sig: recovery.sign_tagged(TAG_REGISTRATION_RECOVERY, &canonical),
            prior_recovery_sig: prior
                .map(|p| p.sign_tagged(TAG_REGISTRATION_PRIOR_RECOVERY, &canonical)),
        })
    }
}

impl SignedRecoveryKeyRegistration {
    /// Verify the registration.
    ///
    /// Always checks both mandatory signatures: `seed_sig` under
    /// `registration.actor_id` and `recovery_sig` under
    /// `registration.recovery_pubkey` (possession of the key being registered).
    ///
    /// `prior` is the [`ChainHead`] the consumer currently holds for this
    /// account, read as one value so the pubkey and the seq can never disagree
    /// about whether a chain exists:
    /// - `None` — a first registration. Refused if the record nonetheless carries
    ///   a `prior_recovery_sig` (nothing to check it against, so accepting it
    ///   would let an unverifiable signature ride along).
    /// - `Some(head)` — a replacement. `prior_recovery_sig` becomes **mandatory**
    ///   and must verify under `head.recovery_pubkey`, and `seq` must advance
    ///   past `head.seq`. Without this, a seed thief could re-register their own
    ///   RecoveryKey over the owner's — the exact takeover this plane exists to
    ///   prevent.
    pub fn verify(&self, prior: Option<&ChainHead>) -> Result<()> {
        let canonical = canonical_encode(&self.registration)?;

        verify_tagged(
            &self.registration.actor_id.0,
            TAG_REGISTRATION_SEED,
            &canonical,
            &self.seed_sig,
        )?;
        verify_tagged(
            &self.registration.recovery_pubkey,
            TAG_REGISTRATION_RECOVERY,
            &canonical,
            &self.recovery_sig,
        )?;

        match (prior, &self.prior_recovery_sig) {
            (None, None) => {}
            (None, Some(_)) => return Err(Error::InvalidSignature),
            (Some(_), None) => return Err(Error::InvalidSignature),
            (Some(head), Some(sig)) => verify_tagged(
                &head.recovery_pubkey,
                TAG_REGISTRATION_PRIOR_RECOVERY,
                &canonical,
                sig,
            )?,
        }

        // Reached with the same `prior` that gated the co-signature above, so a
        // caller can no longer present a prior key while withholding the seq it
        // came at — the state that used to skip this check entirely.
        if let Some(head) = prior
            && self.registration.seq <= head.seq
        {
            return Err(Error::InvalidSignature);
        }
        Ok(())
    }

    /// Verify a **seed-initiated** replacement — the *other* ceremony
    /// (`identity-succession.md:37`): the honest-R-loss arm, which carries no
    /// `prior_recovery_sig` because the prior key is exactly what was lost.
    ///
    /// **Passing this check does NOT land the record.** [`Self::verify`] is the
    /// only rule under which a record enters the chain directly; a record that
    /// merely passes here enters a pending store for
    /// [`RECOVERY_REPLACE_GRACE_SECS`], loudly notified, vetoable instantly by
    /// the current RecoveryKey ([`ReplacementVeto`]), and lands only after an
    /// uncontested window — at which point the *nest* appends it. That landing
    /// is home-nest-attested by construction: no third party can distinguish
    /// "waited 30 uncontested days" from a signature, so chain consumers get
    /// TOFU-grade assurance for this arm (`identity-succession.md:55` already
    /// grants exactly that grade to fetched chains). A thief holding only the
    /// seed can *start* this ceremony but cannot survive the window against an
    /// owner who holds the registered key — the asymmetry the design rests on.
    ///
    /// Checks: both mandatory signatures; `prior_recovery_sig` **must be
    /// absent** (a holder of the prior key has the immediate arm and must use
    /// it — accepting a co-signature here would let an unverified one ride);
    /// a chain head **must exist** (with no registered key there is nothing to
    /// replace and no veto authority to wait out — a first registration is
    /// [`Self::verify`]'s `None` arm, with no window); and `seq` must advance
    /// the head. Callers re-run this **at landing time** against the
    /// then-current head, so a chain that moved during the window (the
    /// RecoveryKey-authorized override) invalidates the pending record.
    pub fn verify_seed_alone(&self, head: &ChainHead) -> Result<()> {
        if self.prior_recovery_sig.is_some() {
            return Err(Error::InvalidSignature);
        }
        let canonical = canonical_encode(&self.registration)?;
        verify_tagged(
            &self.registration.actor_id.0,
            TAG_REGISTRATION_SEED,
            &canonical,
            &self.seed_sig,
        )?;
        verify_tagged(
            &self.registration.recovery_pubkey,
            TAG_REGISTRATION_RECOVERY,
            &canonical,
            &self.recovery_sig,
        )?;
        if self.registration.seq <= head.seq {
            return Err(Error::InvalidSignature);
        }
        Ok(())
    }
}

/// Domain tag for the seed-escrow fetch challenge — the proof of RecoveryKey
/// possession that gates the **pre-identity** blob fetch
/// (`identity-succession.md:44`).
pub const TAG_ESCROW_CHALLENGE: &str = "fauna.recovery.escrow.challenge.v1 2026-07-24";

/// The nonce challenge a client signs to fetch its seed-escrow blob.
///
/// Restore runs with **no account and no session**: total device loss leaves the
/// user holding only the recovery phrase, so the fetch cannot be authenticated by
/// a bearer. Instead the home nest issues a single-use, TTL-bounded `nonce` and
/// the client returns this record's signature under the RecoveryKey registered
/// for `actor_id` — which the nest verifies against its own registration chain.
///
/// **`actor_id` rides inside the signed record even though the nest already binds
/// the nonce to an account.** The nonce binding is store-side state; the
/// signature binding is carried by the bytes themselves, so a signature captured
/// for one account can never be presented as authorization for another, whatever
/// a future store refactor does with its keying.
///
/// There is deliberately no `SignedEscrowChallenge` wrapper type: unlike a
/// registration or a succession statement, this signature authorizes exactly one
/// RPC and is never stored, served, or re-verified by a third party — it rides
/// the request and dies with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscrowChallenge {
    /// The account whose escrow blob is being fetched.
    pub actor_id: ActorId,
    /// The nonce the home nest issued for this attempt.
    #[serde(with = "serde_bytes")]
    pub nonce: [u8; 32],
}

impl EscrowChallenge {
    /// Bind a freshly issued `nonce` to the account it was issued for.
    pub fn new(actor_id: ActorId, nonce: [u8; 32]) -> Self {
        Self { actor_id, nonce }
    }

    /// Sign the challenge with the offline recovery root.
    ///
    /// This is the only signature the restore path produces — the identity seed
    /// is precisely what the caller no longer has.
    pub fn sign(&self, recovery: &RecoveryKey) -> Result<Vec<u8>> {
        let canonical = canonical_encode(self)?;
        Ok(recovery.sign_tagged(TAG_ESCROW_CHALLENGE, &canonical))
    }

    /// Verify `sig` under the RecoveryKey public half the verifier holds for
    /// this account — on the nest, the head of the stored registration chain.
    ///
    /// Verifying against the **head** (rather than any historical link) is what
    /// retires a superseded recovery kit: once a replacement registration lands,
    /// the old kit signs valid bytes that no longer authorize anything.
    pub fn verify(&self, recovery_pubkey: &[u8; 32], sig: &[u8]) -> Result<()> {
        let canonical = canonical_encode(self)?;
        verify_tagged(recovery_pubkey, TAG_ESCROW_CHALLENGE, &canonical, sig)
    }
}

/// Domain tag for the replacement veto — the **current** RecoveryKey holder's
/// instant contest of a pending seed-initiated replacement
/// (`identity-succession.md:37`).
pub const TAG_REPLACEMENT_VETO: &str = "fauna.recovery.replacement.veto.v1 2026-07-24";

/// The nonce challenge the current RecoveryKey holder signs to veto a pending
/// seed-initiated replacement.
///
/// The veto must be **pre-identity**: the scenario it exists for is a seed
/// thief who has revoked every session and invoked the seed-signed lockout, so
/// the real owner may hold nothing but the recovery phrase. And it must be
/// **challenge-gated rather than a bare signed statement**: a replayable veto
/// would let a network adversary who captured one cancel any *future* honest
/// seed-initiated replacement of the same key forever — a permanent DoS on
/// exactly the honest-loss path the pending window exists to serve. The
/// single-use nonce makes each veto authorize exactly one live contest, and it
/// also spares the (possibly locked-out) holder from having to learn which
/// record is pending: a live veto contests *whatever* currently pends.
///
/// Same shape and reasoning as [`EscrowChallenge`] — `actor_id` rides inside
/// the signed record so a captured signature can never be presented for
/// another account, and there is no `Signed…` wrapper because the signature
/// authorizes one RPC and is never stored or re-verified by a third party. The
/// two ceremonies keep **disjoint domain tags and disjoint nonce pools** (the
/// nest holds a separate `ChallengeStore` instance per ceremony), so a nonce
/// or signature minted for the escrow fetch is never presentable to the veto,
/// and vice versa.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplacementVeto {
    /// The account whose pending replacement is being vetoed.
    pub actor_id: ActorId,
    /// The nonce the home nest issued for this attempt.
    #[serde(with = "serde_bytes")]
    pub nonce: [u8; 32],
}

impl ReplacementVeto {
    /// Bind a freshly issued `nonce` to the account it was issued for.
    pub fn new(actor_id: ActorId, nonce: [u8; 32]) -> Self {
        Self { actor_id, nonce }
    }

    /// Sign the veto with the offline recovery root.
    pub fn sign(&self, recovery: &RecoveryKey) -> Result<Vec<u8>> {
        let canonical = canonical_encode(self)?;
        Ok(recovery.sign_tagged(TAG_REPLACEMENT_VETO, &canonical))
    }

    /// Verify `sig` under the RecoveryKey public half the verifier holds for
    /// this account — on the nest, the **head** of the stored registration
    /// chain (the same head-only rule as the escrow fetch: a superseded kit
    /// signs valid bytes that authorize nothing).
    pub fn verify(&self, recovery_pubkey: &[u8; 32], sig: &[u8]) -> Result<()> {
        let canonical = canonical_encode(self)?;
        verify_tagged(recovery_pubkey, TAG_REPLACEMENT_VETO, &canonical, sig)
    }
}

/// The succession statement (`identity-succession.md:49`) — the verifiable
/// old→new link that home nests, federation peers, MLS members and contacts all
/// consume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentitySuccession {
    /// The identity being succeeded.
    pub old_actor_id: ActorId,
    /// The successor identity — a genuinely new actor_id, so `actor_id ≡ Ed25519
    /// pubkey` stays true everywhere and signature verification stays
    /// self-describing.
    pub new_actor_id: ActorId,
    /// The RecoveryKey registered for `old_actor_id` at this `seq` — carried so a
    /// consumer that already holds the binding can verify with no fetch.
    #[serde(with = "serde_bytes")]
    pub recovery_pubkey: [u8; 32],
    /// Advances the registration chain the consumer last saw.
    pub seq: u64,
    pub created_at: Timestamp,
}

/// An [`IdentitySuccession`] with its detached, per-role signatures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedIdentitySuccession {
    pub statement: IdentitySuccession,
    /// By the RecoveryKey registered for `old_actor_id`, over
    /// [`TAG_SUCCESSION_RECOVERY`]. **Mandatory; the load-bearing
    /// authorization.**
    #[serde(with = "serde_bytes")]
    pub recovery_sig: Vec<u8>,
    /// By the successor identity, over [`TAG_SUCCESSION_NEW`]. **Mandatory** —
    /// prevents pointing an account at a key the claimant does not hold.
    #[serde(with = "serde_bytes")]
    pub new_sig: Vec<u8>,
    /// By the old identity, over [`TAG_SUCCESSION_OLD`]. **Optional and never
    /// load-bearing** — see [`SignedIdentitySuccession::old_sig_verifies`].
    #[serde(default, with = "serde_bytes", skip_serializing_if = "Option::is_none")]
    pub old_sig: Option<Vec<u8>>,
}

impl IdentitySuccession {
    /// Sign this statement.
    ///
    /// `old` is the old identity key when the owner still holds it (the theft
    /// case, and the loss case after an escrow restore); pass `None` when they do
    /// not. Passing it changes no consumer's verdict — it is continuity
    /// information only.
    pub fn sign(
        &self,
        recovery: &RecoveryKey,
        new_identity: &SigningKey,
        old_identity: Option<&SigningKey>,
    ) -> Result<SignedIdentitySuccession> {
        let canonical = canonical_encode(self)?;
        Ok(SignedIdentitySuccession {
            statement: self.clone(),
            recovery_sig: recovery.sign_tagged(TAG_SUCCESSION_RECOVERY, &canonical),
            new_sig: new_identity
                .sign(&signing_input(TAG_SUCCESSION_NEW, &canonical))
                .to_bytes()
                .to_vec(),
            old_sig: old_identity.map(|k| {
                k.sign(&signing_input(TAG_SUCCESSION_OLD, &canonical))
                    .to_bytes()
                    .to_vec()
            }),
        })
    }
}

impl SignedIdentitySuccession {
    /// The verification rule every consumer runs (`identity-succession.md:55`).
    ///
    /// The statement is valid iff:
    /// 1. `recovery_sig` verifies under `known.recovery_pubkey` — the binding
    ///    the consumer knows for `old_actor_id`, from its cached
    ///    `Profile.recovery_head`, a previously-seen registration, or a fetch of
    ///    the registration chain from the old identity's home nest. **The caller
    ///    supplies it; the statement's own `recovery_pubkey` field is never
    ///    trusted for this** (a thief could put their own key there), though a
    ///    mismatch between the two is refused so the carried value cannot
    ///    mislead a later reader.
    /// 2. `new_sig` verifies under `new_actor_id`.
    /// 3. `seq` advances past `known.seq` — the chain the consumer last saw.
    ///
    /// `old_sig` is deliberately **not** consulted — not even to reject a
    /// malformed one. A thief can always produce it, so its presence must never
    /// upgrade trust and its absence must never block a valid succession.
    ///
    /// **Why this takes a whole [`ChainHead`]** (it used to take a bare pubkey
    /// plus `Option<u64>`, defended on the ground that a `Profile`-cached
    /// consumer held a pubkey and no seq): the profile now mirrors the coupled
    /// head itself (`Profile.recovery_head` — the review closed the
    /// pubkey-only mirror as unconsumable), so **every** source a consumer can
    /// know the binding from carries both halves — the chain, a registration
    /// record, the mirrored profile head. Pubkey-without-seq is no longer a
    /// representable consumer state, and the seq-advance check can no longer be
    /// silently off. A consumer with no head at all has nothing to verify
    /// against and must fetch the chain first
    /// ([`verify_registration_chain`] / [`verify_succession_against_chain`]).
    pub fn verify(&self, known: &ChainHead) -> Result<()> {
        if self.statement.recovery_pubkey != known.recovery_pubkey {
            return Err(Error::InvalidSignature);
        }
        let canonical = canonical_encode(&self.statement)?;
        verify_tagged(
            &known.recovery_pubkey,
            TAG_SUCCESSION_RECOVERY,
            &canonical,
            &self.recovery_sig,
        )?;
        verify_tagged(
            &self.statement.new_actor_id.0,
            TAG_SUCCESSION_NEW,
            &canonical,
            &self.new_sig,
        )?;
        if self.statement.seq <= known.seq {
            return Err(Error::InvalidSignature);
        }
        Ok(())
    }

    /// Whether `new_sig` verifies under the statement's own `new_actor_id` —
    /// the successor's **self-claim**, and nothing more.
    ///
    /// ⚠ **This is not [`Self::verify`] and authorizes no succession.** Anyone
    /// can mint a key and sign "I succeed X"; only `recovery_sig` under the
    /// known chain head says X's owner agreed. What this does establish is
    /// narrower and unforgeable by anyone who lacks `new_actor_id`'s key: *the
    /// holder of that key named `old_actor_id` as the identity it succeeds*.
    /// That is the whole question for the one consumer this exists for — the
    /// successor itself, deciding whether a stored profile signed by
    /// `old_actor_id` is its own inherited row (`profile.md` § After an
    /// identity succession → the admission rule): its own signature is its own
    /// word, and a hostile nest cannot produce it. A device in that position
    /// holds no [`ChainHead`] for the predecessor, so the full rule is not
    /// available to it — and a third party's verdict about the *account* must
    /// never be built on this.
    pub fn verify_new_sig(&self) -> Result<()> {
        let canonical = canonical_encode(&self.statement)?;
        verify_tagged(
            &self.statement.new_actor_id.0,
            TAG_SUCCESSION_NEW,
            &canonical,
            &self.new_sig,
        )
    }

    /// Whether `old_sig` is present and verifies under `old_actor_id` — **display
    /// continuity only**.
    ///
    /// Deliberately separate from [`SignedIdentitySuccession::verify`] and
    /// deliberately returning `bool` rather than `Result`, so no call site can
    /// thread it into an authorization decision by accident. A client may render
    /// "the previous key also signed this"; nothing may *require* it.
    pub fn old_sig_verifies(&self) -> bool {
        let Some(sig) = &self.old_sig else {
            return false;
        };
        let Ok(canonical) = canonical_encode(&self.statement) else {
            return false;
        };
        verify_tagged(
            &self.statement.old_actor_id.0,
            TAG_SUCCESSION_OLD,
            &canonical,
            sig,
        )
        .is_ok()
    }
}

/// Longest registration chain a consumer will walk for an identity.
///
/// The chain arrives from an **untrusted** source (a federation peer's push, or
/// an anonymous fetch from a nest that may be hostile), so the walk needs a work
/// bound that does not depend on the sender's honesty. Generous by design: a
/// RecoveryKey replacement is a rare, deliberate ceremony, so a real chain is
/// units long — this is a DoS ceiling, not a product limit.
pub const MAX_VERIFIED_CHAIN_LEN: usize = 64;

/// The predecessors `signer` can prove from landed succession statements
/// alone, nearest hop first — **the one shared link verifier** for every
/// reader that asks *does `signer` own what an earlier identity signed*: the
/// profile plane's inherited-base admission (`profile.md` § After an identity
/// succession → *A successor device with no recorded succession link*) and the
/// change-row reader's succession crossing (`mls-group-key-material.md` § M2 →
/// *Writer-signed change records*, ruling (8)(b)).
///
/// `base_actor` is the identity the walk starts at, and `statements` is the
/// path forward from it, oldest first (what `fauna.recovery.succession.lookup`
/// answers for `base_actor`). The walk demands an unbroken chain — each link's
/// `old_actor_id` is the previous link's `new_actor_id`, starting at
/// `base_actor` — that **ends at `signer`**, with every link's `new_sig`
/// verifying under that link's own successor. Anything else proves nothing and
/// answers empty: a broken, reordered, truncated or over-long reply fails
/// closed.
///
/// **Why `new_sig` alone is enough here, and only here.** The question is not
/// "is this succession authorized" (that is `recovery_sig` under the known
/// chain head — [`SignedIdentitySuccession::verify`] — and a reader of someone
/// else's set, or a linkless device, holds no head to run it against). It is
/// "is what this identity signed `signer`'s own". The last link is the
/// signer's own signature naming the identity it succeeded: its own word, which
/// a nest cannot mint. Each earlier link is the word of an identity the
/// signer's chain already vouched for. The weakest party this trusts is a thief
/// who held a predecessor's key — and that party could already sign *as* that
/// predecessor outright. So the walk widens what can be admitted by nothing.
pub fn proven_predecessors(
    signer: &ActorId,
    base_actor: &ActorId,
    statements: &[SignedIdentitySuccession],
) -> Vec<ActorId> {
    let mut chain = Vec::new();
    let mut expected_old = *base_actor;
    for signed in statements.iter().take(MAX_VERIFIED_CHAIN_LEN) {
        let link = &signed.statement;
        if link.old_actor_id != expected_old
            || link.new_actor_id == link.old_actor_id
            || signed.verify_new_sig().is_err()
        {
            return Vec::new();
        }
        chain.push(link.old_actor_id);
        if link.new_actor_id == *signer {
            chain.reverse();
            return chain;
        }
        expected_old = link.new_actor_id;
    }
    Vec::new()
}

/// [`proven_predecessors`] over the statements **as carried** — each the
/// verbatim canonical bytes its submitter signed, oldest first (the shape of
/// `SuccessionLookupReply::statements` and of a roster row's carried chain).
/// The walk starts at the first statement's own `old_actor_id`, so the whole
/// carried chain must be unbroken and end at `signer`; a statement that does
/// not decode proves nothing, like any other break.
pub fn proven_predecessors_carried<B: AsRef<[u8]>>(
    signer: &ActorId,
    statements: &[B],
) -> Vec<ActorId> {
    let Ok(decoded) = statements
        .iter()
        .map(|b| crate::encoding::canonical_decode::<SignedIdentitySuccession>(b.as_ref()))
        .collect::<Result<Vec<_>>>()
    else {
        return Vec::new();
    };
    match decoded.first() {
        Some(first) => {
            let base = first.statement.old_actor_id;
            proven_predecessors(signer, &base, &decoded)
        }
        None => Vec::new(),
    }
}

/// The outcome of [`verify_succession_against_chain`] — a succession a consumer
/// has established under that function's anchoring contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedSuccession {
    /// The superseded identity. Equal to every chain record's `actor_id`.
    pub old_actor_id: ActorId,
    /// The identity that now holds the account.
    pub new_actor_id: ActorId,
    /// The statement's position in the old identity's one monotonic chain.
    pub seq: u64,
    /// The chain head the statement was authorized under — established by the
    /// walk, never the value the statement carried. Consumers that persist a
    /// known head (the anchor a later chain must extend) persist exactly this.
    pub chain_head: ChainHead,
}

/// Walk a registration chain for `old` and return the head it establishes.
///
/// **What a chain proves — and against whom.** Every
/// [`SignedRecoveryKeyRegistration`] carries `seed_sig` over
/// [`TAG_REGISTRATION_SEED`] under its own `registration.actor_id`, so a chain
/// binds itself to the identity it names: a party that does **not** hold the
/// identity seed cannot substitute a chain for that identity. But the
/// adversary of the whole succession plane (`identity-succession.md:28`) is a
/// **seed thief** — and a seed holder mints a perfectly self-consistent chain:
/// a first link has no prior co-signature and no seq constraint, and a
/// seed-alone link needs only the two signatures the thief can produce. A
/// self-consistent chain therefore proves *seed possession*, never *RecoveryKey
/// legitimacy*.
///
/// **The anchoring contract every caller must keep.** The chain handed here
/// must come from a source the caller trusts for this identity under
/// `identity-succession.md:55`'s consumer rule — its own store (the home
/// nest), or a fetch from the home nest the caller *already knew* for the
/// identity — never from the party asserting the succession. A pushed or
/// relayed chain is a hint, not evidence. And a caller that has ever learned
/// this identity's chain head must pass it as `known`: the walk then requires
/// the chain to **visit** that exact head, so a chain that *rewrites or
/// truncates* what the caller already saw is refused even when the serving
/// source has turned hostile. `known = None` grants first-contact TOFU — the
/// grade the doc assigns to a first fetch, and all a consumer with no history
/// can have.
///
/// **What the `known` head does NOT stop, and why the anchoring contract is the
/// real guard.** The plane's adversary holds the seed, so it can *extend* the
/// genuine chain with a seed-alone link that **visits** the known head (step 3's
/// legitimate honest-loss arm) — that chain passes here, and it must, because
/// the same shape is how an honest RecoveryKey loss recovers. The known head
/// therefore bounds a hostile *chain-server* but not a seed thief; only the
/// caller's anchoring contract — proving the *identity* of the box it fetches
/// from, so a thief cannot serve the extension in the first place — closes the
/// takeover.
///
/// The walk, oldest link first:
/// 1. every record names `old` (a chain for another account proves nothing
///    about this one);
/// 2. the first link verifies with no prior head, each later link against the
///    head the previous one established;
/// 3. a later link may be **either** RecoveryKey-authorized
///    ([`SignedRecoveryKeyRegistration::verify`]) **or** a landed seed-alone
///    replacement ([`SignedRecoveryKeyRegistration::verify_seed_alone`]). The
///    second arm is home-nest-attested rather than signature-provable — which
///    is precisely why the anchoring contract above matters: fetched from the
///    identity's own home nest it is the honest-RecoveryKey-loss path at the
///    TOFU grade the doc grants; delivered by anyone else it is attested by
///    nothing;
/// 4. if `known` is present, some link's resulting head must equal it exactly
///    — a chain that rewrites, truncates, or diverges from what the caller
///    already saw is refused.
///
/// Every failure returns [`Error::InvalidSignature`], deliberately without
/// distinguishing which step refused: the input may be attacker-controlled,
/// and a finer-grained verdict would be an oracle for probing an account's
/// chain.
pub fn verify_registration_chain(
    old: ActorId,
    chain: &[SignedRecoveryKeyRegistration],
    known: Option<&ChainHead>,
) -> Result<ChainHead> {
    if chain.is_empty() || chain.len() > MAX_VERIFIED_CHAIN_LEN {
        return Err(Error::InvalidSignature);
    }

    let mut head: Option<ChainHead> = None;
    let mut visited_known = false;
    for link in chain {
        if link.registration.actor_id != old {
            return Err(Error::InvalidSignature);
        }
        match head {
            // A first registration: no prior key exists, so a co-signature would
            // be uncheckable and `verify` refuses one outright.
            None => link.verify(None)?,
            Some(prior) => link
                .verify(Some(&prior))
                // The landed seed-alone arm carries no `prior_recovery_sig`,
                // which the strict rule above refuses by design. It still has to
                // clear both mandatory signatures and advance `seq`.
                .or_else(|_| link.verify_seed_alone(&prior))?,
        }
        let next = ChainHead::new(link.registration.recovery_pubkey, link.registration.seq);
        if let Some(k) = known
            && next == *k
        {
            visited_known = true;
        }
        head = Some(next);
    }
    if known.is_some() && !visited_known {
        return Err(Error::InvalidSignature);
    }

    // `chain` is non-empty, so the walk set a head.
    head.ok_or(Error::InvalidSignature)
}

/// Verify a succession statement against the old identity's **registration
/// chain** — [`verify_registration_chain`]'s walk (including its anchoring
/// contract and `known`-head rule, which callers must read) followed by the
/// consumer rule of `identity-succession.md:55`: the statement verifies against
/// the resulting **head** — head, not any earlier link, because retiring a kit
/// is precisely what a replacement does, and a retired RecoveryKey goes on
/// producing valid signatures forever.
pub fn verify_succession_against_chain(
    statement: &SignedIdentitySuccession,
    chain: &[SignedRecoveryKeyRegistration],
    known: Option<&ChainHead>,
) -> Result<VerifiedSuccession> {
    let old = statement.statement.old_actor_id;
    let head = verify_registration_chain(old, chain, known)?;
    statement.verify(&head)?;

    Ok(VerifiedSuccession {
        old_actor_id: old,
        new_actor_id: statement.statement.new_actor_id,
        seq: statement.statement.seq,
        chain_head: head,
    })
}

/// A parsed recovery-kit field: the 64-hex recovery secret plus the account the
/// kit names, when the payload carried one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedRecoveryKit {
    /// 64-char hex recovery root, as entered.
    pub secret: String,
    /// Actor id from an `&actor=` param, when present and non-empty — says
    /// *which* account is being recovered, and stays authoritative for that
    /// (the escrow blob is AAD-bound to it).
    pub actor_id: Option<String>,
    /// Handle from a `&handle=` param, when present and non-empty. This is the
    /// half that says *where* the account lives: only a handle's `@domain`
    /// locates the home nest a restore must talk to, so an actor id alone
    /// leaves a restore screen still needing to ask
    /// (`onboarding.md` § 1 Identity, `recovery-entry-account-field`). Taken
    /// verbatim, exactly as `fauna://identity`'s own `handle=` is.
    pub handle: Option<String>,
}

impl ImportedRecoveryKit {
    /// Flat `[secret, actor_id, handle]` projection for the wasm + UniFFI faces
    /// (absent halves = `""`). An empty list signals a parse failure —
    /// mirrors [`crate::identity_qr::ImportedIdentity::into_parts`].
    pub fn into_parts(self) -> Vec<String> {
        vec![
            self.secret,
            self.actor_id.unwrap_or_default(),
            self.handle.unwrap_or_default(),
        ]
    }
}

/// Recovery-kit QR/URI codec — the deliberate twin of
/// [`crate::identity_qr::IdentityQr`], on its own `fauna://recovery` host so a
/// recovery secret can never be mistaken for (or pasted as) an identity secret.
pub struct RecoveryKitQr;

impl RecoveryKitQr {
    /// Encode a 64-hex recovery secret into a `fauna://recovery` URI, optionally
    /// carrying the actor id the kit belongs to and the handle it answers to.
    ///
    /// The two optional halves answer different questions and a minting surface
    /// may know either, both, or neither: `actor=` says **which** account (and
    /// stays authoritative — the escrow blob is AAD-bound to it), `handle=`
    /// says **where** it lives (its `@domain` is the only thing that locates
    /// the home nest a restore connects to). The Settings ceremony knows both;
    /// the onboarding kit screen knows only the actor, because no handle is
    /// chosen yet at its ratified position — a restore from that QR asks
    /// (`onboarding.md` § 1 Identity).
    ///
    /// Empty strings are treated as absence, so a caller formatting an unset
    /// field never emits `&handle=` with nothing after it.
    pub fn to_uri(secret_hex: &str, actor_id_hex: Option<&str>, handle: Option<&str>) -> String {
        let mut uri = format!("fauna://recovery?secret={secret_hex}");
        if let Some(a) = actor_id_hex.filter(|a| !a.is_empty()) {
            uri.push_str("&actor=");
            uri.push_str(a);
        }
        if let Some(h) = handle.filter(|h| !h.is_empty()) {
            uri.push_str("&handle=");
            uri.push_str(h);
        }
        uri
    }
}

/// Parse a recovery-kit field (pasted string or scanned QR payload).
///
/// Rides the same [`crate::secret_uri::parse_fauna_secret_uri`] grammar
/// [`crate::identity_qr::parse_import_input`] does — bare 64-hex, the query
/// form, and the colon form — so every app reads one grammar instead of
/// hand-rolling three (priority #4: the richest existing shape, lifted rather
/// than re-derived). Case-insensitive in the `fauna://recovery` scheme/host;
/// returns `None` on any unrecognized input.
///
/// A `fauna://identity` payload is deliberately **not** accepted: the two
/// secrets have different custody rules, and silently taking one for the other
/// would let a user register their identity seed as their own recovery root —
/// which would defeat the plane entirely.
pub fn parse_recovery_kit_input(input: &str) -> Option<ImportedRecoveryKit> {
    let (secret, params) = crate::secret_uri::parse_fauna_secret_uri(input, "fauna://recovery")?;
    let param = |key: &str| params.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
    Some(ImportedRecoveryKit {
        secret,
        actor_id: param("actor").filter(|a| !a.is_empty()).map(str::to_string),
        handle: param("handle")
            .filter(|h| !h.is_empty())
            .map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::ActorKeypair;

    fn root(byte: u8) -> RecoveryKey {
        RecoveryKey::from_bytes([byte; 32])
    }

    fn registration(actor: ActorId, recovery: &RecoveryKey, seq: u64) -> RecoveryKeyRegistration {
        RecoveryKeyRegistration {
            actor_id: actor,
            recovery_pubkey: recovery.public(),
            seq,
            created_at: Timestamp(1_753_000_000),
        }
    }

    fn succession(
        old: ActorId,
        new: ActorId,
        recovery: &RecoveryKey,
        seq: u64,
    ) -> IdentitySuccession {
        IdentitySuccession {
            old_actor_id: old,
            new_actor_id: new,
            recovery_pubkey: recovery.public(),
            seq,
            created_at: Timestamp(1_753_000_000),
        }
    }

    // ---- Known-answer pins (golden bytes) ----
    //
    // These freeze the wire/derivation constants for the life of the key. A diff
    // here is never "update the expected value" — it means an already-issued
    // recovery kit no longer opens its escrow blob, or an already-published
    // registration no longer verifies.

    #[test]
    fn escrow_derivation_is_pinned() {
        let key = root(0x11);
        assert_eq!(
            hex::encode(*key.escrow_secret()),
            hex::encode(blake3::derive_key(
                "fauna recovery escrow x25519 2026-07-23",
                &[0x11u8; 32]
            )),
            "escrow secret must be derive_key(context, root) with the ratified context string"
        );
        // Full pin of both halves, so a change to *either* the context string or
        // the X25519 public-derivation convention is caught.
        assert_eq!(
            hex::encode(*key.escrow_secret()),
            "5b934a765a348d3fa3fb5a99c0ec5e9e84951cd24b0edca834a00d6485978499"
        );
        assert_eq!(
            hex::encode(key.escrow_public()),
            "a080343057d5c8aa35466a084c764cb439122636db7733dafbf5e1ec33085648"
        );
    }

    #[test]
    fn escrow_key_is_domain_separated_from_the_signing_role() {
        // The same 32 bytes serve two roles; if the escrow half were the root
        // itself, holding one role's material would hand over the other.
        let key = root(0x11);
        assert_ne!(*key.escrow_secret(), key.to_bytes());
        assert_ne!(key.escrow_public(), key.public());
    }

    #[test]
    fn recovery_public_is_the_root_used_directly_as_an_ed25519_seed() {
        let key = root(0x11);
        assert_eq!(
            key.public(),
            SigningKey::from_bytes(&[0x11u8; 32])
                .verifying_key()
                .to_bytes()
        );
    }

    #[test]
    fn signing_input_framing_is_injective_over_the_tag_set() {
        // Without the length prefix, a tag that is a prefix of another plus a
        // shifted record boundary could produce identical signing input.
        assert_ne!(
            signing_input("fauna.a", b"bc"),
            signing_input("fauna.ab", b"c")
        );
    }

    // ---- Registration ----

    #[test]
    fn a_first_registration_verifies_under_both_mandatory_signers() {
        let identity = ActorKeypair::generate();
        let recovery = root(0x22);
        let reg = registration(identity.actor_id(), &recovery, 0);
        let signed = reg.sign(identity.signing_key(), &recovery, None).unwrap();

        signed.verify(None).expect("first registration verifies");
    }

    #[test]
    fn a_registration_signed_by_a_different_recovery_key_is_refused() {
        // The `recovery_sig` must prove possession of the key actually named.
        let identity = ActorKeypair::generate();
        let named = root(0x22);
        let other = root(0x23);
        let reg = registration(identity.actor_id(), &named, 0);
        let mut signed = reg.sign(identity.signing_key(), &named, None).unwrap();
        signed.recovery_sig = other.sign_tagged(
            TAG_REGISTRATION_RECOVERY,
            &canonical_encode(&signed.registration).unwrap(),
        );

        assert!(signed.verify(None).is_err());
    }

    #[test]
    fn a_registration_signed_by_the_wrong_identity_is_refused() {
        let identity = ActorKeypair::generate();
        let thief = ActorKeypair::generate();
        let recovery = root(0x22);
        let reg = registration(identity.actor_id(), &recovery, 0);
        let signed = reg.sign(thief.signing_key(), &recovery, None).unwrap();

        assert!(signed.verify(None).is_err());
    }

    #[test]
    fn a_replacement_requires_the_prior_recovery_key_co_signature() {
        // This is the seed-thief takeover the plane exists to prevent: a thief
        // holds the seed and can mint a fresh RecoveryKey, but cannot co-sign
        // with the RecoveryKey they do not hold.
        let identity = ActorKeypair::generate();
        let old_recovery = root(0x22);
        let thief_recovery = root(0x99);
        let reg = registration(identity.actor_id(), &thief_recovery, 1);

        let unauthorized = reg
            .sign(identity.signing_key(), &thief_recovery, None)
            .unwrap();
        assert!(
            unauthorized
                .verify(Some(&ChainHead::new(old_recovery.public(), 0)))
                .is_err(),
            "a seed-only replacement over an existing RecoveryKey must be refused"
        );

        let authorized = reg
            .sign(identity.signing_key(), &thief_recovery, Some(&old_recovery))
            .unwrap();
        authorized
            .verify(Some(&ChainHead::new(old_recovery.public(), 0)))
            .expect("a prior-RecoveryKey-authorized replacement verifies");
    }

    #[test]
    fn a_replacement_co_signed_by_the_wrong_prior_key_is_refused() {
        let identity = ActorKeypair::generate();
        let real_prior = root(0x22);
        let wrong_prior = root(0x33);
        let new_recovery = root(0x44);
        let reg = registration(identity.actor_id(), &new_recovery, 1);
        let signed = reg
            .sign(identity.signing_key(), &new_recovery, Some(&wrong_prior))
            .unwrap();

        assert!(
            signed
                .verify(Some(&ChainHead::new(real_prior.public(), 0)))
                .is_err()
        );
    }

    #[test]
    fn a_registration_must_advance_the_chain() {
        let identity = ActorKeypair::generate();
        let prior = root(0x22);
        let new_recovery = root(0x44);
        // Same seq as the one already seen — a replay of an older link.
        let reg = registration(identity.actor_id(), &new_recovery, 3);
        let signed = reg
            .sign(identity.signing_key(), &new_recovery, Some(&prior))
            .unwrap();

        assert!(
            signed
                .verify(Some(&ChainHead::new(prior.public(), 3)))
                .is_err()
        );
        signed
            .verify(Some(&ChainHead::new(prior.public(), 2)))
            .expect("seq 3 advances past 2");
    }

    #[test]
    fn a_stray_prior_signature_on_a_first_registration_is_refused() {
        let identity = ActorKeypair::generate();
        let recovery = root(0x22);
        let stray = root(0x55);
        let reg = registration(identity.actor_id(), &recovery, 0);
        let signed = reg
            .sign(identity.signing_key(), &recovery, Some(&stray))
            .unwrap();

        assert!(signed.verify(None).is_err());
    }

    // ---- Escrow fetch challenge ----

    #[test]
    fn an_escrow_challenge_signature_verifies_under_the_registered_recovery_key() {
        let actor = ActorKeypair::generate().actor_id();
        let recovery = root(0x22);
        let challenge = EscrowChallenge::new(actor, [0x7Au8; 32]);

        let sig = challenge.sign(&recovery).unwrap();
        challenge
            .verify(&recovery.public(), &sig)
            .expect("the registered recovery key authorizes the fetch");
    }

    #[test]
    fn an_escrow_challenge_signature_does_not_transfer_to_another_nonce() {
        // A captured signature must not authorize a *later* fetch: the nonce is
        // single-use nest-side, and it is covered by the signature so a replay
        // cannot be re-framed onto a fresh challenge either.
        let actor = ActorKeypair::generate().actor_id();
        let recovery = root(0x22);
        let issued = EscrowChallenge::new(actor, [0x7Au8; 32]);
        let sig = issued.sign(&recovery).unwrap();

        let reissued = EscrowChallenge::new(actor, [0x7Bu8; 32]);
        assert!(
            reissued.verify(&recovery.public(), &sig).is_err(),
            "a signature over one nonce must not authorize another"
        );
    }

    #[test]
    fn an_escrow_challenge_signature_does_not_transfer_to_another_account() {
        // The account rides *inside* the signed record, so a signature the owner
        // produced for their own restore can never be presented as authorization
        // over someone else's escrow blob.
        let mine = ActorKeypair::generate().actor_id();
        let theirs = ActorKeypair::generate().actor_id();
        let recovery = root(0x22);
        let nonce = [0x7Au8; 32];

        let sig = EscrowChallenge::new(mine, nonce).sign(&recovery).unwrap();
        assert!(
            EscrowChallenge::new(theirs, nonce)
                .verify(&recovery.public(), &sig)
                .is_err()
        );
    }

    #[test]
    fn a_different_recovery_root_cannot_authorize_an_escrow_fetch() {
        // The whole gate: only the offline root registered for the account opens
        // the escrow plane — the seed thief holds neither.
        let actor = ActorKeypair::generate().actor_id();
        let registered = root(0x22);
        let attacker = root(0x99);
        let challenge = EscrowChallenge::new(actor, [0x7Au8; 32]);

        let sig = challenge.sign(&attacker).unwrap();
        assert!(challenge.verify(&registered.public(), &sig).is_err());
    }

    #[test]
    fn an_escrow_challenge_signature_is_not_a_registration_signature() {
        // The escrow tag is a seventh role in the same tag set: bytes signed to
        // authorize a fetch must never be replayable as a registration
        // co-signature (or vice-versa).
        let actor = ActorKeypair::generate().actor_id();
        let recovery = root(0x22);
        let challenge = EscrowChallenge::new(actor, [0x7Au8; 32]);
        let canonical = canonical_encode(&challenge).unwrap();
        let sig = challenge.sign(&recovery).unwrap();

        assert!(verify_tagged(&recovery.public(), TAG_ESCROW_CHALLENGE, &canonical, &sig).is_ok());
        for foreign in [
            TAG_REGISTRATION_RECOVERY,
            TAG_REGISTRATION_PRIOR_RECOVERY,
            TAG_SUCCESSION_RECOVERY,
        ] {
            assert!(
                verify_tagged(&recovery.public(), foreign, &canonical, &sig).is_err(),
                "an escrow-challenge signature must not verify under {foreign}"
            );
        }
    }

    // ---- Seed-alone replacement (the pending-window arm) ----

    #[test]
    fn a_seed_alone_replacement_verifies_without_the_prior_cosignature() {
        let identity = ActorKeypair::generate();
        let old_recovery = root(0x22);
        let new_recovery = root(0x33);
        let reg = registration(identity.actor_id(), &new_recovery, 1);
        let signed = reg
            .sign(identity.signing_key(), &new_recovery, None)
            .unwrap();

        let head = ChainHead::new(old_recovery.public(), 0);
        signed
            .verify_seed_alone(&head)
            .expect("the honest-loss arm verifies with no prior co-signature");
    }

    #[test]
    fn a_seed_alone_record_never_lands_via_the_strict_verify() {
        // The arm split is the security boundary: the same record that passes
        // `verify_seed_alone` must still be refused by the strict rule, so no
        // handler that consults only `verify` can ever land a windowless
        // seed-alone replacement.
        let identity = ActorKeypair::generate();
        let old_recovery = root(0x22);
        let new_recovery = root(0x33);
        let reg = registration(identity.actor_id(), &new_recovery, 1);
        let signed = reg
            .sign(identity.signing_key(), &new_recovery, None)
            .unwrap();

        let head = ChainHead::new(old_recovery.public(), 0);
        signed
            .verify_seed_alone(&head)
            .expect("pending arm accepts");
        assert!(
            signed.verify(Some(&head)).is_err(),
            "the strict rule must keep refusing a record with no prior co-signature"
        );
    }

    #[test]
    fn a_seed_alone_record_carrying_a_prior_cosignature_is_refused() {
        // A holder of the prior key has the immediate arm and must use it;
        // accepting the co-signature here would let an unverified one ride.
        let identity = ActorKeypair::generate();
        let old_recovery = root(0x22);
        let new_recovery = root(0x33);
        let reg = registration(identity.actor_id(), &new_recovery, 1);
        let signed = reg
            .sign(identity.signing_key(), &new_recovery, Some(&old_recovery))
            .unwrap();

        let head = ChainHead::new(old_recovery.public(), 0);
        assert!(signed.verify_seed_alone(&head).is_err());
    }

    #[test]
    fn a_seed_alone_record_must_advance_the_head() {
        // Also the landing-time supersession check: a RecoveryKey-authorized
        // registration landing during the window moves the head past the
        // pending record's seq, which invalidates it here.
        let identity = ActorKeypair::generate();
        let old_recovery = root(0x22);
        let new_recovery = root(0x33);
        let reg = registration(identity.actor_id(), &new_recovery, 1);
        let signed = reg
            .sign(identity.signing_key(), &new_recovery, None)
            .unwrap();

        let moved_head = ChainHead::new(old_recovery.public(), 1);
        assert!(signed.verify_seed_alone(&moved_head).is_err());
    }

    #[test]
    fn a_seed_alone_record_signed_by_a_different_seed_is_refused() {
        let identity = ActorKeypair::generate();
        let thief = ActorKeypair::generate();
        let old_recovery = root(0x22);
        let new_recovery = root(0x33);
        // A record naming the victim's actor_id but seed-signed by another key.
        let reg = registration(identity.actor_id(), &new_recovery, 1);
        let signed = reg.sign(thief.signing_key(), &new_recovery, None).unwrap();

        let head = ChainHead::new(old_recovery.public(), 0);
        assert!(signed.verify_seed_alone(&head).is_err());
    }

    // ---- Replacement veto ----

    #[test]
    fn a_replacement_veto_verifies_under_the_registered_recovery_key() {
        let actor = ActorKeypair::generate().actor_id();
        let recovery = root(0x22);
        let veto = ReplacementVeto::new(actor, [0x5Cu8; 32]);
        let sig = veto.sign(&recovery).unwrap();
        veto.verify(&recovery.public(), &sig)
            .expect("veto verifies");
    }

    #[test]
    fn a_veto_signature_does_not_transfer_to_another_nonce_or_account() {
        // The single-use nonce is the anti-replay property the whole veto
        // design rests on: a captured veto must not cancel a future pending.
        let mine = ActorKeypair::generate().actor_id();
        let theirs = ActorKeypair::generate().actor_id();
        let recovery = root(0x22);
        let nonce = [0x5Cu8; 32];
        let sig = ReplacementVeto::new(mine, nonce).sign(&recovery).unwrap();

        assert!(
            ReplacementVeto::new(mine, [0x5Du8; 32])
                .verify(&recovery.public(), &sig)
                .is_err()
        );
        assert!(
            ReplacementVeto::new(theirs, nonce)
                .verify(&recovery.public(), &sig)
                .is_err()
        );
    }

    #[test]
    fn a_different_recovery_root_cannot_veto() {
        let actor = ActorKeypair::generate().actor_id();
        let registered = root(0x22);
        let attacker = root(0x99);
        let veto = ReplacementVeto::new(actor, [0x5Cu8; 32]);
        let sig = veto.sign(&attacker).unwrap();
        assert!(veto.verify(&registered.public(), &sig).is_err());
    }

    #[test]
    fn a_veto_signature_and_an_escrow_challenge_signature_do_not_cross() {
        // `ReplacementVeto` and `EscrowChallenge` have the IDENTICAL CBOR shape
        // `{ actor_id, nonce }`, so the domain tag is the ONLY thing separating
        // the two ceremonies — this pin is what keeps that load-bearing.
        let actor = ActorKeypair::generate().actor_id();
        let recovery = root(0x22);
        let nonce = [0x5Cu8; 32];

        let veto_sig = ReplacementVeto::new(actor, nonce).sign(&recovery).unwrap();
        assert!(
            EscrowChallenge::new(actor, nonce)
                .verify(&recovery.public(), &veto_sig)
                .is_err(),
            "a veto signature must not authorize an escrow fetch"
        );

        let escrow_sig = EscrowChallenge::new(actor, nonce).sign(&recovery).unwrap();
        assert!(
            ReplacementVeto::new(actor, nonce)
                .verify(&recovery.public(), &escrow_sig)
                .is_err(),
            "an escrow-challenge signature must not cancel a pending replacement"
        );
    }

    // ---- Succession ----

    #[test]
    fn a_succession_verifies_under_the_known_recovery_key() {
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let recovery = root(0x22);
        let stmt = succession(old.actor_id(), new.actor_id(), &recovery, 1);
        let signed = stmt
            .sign(&recovery, new.signing_key(), Some(old.signing_key()))
            .unwrap();

        signed
            .verify(&ChainHead::new(recovery.public(), 0))
            .expect("succession verifies");
    }

    #[test]
    fn a_succession_without_the_recovery_key_is_refused() {
        // The thief holds both identity keys and can mint a new one; without the
        // offline RecoveryKey they still cannot author a valid statement.
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let real = root(0x22);
        let thief = root(0x99);
        let stmt = succession(old.actor_id(), new.actor_id(), &thief, 1);
        let signed = stmt
            .sign(&thief, new.signing_key(), Some(old.signing_key()))
            .unwrap();

        assert!(
            signed.verify(&ChainHead::new(real.public(), 0)).is_err(),
            "a statement authorized by a key the consumer does not know must be refused"
        );
    }

    #[test]
    fn a_succession_must_prove_successor_possession() {
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let impostor = ActorKeypair::generate();
        let recovery = root(0x22);
        // Points at `new`, but signed by a key the claimant actually holds.
        let stmt = succession(old.actor_id(), new.actor_id(), &recovery, 1);
        let signed = stmt.sign(&recovery, impostor.signing_key(), None).unwrap();

        assert!(
            signed
                .verify(&ChainHead::new(recovery.public(), 0))
                .is_err()
        );
    }

    #[test]
    fn a_succession_must_advance_the_chain() {
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let recovery = root(0x22);
        let stmt = succession(old.actor_id(), new.actor_id(), &recovery, 2);
        let signed = stmt.sign(&recovery, new.signing_key(), None).unwrap();

        assert!(
            signed
                .verify(&ChainHead::new(recovery.public(), 2))
                .is_err()
        );
        signed
            .verify(&ChainHead::new(recovery.public(), 1))
            .expect("seq 2 advances past 1");
    }

    // ---- Succession against a DELIVERED chain (the peer/federation consumer) ----
    //
    // Everything below feeds `verify_succession_against_chain` input that an
    // untrusted party could have authored, because that is exactly the slice-4
    // threat model: a federation push arrives from a nest with no standing to
    // assert anything about the account it names.

    /// One registration + a statement past it — the shape a peer receives when the
    /// owner never replaced their kit.
    fn chain_of_one(
        old: &ActorKeypair,
        recovery: &RecoveryKey,
    ) -> Vec<SignedRecoveryKeyRegistration> {
        vec![
            registration(old.actor_id(), recovery, 1)
                .sign(old.signing_key(), recovery, None)
                .unwrap(),
        ]
    }

    #[test]
    fn a_delivered_chain_authorizes_a_succession_end_to_end() {
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let recovery = root(0x22);
        let chain = chain_of_one(&old, &recovery);
        let stmt = succession(old.actor_id(), new.actor_id(), &recovery, 2)
            .sign(&recovery, new.signing_key(), Some(old.signing_key()))
            .unwrap();

        let verified = verify_succession_against_chain(&stmt, &chain, None)
            .expect("chain authorizes the statement");
        assert_eq!(verified.old_actor_id, old.actor_id());
        assert_eq!(verified.new_actor_id, new.actor_id());
        assert_eq!(verified.seq, 2);
        assert_eq!(verified.chain_head.recovery_pubkey, recovery.public());
    }

    #[test]
    fn a_chain_naming_another_account_proves_nothing() {
        // The whole substitution attack: a hostile relay swaps in a chain for an
        // account whose RecoveryKey *it* holds, hoping the consumer only checks
        // that the statement verifies under "a" registered key.
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let attacker = ActorKeypair::generate();
        let recovery = root(0x22);
        let foreign_chain = chain_of_one(&attacker, &recovery);
        let stmt = succession(old.actor_id(), new.actor_id(), &recovery, 2)
            .sign(&recovery, new.signing_key(), None)
            .unwrap();

        assert!(
            verify_succession_against_chain(&stmt, &foreign_chain, None).is_err(),
            "a chain must be bound to the identity the statement supersedes"
        );
    }

    #[test]
    fn a_chain_link_not_signed_by_the_identity_seed_is_refused() {
        // The same attack one level down: right `actor_id` in the record, wrong
        // hand on the pen. `seed_sig` is what makes a delivered chain
        // self-binding, so this is the property the whole design rests on.
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let attacker = ActorKeypair::generate();
        let recovery = root(0x99);
        let forged = vec![
            registration(old.actor_id(), &recovery, 1)
                .sign(attacker.signing_key(), &recovery, None)
                .unwrap(),
        ];
        let stmt = succession(old.actor_id(), new.actor_id(), &recovery, 2)
            .sign(&recovery, new.signing_key(), None)
            .unwrap();

        assert!(verify_succession_against_chain(&stmt, &forged, None).is_err());
    }

    #[test]
    fn an_empty_chain_is_refused() {
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let recovery = root(0x22);
        let stmt = succession(old.actor_id(), new.actor_id(), &recovery, 2)
            .sign(&recovery, new.signing_key(), None)
            .unwrap();

        assert!(
            verify_succession_against_chain(&stmt, &[], None).is_err(),
            "no registration means no succession capability at all"
        );
    }

    #[test]
    fn a_chain_that_never_visits_the_known_head_is_refused() {
        // The seed thief's shape: a from-scratch single-link chain under a key
        // the thief controls, every signature genuine because the seed is
        // stolen. Against a consumer that already learned the real head, the
        // walk must refuse it — and the same chain must pass at TOFU grade
        // (`known = None`), proving the known-head rule is what refuses.
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let real = root(0x22);
        let stolen = root(0x99);
        let thief_chain = vec![
            registration(old.actor_id(), &stolen, 5)
                .sign(old.signing_key(), &stolen, None)
                .unwrap(),
        ];
        let stmt = succession(old.actor_id(), new.actor_id(), &stolen, 6)
            .sign(&stolen, new.signing_key(), None)
            .unwrap();

        let known = ChainHead::new(real.public(), 1);
        assert!(
            verify_succession_against_chain(&stmt, &thief_chain, Some(&known)).is_err(),
            "a chain that re-mints from scratch must be refused by a consumer with history"
        );
        verify_succession_against_chain(&stmt, &thief_chain, None)
            .expect("the same chain is first-contact TOFU without history — the rule under test");
    }

    #[test]
    fn a_chain_extending_the_known_head_is_accepted() {
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let (chain, head_key) = valid_chain_of(&old, 2);
        let stmt = succession(old.actor_id(), new.actor_id(), &head_key, 3)
            .sign(&head_key, new.signing_key(), None)
            .unwrap();

        // The caller saw link 1 before the replacement landed: the chain
        // extends what it knows.
        let earlier = ChainHead::new(root(1).public(), 1);
        verify_succession_against_chain(&stmt, &chain, Some(&earlier))
            .expect("a chain extending the known head verifies");
        // The caller is exactly current: the known head is the final link.
        let current = ChainHead::new(root(2).public(), 2);
        verify_succession_against_chain(&stmt, &chain, Some(&current))
            .expect("a chain whose final link is the known head verifies");
        // And the walk itself reports the head, for callers that persist it.
        assert_eq!(
            verify_registration_chain(old.actor_id(), &chain, Some(&earlier)).unwrap(),
            ChainHead::new(root(2).public(), 2)
        );
    }

    #[test]
    fn a_truncated_or_diverged_chain_is_refused_against_the_known_head() {
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        // Truncation / rollback: the serving source answers with less chain
        // than the caller has already seen.
        let (short, short_head) = valid_chain_of(&old, 1);
        let known = ChainHead::new(root(2).public(), 2);
        let stmt = succession(old.actor_id(), new.actor_id(), &short_head, 9)
            .sign(&short_head, new.signing_key(), None)
            .unwrap();
        assert!(
            verify_succession_against_chain(&stmt, &short, Some(&known)).is_err(),
            "a chain shorter than what the caller saw must be refused"
        );

        // Divergence: same seq, different key — a rewritten history that never
        // passes through the head the caller holds.
        let diverged_key = root(0x33);
        let mut diverged = valid_chain_of(&old, 1).0;
        diverged.push(
            registration(old.actor_id(), &diverged_key, 2)
                .sign(old.signing_key(), &diverged_key, Some(&root(1)))
                .unwrap(),
        );
        let stmt = succession(old.actor_id(), new.actor_id(), &diverged_key, 3)
            .sign(&diverged_key, new.signing_key(), None)
            .unwrap();
        assert!(
            verify_succession_against_chain(&stmt, &diverged, Some(&known)).is_err(),
            "a chain that diverges from the known head must be refused"
        );
    }

    /// `n` genuinely valid, properly co-signed links — key `i+1` replacing key
    /// `i` at `seq = i + 1`. Repeating one link would be refused by the
    /// seq-advance rule instead, which would make the bound test pass for the
    /// wrong reason (it did, until the mutation matrix caught it).
    fn valid_chain_of(
        old: &ActorKeypair,
        n: usize,
    ) -> (Vec<SignedRecoveryKeyRegistration>, RecoveryKey) {
        let links = (0..n)
            .map(|i| {
                let key = root(i as u8 + 1);
                let prior = (i > 0).then(|| root(i as u8));
                registration(old.actor_id(), &key, i as u64 + 1)
                    .sign(old.signing_key(), &key, prior.as_ref())
                    .unwrap()
            })
            .collect();
        (links, root(n as u8))
    }

    #[test]
    fn a_chain_beyond_the_walk_bound_is_refused() {
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();

        // Exactly at the bound: a fully valid chain must still verify, so the
        // refusal below is the bound and not some accident of chain building.
        let (chain, head_key) = valid_chain_of(&old, MAX_VERIFIED_CHAIN_LEN);
        let stmt = succession(old.actor_id(), new.actor_id(), &head_key, 1_000)
            .sign(&head_key, new.signing_key(), None)
            .unwrap();
        verify_succession_against_chain(&stmt, &chain, None)
            .expect("a chain at the bound still verifies");

        // One link past it: refused on work grounds alone, with every signature
        // in the chain still valid.
        let (long, head_key) = valid_chain_of(&old, MAX_VERIFIED_CHAIN_LEN + 1);
        let stmt = succession(old.actor_id(), new.actor_id(), &head_key, 1_000)
            .sign(&head_key, new.signing_key(), None)
            .unwrap();
        assert!(verify_succession_against_chain(&stmt, &long, None).is_err());
    }

    #[test]
    fn a_retired_recovery_key_no_longer_authorizes_a_succession() {
        // Retiring a key is a decision the *chain* records, never a property of
        // the key: the old root goes on producing valid signatures forever.
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let first = root(0x22);
        let second = root(0x33);
        let chain = vec![
            registration(old.actor_id(), &first, 1)
                .sign(old.signing_key(), &first, None)
                .unwrap(),
            registration(old.actor_id(), &second, 2)
                .sign(old.signing_key(), &second, Some(&first))
                .unwrap(),
        ];

        let by_retired = succession(old.actor_id(), new.actor_id(), &first, 3)
            .sign(&first, new.signing_key(), None)
            .unwrap();
        assert!(
            verify_succession_against_chain(&by_retired, &chain, None).is_err(),
            "the replaced key must not still re-point the account"
        );

        let by_current = succession(old.actor_id(), new.actor_id(), &second, 3)
            .sign(&second, new.signing_key(), None)
            .unwrap();
        verify_succession_against_chain(&by_current, &chain, None)
            .expect("the chain head authorizes the statement");
    }

    #[test]
    fn a_landed_seed_alone_replacement_is_accepted_at_tofu_grade() {
        // The honest-RecoveryKey-loss arm: the replacement carries no prior
        // co-signature (the prior key is exactly what was lost) and landed only
        // after the home nest's uncontested 30-day window. A peer cannot verify
        // that wait — refusing the link here would make the loss path unusable
        // off the home nest, which `identity-succession.md:55` explicitly does
        // not require.
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let lost = root(0x22);
        let replacement = root(0x44);
        let chain = vec![
            registration(old.actor_id(), &lost, 1)
                .sign(old.signing_key(), &lost, None)
                .unwrap(),
            registration(old.actor_id(), &replacement, 2)
                .sign(old.signing_key(), &replacement, None)
                .unwrap(),
        ];
        let stmt = succession(old.actor_id(), new.actor_id(), &replacement, 3)
            .sign(&replacement, new.signing_key(), None)
            .unwrap();

        let verified = verify_succession_against_chain(&stmt, &chain, None)
            .expect("a landed seed-alone replacement still heads the chain");
        assert_eq!(verified.chain_head.recovery_pubkey, replacement.public());
    }

    #[test]
    fn a_chain_link_that_does_not_advance_the_seq_is_refused() {
        // Guards the replay direction: an archived registration re-presented as
        // if it were the current head.
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let first = root(0x22);
        let stale = root(0x55);
        let chain = vec![
            registration(old.actor_id(), &first, 2)
                .sign(old.signing_key(), &first, None)
                .unwrap(),
            registration(old.actor_id(), &stale, 1)
                .sign(old.signing_key(), &stale, Some(&first))
                .unwrap(),
        ];
        let stmt = succession(old.actor_id(), new.actor_id(), &stale, 3)
            .sign(&stale, new.signing_key(), None)
            .unwrap();

        assert!(verify_succession_against_chain(&stmt, &chain, None).is_err());
    }

    #[test]
    fn a_statement_that_does_not_advance_past_the_chain_head_is_refused() {
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let recovery = root(0x22);
        let chain = vec![
            registration(old.actor_id(), &recovery, 5)
                .sign(old.signing_key(), &recovery, None)
                .unwrap(),
        ];
        let stmt = succession(old.actor_id(), new.actor_id(), &recovery, 5)
            .sign(&recovery, new.signing_key(), None)
            .unwrap();

        assert!(verify_succession_against_chain(&stmt, &chain, None).is_err());
    }

    #[test]
    fn a_carried_recovery_pubkey_that_disagrees_with_the_known_one_is_refused() {
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let recovery = root(0x22);
        let other = root(0x77);
        let mut stmt = succession(old.actor_id(), new.actor_id(), &recovery, 1);
        stmt.recovery_pubkey = other.public();
        // Re-sign so `recovery_sig` itself is valid under the real key: the
        // refusal must come from the field mismatch, not a broken signature.
        let signed = stmt.sign(&recovery, new.signing_key(), None).unwrap();

        assert!(
            signed
                .verify(&ChainHead::new(recovery.public(), 0))
                .is_err()
        );
    }

    // ---- `old_sig` is never load-bearing (identity-succession.md:53) ----

    #[test]
    fn an_absent_old_sig_does_not_block_a_valid_succession() {
        // The loss case has no old key at all until an escrow restore.
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let recovery = root(0x22);
        let stmt = succession(old.actor_id(), new.actor_id(), &recovery, 1);
        let signed = stmt.sign(&recovery, new.signing_key(), None).unwrap();

        assert!(signed.old_sig.is_none());
        signed
            .verify(&ChainHead::new(recovery.public(), 0))
            .expect("a succession with no old_sig is valid");
        assert!(!signed.old_sig_verifies());
    }

    #[test]
    fn a_garbage_old_sig_does_not_change_the_verdict() {
        // Counterintuitive but load-bearing: `verify` must not even *look* at
        // `old_sig`, so a malformed one can neither fail a good statement nor
        // pass a bad one. Only the display-only probe reports on it.
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let recovery = root(0x22);
        let stmt = succession(old.actor_id(), new.actor_id(), &recovery, 1);
        let mut signed = stmt
            .sign(&recovery, new.signing_key(), Some(old.signing_key()))
            .unwrap();
        assert!(
            signed.old_sig_verifies(),
            "baseline: the real old_sig verifies"
        );

        signed.old_sig = Some(vec![0xAA; 64]);
        signed
            .verify(&ChainHead::new(recovery.public(), 0))
            .expect("a garbage old_sig must not fail verification");
        assert!(!signed.old_sig_verifies());

        signed.old_sig = Some(vec![0xAA; 3]); // not even signature-shaped
        signed
            .verify(&ChainHead::new(recovery.public(), 0))
            .expect("a malformed old_sig must not fail verification");
        assert!(!signed.old_sig_verifies());
    }

    #[test]
    fn an_old_sig_cannot_substitute_for_the_recovery_signature() {
        // The retired `KeyRotation` shape was exactly old-sig + new-sig; a thief
        // holds the old key, so that pair must never authorize a succession.
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let recovery = root(0x22);
        let stmt = succession(old.actor_id(), new.actor_id(), &recovery, 1);
        let mut signed = stmt
            .sign(&recovery, new.signing_key(), Some(old.signing_key()))
            .unwrap();
        // Replace the load-bearing signature with the old key's.
        signed.recovery_sig = signed.old_sig.clone().unwrap();

        assert!(
            signed
                .verify(&ChainHead::new(recovery.public(), 0))
                .is_err()
        );
    }

    // ---- Cross-role replay ----

    #[test]
    fn a_signature_minted_for_one_role_does_not_verify_as_another() {
        // This is what the per-role domain tags buy.
        let old = ActorKeypair::generate();
        let new = ActorKeypair::generate();
        let recovery = root(0x22);
        let stmt = succession(old.actor_id(), new.actor_id(), &recovery, 1);
        let canonical = canonical_encode(&stmt).unwrap();
        let as_recovery_role = recovery.sign_tagged(TAG_SUCCESSION_RECOVERY, &canonical);

        assert!(
            verify_tagged(
                &recovery.public(),
                TAG_SUCCESSION_RECOVERY,
                &canonical,
                &as_recovery_role
            )
            .is_ok()
        );
        assert!(
            verify_tagged(
                &recovery.public(),
                TAG_REGISTRATION_RECOVERY,
                &canonical,
                &as_recovery_role
            )
            .is_err(),
            "the same bytes must not verify under a different role tag"
        );
    }

    // ---- Kit grammar ----

    #[test]
    fn kit_uri_round_trips_with_and_without_the_actor() {
        let secret = "a".repeat(64);
        let actor = "b".repeat(64);

        let with = RecoveryKitQr::to_uri(&secret, Some(&actor), None);
        let parsed = parse_recovery_kit_input(&with).expect("parses");
        assert_eq!(parsed.secret, secret);
        assert_eq!(parsed.actor_id.as_deref(), Some(actor.as_str()));

        let without = RecoveryKitQr::to_uri(&secret, None, None);
        let parsed = parse_recovery_kit_input(&without).expect("parses");
        assert_eq!(parsed.secret, secret);
        assert_eq!(parsed.actor_id, None);
    }

    /// The `&handle=` half — the only thing in a kit payload that can *locate*
    /// the account's home nest, so a restore screen holding a kit that carries
    /// one never has to ask (`onboarding.md` § 1 Identity, `recovery-entry-
    /// account-field`). Deliberately the same param name and position
    /// `fauna://identity` uses (priority #3).
    #[test]
    fn kit_uri_round_trips_the_handle() {
        let secret = "a".repeat(64);
        let actor = "b".repeat(64);

        let both = RecoveryKitQr::to_uri(&secret, Some(&actor), Some("alice@fauna.test"));
        assert_eq!(
            both,
            format!("fauna://recovery?secret={secret}&actor={actor}&handle=alice@fauna.test")
        );
        let parsed = parse_recovery_kit_input(&both).expect("parses");
        assert_eq!(parsed.actor_id.as_deref(), Some(actor.as_str()));
        assert_eq!(parsed.handle.as_deref(), Some("alice@fauna.test"));

        // A handle with no actor is legitimate: the handle alone both locates
        // the nest and resolves to the account there.
        let handle_only = RecoveryKitQr::to_uri(&secret, None, Some("alice@fauna.test"));
        assert_eq!(
            handle_only,
            format!("fauna://recovery?secret={secret}&handle=alice@fauna.test")
        );
        let parsed = parse_recovery_kit_input(&handle_only).expect("parses");
        assert_eq!(parsed.actor_id, None);
        assert_eq!(parsed.handle.as_deref(), Some("alice@fauna.test"));
    }

    /// An empty `handle=` is absence, not a handle named `""` — the same
    /// treatment `actor=` and `fauna://identity`'s own `handle=` get, so a
    /// minting site that formats an unset field never produces a payload that
    /// sends a restore at a nameless account.
    #[test]
    fn an_empty_handle_param_reads_as_absent() {
        let secret = "a".repeat(64);
        let parsed = parse_recovery_kit_input(&format!("fauna://recovery?secret={secret}&handle="))
            .expect("parses");
        assert_eq!(parsed.handle, None);
        assert_eq!(
            RecoveryKitQr::to_uri(&secret, None, Some("")),
            format!("fauna://recovery?secret={secret}")
        );
    }

    /// A handle-less kit (the onboarding kit screen mints actor-only) still
    /// parses — the param is additive, and a written-down phrase outlives
    /// every app version (version-compatibility.md).
    #[test]
    fn a_handle_less_kit_payload_still_parses() {
        let secret = "a".repeat(64);
        let actor = "b".repeat(64);
        let parsed =
            parse_recovery_kit_input(&format!("fauna://recovery?secret={secret}&actor={actor}"))
                .expect("parses");
        assert_eq!(parsed.actor_id.as_deref(), Some(actor.as_str()));
        assert_eq!(parsed.handle, None);
    }

    #[test]
    fn kit_grammar_accepts_the_same_union_identity_import_does() {
        let secret = "a".repeat(64);
        for form in [
            secret.clone(),
            format!("  {secret}  "),
            format!("fauna://recovery?secret={secret}"),
            format!("FAUNA://RECOVERY?secret={secret}"),
            format!("fauna://recovery:{secret}"),
        ] {
            assert_eq!(
                parse_recovery_kit_input(&form).map(|k| k.secret),
                Some(secret.clone()),
                "form should parse: {form}"
            );
        }
    }

    #[test]
    fn an_identity_payload_is_not_accepted_as_a_recovery_kit() {
        let secret = "a".repeat(64);
        assert!(parse_recovery_kit_input(&format!("fauna://identity?secret={secret}")).is_none());
        assert!(parse_recovery_kit_input(&format!("fauna://identity:{secret}")).is_none());
    }

    #[test]
    fn malformed_kit_input_is_rejected() {
        for bad in [
            "",
            "not a kit",
            "fauna://recovery?secret=short",
            "fauna://recovery?actor=aaaa",
            &format!("fauna://recovery?secret={}", "z".repeat(64)),
        ] {
            assert!(
                parse_recovery_kit_input(bad).is_none(),
                "should reject: {bad}"
            );
        }
    }

    #[test]
    fn kit_parts_projection_matches_the_identity_import_shape() {
        let kit = ImportedRecoveryKit {
            secret: "a".repeat(64),
            actor_id: None,
            handle: None,
        };
        assert_eq!(
            kit.into_parts(),
            vec!["a".repeat(64), String::new(), String::new()]
        );

        let full = ImportedRecoveryKit {
            secret: "a".repeat(64),
            actor_id: Some("b".repeat(64)),
            handle: Some("alice@fauna.test".into()),
        };
        assert_eq!(
            full.into_parts(),
            vec![
                "a".repeat(64),
                "b".repeat(64),
                "alice@fauna.test".to_string()
            ]
        );
    }

    #[test]
    fn a_recovery_root_hex_round_trips() {
        let key = RecoveryKey::generate();
        let hex = key.to_hex();
        assert_eq!(hex.len(), 64);
        assert_eq!(
            RecoveryKey::from_hex(&hex).unwrap().to_bytes(),
            key.to_bytes()
        );
    }

    #[test]
    fn signed_records_round_trip_through_canonical_dag_cbor() {
        let identity = ActorKeypair::generate();
        let recovery = root(0x22);
        let reg = registration(identity.actor_id(), &recovery, 0);
        let signed = reg.sign(identity.signing_key(), &recovery, None).unwrap();
        let bytes = canonical_encode(&signed).unwrap();
        let back: SignedRecoveryKeyRegistration =
            crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(back, signed);
        back.verify(None).expect("survives a wire round-trip");

        let new = ActorKeypair::generate();
        let stmt = succession(identity.actor_id(), new.actor_id(), &recovery, 1);
        let signed = stmt
            .sign(&recovery, new.signing_key(), Some(identity.signing_key()))
            .unwrap();
        let bytes = canonical_encode(&signed).unwrap();
        let back: SignedIdentitySuccession = crate::encoding::canonical_decode(&bytes).unwrap();
        assert_eq!(back, signed);
        back.verify(&ChainHead::new(recovery.public(), 0))
            .expect("survives a wire round-trip");
    }
}
