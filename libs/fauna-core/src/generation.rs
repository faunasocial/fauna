//! The R14 (account-data-plane.md § The ratified decisions) generation machinery's **value types and merge joins** — the five
//! machinery kinds' payloads (`account-data-plane.md` § The generation
//! machinery) and the two lattice joins their CrdtPerField merge arms
//! delegate to (`fauna_protocol::merge_policy` is the dispatcher; the laws
//! are asserted here, where the joins live — the seen-set pattern).
//!
//! Both joins are **byte-level over the canonical encodings**: the winner is
//! one of the two input byte strings verbatim, never a re-encode. Two builds
//! that disagree about field sets or declaration order therefore still pick
//! the same winner — merge determinism is pinned to the wire truth, not to
//! this build's Rust — and byte equality with the current value is the
//! echo-stop for free.
//!
//! # The two-phase lattices
//!
//! * [`DeviceSetRecord`]: `Removed` **absorbs** `Enrolled` — the charter's
//!   per-id monotone lattice. Re-enrolling a machine mints a fresh device id,
//!   so "add after remove" is unrepresentable on any id and add-wins
//!   resurrection is
//!   structurally absent. Within the `Enrolled` phase the join is
//!   **key-aware** (ruled 2026-09-16): a record whose `device_sig` verifies
//!   under the cell's device id outranks any that does not, then byte-order
//!   max — so the one writer who honestly authors an id's enrollment (the
//!   device itself) is never displaced by a `BackupKey` holder re-filing its
//!   cert beside a foreign X-Wing key. Within `Removed`, and between two
//!   non-verifying rows, the byte-order max stays — arbitrary,
//!   deterministic everywhere, and unreachable in honest operation
//!   (concurrent removals differ only in remover/stamp).
//! * [`GenerationMintRecord`]: `Shredded` **absorbs** `Minted`. The shred
//!   marker is an **in-value lattice state, not a T14 tombstone** (build
//!   ruling, 2026-08-13, recorded in the charter §): CrdtPerField
//!   structurally refuses tombstones — degrading one to a stamp comparison
//!   breaks convergence, the E0 law — so the mint kind records deletion the
//!   same way the device-set records removal. `Shredded` keeps the mint's
//!   DAG core (parents, member ids, minter, commitment — gen-0-readable
//!   metadata regardless, a stated R14 exposure) and **drops the wrap
//!   ciphertexts**, so an honest replica that merges the shred no longer
//!   carries unwrappable key material for the dead generation.

use serde::{Deserialize, Serialize};

use crate::encoding::canonical_decode;
use crate::error::Error;

// ── The verified-signature memo ─────────────────────────────────────────────
//
// Every pump pass rebuilds the fleet view and re-reads every machinery row
// — the top-up, unkeyable and reclamation passes each build their own, the
// resolver runs per origination — and each build re-verifies the same
// Ed25519 signatures it verified a moment ago: one per enrollment cert, one
// per healer cell, one per receipt, one per mint. Verification is a pure
// function of (key, message, signature), so a process-wide memo of the
// checks that PASSED is exactly as trustworthy as the check itself, and it
// turns a fleet's steady-state pass from a few thousand signature
// verifications into a few thousand hash lookups. Bounded: past the cap the
// memo is cleared, never partially evicted (a hash set has no cheap order).
// Failures are never memoized — a row that fails is re-tried on every read,
// which is the honest posture for attacker-suppliable bytes.

/// The memo's entry cap. Sized for a large fleet's live machinery rows with
/// headroom (`MAX_MINT_MEMBERS` enrollments, their cells, receipts); a
/// hard-coded constant, invariant bucket (1).
const VERIFIED_MEMO_CAP: usize = 16_384;

fn verified_memo() -> &'static std::sync::Mutex<std::collections::HashSet<[u8; 32]>> {
    static MEMO: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<[u8; 32]>>> =
        std::sync::OnceLock::new();
    MEMO.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
}

fn memo_remembers(key: &[u8; 32]) -> bool {
    verified_memo()
        .lock()
        .map(|m| m.contains(key))
        .unwrap_or(false)
}

fn memo_remember(key: [u8; 32]) {
    if let Ok(mut m) = verified_memo().lock() {
        if m.len() >= VERIFIED_MEMO_CAP {
            m.clear();
        }
        m.insert(key);
    }
}

/// [`crate::identity::verify_detached`] behind the verified-signature memo.
fn verified_signature(public_key: &[u8; 32], message: &[u8], signature: &[u8]) -> bool {
    let mut h = blake3::Hasher::new_derive_key("fauna.generation.verified-signature.v1");
    h.update(public_key);
    h.update(&(message.len() as u64).to_be_bytes());
    h.update(message);
    h.update(signature);
    let key = *h.finalize().as_bytes();
    if memo_remembers(&key) {
        return true;
    }
    let ok = crate::identity::verify_detached(public_key, message, signature);
    if ok {
        memo_remember(key);
    }
    ok
}

/// One device's row in `fauna.state.device-set`. Logical key = the device id
/// hex (the device principal's Ed25519 public key = its peer-plane NodeId).
///
/// Fleet-membership truth for generation admissibility and wrap targeting —
/// **per-account and role-defined**: only `BackupKey`-carrying reading
/// enrollments can author machinery entries at all (they seal under the
/// fleet-only gen-0 branch), so keyless serving replicas — a docker nest, a
/// friend's app hosting a custodian replica, a relay-only kiosk — can never
/// join this set even holding a valid `DeviceAuthorization` (charter § The
/// generation machinery owns the statement).
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `consensus` ground: every reader must reach the same
/// decision from the value, so there is no unknown arm and a reader that cannot
/// decode one fails rather than guess). A new variant is an edit to
/// `tools/check-additive-evolution/enum_ledger.txt`, made in the same change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceSetRecord {
    /// An enrolled fleet member — a mint wrap target.
    Enrolled {
        /// The device's X-Wing device-KEM **public** key: the wrap target a
        /// minter seals this device's generation wraps to. Derived from the
        /// device principal's Ed25519 secret under a frozen context (the
        /// schedule build design's `derive_recipient_xwing_keypair` shape;
        /// the derivation lands with the mint step).
        #[serde(with = "serde_bytes")]
        xwing_pubkey: Vec<u8>,
        /// The root- or chain-signed `DeviceAuthorization` covering this id:
        /// the canonical dag-cbor bytes of an
        /// [`crate::encoding::EmbedAsBytes`] `{envelope, bytes}` pair — the
        /// same self-contained carriage every witness surface uses. The
        /// charter licenses "digest + chain reference" as the alternative;
        /// full carriage is this build's choice — [`FleetView::build`]
        /// verifies it in place with no chain lookup, and the 64 KiB entry
        /// bound is orders of magnitude away.
        #[serde(with = "serde_bytes")]
        authorization: Vec<u8>,
        /// Enrollment stamp, unix ms. Advisory (clocks skew) — never
        /// merge-ordering; the lattice orders, stamps inform.
        enrolled_at_ms: i64,
        /// The device's **self-signature**: Ed25519 by the device id (the cell
        /// key IS the verification key — the reach/unkeyable pattern) over
        /// [`enrollment_signing_bytes`], which binds `xwing_pubkey`,
        /// `authorization` and the stamp to *this* id. Without it the record
        /// was not self-authenticating: the cert covers only the id, so any
        /// `BackupKey` holder could re-publish a member's valid cert beside
        /// its own X-Wing key and, winning the byte-order join, redirect every
        /// later wrap to itself under the member's name (charter § The
        /// generation machinery → the device-set bullet, *The self-signed
        /// enrollment*, ruled 2026-09-16). Required for membership:
        /// [`FleetView::build`] flags an `Enrolled` row whose signature is
        /// absent or does not verify at its cell, and [`join_device_set`]
        /// ranks a row that verifies above any that does not. (Pre-ruling
        /// unsigned rows were admitted until the compat-remnant sweep retired
        /// them, program 4.) The attribute keeps the field's additive-evolution
        /// shape: an absent signature decodes as empty and is refused at the
        /// view, never at decode.
        #[serde(default, skip_serializing_if = "Vec::is_empty", with = "serde_bytes")]
        device_sig: Vec<u8>,
    },
    /// The absorbing phase: this id is excluded from every future generation.
    /// The enrollment payload deliberately does not survive the join — a
    /// removed id's wrap target is dead, and historical member sets live in
    /// the mint DAG.
    Removed {
        /// Removal stamp, unix ms. Advisory, like the enrollment stamp.
        removed_at_ms: i64,
        /// The removing writer's 32-byte id — an enrolled non-removed device
        /// or a root-holding surface (authority is verified at the reader;
        /// the accepted vandalism-not-escalation posture is the charter's).
        #[serde(with = "serde_bytes")]
        removed_by: [u8; 32],
    },
}

/// The DAG core of one generation mint — everything about the mint except
/// the wrap ciphertexts, which is exactly what survives a shred.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintCore {
    /// Parent tip id(s) — one for an ordinary mint, several when a mint
    /// merges concurrent forks.
    #[serde(with = "crate::byte_array::vec")]
    pub parents: Vec<[u8; 32]>,
    /// The member set at mint: device ids from merged device-set state.
    /// Admissibility is evaluated against the *observer's* merged device-set
    /// state, never trusted from this list alone.
    #[serde(with = "crate::byte_array::vec")]
    pub member_ids: Vec<[u8; 32]>,
    /// The minting device id.
    #[serde(with = "serde_bytes")]
    pub minter: [u8; 32],
    /// `BLAKE3(gen_key)` under the frozen commit context
    /// (`owner-key-material.md` owns it). The content-derived generation id
    /// hashes over this, so every unwrap can refuse a substituted key.
    #[serde(with = "serde_bytes")]
    pub key_commitment: [u8; 32],
    /// Mint stamp, unix ms. Advisory.
    pub minted_at_ms: i64,
}

/// One member's inline X-Wing wrap of the generation key, riding the mint
/// entry itself ("its wrap is inline when it was in the mint's member set").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberWrap {
    /// The target device id (must be in [`MintCore::member_ids`]).
    #[serde(with = "serde_bytes")]
    pub device_id: [u8; 32],
    /// The X-Wing envelope sealing the generation key to that device's KEM
    /// public key (`fauna_mls::wrapped_blob::xwing_envelope`).
    #[serde(with = "serde_bytes")]
    pub wrap: Vec<u8>,
}

/// One generation's row in `fauna.state.generation-mint`. Logical key = the
/// content-derived generation id hex. Immutable-by-lattice: the only change a
/// mint entry ever undergoes is the one-way step to [`Self::Shredded`].
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `consensus` ground: every reader must reach the same
/// decision from the value, so there is no unknown arm and a reader that cannot
/// decode one fails rather than guess). A new variant is an edit to
/// `tools/check-additive-evolution/enum_ledger.txt`, made in the same change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GenerationMintRecord {
    /// A live generation: DAG core + the minter's signature + the per-member
    /// inline wraps.
    Minted {
        core: MintCore,
        /// The minter's Ed25519 signature over
        /// [`mint_minter_signing_bytes`] of the content-derived generation id
        /// — a mint is an authenticated statement by the device
        /// [`MintCore::minter`] names (ST-007: the resolver is the trust
        /// boundary, and a `BackupKey` holder can craft this row directly, so
        /// authorship must verify at the reader, not just at `build_mint`).
        /// Sibling of the core, deliberately outside the id derivation: two
        /// same-key variants differing only here converge byte-level like any
        /// lattice value, and a surviving invalid-sig variant refuses
        /// uniformly everywhere — healed by the candidate-aware first-need
        /// mint rather than wedging anyone.
        #[serde(with = "serde_bytes")]
        minter_sig: Vec<u8>,
        wraps: Vec<MemberWrap>,
    },
    /// The absorbing shred marker: devices drop the key, holders delete the
    /// escrow wrap, and this row's surviving bytes carry no wrap ciphertext.
    ///
    /// **Two acts, two gates** (ruled 2026-10-08 —
    /// `account-data-taxonomy.md` § The generation machinery → *Fleet-scope
    /// reclamation*, the shredder's veto → *the authored shred*; the group
    /// plane's shape, [`crate::group_generation::GroupGenerationMintRecord`]).
    /// The join absorbs and the resolver drops the generation's candidacy on
    /// ANY decodable `Shredded`, authored or not: the `Minted` row's wraps
    /// are gone from the plane either way, so the generation could not be
    /// keyed from the plane again and the fleet re-mints — availability only,
    /// the class a `BackupKey` holder already reaches by same-key `Minted`
    /// suppression. **Dropping a key, sweeping an escrow wrap or retiring a
    /// row because a generation reads `Shredded` is a different act and never
    /// rides an unauthored row**: `BackupKey` seals this row and every reading
    /// replica, removed devices included, holds `BackupKey`, so a consumer
    /// acts only when [`Self::shred_is_authored`] answers `Ok`.
    Shredded {
        core: MintCore,
        /// Shred stamp, unix ms. Advisory; signed, so it cannot be moved
        /// under the signature.
        shredded_at_ms: i64,
        /// The shredding writer's 32-byte device id — and the Ed25519
        /// verification key for `shredder_sig` (a device id IS the device
        /// principal's public key, the `minter_sig` pattern). **Attribution
        /// only unless the row is authored** — no reader consults it alone.
        #[serde(with = "serde_bytes")]
        shredded_by: [u8; 32],
        /// Ed25519 by `shredded_by` over [`shred_signing_bytes`]. Additive:
        /// empty on a row forged without the device secret or written by a
        /// binary older than the ruling (either is then unauthored), skipped
        /// when empty so such a row encodes exactly as it always did. No
        /// authorization rides beside it: the device set is this plane's
        /// authority and every reader holds it as its [`FleetView`] — where
        /// the group plane's cert chain is not, which is why that plane's
        /// shred carries the cert and this one does not.
        #[serde(default, skip_serializing_if = "Vec::is_empty", with = "serde_bytes")]
        shredder_sig: Vec<u8>,
    },
}

/// Domain-separation tag for an authored shred's signature (frozen — the
/// [`MINT_MINTER_SIG_CONTEXT`] replay discipline).
pub const SHRED_SIG_CONTEXT: &[u8] = b"fauna.generation.shred.v1\0";

/// The exact bytes a shredder signs: the domain tag, the content-derived
/// generation id (which commits to the whole core), the shredder's device id
/// and the big-endian stamp — fixed-width throughout.
#[must_use]
pub fn shred_signing_bytes(
    generation_id: &[u8; 32],
    shredded_by: &[u8; 32],
    shredded_at_ms: i64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(SHRED_SIG_CONTEXT.len() + 32 * 2 + 8);
    out.extend_from_slice(SHRED_SIG_CONTEXT);
    out.extend_from_slice(generation_id);
    out.extend_from_slice(shredded_by);
    out.extend_from_slice(&shredded_at_ms.to_be_bytes());
    out
}

/// Build one **authored** shred as the shredding device — the ONE production
/// shape of a `Shredded` row since the ruling. The signature's verification
/// key is the shredder's own device id, so no key distribution rides this.
///
/// # Errors
/// Only on a core that fails canonical encoding.
pub fn sign_shred(
    shredder: &ed25519_dalek::SigningKey,
    core: MintCore,
    shredded_at_ms: i64,
) -> Result<GenerationMintRecord, Error> {
    use ed25519_dalek::Signer;
    let id = generation_id(&core)?;
    let shredded_by = shredder.verifying_key().to_bytes();
    let shredder_sig = shredder
        .sign(&shred_signing_bytes(&id, &shredded_by, shredded_at_ms))
        .to_bytes()
        .to_vec();
    Ok(GenerationMintRecord::Shredded {
        core,
        shredded_at_ms,
        shredded_by,
        shredder_sig,
    })
}

impl GenerationMintRecord {
    /// Is this a `Shredded` row **authored by a verified, non-removed member
    /// of `view`** — the only shape a consumer may drop a generation key,
    /// sweep an escrow wrap or retire a row on? `shredded_by` must answer
    /// [`FleetView::is_verified_member`] at the reader's CURRENT view (the
    /// device set is monotone and has no clock, so membership is read now,
    /// never at the stamp: a shred by a device removed later goes inert
    /// wherever it has not yet been acted on — the fail-safe direction), and
    /// `shredder_sig` must verify under it over the **recomputed** generation
    /// id (never the row key an attacker chose). Returns that id.
    ///
    /// Runs the strict primitive ([`crate::identity::verify_detached`])
    /// because `shredded_by` is the row's own claimed key — attacker-chosen
    /// on an inbound row.
    ///
    /// # Errors
    /// A `Minted` row, an unauthored (sig-less or forged) shred, a shredder
    /// that is no verified member, or a signature that does not verify.
    pub fn shred_is_authored(&self, view: &FleetView) -> Result<[u8; 32], String> {
        let GenerationMintRecord::Shredded {
            core,
            shredded_at_ms,
            shredded_by,
            shredder_sig,
        } = self
        else {
            return Err("a Minted row shreds nothing".to_string());
        };
        if shredder_sig.is_empty() {
            return Err("shred is unauthored — it carries no signature".to_string());
        }
        let id = generation_id(core).map_err(|e| format!("shred core does not encode: {e}"))?;
        if !view.is_verified_member(shredded_by) {
            return Err("shredder is not a verified, non-removed fleet member".to_string());
        }
        if shredder_sig.len() != 64
            || !crate::identity::verify_detached(
                shredded_by,
                &shred_signing_bytes(&id, shredded_by, *shredded_at_ms),
                shredder_sig,
            )
        {
            return Err(
                "shred signature does not verify under the shredder's device id".to_string(),
            );
        }
        Ok(id)
    }
}

/// One healer-attributed top-up wrap in `fauna.state.generation-wrap`, at the
/// three-segment logical key
/// `"<generation-id-hex>/<target-device-id-hex>/<healer-device-id-hex>"` —
/// the hardening's cell shape.
///
/// Each healer owns its own cell, and the record is **self-authenticating**:
/// [`Self::verifies_at`] checks the in-value Ed25519 signature by the healer
/// device key named in the cell key, over every field including the wrap
/// bytes' hash. A `BackupKey` holder without that device's Ed25519 secret can
/// neither displace a healer's row (the kind's join prefers verifying bytes)
/// nor mint suppressing coverage (the top-up pass counts only verifying rows
/// authored by currently-verified, non-removed fleet members). An enum of one
/// variant: it was minted so a succession burn could land as an authenticated
/// absorbing variant, but no burn at succession is owed
/// (`account-data-taxonomy.md` § The generation machinery, re-ruled
/// 2026-10-01) — and on the ground below a second variant could not land
/// additively anyway: it would be a major-version change.
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `consensus` ground: every reader must reach the same
/// decision from the value, so there is no unknown arm and a reader that cannot
/// decode one fails rather than guess). A new variant is an edit to
/// `tools/check-additive-evolution/enum_ledger.txt`, made in the same change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GenerationWrapRecordV2 {
    /// A live top-up wrap.
    Wrap {
        /// The generation whose key this wraps — echoes cell segment 1.
        #[serde(with = "serde_bytes")]
        generation_id: [u8; 32],
        /// The target device id — echoes cell segment 2.
        #[serde(with = "serde_bytes")]
        target_device: [u8; 32],
        /// The authoring healer's device id — echoes cell segment 3, and IS
        /// the Ed25519 verification key for `healer_sig` (a device id is its
        /// device principal's public key — the `minter_sig` pattern).
        #[serde(with = "serde_bytes")]
        healer: [u8; 32],
        /// The healer's clock at authoring, unix ms. Signed, so a replayed
        /// old row cannot claim freshness; ranks re-publications within one
        /// healer's cell.
        at_ms: i64,
        /// The X-Wing envelope sealing the generation key to the target's
        /// KEM public key.
        #[serde(with = "serde_bytes")]
        wrap: Vec<u8>,
        /// Ed25519 by `healer` over [`topup_healer_signing_bytes`] — which
        /// covers the ids, the stamp, and the wrap hash, so no field can be
        /// swapped under a carried-through signature.
        #[serde(with = "serde_bytes")]
        healer_sig: Vec<u8>,
    },
}

impl GenerationWrapRecordV2 {
    /// Does this record verify **at its cell** — every field echoing the cell
    /// key's three segments, and `healer_sig` verifying under the cell's
    /// healer id over exactly these bytes?
    ///
    /// Pure over the record and the cell coordinates: this is the predicate
    /// the kind's join ranks by and the top-up pass's coverage check starts
    /// from (membership of the healer is the caller's second, view-consulting
    /// half — a *merge* may never consult external state).
    #[must_use]
    pub fn verifies_at(
        &self,
        cell_generation: &[u8; 32],
        cell_target: &[u8; 32],
        cell_healer: &[u8; 32],
    ) -> bool {
        let GenerationWrapRecordV2::Wrap {
            generation_id,
            target_device,
            healer,
            at_ms,
            wrap,
            healer_sig,
        } = self;
        if generation_id != cell_generation || target_device != cell_target || healer != cell_healer
        {
            return false;
        }
        verified_signature(
            healer,
            &topup_healer_signing_bytes(generation_id, target_device, healer, *at_ms, wrap),
            healer_sig,
        )
    }
}

/// One identity's published escrow target in `fauna.state.escrow-target`.
/// Logical key = [`escrow_target_identity_key`] (`identity/<actor-id-hex>`),
/// **one row per identity the account has had**: a successor publishes its
/// own row beside the predecessor's instead of colliding with an Immutable
/// row it could never replace (the pre-2026-09-28 constant key `"identity"`
/// made the predecessor's target the only one that could ever exist, so every
/// post-succession mint escrowed to a key the successor cannot derive).
/// Additional holder targets land **additively as sibling rows** keyed by
/// holder id, never by growing this one. Immutable: the value is deterministic
/// from the identity seed, so concurrent seed-holding writers produce
/// identical bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscrowTargetRecord {
    /// The identity's X-Wing escrow **public** key — derived from the seed
    /// under a frozen context by a seed-holding surface, published once.
    /// `BackupKey`-derived targets are structurally wrong here (every reading
    /// replica, removed devices included, retains `BackupKey`).
    #[serde(with = "serde_bytes")]
    pub xwing_escrow_pubkey: Vec<u8>,
}

/// One holder's durable receipt in `fauna.state.escrow-receipt`. Logical key =
/// [`escrow_receipt_cell_key`]
/// (`"<generation-id-hex>/<holder-id-hex>/<actor-id-hex>"`). Immutable — a
/// receipt is a fact, and re-deposit is idempotent per (generation id, wrap
/// hash). The identity segment and the signed [`Self::target_key`] are what
/// let a successor's re-escrow receipt land beside the predecessor's rather
/// than lose to it, and what stop a predecessor's receipt from acking a
/// generation for the successor (the succession rider).
///
/// **Holder-generic by contract** (a design constraint): `holder_id`
/// selects the verification profile — the v1 profile is the nest deployment
/// identity clients already pin, and T16-era non-nest holders slot in
/// additively. Verification code must never assume the deployment key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EscrowReceiptRecord {
    /// The escrowed generation.
    #[serde(with = "serde_bytes")]
    pub generation_id: [u8; 32],
    /// The holder's 32-byte verification identity (v1: the nest deployment
    /// key; never assumed — see the type doc).
    #[serde(with = "serde_bytes")]
    pub holder_id: [u8; 32],
    /// `BLAKE3` of the deposited escrow wrap — what makes the put idempotent
    /// and the receipt bind to one exact ciphertext.
    #[serde(with = "serde_bytes")]
    pub wrap_hash: [u8; 32],
    /// The escrow-target row's logical key the wrap was sealed under
    /// ([`escrow_target_identity_key`] — `identity/<actor-id-hex>`), echoed
    /// from the deposit request and **signed**: a receipt acks a deposit for
    /// exactly one identity, so the tip resolver counts only receipts naming
    /// the observer's own identity, and a predecessor's receipt (whose wrap
    /// the succession ceremony burns) never acks a generation for the
    /// successor.
    pub target_key: String,
    /// Holder-side stamp, unix ms. Advisory.
    pub stamped_at_ms: i64,
    /// The holder's signature over [`escrow_receipt_signing_bytes`] — the
    /// domain-separated (generation id, holder id, wrap hash, target key,
    /// stamp) payload.
    #[serde(with = "serde_bytes")]
    pub holder_sig: Vec<u8>,
}

/// Domain-separation tag for the escrow-receipt signature (step 4 of the
/// R14 build; frozen — a signature made here must never be replayable into
/// another context that signs bare bytes under the same key, and vice versa).
/// `.v2` since 2026-09-28: the payload gained the target-key hash (the
/// succession rider); v1 receipts were never produced by any installation
/// (the 2026-09-24 baseline reset), so no reader keeps a v1 arm.
pub const ESCROW_RECEIPT_SIG_CONTEXT: &[u8] = b"fauna.generation.escrow-receipt.v2\0";

/// The exact bytes a holder signs for one receipt — and the only bytes a
/// verifier checks. Fixed-width fields after the tag, so no length framing is
/// needed: tag ‖ generation id ‖ holder id ‖ wrap hash ‖ BLAKE3(target key) ‖
/// stamp (i64 LE). The target key rides as its hash so the payload stays
/// fixed-width whatever key grammar a future holder class binds under.
pub fn escrow_receipt_signing_bytes(
    generation_id: &[u8; 32],
    holder_id: &[u8; 32],
    wrap_hash: &[u8; 32],
    target_key: &str,
    stamped_at_ms: i64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(ESCROW_RECEIPT_SIG_CONTEXT.len() + 32 + 32 + 32 + 32 + 8);
    out.extend_from_slice(ESCROW_RECEIPT_SIG_CONTEXT);
    out.extend_from_slice(generation_id);
    out.extend_from_slice(holder_id);
    out.extend_from_slice(wrap_hash);
    out.extend_from_slice(blake3::hash(target_key.as_bytes()).as_bytes());
    out.extend_from_slice(&stamped_at_ms.to_le_bytes());
    out
}

/// Sign one escrow receipt as `holder` — the v1 profile: `holder_id` IS the
/// holder's Ed25519 verification key (for the nest, the deployment identity
/// clients already pin). Holder-generic by construction: nothing here knows
/// which *kind* of holder is signing.
pub fn sign_escrow_receipt(
    holder: &ed25519_dalek::SigningKey,
    generation_id: [u8; 32],
    wrap_hash: [u8; 32],
    target_key: &str,
    stamped_at_ms: i64,
) -> EscrowReceiptRecord {
    use ed25519_dalek::Signer;
    let holder_id = holder.verifying_key().to_bytes();
    let sig = holder.sign(&escrow_receipt_signing_bytes(
        &generation_id,
        &holder_id,
        &wrap_hash,
        target_key,
        stamped_at_ms,
    ));
    EscrowReceiptRecord {
        generation_id,
        holder_id,
        wrap_hash,
        target_key: target_key.to_string(),
        stamped_at_ms,
        holder_sig: sig.to_bytes().to_vec(),
    }
}

/// Verify a receipt's integrity **holder-generically**: the signature must
/// verify over [`escrow_receipt_signing_bytes`] under the verification
/// profile `holder_id` selects. The v1 profile — the only one today — reads
/// `holder_id` as an Ed25519 verification key; future holder classes slot in
/// additively here, never by callers hard-coding a key ("receipt =
/// deployment-key-signed" is exactly what this function exists to prevent).
///
/// Integrity only: whether the verified holder is one this account TRUSTS
/// (the pinned deployment identity, a chosen T16-era holder) is the caller's
/// question, answered against its own pin/holder set. The signature check
/// itself runs the strict primitive ([`crate::identity::verify_detached`]) —
/// `holder_id` is read off the receipt, so a permissive verify would let a
/// small-order holder id carry an all-zero signature and hand every caller a
/// "verified" receipt from a holder nobody holds a key to, leaving the trust
/// question as the only thing standing between a forged receipt and its
/// consumer.
pub fn verify_escrow_receipt(receipt: &EscrowReceiptRecord) -> Result<(), Error> {
    if receipt.holder_sig.len() != 64 {
        return Err(Error::Encoding("receipt signature must be 64 bytes".into()));
    }
    if !verified_signature(
        &receipt.holder_id,
        &escrow_receipt_signing_bytes(
            &receipt.generation_id,
            &receipt.holder_id,
            &receipt.wrap_hash,
            &receipt.target_key,
            receipt.stamped_at_ms,
        ),
        &receipt.holder_sig,
    ) {
        return Err(Error::Encoding("receipt signature does not verify".into()));
    }
    Ok(())
}

// ── Content-derived ids and the machinery keypairs (build step 5) ───────────

/// Domain-separation context for the **content-derived generation id** —
/// `BLAKE3` over the mint's canonical [`MintCore`] bytes under this context.
/// Frozen: the id is the mint row's logical key on every replica forever.
///
/// The core includes `key_commitment` (`owner-key-material.md` § The schedule
/// build design → *Key↔id binding*), so two same-shaped concurrent mints —
/// same parents, member set, minter — still fork into distinct ids: their
/// random keys differ, so their commitments differ, so the ids differ. Wraps
/// are deliberately **outside** the id: wrap ciphertexts are randomized, and
/// the id must be a deterministic function of what the mint *asserts*, not of
/// encryption randomness.
pub const GENERATION_ID_CONTEXT: &str = "fauna.generation.id.v1 2026-08-13";

/// The content-derived generation id of a mint: `BLAKE3` of the canonical
/// encoding of its DAG core under [`GENERATION_ID_CONTEXT`].
///
/// # Errors
/// Only on a core that fails canonical encoding — unreachable for honestly
/// constructed values.
pub fn generation_id(core: &MintCore) -> Result<[u8; 32], Error> {
    let bytes = crate::encoding::canonical_encode(core)?;
    Ok(blake3::derive_key(GENERATION_ID_CONTEXT, &bytes))
}

/// Domain-separation tag for the minter signature (ST-007; frozen — same
/// replay discipline as [`ESCROW_RECEIPT_SIG_CONTEXT`]).
pub const MINT_MINTER_SIG_CONTEXT: &[u8] = b"fauna.generation.mint-minter.v1\0";

/// The exact bytes a minter signs for one mint — the domain-separated
/// content-derived generation id, which already commits to the whole core
/// (parents, member set, minter, key commitment, stamp) under its own frozen
/// context, so this signature covers everything the id covers with no second
/// encoding of the core.
pub fn mint_minter_signing_bytes(generation_id: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(MINT_MINTER_SIG_CONTEXT.len() + 32);
    out.extend_from_slice(MINT_MINTER_SIG_CONTEXT);
    out.extend_from_slice(generation_id);
    out
}

/// Sign one mint as its minter. The signature's verification key is
/// [`MintCore::minter`] itself — a device id IS the device principal's
/// Ed25519 public key — so no key distribution rides this at all.
pub fn sign_mint_as_minter(
    minter: &ed25519_dalek::SigningKey,
    generation_id: &[u8; 32],
) -> Vec<u8> {
    use ed25519_dalek::Signer;
    minter
        .sign(&mint_minter_signing_bytes(generation_id))
        .to_bytes()
        .to_vec()
}

/// Verify a mint's minter signature against the **recomputed** content-derived
/// id (never the row key an attacker chose) and the core's own claimed minter.
///
/// Runs the strict primitive ([`crate::identity::verify_detached`]) because
/// `minter` is the core's own claimed key — attacker-chosen on an inbound row,
/// so a small-order key must never make an all-zero signature binding.
///
/// # Errors
/// A signature of the wrong width, or one that does not verify (a minter id
/// that is not a usable Ed25519 key lands in the latter).
pub fn verify_mint_minter_sig(
    minter: &[u8; 32],
    generation_id: &[u8; 32],
    sig: &[u8],
) -> Result<(), Error> {
    if sig.len() != 64 {
        return Err(Error::Encoding("minter signature must be 64 bytes".into()));
    }
    if !verified_signature(minter, &mint_minter_signing_bytes(generation_id), sig) {
        return Err(Error::Encoding("minter signature does not verify".into()));
    }
    Ok(())
}

/// **Authenticated authorship of a `Minted` row (ST-007)** — the one gate
/// every reader that acts on a mint's claims applies: the admissible-tip
/// resolver ([`resolve_admissible_tip`]) and the target-authored "cannot key"
/// signal pass. An honest `build_mint` (fauna-mls) cannot emit any of the three shapes
/// refused here, so each is a forgery:
///
/// - an **empty member set** (checked first — `all()` over nothing is the
///   vacuity the original admissibility predicate fell to);
/// - a **minter outside its own member set**;
/// - a **`minter_sig` that does not verify** over the recomputed
///   `generation_id` under the core's own claimed minter.
///
/// `generation_id` must be the id recomputed from `core` (the caller's
/// Key↔id binding), never a row key an attacker chose.
///
/// This proves only that the named minter signed; it says nothing about
/// whether that minter is *still* a member. A removed device signs validly
/// with its own key, so a reader deciding on current trust must ALSO require
/// [`FleetView::is_verified_member`] on `core.minter` (the resolver gets it
/// from full admissibility — every member verified).
///
/// # Errors
/// The human-readable reason the row is a forgery (the resolver's
/// `invalid` entry text).
pub fn verify_mint_authorship(
    generation_id: &[u8; 32],
    core: &MintCore,
    minter_sig: &[u8],
) -> Result<(), String> {
    if core.member_ids.is_empty() {
        return Err("mint member set is empty — a forged row; build_mint refuses it".to_string());
    }
    if !core.member_ids.contains(&core.minter) {
        return Err("mint minter is outside its own member set — a forged row".to_string());
    }
    verify_mint_minter_sig(&core.minter, generation_id, minter_sig)
        .map_err(|e| format!("mint authorship fails: {e}"))
}

/// Domain-separation tag for the top-up healer signature (frozen — same
/// replay discipline as [`MINT_MINTER_SIG_CONTEXT`]).
pub const TOPUP_HEALER_SIG_CONTEXT: &[u8] = b"fauna.generation.topup-healer.v1\0";

/// The exact bytes a healer signs for one [`GenerationWrapRecordV2`] wrap:
/// the domain tag, the three fixed-width ids, the big-endian stamp, and the
/// BLAKE3 hash of the wrap ciphertext — every field of the record, so no
/// field (the wrap bytes included) can be swapped under a carried-through
/// signature. Fixed-width throughout: no length ambiguity, no second
/// encoding.
#[must_use]
pub fn topup_healer_signing_bytes(
    generation_id: &[u8; 32],
    target_device: &[u8; 32],
    healer: &[u8; 32],
    at_ms: i64,
    wrap: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(TOPUP_HEALER_SIG_CONTEXT.len() + 32 * 4 + 8);
    out.extend_from_slice(TOPUP_HEALER_SIG_CONTEXT);
    out.extend_from_slice(generation_id);
    out.extend_from_slice(target_device);
    out.extend_from_slice(healer);
    out.extend_from_slice(&at_ms.to_be_bytes());
    out.extend_from_slice(blake3::hash(wrap).as_bytes());
    out
}

/// Sign one top-up wrap as its healer. The verification key is the healer's
/// device id itself (the [`sign_mint_as_minter`] pattern), so no key
/// distribution rides this.
pub fn sign_topup_as_healer(
    healer: &ed25519_dalek::SigningKey,
    generation_id: &[u8; 32],
    target_device: &[u8; 32],
    at_ms: i64,
    wrap: &[u8],
) -> Vec<u8> {
    use ed25519_dalek::Signer;
    healer
        .sign(&topup_healer_signing_bytes(
            generation_id,
            target_device,
            &healer.verifying_key().to_bytes(),
            at_ms,
            wrap,
        ))
        .to_bytes()
        .to_vec()
}

/// A parsed `fauna.state.generation-wrap` logical key — the per-healer
/// three-segment cell `"<generation>/<target>/<healer>"`,
/// the kind's only cell shape. (A two-segment cell key is not a cell of this
/// kind: the unattributed v1 wrap shape that lived there was retired
/// 2026-09-24 by the compat-remnant sweep — version-compatibility.md
/// § Dimension 2, program 4.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WrapCellKey {
    /// Segment 1.
    pub generation_id: [u8; 32],
    /// Segment 2.
    pub target_device: [u8; 32],
    /// Segment 3 — the cell's owning healer, and the verification key its
    /// rows must verify under.
    pub healer: [u8; 32],
}

/// The per-healer three-segment cell key. One
/// derivation, shared by the writer, both read paths, and the tests — never
/// re-formatted ad hoc.
#[must_use]
pub fn wrap_cell_key_per_healer(
    generation_id: &[u8; 32],
    target_device: &[u8; 32],
    healer: &[u8; 32],
) -> String {
    format!(
        "{}/{}/{}",
        crate::hex32::encode(generation_id),
        crate::hex32::encode(target_device),
        crate::hex32::encode(healer)
    )
}

/// Parse a wrap-kind logical key into its cell — canonical-or-nothing, the
/// same injectivity rule the device-set and mint keys hold: exactly three
/// segments, each exactly the canonical lowercase hex of its 32 bytes, so two
/// spellings of one cell cannot coexist as two cells.
#[must_use]
pub fn parse_wrap_cell_key(key: &str) -> Option<WrapCellKey> {
    let canonical = |segment: &str| -> Option<[u8; 32]> {
        let bytes = crate::hex32::decode(segment).ok()?;
        (crate::hex32::encode(&bytes) == segment).then_some(bytes)
    };
    let segments: Vec<&str> = key.split('/').collect();
    match segments.as_slice() {
        [g, t, h] => Some(WrapCellKey {
            generation_id: canonical(g)?,
            target_device: canonical(t)?,
            healer: canonical(h)?,
        }),
        _ => None,
    }
}

/// The `fauna.state.generation-wrap` join for a **per-healer** cell,
/// byte-level: both sides must decode, then rank =
/// (verifies-at-this-cell, at_ms, bytes), lexicographic max, winner returned
/// verbatim.
///
/// A verifying record beats any record that does not verify — which is the
/// whole fix: a `BackupKey` holder without the cell healer's Ed25519 secret
/// can never displace that healer's row, whatever stamp it invents. Within
/// verifying records (one honest author — the cell's healer — so this is its
/// own re-publications), the signed `at_ms` ranks, bytes break ties. A record
/// that decodes and does not verify merges in and loses, so a forgery stalls
/// nothing; refusing it outright is the adoption arm's job, at first contact.
///
/// **Decode-or-fail — a merge never ranks a value it cannot decode**
/// (`transport.md` § Schema and forward-compat discipline → *Rule 3 in
/// full*, the `consensus` ground; the record type is closed by design). A
/// side that does not decode — a later build's variant, or junk — is an
/// `Err`, which the walk reads as a row-content refusal: skipped without
/// being accounted, so `reconcile` presents it again to a build that can read
/// it. Ranking it lowest instead would keep the current row and account the
/// newer one as handled, and the store's idempotent ingest would never let
/// the later build apply it. The same contract holds for
/// [`join_generation_unkeyable`] and the group plane's four byte joins
/// (`crate::group_generation::{join_group_topup, join_group_unkeyable}`,
/// `crate::group_scope::{join_group_roster, join_authority_revocation}`),
/// and is the one [`join_device_set`] has always had.
pub fn join_generation_wrap_per_healer<'a>(
    generation_id: &[u8; 32],
    target_device: &[u8; 32],
    healer: &[u8; 32],
    current: &'a [u8],
    incoming: &'a [u8],
) -> Result<&'a [u8], Error> {
    let rank = |bytes: &[u8]| -> Result<(bool, i64), Error> {
        let rec = canonical_decode::<GenerationWrapRecordV2>(bytes)?;
        let verifies = rec.verifies_at(generation_id, target_device, healer);
        let GenerationWrapRecordV2::Wrap { at_ms, .. } = rec;
        Ok((verifies, at_ms))
    };
    let (cur, inc) = (rank(current)?, rank(incoming)?);
    Ok(if (cur, current) >= (inc, incoming) {
        current
    } else {
        incoming
    })
}

// ── The target-authored "cannot key generation G" signal (charter
// § The generation machinery, the unkeyable kind's bullet) ──────────────────

/// One target-authored signal in `fauna.state.generation-unkeyable`, at the
/// two-segment logical key `"<generation-id-hex>/<target-device-id-hex>"` —
/// one cell per (generation, target), authored ONLY by the target.
///
/// The cure for the one durable forged-partition case the hardening
/// left: in-place corruption of a mint member's own inline wrap. The member is
/// in the signed `member_ids` set and a wrap is present, so healers read the
/// pair as covered — only the target can try the open, and this record is how
/// it publishes "I hold this mint row and cannot key it". Rows verify only
/// under the **target's own device key** (cell segment 2 IS the Ed25519
/// verification key — the `minter_sig`/`healer_sig` pattern), so the signal is
/// unforgeable and un-squattable by construction.
///
/// The anti-churn contract rides the `tried` evidence: a healer republishes at
/// most once per assertion (its gate is "my current row's wrap hash is in
/// `tried`, or I have no row"), a satisfied target retracts with a later
/// [`Self::Satisfied`] in the same cell, and a converged pair goes byte-quiet.
/// A target that crashes after asserting therefore extracts at most one wrap
/// per healer, ever, for that assertion.
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `consensus` ground: every reader must reach the same
/// decision from the value, so there is no unknown arm and a reader that cannot
/// decode one fails rather than guess). A new variant is an edit to
/// `tools/check-additive-evolution/enum_ledger.txt`, made in the same change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GenerationUnkeyableRecord {
    /// "I cannot key this generation despite apparent coverage" — with the
    /// evidence already tried and failed.
    Asserted {
        /// The generation this device cannot key — echoes cell segment 1.
        #[serde(with = "serde_bytes")]
        generation_id: [u8; 32],
        /// The asserting target device id — echoes cell segment 2, and IS the
        /// Ed25519 verification key for `target_sig`.
        #[serde(with = "serde_bytes")]
        target_device: [u8; 32],
        /// The target's clock at assertion, unix ms. Signed and rising —
        /// ranks re-assertions and retractions within the cell.
        asserted_at_ms: i64,
        /// BLAKE3 hashes of the wrap ciphertexts tried and refused: the
        /// target's own inline `MemberWrap` entries plus every verifying
        /// per-healer top-up row targeting it by a currently-verified
        /// non-removed member — sorted, deduped, capped at
        /// [`MAX_UNKEYABLE_TRIED`] by byte order. Unverified-healer wraps
        /// are still *tried* on read but excluded here: the healer gate never
        /// consults them, and admitting them would let unauthenticated junk
        /// flood the capped list.
        #[serde(with = "crate::byte_array::vec")]
        tried: Vec<[u8; 32]>,
        /// Ed25519 by `target_device` over
        /// [`unkeyable_target_signing_bytes`] — every field covered.
        #[serde(with = "serde_bytes")]
        target_sig: Vec<u8>,
    },
    /// The retraction: "I can key this generation now" — published with a
    /// stamp rising past the assertion it retires, same cell.
    Satisfied {
        /// Echoes cell segment 1.
        #[serde(with = "serde_bytes")]
        generation_id: [u8; 32],
        /// Echoes cell segment 2 — the verification key, as above.
        #[serde(with = "serde_bytes")]
        target_device: [u8; 32],
        /// Rises past the retired assertion's stamp.
        asserted_at_ms: i64,
        /// Ed25519 by `target_device`, empty `tried` domain.
        #[serde(with = "serde_bytes")]
        target_sig: Vec<u8>,
    },
}

impl GenerationUnkeyableRecord {
    /// Does this record verify **at its cell** — both ids echoing the cell
    /// key's segments, and `target_sig` verifying under the cell's target id
    /// over exactly these fields? Pure over the record and the cell
    /// coordinates (the [`GenerationWrapRecordV2::verifies_at`] contract: a
    /// merge may never consult external state).
    #[must_use]
    pub fn verifies_at(&self, cell_generation: &[u8; 32], cell_target: &[u8; 32]) -> bool {
        let (generation_id, target_device, variant, at_ms, tried, sig) = match self {
            GenerationUnkeyableRecord::Asserted {
                generation_id,
                target_device,
                asserted_at_ms,
                tried,
                target_sig,
            } => (
                generation_id,
                target_device,
                UNKEYABLE_VARIANT_ASSERTED,
                *asserted_at_ms,
                tried.as_slice(),
                target_sig,
            ),
            GenerationUnkeyableRecord::Satisfied {
                generation_id,
                target_device,
                asserted_at_ms,
                target_sig,
            } => (
                generation_id,
                target_device,
                UNKEYABLE_VARIANT_SATISFIED,
                *asserted_at_ms,
                [].as_slice(),
                target_sig,
            ),
        };
        if generation_id != cell_generation || target_device != cell_target {
            return false;
        }
        verified_signature(
            target_device,
            &unkeyable_target_signing_bytes(generation_id, target_device, variant, at_ms, tried),
            sig,
        )
    }

    /// The signed stamp, whichever variant.
    #[must_use]
    pub fn asserted_at_ms(&self) -> i64 {
        match self {
            GenerationUnkeyableRecord::Asserted { asserted_at_ms, .. }
            | GenerationUnkeyableRecord::Satisfied { asserted_at_ms, .. } => *asserted_at_ms,
        }
    }
}

/// Domain-separation tag for the unkeyable-signal target signature (frozen —
/// same replay discipline as [`TOPUP_HEALER_SIG_CONTEXT`]).
pub const UNKEYABLE_TARGET_SIG_CONTEXT: &[u8] = b"fauna.generation.unkeyable-target.v1\0";

/// The `Asserted` variant's byte in the signing domain.
pub const UNKEYABLE_VARIANT_ASSERTED: u8 = 1;
/// The `Satisfied` variant's byte in the signing domain.
pub const UNKEYABLE_VARIANT_SATISFIED: u8 = 2;

/// Cap on an assertion's `tried` evidence list. Sized for the real bound —
/// one hash per fleet member (per-healer cells) plus the inline wrap — under
/// the uniform `MAX_ITEM_VALUE_BYTES` value budget; overflow trims by byte
/// order, deterministically. A hard-coded constant, invariant bucket (1).
pub const MAX_UNKEYABLE_TRIED: usize = 64;

/// The exact bytes a target signs for one [`GenerationUnkeyableRecord`]: the
/// domain tag, the variant byte, both fixed-width ids, the big-endian stamp,
/// the big-endian hash count, and the concatenated 32-byte hashes — every
/// field of either variant, fixed-width throughout, so no field can be
/// swapped (and no variant re-interpreted) under a carried-through signature.
#[must_use]
pub fn unkeyable_target_signing_bytes(
    generation_id: &[u8; 32],
    target_device: &[u8; 32],
    variant: u8,
    asserted_at_ms: i64,
    tried: &[[u8; 32]],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        UNKEYABLE_TARGET_SIG_CONTEXT.len() + 1 + 32 * 2 + 8 + 4 + 32 * tried.len(),
    );
    out.extend_from_slice(UNKEYABLE_TARGET_SIG_CONTEXT);
    out.push(variant);
    out.extend_from_slice(generation_id);
    out.extend_from_slice(target_device);
    out.extend_from_slice(&asserted_at_ms.to_be_bytes());
    out.extend_from_slice(&u32::try_from(tried.len()).unwrap_or(u32::MAX).to_be_bytes());
    for hash in tried {
        out.extend_from_slice(hash);
    }
    out
}

/// Sign one unkeyable record as its target. The verification key is the
/// target's device id itself (the [`sign_topup_as_healer`] pattern). Pass an
/// empty `tried` for a `Satisfied`.
pub fn sign_unkeyable_as_target(
    target: &ed25519_dalek::SigningKey,
    generation_id: &[u8; 32],
    variant: u8,
    asserted_at_ms: i64,
    tried: &[[u8; 32]],
) -> Vec<u8> {
    use ed25519_dalek::Signer;
    target
        .sign(&unkeyable_target_signing_bytes(
            generation_id,
            &target.verifying_key().to_bytes(),
            variant,
            asserted_at_ms,
            tried,
        ))
        .to_bytes()
        .to_vec()
}

/// The `fauna.state.generation-unkeyable` cell key. One derivation, shared by
/// the writer, both consumers, and the tests — never re-formatted ad hoc.
#[must_use]
pub fn unkeyable_cell_key(generation_id: &[u8; 32], target_device: &[u8; 32]) -> String {
    format!(
        "{}/{}",
        crate::hex32::encode(generation_id),
        crate::hex32::encode(target_device)
    )
}

/// Parse an unkeyable-kind logical key — canonical-or-nothing, the
/// [`parse_wrap_cell_key`] injectivity rule.
#[must_use]
pub fn parse_unkeyable_cell_key(key: &str) -> Option<([u8; 32], [u8; 32])> {
    let canonical = |segment: &str| -> Option<[u8; 32]> {
        let bytes = crate::hex32::decode(segment).ok()?;
        (crate::hex32::encode(&bytes) == segment).then_some(bytes)
    };
    let segments: Vec<&str> = key.split('/').collect();
    match segments.as_slice() {
        [g, t] => Some((canonical(g)?, canonical(t)?)),
        _ => None,
    }
}

/// The `fauna.state.generation-unkeyable` join — byte-level, decode-or-fail:
/// rank = (verifies-at-this-cell, signed stamp, bytes), lexicographic max,
/// winner returned verbatim (the [`join_generation_wrap_per_healer`] shape
/// and contract, for the same reasons: a verifying record beats any record
/// that does not verify, so nothing displaces the target's own row; within
/// verifying records — one honest author, the cell's target — the signed
/// stamp ranks its own re-assertions and retractions, bytes break ties; a
/// side that does not decode is an `Err`, never ranked; first-contact
/// strictness lives in the adoption arm).
pub fn join_generation_unkeyable<'a>(
    generation_id: &[u8; 32],
    target_device: &[u8; 32],
    current: &'a [u8],
    incoming: &'a [u8],
) -> Result<&'a [u8], Error> {
    let rank = |bytes: &[u8]| -> Result<(bool, i64), Error> {
        let rec = canonical_decode::<GenerationUnkeyableRecord>(bytes)?;
        Ok((
            rec.verifies_at(generation_id, target_device),
            rec.asserted_at_ms(),
        ))
    };
    let (cur, inc) = (rank(current)?, rank(incoming)?);
    Ok(if (cur, current) >= (inc, incoming) {
        current
    } else {
        incoming
    })
}

// ── The target-authored "I hold these generations" statement (charter § The
// generation machinery, the reach kind's bullet + *Fleet-scope reclamation*) ──

/// One device's row in `fauna.state.device-reach`, at the logical key = the
/// device id hex — one cell per device, authored ONLY by that device.
///
/// The possession evidence the reclamation ruling rests on: a target's own
/// signed list of the live `Minted` generations it can key. The top-up pass
/// counts a verifying reach by a currently-verified non-removed member as
/// coverage for every generation it lists — ahead of any per-healer cell —
/// which is what lets a healer retire its own cells the moment the target
/// says it holds the key. Rows verify only under the device's own key (the cell key
/// IS the Ed25519 verification key — the `minter_sig`/`healer_sig`/
/// `target_sig` pattern), so the evidence is unforgeable and un-squattable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceReachRecord {
    /// The device — echoes the cell key, and IS the verification key.
    #[serde(with = "serde_bytes")]
    pub device_id: [u8; 32],
    /// The device's clock at publication, unix ms. Signed and rising — ranks
    /// re-publications within the cell.
    pub at_ms: i64,
    /// The generation ids this device can key, sorted ascending and deduped
    /// (canonical, so two publications of one set are byte-identical). Live
    /// `Minted` generations only: a shredded generation drops out, which is
    /// what keeps the row bounded over a long life.
    #[serde(with = "crate::byte_array::vec")]
    pub holds: Vec<[u8; 32]>,
    /// Ed25519 by `device_id` over [`reach_signing_bytes`] — every field.
    #[serde(with = "serde_bytes")]
    pub device_sig: Vec<u8>,
}

impl DeviceReachRecord {
    /// Does this record verify **at its cell** — `device_id` echoing the cell
    /// key and `device_sig` verifying under it over exactly these fields?
    /// Pure over the record and the cell coordinate (a merge may never consult
    /// external state).
    #[must_use]
    pub fn verifies_at(&self, cell_device: &[u8; 32]) -> bool {
        if self.device_id != *cell_device {
            return false;
        }
        verified_signature(
            &self.device_id,
            &reach_signing_bytes(&self.device_id, self.at_ms, &self.holds),
            &self.device_sig,
        )
    }

    /// Does this (already verified) reach list `generation`?
    #[must_use]
    pub fn holds(&self, generation: &[u8; 32]) -> bool {
        self.holds.binary_search(generation).is_ok()
    }
}

/// Domain-separation tag for the reach signature (frozen — the
/// [`UNKEYABLE_TARGET_SIG_CONTEXT`] replay discipline).
pub const REACH_SIG_CONTEXT: &[u8] = b"fauna.generation.device-reach.v1\0";

/// The exact bytes a device signs for one [`DeviceReachRecord`]: the domain
/// tag, the fixed-width id, the big-endian stamp, the big-endian count, and
/// the concatenated 32-byte generation ids — fixed-width throughout, so no
/// field can be swapped under a carried-through signature.
#[must_use]
pub fn reach_signing_bytes(device_id: &[u8; 32], at_ms: i64, holds: &[[u8; 32]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(REACH_SIG_CONTEXT.len() + 32 + 8 + 4 + 32 * holds.len());
    out.extend_from_slice(REACH_SIG_CONTEXT);
    out.extend_from_slice(device_id);
    out.extend_from_slice(&at_ms.to_be_bytes());
    out.extend_from_slice(&u32::try_from(holds.len()).unwrap_or(u32::MAX).to_be_bytes());
    for id in holds {
        out.extend_from_slice(id);
    }
    out
}

/// Build and sign this device's reach over `holds` (sorted + deduped here,
/// so a caller cannot publish a non-canonical list).
pub fn sign_device_reach(
    device: &ed25519_dalek::SigningKey,
    at_ms: i64,
    holds: impl IntoIterator<Item = [u8; 32]>,
) -> DeviceReachRecord {
    use ed25519_dalek::Signer;
    let device_id = device.verifying_key().to_bytes();
    let mut holds: Vec<[u8; 32]> = holds.into_iter().collect();
    holds.sort_unstable();
    holds.dedup();
    let device_sig = device
        .sign(&reach_signing_bytes(&device_id, at_ms, &holds))
        .to_bytes()
        .to_vec();
    DeviceReachRecord {
        device_id,
        at_ms,
        holds,
        device_sig,
    }
}

/// The `fauna.state.device-reach` cell key — the canonical device id hex.
#[must_use]
pub fn reach_cell_key(device_id: &[u8; 32]) -> String {
    crate::hex32::encode(device_id)
}

/// Parse a reach-kind logical key — canonical-or-nothing, the
/// [`parse_wrap_cell_key`] injectivity rule.
#[must_use]
pub fn parse_reach_cell_key(key: &str) -> Option<[u8; 32]> {
    let bytes = crate::hex32::decode(key).ok()?;
    (crate::hex32::encode(&bytes) == key).then_some(bytes)
}

/// The `fauna.state.device-reach` join — byte-level and total: rank =
/// (verifies-at-this-cell, signed stamp, bytes), lexicographic max, winner
/// returned verbatim (the [`join_generation_unkeyable`] shape, for the same
/// reasons).
#[must_use]
pub fn join_device_reach<'a>(
    device_id: &[u8; 32],
    current: &'a [u8],
    incoming: &'a [u8],
) -> &'a [u8] {
    let rank = |bytes: &[u8]| -> (bool, i64) {
        match canonical_decode::<DeviceReachRecord>(bytes) {
            Ok(rec) => (rec.verifies_at(device_id), rec.at_ms),
            Err(_) => (false, i64::MIN),
        }
    };
    if (rank(current), current) >= (rank(incoming), incoming) {
        current
    } else {
        incoming
    }
}

// ── The remover's "seal nothing more under G" statement (charter § The
// generation machinery, the closed kind's bullet + *The mint protocol,
// trigger (b)*) ──

/// One generation's row in `fauna.state.generation-closed`, at the logical
/// key = the generation id hex — one cell per generation.
///
/// **The row's presence is the statement; nothing in the value is consulted.**
/// The device that writes another device's `Removed` row writes one of these
/// per generation the removed device may key ([`closure_set`]), and
/// [`resolve_admissible_tip`] takes no closed generation as a sealing
/// candidate. The fields are audit only, which is why the kind carries no
/// signature: two rows at one cell say the same thing, and no forgery can
/// make a closed generation open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationClosedRecord {
    /// The device that closed the generation — the remover.
    #[serde(with = "serde_bytes")]
    pub closed_by: [u8; 32],
    /// The removed device whose removal this closure answers.
    #[serde(with = "serde_bytes")]
    pub answers: [u8; 32],
    /// The closing device's clock, unix ms. Advisory, like every stamp here.
    pub closed_at_ms: i64,
}

/// The `fauna.state.generation-closed` cell key — the canonical generation id
/// hex.
#[must_use]
pub fn closed_cell_key(generation_id: &[u8; 32]) -> String {
    crate::hex32::encode(generation_id)
}

/// Parse a closed-kind logical key — canonical-or-nothing, the
/// [`parse_wrap_cell_key`] injectivity rule.
#[must_use]
pub fn parse_closed_cell_key(key: &str) -> Option<[u8; 32]> {
    let bytes = crate::hex32::decode(key).ok()?;
    (crate::hex32::encode(&bytes) == key).then_some(bytes)
}

/// The `fauna.state.generation-closed` join — the byte-order max of the two
/// canonical encodings, winner returned verbatim. Total, and free to be
/// arbitrary: no reader consults the value.
#[must_use]
pub fn join_generation_closed<'a>(current: &'a [u8], incoming: &'a [u8]) -> &'a [u8] {
    if current >= incoming {
        current
    } else {
        incoming
    }
}

/// The generations merged `fauna.state.generation-closed` rows name — what
/// [`resolve_admissible_tip`] takes as `closed`. A row counts by its key
/// alone; a key that is not a canonical generation id names nothing.
pub fn closed_generations<'a, C>(closed_rows: C) -> std::collections::BTreeSet<[u8; 32]>
where
    C: IntoIterator<Item = (&'a str, &'a [u8])>,
{
    closed_rows
        .into_iter()
        .filter_map(|(key, _)| parse_closed_cell_key(key))
        .collect()
}

/// **The closure set** of one removal (charter § The generation machinery →
/// *The mint protocol, trigger (b)*): the generations the remover closes
/// when it removes `removed` — every `Minted` row that is id-bound, passes
/// [`verify_mint_authorship`], and whose members `view` wholly verifies with
/// `removed` already excluded. Ascending id order.
///
/// These are the generations that could still be a sealing candidate once the
/// removal merges, at some observer: no ack check and no keyability check
/// (both are observer-relative), and no cap (an admissible generation takes a
/// verified member's device secret to author, so invented mints draw nothing).
/// A mint that names `removed` is left out — the member rule already unseats
/// it wherever the `Removed` row merges.
pub fn closure_set<'a, M>(view: &FleetView, mint_rows: M, removed: &[u8; 32]) -> Vec<[u8; 32]>
where
    M: IntoIterator<Item = (&'a str, &'a [u8])>,
{
    let mut closing = std::collections::BTreeSet::new();
    for (key, value) in mint_rows {
        let Ok(GenerationMintRecord::Minted {
            core, minter_sig, ..
        }) = canonical_decode::<GenerationMintRecord>(value)
        else {
            continue;
        };
        let Ok(id) = generation_id(&core) else {
            continue;
        };
        if crate::hex32::encode(&id) != key
            || verify_mint_authorship(&id, &core, &minter_sig).is_err()
        {
            continue;
        }
        if core
            .member_ids
            .iter()
            .all(|m| m != removed && view.is_verified_member(m))
        {
            closing.insert(id);
        }
    }
    closing.into_iter().collect()
}

/// Cap on a first-need heal-mint's parent list — the current DAG leaves,
/// byte-order max preferred under flood (charter § The mint protocol; a
/// hard-coded constant, invariant bucket (1)).
pub const MAX_MINT_PARENTS: usize = 32;

/// Cap on the **inline** member wraps one `Minted` row carries — the bounded
/// mint (charter § The mint protocol → *The bounded mint*, ruled 2026-09-15).
///
/// A mint over more verified members than this still lists them ALL in
/// [`MintCore::member_ids`] — the signed set admissibility and severance
/// range over is unchanged — and wraps inline only the members
/// [`inline_wrap_members`] names; every other member is reached by the
/// minter's own `fauna.state.generation-wrap` top-up rows, written in the
/// same sequence ahead of the escrow receipt. One inline wrap is about
/// 1.3 KB as encoded (an X-Wing encapsulation plus the AEAD'd key, carried
/// as a CBOR byte string), so the cap is what keeps a mint row under the
/// plane's per-entry byte cap at any fleet size up to [`MAX_MINT_MEMBERS`].
/// Sized small on purpose: the spill costs the same rows whether the cap is
/// 8 or 16, while every inline wrap the cap admits comes out of the member
/// list's byte budget. The arithmetic is pinned by MEASUREMENT in
/// `fauna_account_plane::generation_mint`'s tests — never by these two numbers
/// alone. Hard-coded: invariant bucket (1).
pub const MAX_INLINE_MEMBER_WRAPS: usize = 8;

/// Ceiling on a mint's member set — the bound the member list's own encoding
/// imposes under the per-entry byte cap once the inline wraps are capped:
/// every listed id costs 34 B as encoded (a 32-byte byte string), a full
/// parent list about 1.1 KB, the inline wraps about 11 KB, and the whole row must seal under
/// 64 KiB. `build_mint` refuses a larger fleet before any key material
/// exists, naming this ceiling; the writer door's pre-deposit size check is
/// the backstop behind it. The plane's live-entry COUNT cap binds earlier for
/// large fleets (charter § The bounded mint states the arithmetic); this
/// constant is the byte ceiling only. Hard-coded: invariant bucket (1).
pub const MAX_MINT_MEMBERS: usize = 512;

/// The inline-wrap set of a mint over `member_ids` minted by `minter`: the
/// minter itself — always, because candidacy at the minter is decided by
/// what the *plane* distributes, never by its retained bundle
/// (`generation_tip::resolve_tip`), and a self-top-up is noise the top-up
/// pass deliberately never writes — plus the first
/// `MAX_INLINE_MEMBER_WRAPS - 1` other ids in ascending byte order.
///
/// Pure and deterministic over its inputs, so every reader of a mint row
/// names the same set the builder did. A `member_ids` list
/// that does not contain `minter` still yields the minter inline; callers
/// guarantee membership (`build_mint` refuses a foreign minter, the resolver
/// counts one as a forgery).
#[must_use]
pub fn inline_wrap_members(
    member_ids: &[[u8; 32]],
    minter: &[u8; 32],
) -> std::collections::BTreeSet<[u8; 32]> {
    let mut others: Vec<[u8; 32]> = member_ids
        .iter()
        .copied()
        .filter(|id| id != minter)
        .collect();
    others.sort_unstable();
    others.dedup();
    let mut inline = std::collections::BTreeSet::new();
    inline.insert(*minter);
    inline.extend(
        others
            .into_iter()
            .take(MAX_INLINE_MEMBER_WRAPS.saturating_sub(1)),
    );
    inline
}

/// Domain-separation context for the ML-KEM-768 half of a device's X-Wing
/// device-KEM keypair ([`derive_device_xwing_keypair`]). Frozen — the public
/// half rests in every device-set enrollment row.
pub const DEVICE_KEM_MLKEM_CONTEXT: &str = "fauna.generation.device-kem.mlkem.v1 2026-08-13";

/// Domain-separation context for the X25519 half of the device KEM keypair.
/// A distinct scalar from the device's Ed25519 identity — no cross-protocol
/// key reuse (the `derive_keypair_from_ikm` discipline).
pub const DEVICE_KEM_X25519_CONTEXT: &str = "fauna.generation.device-kem.x25519.v1 2026-08-13";

/// The device's X-Wing device-KEM keypair — the wrap target a minter seals
/// this device's generation wraps to — derived deterministically from the
/// device principal's Ed25519 secret under the two frozen contexts above
/// (derive-then-keygen, the `derive_recipient_xwing_keypair` shape; X-Wing
/// keygen is deterministic from a 32-byte root).
///
/// Deterministic ⇒ nothing new is stored or synced: the device re-derives the
/// secret half on demand, and publishes the public half once, in its
/// `fauna.state.device-set` enrollment record — which is where a minter finds
/// every member's wrap target.
#[must_use]
pub fn derive_device_xwing_keypair(device_ed25519_secret: &[u8; 32]) -> fauna_pq_kem::XWingKeyPair {
    fauna_pq_kem::derive_keypair_from_ikm(
        device_ed25519_secret,
        DEVICE_KEM_MLKEM_CONTEXT,
        DEVICE_KEM_X25519_CONTEXT,
    )
}

/// Domain-separation context for the ML-KEM-768 half of the identity's X-Wing
/// **escrow** keypair ([`derive_escrow_xwing_keypair`]). Frozen — escrow wraps
/// rest with every holder forever.
pub const ESCROW_KEM_MLKEM_CONTEXT: &str = "fauna.generation.escrow-kem.mlkem.v1 2026-08-13";

/// Domain-separation context for the X25519 half of the escrow keypair. A
/// distinct scalar from the identity's Ed25519 signing key and from the
/// classical recovery escrow (`RecoveryKey`'s X25519 — a *different* root and
/// context; that one seals the seed blob to the recovery kit, this one seals
/// generation keys to the identity).
pub const ESCROW_KEM_X25519_CONTEXT: &str = "fauna.generation.escrow-kem.x25519.v1 2026-08-13";

/// The identity's X-Wing escrow keypair — the target every per-generation
/// escrow wrap seals to — derived deterministically from the **identity
/// seed** under the two frozen contexts above.
///
/// Seed-derived and never `BackupKey`-derived, structurally
/// (`owner-key-material.md` § The schedule build design → *The escrow
/// target*): every reading replica — removed devices included — retains
/// `BackupKey` forever, so a `BackupKey`-reachable escrow wrap would hand the
/// generation keys back to exactly the party R14 severs. Recovery: seed
/// ceremony → re-derive this secret → unwrap the blob any one surviving
/// holder serves.
#[must_use]
pub fn derive_escrow_xwing_keypair(identity_seed: &[u8; 32]) -> fauna_pq_kem::XWingKeyPair {
    fauna_pq_kem::derive_keypair_from_ikm(
        identity_seed,
        ESCROW_KEM_MLKEM_CONTEXT,
        ESCROW_KEM_X25519_CONTEXT,
    )
}

/// The identity's `fauna.state.escrow-target` row value, built by a
/// seed-holding surface (first onboarding for new accounts; any seed-holding
/// session once, for existing accounts, before their first mint).
///
/// Deterministic from the seed ⇒ concurrent seed-holding writers produce
/// byte-identical values, which is what lets the kind register Immutable with
/// no race to arbitrate. Without this row on the plane no mint can escrow,
/// and fleet-only sealing stays refused with a precise error — the mint door
/// owns that refusal (`fauna_sync_engine::generation_mint`).
#[must_use]
pub fn escrow_target_record(identity_seed: &[u8; 32]) -> EscrowTargetRecord {
    EscrowTargetRecord {
        xwing_escrow_pubkey: derive_escrow_xwing_keypair(identity_seed)
            .public
            .to_bytes()
            .to_vec(),
    }
}

/// The `fauna.state.escrow-target` logical key of ONE identity's target row:
/// `identity/<actor-id-hex>` — one Immutable row per identity the account has
/// had, so a successor publishes its own row beside the predecessor's instead
/// of colliding with it. The same string is the escrow wrap's AAD `target_key`
/// (`fauna_mls::wrapped_blob::generation_wraps`) and the receipt's signed
/// [`EscrowReceiptRecord::target_key`], so a wrap or receipt made for identity
/// X can never be read as one for identity Y. Grammar owner: the account-data
/// taxonomy's generation-machinery section; `fauna_protocol::merge_policy`
/// re-exports this beside the kind string. (The constant key `"identity"` it
/// replaces was retired 2026-09-28 under the baseline reset — nothing sealed
/// under it exists anywhere.)
#[must_use]
pub fn escrow_target_identity_key(actor_id: &ActorId) -> String {
    format!("identity/{}", crate::hex32::encode(&actor_id.0))
}

/// The `fauna.state.escrow-receipt` logical key:
/// `<generation-id-hex>/<holder-id-hex>/<actor-id-hex>` — one Immutable
/// receipt per (generation, holder, identity). The identity segment is what
/// lets a successor's re-escrow receipt land beside the predecessor's rather
/// than lose the Immutable first-wins to it.
#[must_use]
pub fn escrow_receipt_cell_key(
    generation_id: &[u8; 32],
    holder_id: &[u8; 32],
    actor_id: &ActorId,
) -> String {
    format!(
        "{}/{}/{}",
        crate::hex32::encode(generation_id),
        crate::hex32::encode(holder_id),
        crate::hex32::encode(&actor_id.0)
    )
}

/// Domain-separation tag for the enrollment self-signature (frozen — the
/// [`REACH_SIG_CONTEXT`] replay discipline).
pub const DEVICE_SET_SIG_CONTEXT: &[u8] = b"fauna.generation.device-set-enrollment.v1\0";

/// The exact bytes a device signs for its own [`DeviceSetRecord::Enrolled`]:
/// the domain tag, the fixed-width device id, the big-endian stamp, then the
/// two variable-length fields each behind a big-endian byte count — so no
/// field can be swapped, and no byte moved between fields, under a
/// carried-through signature. The id is IN the preimage even though the cell
/// key already names it: a signature over the payload alone would verify
/// wherever the payload was filed, and the join must reject the same bytes
/// re-filed under another device's cell.
#[must_use]
pub fn enrollment_signing_bytes(
    device_id: &[u8; 32],
    xwing_pubkey: &[u8],
    authorization: &[u8],
    enrolled_at_ms: i64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        DEVICE_SET_SIG_CONTEXT.len() + 32 + 8 + 4 + xwing_pubkey.len() + 4 + authorization.len(),
    );
    out.extend_from_slice(DEVICE_SET_SIG_CONTEXT);
    out.extend_from_slice(device_id);
    out.extend_from_slice(&enrolled_at_ms.to_be_bytes());
    out.extend_from_slice(
        &u32::try_from(xwing_pubkey.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    out.extend_from_slice(xwing_pubkey);
    out.extend_from_slice(
        &u32::try_from(authorization.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    out.extend_from_slice(authorization);
    out
}

/// Build and self-sign this device's enrollment record — the ONE production
/// shape of an `Enrolled` row since the self-signature ruling. The X-Wing
/// public half is derived here from the same secret that signs
/// ([`derive_device_xwing_keypair`]), so a caller cannot publish a wrap
/// target its own device KEM secret does not open. `authorization` is the
/// canonical `EmbedAsBytes` carriage of the root-or-chain-signed
/// `DeviceAuthorization` covering this device.
#[must_use]
pub fn sign_device_enrollment(
    device: &ed25519_dalek::SigningKey,
    authorization: Vec<u8>,
    enrolled_at_ms: i64,
) -> DeviceSetRecord {
    use ed25519_dalek::Signer;
    let device_id = device.verifying_key().to_bytes();
    let xwing_pubkey = derive_device_xwing_keypair(&device.to_bytes())
        .public
        .to_bytes()
        .to_vec();
    let device_sig = device
        .sign(&enrollment_signing_bytes(
            &device_id,
            &xwing_pubkey,
            &authorization,
            enrolled_at_ms,
        ))
        .to_bytes()
        .to_vec();
    DeviceSetRecord::Enrolled {
        xwing_pubkey,
        authorization,
        enrolled_at_ms,
        device_sig,
    }
}

impl DeviceSetRecord {
    /// Does this record **self-verify at its cell** — an `Enrolled` whose
    /// `device_sig` verifies under `cell_device` over exactly its own fields?
    /// Pure over the record and the cell coordinate (a merge may never
    /// consult external state). `false` for a `Removed`, and for an
    /// `Enrolled` with an absent or non-verifying signature — which is what
    /// lets the join rank a device's own signed row above anything else at
    /// its cell, and what the fleet view requires of every member.
    #[must_use]
    pub fn self_verifies_at(&self, cell_device: &[u8; 32]) -> bool {
        match self {
            DeviceSetRecord::Enrolled {
                xwing_pubkey,
                authorization,
                enrolled_at_ms,
                device_sig,
            } => {
                !device_sig.is_empty()
                    && verified_signature(
                        cell_device,
                        &enrollment_signing_bytes(
                            cell_device,
                            xwing_pubkey,
                            authorization,
                            *enrolled_at_ms,
                        ),
                        device_sig,
                    )
            }
            DeviceSetRecord::Removed { .. } => false,
        }
    }
}

/// The `fauna.state.device-set` cell key — the canonical device id hex (the
/// [`reach_cell_key`] shape: one cell per device).
#[must_use]
pub fn device_set_cell_key(device_id: &[u8; 32]) -> String {
    crate::hex32::encode(device_id)
}

/// Parse a device-set logical key — canonical-or-nothing, the
/// [`parse_reach_cell_key`] injectivity rule.
#[must_use]
pub fn parse_device_set_cell_key(key: &str) -> Option<[u8; 32]> {
    parse_reach_cell_key(key)
}

/// The `fauna.state.device-set` join, byte-level and **key-aware**: both
/// sides must decode as [`DeviceSetRecord`] (validation — a value that will
/// not decode fails loudly, never merges silently), then rank = (`Removed`
/// absorbs, self-verifies at `cell_device`, bytes), lexicographic max, the
/// winning input returned verbatim — so a device's own signed enrollment is
/// never displaced by a forgery carrying its cert; two non-verifying rows
/// break ties by byte order (honest operation never produces two differing
/// `Enrolled` rows at one cell, each device writing only its own). A join over the lexicographic product of three total orders:
/// commutative, associative, idempotent by construction.
pub fn join_device_set(
    cell_device: &[u8; 32],
    current: &[u8],
    incoming: &[u8],
) -> Result<Vec<u8>, Error> {
    let cur: DeviceSetRecord = canonical_decode(current)?;
    let inc: DeviceSetRecord = canonical_decode(incoming)?;
    let rank = |rec: &DeviceSetRecord| -> (bool, bool) {
        (
            matches!(rec, DeviceSetRecord::Removed { .. }),
            rec.self_verifies_at(cell_device),
        )
    };
    Ok(if (rank(&cur), current) >= (rank(&inc), incoming) {
        current.to_vec()
    } else {
        incoming.to_vec()
    })
}

/// The `fauna.state.generation-mint` join, byte-level: `Shredded` absorbs,
/// byte-order max within a phase — the [`two_phase_winner`] lattice
/// [`join_device_set`] shared before it became key-aware; same decode-or-fail
/// contract.
pub fn join_generation_mint(current: &[u8], incoming: &[u8]) -> Result<Vec<u8>, Error> {
    let cur: GenerationMintRecord = canonical_decode(current)?;
    let inc: GenerationMintRecord = canonical_decode(incoming)?;
    let cur_shredded = matches!(cur, GenerationMintRecord::Shredded { .. });
    let inc_shredded = matches!(inc, GenerationMintRecord::Shredded { .. });
    Ok(two_phase_winner(current, cur_shredded, incoming, inc_shredded).to_vec())
}

// ── The verified fleet view — the device-set reader (build step 3) ──────────
//
// The charter's authority clause ("enrollment carries a valid root-or-chain-
// signed `DeviceAuthorization`; removal by an enrolled, non-removed device or
// a root surface — verified at the reader") is realized here as a **pure,
// deterministic view over merged rows**, per the step-3 build ruling
// (2026-08-13, recorded in `account-data-plane.md` § The generation
// machinery):
//
// * **Membership is cert-verified** — the load-bearing boundary. Only ids
//   whose `Enrolled` row carries a `DeviceAuthorization` signed by the
//   account root — and by nothing else: the device set does not cross a
//   succession, so no predecessor identity signs here (ruled 2026-10-01,
//   `account-data-taxonomy.md` § The generation machinery → *The source of
//   `prior`*) — and covering exactly that id become members; only members
//   are wrap targets, and a generation tip is admissible only when every member of its mint verifies here. A removed
//   device still holds `BackupKey` forever (stated R14 exposure), so it can
//   *seal* device-set rows — this check is what stops it (or any other
//   fleet-key holder) from ever placing an unauthorized id into the wrap
//   target set.
// * **Exclusion is unconditional** — a `Removed` row excludes its id
//   regardless of attribution. A writer-authority *precondition* on removals
//   would consult non-monotone merged state (was the remover enrolled and
//   non-removed?): two replicas meeting a mutual-removal race in opposite
//   orders would refuse opposite rows and diverge permanently — breaking
//   exactly the removal-monotonicity the admissible-tip convergence argument
//   rests on. And refusing a "stop trusting" signal on verification grounds
//   extends trust in the dangerous direction: an ignored removal keeps a
//   stolen device in the wrap-target set. Honoring removals eagerly is
//   fail-safe; the blast radius is the charter's accepted
//   vandalism-not-escalation posture, backstopped by the root ceremony.
// * **Attribution is advisory** — `removed_by` is classified (root / member /
//   unverified) for audit surfaces, never consulted for exclusion. Stricter
//   removal authority (root-signed removal certificates, quorum) is the
//   charter's post-W5 (account-data-plane.md § Workstreams) opt-in narrowing; it lands as additive value fields
//   and this view is its extension point.

use crate::data::DeviceAuthorization;
use crate::encoding::{EmbedAsBytes, decode_signed_bytes, verify_envelope};
use crate::identity::ActorId;
use std::collections::BTreeMap;

/// One verified fleet member — a mint wrap target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetMember {
    /// The device id (= device principal Ed25519 public key = NodeId).
    pub device_id: [u8; 32],
    /// The X-Wing device-KEM public key a minter wraps to.
    pub xwing_pubkey: Vec<u8>,
    /// The enrollment's asserted instant, unix ms (advisory, like all stamps).
    pub enrolled_at_ms: i64,
}

/// Advisory classification of a removal's claimed authority — audit surface
/// material, never an exclusion gate (module ruling above).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalAttribution {
    /// `removed_by` is the account root.
    Root,
    /// `removed_by` is itself a verified member of this view.
    Member,
    /// `removed_by` matches no verifiable authority — the claim is recorded,
    /// the exclusion stands regardless.
    Unverified,
}

/// The verified reading of an account's merged `fauna.state.device-set` rows.
///
/// Build it from **merged plane state** (one row per device id) plus the
/// account's identity line; every verdict is a deterministic function of those
/// inputs — no clock, no arrival order, no local preference — so two replicas
/// with converged rows hold identical views.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FleetView {
    members: BTreeMap<[u8; 32], FleetMember>,
    removed: BTreeMap<[u8; 32], RemovalAttribution>,
    /// Rows that verify as nothing: `(row key, reason)`. A flagged row makes
    /// its id a non-member (never a wrap target, never admissible) but does
    /// NOT exclude an id another, valid row enrolls — the canonical-key rule
    /// below makes that collision impossible anyway.
    invalid: Vec<(String, String)>,
}

impl FleetView {
    /// Fold merged device-set rows — `(logical key, merged value bytes)` —
    /// into the verified view.
    ///
    /// `root` is the account's current identity, and the only signer an
    /// enrollment cert verifies against. The device's attested predecessor
    /// set is deliberately no input: the device set does not cross a
    /// succession — every device enrolls afresh under the successor root —
    /// so a cert a retired root signed enrolls nothing here, whatever else
    /// the replica knows about that identity (`account-data-taxonomy.md`
    /// § The generation machinery → *The source of `prior`*, ruled
    /// 2026-10-01).
    ///
    /// A row whose key is not the canonical lowercase hex of the 32-byte id,
    /// or whose value does not decode, or whose enrollment cert does not
    /// verify, is flagged [`Self::invalid`] — loudly countable, never a
    /// member. Keys are canonical-or-refused so (id → row) stays injective;
    /// honest writers always write `hex32::encode(id)`.
    pub fn build<'a, I>(root: &ActorId, rows: I) -> Self
    where
        I: IntoIterator<Item = (&'a str, &'a [u8])>,
    {
        let mut view = FleetView::default();
        let mut removed_by: BTreeMap<[u8; 32], [u8; 32]> = BTreeMap::new();
        for (key, value) in rows {
            let id = match crate::hex32::decode(key) {
                Ok(id) if crate::hex32::encode(&id) == key => id,
                _ => {
                    view.invalid.push((
                        key.to_string(),
                        "row key is not canonical lowercase 32-byte hex".to_string(),
                    ));
                    continue;
                }
            };
            let record: DeviceSetRecord = match canonical_decode(value) {
                Ok(r) => r,
                Err(e) => {
                    view.invalid
                        .push((key.to_string(), format!("value does not decode: {e}")));
                    continue;
                }
            };
            // An enrollment must carry the device's self-signature, verifying
            // at this cell: without it the cert alone would let any
            // `BackupKey` holder re-file a member's enrollment beside a
            // foreign X-Wing key. (The unsigned shape pre-ruling binaries
            // wrote was admitted until the compat-remnant sweep retired it,
            // program 4.)
            if matches!(record, DeviceSetRecord::Enrolled { .. }) && !record.self_verifies_at(&id) {
                view.invalid.push((
                    key.to_string(),
                    "device self-signature is absent or does not verify at this cell".to_string(),
                ));
                continue;
            }
            match record {
                DeviceSetRecord::Enrolled {
                    xwing_pubkey,
                    authorization,
                    enrolled_at_ms,
                    ..
                } => match verify_enrollment_cert(&id, &authorization, enrolled_at_ms, root) {
                    Ok(()) => {
                        view.members.insert(
                            id,
                            FleetMember {
                                device_id: id,
                                xwing_pubkey,
                                enrolled_at_ms,
                            },
                        );
                    }
                    Err(reason) => view.invalid.push((key.to_string(), reason)),
                },
                DeviceSetRecord::Removed { removed_by: by, .. } => {
                    removed_by.insert(id, by);
                }
            }
        }
        // Attribution runs after the member set is final (two passes, no
        // fixpoint: attribution never feeds back into membership).
        for (id, by) in removed_by {
            let attribution = if by == root.0 {
                RemovalAttribution::Root
            } else if view.members.contains_key(&by) {
                RemovalAttribution::Member
            } else {
                RemovalAttribution::Unverified
            };
            view.removed.insert(id, attribution);
        }
        view
    }

    /// Is `id` a verified, non-removed fleet member?
    ///
    /// This is the admissibility primitive: a generation tip is admissible
    /// only if **every** id in its mint's member set answers true here
    /// (charter § The generation machinery — an excluded, unknown, or
    /// unverifiable member makes the tip inadmissible at this observer).
    pub fn is_verified_member(&self, id: &[u8; 32]) -> bool {
        self.members.contains_key(id) && !self.removed.contains_key(id)
    }

    /// Does a `Removed` row exclude `id`? (Unconditional — module ruling.)
    pub fn is_excluded(&self, id: &[u8; 32]) -> bool {
        self.removed.contains_key(id)
    }

    /// The wrap targets a mint seals to: every verified member, in id order.
    pub fn wrap_targets(&self) -> impl Iterator<Item = &FleetMember> {
        self.members
            .iter()
            .filter(|(id, _)| !self.removed.contains_key(*id))
            .map(|(_, m)| m)
    }

    /// Removed ids with the advisory attribution of each removal's claim.
    pub fn removed(&self) -> impl Iterator<Item = (&[u8; 32], RemovalAttribution)> {
        self.removed.iter().map(|(id, a)| (id, *a))
    }

    /// Rows that verified as nothing, with reasons — for logs and audit
    /// surfaces; an empty list is the healthy state.
    pub fn invalid(&self) -> &[(String, String)] {
        &self.invalid
    }
}

/// Verify an enrollment's embedded `DeviceAuthorization` against the account
/// identity line. Deterministic: every input is in the row or the identity
/// line — deliberately no `now` (a clock-dependent membership view would
/// diverge across observers; membership ends by removal, never by silent
/// cert expiry).
fn verify_enrollment_cert(
    id: &[u8; 32],
    authorization: &[u8],
    enrolled_at_ms: i64,
    root: &ActorId,
) -> Result<(), String> {
    // The whole verdict is a pure function of its inputs (no clock), so a
    // cert that verified for this exact (id, stamp, identity line, bytes)
    // verifies again — the verified-signature memo, at the cert level.
    let memo_key = {
        let mut h = blake3::Hasher::new_derive_key("fauna.generation.verified-enrollment.v2");
        h.update(id);
        h.update(&enrolled_at_ms.to_be_bytes());
        h.update(&root.0);
        h.update(authorization);
        *h.finalize().as_bytes()
    };
    if memo_remembers(&memo_key) {
        return Ok(());
    }
    let wire: EmbedAsBytes = canonical_decode(authorization)
        .map_err(|e| format!("authorization is not an EmbedAsBytes carriage: {e}"))?;
    let (cert_bytes, cert_env) = wire
        .into_signed()
        .map_err(|e| format!("authorization envelope malformed: {e}"))?;
    let cert: DeviceAuthorization = decode_signed_bytes(&cert_bytes)
        .map_err(|e| format!("authorization bytes are not a DeviceAuthorization: {e}"))?;
    verify_envelope(&cert, &cert_bytes, &cert_env)
        .map_err(|_| "authorization signature invalid".to_string())?;
    // Root only: the signer must be the account's current identity. A
    // retired predecessor root is refused (the device set does not cross a
    // succession), and a device key is never acceptable — "a device cannot
    // authorize a device" (`devices.md` § Device-signed authoring).
    if cert.actor_id != *root {
        return Err("authorization is not signed by the account root".to_string());
    }
    // The cert must cover exactly this row's id — a valid cert for one
    // device must not enroll another.
    if cert.device_key != *id {
        return Err("authorization covers a different device id".to_string());
    }
    // Expiry anchors to the enrollment's asserted instant (the authoring-twin
    // pattern: `verify_authoring_envelope` checks against `created_at`).
    // Cert timestamps are seconds (the admission-witness axis); the
    // enrollment stamp is millis.
    if let Some(expires_at) = cert.expires_at
        && expires_at.0.saturating_mul(1_000) < u64::try_from(enrolled_at_ms).unwrap_or(0)
    {
        return Err("authorization expired before the enrollment's asserted instant".to_string());
    }
    // `cert.capabilities` is deliberately not consulted: membership is
    // bundle-level — sealing under the fleet-only gen-0 branch is the
    // possession proof — mirroring the admission seam's carrier-level ruling.
    memo_remember(memo_key);
    Ok(())
}

// ── Tip resolution — the writer-door question (build step 6) ────────────────
//
// "Resolve the current admissible, escrow-acked generation tip; refuse a
// `GenerationTip` origination if none exists" (charter § The generation
// machinery — the gate's real shape). Like [`FleetView`], this is a **pure,
// deterministic view over merged rows** — no clock, no arrival order, no
// local preference — so two replicas with converged rows resolve the same
// tip, which is the whole convergence story for random keys
// (`owner-key-material.md` § Path A-sibling-2 → Rotation → *Convergence*).
//
// Resolver rulings (build step 6, 2026-08-13, recorded in the machinery §):
//
// * **A candidate** is a `Minted` (never `Shredded`) row whose logical key
//   equals the recomputed content-derived id of its own core (a row squatting
//   someone else's key, or carrying a forged id, verifies as nothing), whose
//   member set is wholly verified in the observer's [`FleetView`]
//   (admissibility — the ratified "excludes **or cannot verify**"), and whose
//   generation some **trusted holder's verifying receipt** acks
//   (escrow-before-first-seal; the resolver checks receipt integrity +
//   holder trust + the generation binding — the exact wrap-hash binding was
//   the depositor's check, and the ciphertext is not merged state).
// * **Supersession retires ancestors, candidates retire nothing else**: a
//   candidate is retired only when it is a (transitive) ancestor of another
//   candidate — the parent walk crosses inadmissible and shredded
//   intermediates, but only a *candidate* descendant retires. An
//   inadmissible or unacked descendant must not retire an admissible
//   ancestor: otherwise any fleet-key holder could wedge all fleet sealing
//   by publishing one junk-membered mint naming the tip as parent — a
//   client-causable unrecoverable state, so the invariant chooses.
// * **The winner among surviving candidates is the byte-order max of the
//   content-derived generation id** — order derived from content, never from
//   arrival or locality (the plane's standing no-local-preference
//   discipline, the same idiom as the lattice tiebreak).

/// The resolved sealing tip: the winning mint's id, core, and inline wraps
/// (the writer's own key source — its wrap is inline when it was in the
/// mint's member set; otherwise the top-up kind serves it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissibleTip {
    /// The content-derived generation id — what form-v2 entries carry.
    pub generation_id: [u8; 32],
    /// The winning mint's DAG core (its `key_commitment` is what every
    /// unwrap verifies against).
    pub core: MintCore,
    /// The winning mint's inline member wraps.
    pub wraps: Vec<MemberWrap>,
}

/// The outcome of one resolution pass over merged mint + receipt rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TipResolution {
    /// The current candidate tip **for this observer** — admissible, escrow-
    /// acked, and keyable here. `None` is the refusal case ("no fleet-only
    /// sealing until a mint escrows"), before any mint exists it is simply
    /// the R14 gate — and since the candidate-aware first-need ruling it is
    /// also the mint trigger: no candidate for this observer IS first-need.
    pub tip: Option<AdmissibleTip>,
    /// Rows that verified as nothing: `(row key, reason)` — countable, never
    /// candidates, exactly the [`FleetView::invalid`] shape. Structural
    /// forgeries land here too (empty member set, minter outside the member
    /// set, a minter signature that does not verify): an honest builder
    /// cannot produce them (ST-007).
    pub invalid: Vec<(String, String)>,
    /// Rows that pass every observer-independent check but carry no wrap this
    /// observer can open — a legitimate mint that raced this device's
    /// enrollment looks exactly like this until a top-up lands, so these are
    /// diagnostics ("a top-up fixes it"), never `invalid`.
    pub unkeyable: Vec<String>,
    /// The mint DAG's current leaves — every id-bound row no other id-bound
    /// row names as a parent (shredded rows included: their edges and ids
    /// stay). Ascending byte order. The first-need heal-mint's parent source
    /// (charter § The mint protocol — capped at [`MAX_MINT_PARENTS`] by the
    /// consumer, byte-order max preferred).
    pub leaf_ids: Vec<[u8; 32]>,
}

/// Escrow acks: the generation ids some **trusted** holder's verifying receipt
/// covers, from merged `fauna.state.escrow-receipt` rows. A receipt that does
/// not decode, fails integrity, or names an untrusted holder acks nothing and
/// is pushed onto `invalid` as `(row key, reason)`.
///
/// One predicate, three consumers: the tip resolver's escrow-before-first-seal
/// clause, the escrow-recovery pass's "is there a wrap worth asking the
/// holder for" gate, and the re-escrow pass's "is this generation acked for
/// the identity I am" gate — a receipt is holder-signed and binds the
/// generation id AND the target key, so a forged mint row can never carry
/// one and a predecessor's receipt never acks for a successor. `target_key`
/// is the observer's own [`escrow_target_identity_key`].
pub fn escrow_acked_generations<'a, R>(
    receipt_rows: R,
    trusted_holders: &[[u8; 32]],
    target_key: &str,
    invalid: &mut Vec<(String, String)>,
) -> std::collections::BTreeSet<[u8; 32]>
where
    R: IntoIterator<Item = (&'a str, &'a [u8])>,
{
    let mut acked = std::collections::BTreeSet::new();
    for (key, value) in receipt_rows {
        let receipt: EscrowReceiptRecord = match canonical_decode(value) {
            Ok(r) => r,
            Err(e) => {
                invalid.push((key.to_string(), format!("receipt does not decode: {e}")));
                continue;
            }
        };
        if let Err(e) = verify_escrow_receipt(&receipt) {
            invalid.push((key.to_string(), format!("receipt fails verification: {e}")));
            continue;
        }
        if !trusted_holders.contains(&receipt.holder_id) {
            invalid.push((key.to_string(), "receipt holder is not trusted".to_string()));
            continue;
        }
        // Another identity's receipt (a predecessor's, whose wrap the
        // succession ceremony burned) — valid, and not this observer's ack:
        // the succession rider re-escrows and re-receipts under the current
        // identity's key. Skipped, never `invalid`.
        if receipt.target_key != target_key {
            continue;
        }
        acked.insert(receipt.generation_id);
    }
    acked
}

/// The generations some holder — **any** holder, trusted or not — has
/// receipted for `target_key`: [`escrow_acked_generations`] without its trust
/// clause. Every receipt still decodes and verifies (a holder-signed statement
/// binding the generation and the target), so a row nobody signed counts
/// nothing.
///
/// The re-escrow pass's succession test (`account-data-taxonomy.md` § The
/// generation machinery → *A holder change re-receipts and never mints*, (2)):
/// a tip some holder receipted for this identity's own target was escrowed
/// under this identity, so only its holder changed — a second nest, a rotated
/// nest — and a deposit restores it; a tip no holder receipted for it was
/// escrowed only to a predecessor's target, which is a succession, and forward
/// sealing must mint past it. The question is about who sealed, never about
/// whom this device trusts now.
pub fn escrow_receipted_generations<'a, R>(
    receipt_rows: R,
    target_key: &str,
) -> std::collections::BTreeSet<[u8; 32]>
where
    R: IntoIterator<Item = (&'a str, &'a [u8])>,
{
    receipt_rows
        .into_iter()
        .filter_map(|(_, value)| canonical_decode::<EscrowReceiptRecord>(value).ok())
        .filter(|r| verify_escrow_receipt(r).is_ok() && r.target_key == target_key)
        .map(|r| r.generation_id)
        .collect()
}

/// Resolve this observer's current candidate generation tip from merged
/// `fauna.state.generation-mint` rows, merged `fauna.state.escrow-receipt`
/// rows, the observer's verified [`FleetView`], the observer's trusted holder
/// set, and the observer's own key reach.
///
/// Candidacy is the full ST-007 predicate (the charter's resolver rulings):
/// Key↔id binding, authenticated authorship (non-empty member set, minter in
/// it, verifying minter signature), wholly view-verified members, a trusted
/// holder's ack, and **observer-keyability** via `observer_keyable` — the
/// injected "can I actually obtain this generation's key" check (an inline
/// wrap or merged top-up that opens and matches the commitment; injected
/// because the KEM open lives above this crate and takes the observer's
/// device secret). A mint failing it neither wins nor retires — sealing-tip
/// choice is observer-relative by design; reads stay integrity-only and never
/// come here. Deterministic per observer: the closure must be a pure function
/// of its inputs and the observer's own key material.
///
/// `closed` is ruling (4): the generations merged
/// `fauna.state.generation-closed` rows name ([`closed_generations`]). A
/// closed generation is no candidate whatever else it passes — it neither
/// wins nor retires an ancestor, like every other non-candidate. Its edges
/// and its id stay in the DAG, so the first-need mint that follows names it
/// among its parents. A closed id no mint row carries changes nothing.
pub fn resolve_admissible_tip<'a, M, R, K>(
    view: &FleetView,
    mint_rows: M,
    receipt_rows: R,
    closed: &std::collections::BTreeSet<[u8; 32]>,
    trusted_holders: &[[u8; 32]],
    target_key: &str,
    observer_keyable: K,
) -> TipResolution
where
    M: IntoIterator<Item = (&'a str, &'a [u8])>,
    R: IntoIterator<Item = (&'a str, &'a [u8])>,
    K: Fn(&[u8; 32], &MintCore, &[MemberWrap]) -> bool,
{
    use std::collections::{BTreeMap, BTreeSet};

    let mut resolution = TipResolution::default();

    let acked = escrow_acked_generations(
        receipt_rows,
        trusted_holders,
        target_key,
        &mut resolution.invalid,
    );

    // Decode every mint row; keep the whole DAG (shredded included) for the
    // ancestor walk, and select candidates.
    let mut parents: BTreeMap<[u8; 32], Vec<[u8; 32]>> = BTreeMap::new();
    let mut candidates: BTreeMap<[u8; 32], AdmissibleTip> = BTreeMap::new();
    for (key, value) in mint_rows {
        let record: GenerationMintRecord = match canonical_decode(value) {
            Ok(r) => r,
            Err(e) => {
                resolution
                    .invalid
                    .push((key.to_string(), format!("mint does not decode: {e}")));
                continue;
            }
        };
        let (core, minter_sig, wraps) = match record {
            GenerationMintRecord::Minted {
                core,
                minter_sig,
                wraps,
            } => (core, minter_sig, wraps),
            GenerationMintRecord::Shredded { core, .. } => {
                // Never a candidate, but its edges — and its id, for the
                // leaf set — stay in the DAG.
                if let Ok(id) = generation_id(&core) {
                    parents.insert(id, core.parents.clone());
                }
                continue;
            }
        };
        // The logical key must be the recomputed content-derived id of the
        // core — the Key↔id binding read back at the resolver, so a row
        // squatting another key (or a forged id over someone's core) is
        // never a candidate and never an edge.
        let id = match generation_id(&core) {
            Ok(id) => id,
            Err(e) => {
                resolution
                    .invalid
                    .push((key.to_string(), format!("mint core does not encode: {e}")));
                continue;
            }
        };
        if crate::hex32::encode(&id) != key {
            resolution.invalid.push((
                key.to_string(),
                "mint row key is not its core's content-derived id".to_string(),
            ));
            continue;
        }
        parents.insert(id, core.parents.clone());
        // Authenticated authorship (ST-007) — the shared gate, counted as a
        // forgery and never a candidate on failure.
        if let Err(reason) = verify_mint_authorship(&id, &core, &minter_sig) {
            resolution.invalid.push((key.to_string(), reason));
            continue;
        }
        // Ruling (4): a remover closed it — nothing more is sealed under it.
        if closed.contains(&id) {
            continue;
        }
        // Admissibility: every member verified in the observer's view.
        if !core.member_ids.iter().all(|m| view.is_verified_member(m)) {
            continue;
        }
        // Escrow-before-first-seal: unacked mints are not candidates.
        if !acked.contains(&id) {
            continue;
        }
        // Observer-keyability: a mint this observer cannot key must neither
        // win nor retire — it is some OTHER device's candidate, not ours.
        if !observer_keyable(&id, &core, &wraps) {
            resolution.unkeyable.push(key.to_string());
            continue;
        }
        candidates.insert(
            id,
            AdmissibleTip {
                generation_id: id,
                core,
                wraps,
            },
        );
    }

    // The DAG's current leaves: every id-bound row nobody names as a parent —
    // the heal-mint's parent source, computed over ALL id-bound rows
    // (candidates, non-candidates, shredded) so a heal supersedes attacker
    // and orphan rows instead of forking a parentless second root.
    let referenced: BTreeSet<[u8; 32]> = parents.values().flatten().copied().collect();
    resolution.leaf_ids = parents
        .keys()
        .filter(|id| !referenced.contains(*id))
        .copied()
        .collect();

    // Supersession: retire every candidate that is a transitive ancestor of
    // another candidate. The walk crosses non-candidate intermediates.
    let mut retired: BTreeSet<[u8; 32]> = BTreeSet::new();
    for start in candidates.keys() {
        let mut stack: Vec<[u8; 32]> = parents.get(start).cloned().unwrap_or_default();
        let mut seen: BTreeSet<[u8; 32]> = BTreeSet::new();
        while let Some(ancestor) = stack.pop() {
            if !seen.insert(ancestor) {
                continue;
            }
            if candidates.contains_key(&ancestor) {
                retired.insert(ancestor);
            }
            if let Some(more) = parents.get(&ancestor) {
                stack.extend(more.iter().copied());
            }
        }
    }

    // The winner: byte-order max of the surviving candidates' ids (BTreeMap
    // iterates ascending, so the last surviving entry wins).
    resolution.tip = candidates
        .into_iter()
        .filter(|(id, _)| !retired.contains(id))
        .map(|(_, tip)| tip)
        .next_back();
    resolution
}

/// The shared two-phase lattice: the absorbing phase wins; within one phase
/// the byte-order max of the canonical encodings wins. A join over the
/// lexicographic product (phase, bytes) of two total orders — commutative,
/// associative, idempotent by construction, and independent of this build's
/// Rust (the tests below still assert all four, red-verified).
pub(crate) fn two_phase_winner<'a>(
    a: &'a [u8],
    a_absorbing: bool,
    b: &'a [u8],
    b_absorbing: bool,
) -> &'a [u8] {
    match (a_absorbing, b_absorbing) {
        (true, false) => a,
        (false, true) => b,
        // Same phase: byte-order max — deterministic everywhere, unreachable
        // with differing bytes in honest operation.
        _ => {
            if a >= b {
                a
            } else {
                b
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::encoding::canonical_encode;

    /// An unsigned `Enrolled` — never a member at the view, but still a
    /// lattice element the join must order (a vandal can file one).
    fn enrolled(seed: u8) -> Vec<u8> {
        canonical_encode(&DeviceSetRecord::Enrolled {
            xwing_pubkey: vec![seed; 8],
            authorization: vec![seed ^ 0xff; 8],
            enrolled_at_ms: 1_000 + seed as i64,
            device_sig: Vec::new(),
        })
        .expect("encode")
    }

    /// A cell no `enrolled(_)` row signs for — those rows rank as unverified
    /// there whatever the id, so the laws below hold at any cell.
    const UNSIGNED_CELL: [u8; 32] = [0x4Cu8; 32];

    fn removed(stamp: i64) -> Vec<u8> {
        canonical_encode(&DeviceSetRecord::Removed {
            removed_at_ms: stamp,
            removed_by: [7u8; 32],
        })
        .expect("encode")
    }

    fn core() -> MintCore {
        MintCore {
            parents: vec![[1u8; 32]],
            member_ids: vec![[2u8; 32], [3u8; 32]],
            minter: [2u8; 32],
            key_commitment: [9u8; 32],
            minted_at_ms: 5_000,
        }
    }

    fn minted(seed: u8) -> Vec<u8> {
        canonical_encode(&GenerationMintRecord::Minted {
            core: core(),
            minter_sig: vec![0xAA; 64],
            wraps: vec![MemberWrap {
                device_id: [2u8; 32],
                wrap: vec![seed; 16],
            }],
        })
        .expect("encode")
    }

    fn shredded() -> Vec<u8> {
        canonical_encode(&GenerationMintRecord::Shredded {
            core: core(),
            shredded_at_ms: 9_000,
            shredded_by: [2u8; 32],
            shredder_sig: vec![],
        })
        .expect("encode")
    }

    /// The watch item, asserted at the join itself: an `Enrolled` write
    /// arriving after (or concurrently with) a `Removed` — any order, any
    /// stamps — never resurrects the id. Red-verified by inverting the
    /// absorbing arm during development (the join then returns the enrolled
    /// side and every assertion here fails).
    #[test]
    fn removed_absorbs_enrolled_in_both_orders() {
        let e = enrolled(1);
        let r = removed(1); // stamp far below the enrollment's — irrelevant
        assert_eq!(join_device_set(&UNSIGNED_CELL, &e, &r).unwrap(), r);
        assert_eq!(join_device_set(&UNSIGNED_CELL, &r, &e).unwrap(), r);
        // A "newer" enrollment attempt still loses: stamps never order this.
        let newer = enrolled(200);
        assert_eq!(join_device_set(&UNSIGNED_CELL, &r, &newer).unwrap(), r);
        // And so does the device's OWN signed re-enrollment: the phase
        // outranks the self-signature — removal stays absorbing (
        // watch item, unchanged by the self-signature ruling).
        let device = device_signing_key(0x32);
        let signed =
            canonical_encode(&sign_device_enrollment(&device, vec![0x0C; 8], 9_500)).unwrap();
        let cell = device.verifying_key().to_bytes();
        assert_eq!(join_device_set(&cell, &signed, &r).unwrap(), r);
        assert_eq!(join_device_set(&cell, &r, &signed).unwrap(), r);
    }

    #[test]
    fn device_set_join_is_commutative_associative_idempotent() {
        let device = device_signing_key(0x33);
        let cell = device.verifying_key().to_bytes();
        let a = enrolled(1);
        let b = enrolled(2);
        let s = canonical_encode(&sign_device_enrollment(&device, vec![0x0C; 8], 3_000)).unwrap();
        let r = removed(50);
        for x in [&a, &b, &s, &r] {
            assert_eq!(join_device_set(&cell, x, x).unwrap(), **x, "idempotent");
        }
        for (x, y) in [(&a, &b), (&a, &s), (&b, &s), (&a, &r), (&b, &r), (&s, &r)] {
            assert_eq!(
                join_device_set(&cell, x, y).unwrap(),
                join_device_set(&cell, y, x).unwrap(),
                "commutative"
            );
        }
        // Associativity over every triple of the four, both groupings.
        let all = [&a, &b, &s, &r];
        for x in all {
            for y in all {
                for z in all {
                    let xy = join_device_set(&cell, x, y).unwrap();
                    let yz = join_device_set(&cell, y, z).unwrap();
                    assert_eq!(
                        join_device_set(&cell, &xy, z).unwrap(),
                        join_device_set(&cell, x, &yz).unwrap(),
                        "associative"
                    );
                }
            }
        }
    }

    /// **The pin at the join itself.** A `BackupKey` holder copies
    /// a victim's valid cert verbatim into a second `Enrolled` row for the
    /// victim's id, substitutes its OWN X-Wing pubkey, and picks bytes that
    /// win the byte-order max — every later wrap would then seal to the
    /// attacker under the victim's name. The device's self-signature is what
    /// stops it: a row verifying under the cell's device beats any bytes that
    /// do not — an unsigned forgery and one carrying a bad signature with a
    /// fabricated stamp — in both merge orders. Red-verified 2026-09-16: with
    /// the pre-signature byte-order join the unsigned forgery won this
    /// assertion outright.
    #[test]
    fn a_self_signed_enrollment_beats_a_forged_row_at_its_cell() {
        let device = device_signing_key(0x31);
        let cell = device.verifying_key().to_bytes();
        let cert = vec![0x0C; 8];
        let signed = sign_device_enrollment(&device, cert.clone(), 5_000);
        let honest = canonical_encode(&signed).unwrap();
        // The attacker's key under the victim's cert, unsigned.
        let unsigned_forgery = canonical_encode(&DeviceSetRecord::Enrolled {
            xwing_pubkey: vec![0xFF; fauna_pq_kem::XWING_ENCAPS_KEY_LEN],
            authorization: cert.clone(),
            enrolled_at_ms: 5_000,
            device_sig: Vec::new(),
        })
        .unwrap();
        // …and the same under junk signature bytes, crafted to win a pure
        // byte compare against the honest row.
        let bad_sig_forgery = canonical_encode(&DeviceSetRecord::Enrolled {
            xwing_pubkey: vec![0xFF; fauna_pq_kem::XWING_ENCAPS_KEY_LEN],
            authorization: cert,
            enrolled_at_ms: i64::MAX,
            device_sig: vec![0xFF; 64],
        })
        .unwrap();
        assert!(
            bad_sig_forgery > honest,
            "a junk-signed forgery can still win a pure byte compare"
        );
        for vandal in [unsigned_forgery.as_slice(), bad_sig_forgery.as_slice()] {
            assert_eq!(
                join_device_set(&cell, &honest, vandal).unwrap(),
                honest,
                "a verifying row is never displaced by unverifiable bytes"
            );
            assert_eq!(
                join_device_set(&cell, vandal, &honest).unwrap(),
                honest,
                "…in either merge order"
            );
        }
        // Two non-verifying rows resolve by byte-order max.
        assert_eq!(
            join_device_set(&cell, &enrolled(1), &enrolled(2)).unwrap(),
            enrolled(2)
        );
        assert_eq!(
            join_device_set(&cell, &enrolled(2), &enrolled(1)).unwrap(),
            enrolled(2)
        );
    }

    /// The self-signature covers every field AND the cell: a swapped pubkey,
    /// a swapped cert, a bumped stamp, or the same bytes re-filed under
    /// another device's cell un-verifies the record.
    #[test]
    fn self_verifies_at_refuses_every_tampered_field() {
        let device = device_signing_key(0x34);
        let cell = device.verifying_key().to_bytes();
        let record = sign_device_enrollment(&device, vec![0x0C; 8], 5_000);
        assert!(record.self_verifies_at(&cell));
        assert!(
            !record.self_verifies_at(&[0x35u8; 32]),
            "a foreign cell: the id is inside the preimage"
        );
        let DeviceSetRecord::Enrolled {
            xwing_pubkey,
            authorization,
            enrolled_at_ms,
            device_sig,
        } = record
        else {
            unreachable!()
        };
        let tampered = [
            DeviceSetRecord::Enrolled {
                xwing_pubkey: vec![0xFF; xwing_pubkey.len()],
                authorization: authorization.clone(),
                enrolled_at_ms,
                device_sig: device_sig.clone(),
            },
            DeviceSetRecord::Enrolled {
                xwing_pubkey: xwing_pubkey.clone(),
                authorization: vec![0x0D; 8],
                enrolled_at_ms,
                device_sig: device_sig.clone(),
            },
            DeviceSetRecord::Enrolled {
                xwing_pubkey: xwing_pubkey.clone(),
                authorization: authorization.clone(),
                enrolled_at_ms: enrolled_at_ms + 1,
                device_sig: device_sig.clone(),
            },
            // Bytes moved between the two variable-length fields under the
            // same signature: the length prefixes are what refuse this.
            DeviceSetRecord::Enrolled {
                xwing_pubkey: xwing_pubkey[..xwing_pubkey.len() - 1].to_vec(),
                authorization: [&xwing_pubkey[xwing_pubkey.len() - 1..], &authorization[..]]
                    .concat(),
                enrolled_at_ms,
                device_sig: device_sig.clone(),
            },
            DeviceSetRecord::Enrolled {
                xwing_pubkey,
                authorization,
                enrolled_at_ms,
                device_sig: Vec::new(),
            },
        ];
        for t in tampered {
            assert!(!t.self_verifies_at(&cell), "{t:?}");
        }
        let removed_record: DeviceSetRecord = canonical_decode(&removed(1)).unwrap();
        assert!(
            !removed_record.self_verifies_at(&cell),
            "a Removed never self-verifies"
        );
    }

    #[test]
    fn shredded_absorbs_minted_and_drops_wraps() {
        let m = minted(4);
        let s = shredded();
        assert_eq!(join_generation_mint(&m, &s).unwrap(), s);
        assert_eq!(join_generation_mint(&s, &m).unwrap(), s);
        // The surviving bytes decode to a record with no wrap material.
        let survived: GenerationMintRecord =
            canonical_decode(&join_generation_mint(&m, &s).unwrap()).unwrap();
        assert!(matches!(survived, GenerationMintRecord::Shredded { .. }));
    }

    #[test]
    fn same_phase_ties_break_by_byte_order_deterministically() {
        let a = minted(1);
        let b = minted(2);
        let winner = join_generation_mint(&a, &b).unwrap();
        assert_eq!(winner, join_generation_mint(&b, &a).unwrap());
        assert!(winner == a || winner == b);
    }

    #[test]
    fn joins_refuse_undecodable_values() {
        assert!(join_device_set(&UNSIGNED_CELL, b"junk", &removed(1)).is_err());
        assert!(join_generation_mint(&minted(1), b"junk").is_err());
    }

    #[test]
    fn records_round_trip_canonically() {
        let signed = canonical_encode(&sign_device_enrollment(
            &device_signing_key(0x36),
            vec![1; 4],
            7,
        ))
        .unwrap();
        for bytes in [enrolled(3), removed(3), signed] {
            let rec: DeviceSetRecord = canonical_decode(&bytes).unwrap();
            assert_eq!(canonical_encode(&rec).unwrap(), bytes);
        }
        // An absent signature decodes as empty (the field's additive
        // `serde(default)`), and an empty one is skipped on encode — so the
        // view, not the decoder, is what refuses an unsigned row.
        let unsigned: DeviceSetRecord = canonical_decode(&enrolled(3)).unwrap();
        assert!(matches!(
            unsigned,
            DeviceSetRecord::Enrolled { ref device_sig, .. } if device_sig.is_empty()
        ));
        assert!(
            !enrolled(3).windows(10).any(|w| w == b"device_sig"),
            "an unsigned record carries no signature key"
        );
        for bytes in [minted(3), shredded()] {
            let rec: GenerationMintRecord = canonical_decode(&bytes).unwrap();
            assert_eq!(canonical_encode(&rec).unwrap(), bytes);
        }
        let target = EscrowTargetRecord {
            xwing_escrow_pubkey: vec![4u8; 32],
        };
        let receipt = EscrowReceiptRecord {
            generation_id: [1u8; 32],
            holder_id: [5u8; 32],
            wrap_hash: [6u8; 32],
            target_key: "identity/test".into(),
            stamped_at_ms: 1,
            holder_sig: vec![7u8; 64],
        };
        let t: EscrowTargetRecord = canonical_decode(&canonical_encode(&target).unwrap()).unwrap();
        assert_eq!(t, target);
        let r: EscrowReceiptRecord =
            canonical_decode(&canonical_encode(&receipt).unwrap()).unwrap();
        assert_eq!(r, receipt);
    }

    // ── The per-healer wrap cells ─────────────────────

    /// A signed, verifying v2 wrap authored by the seed-derived healer.
    fn signed_v2_wrap(healer_seed: u8, at_ms: i64) -> ([u8; 32], Vec<u8>) {
        let healer_key = device_signing_key(healer_seed);
        let healer = healer_key.verifying_key().to_bytes();
        let wrap = vec![0x42u8; 24];
        let record = GenerationWrapRecordV2::Wrap {
            generation_id: [1u8; 32],
            target_device: [2u8; 32],
            healer,
            at_ms,
            wrap: wrap.clone(),
            healer_sig: sign_topup_as_healer(&healer_key, &[1u8; 32], &[2u8; 32], at_ms, &wrap),
        };
        (healer, canonical_encode(&record).unwrap())
    }

    /// **The pin at the join itself**: a record that verifies under
    /// the cell's healer beats a forged record with a fabricated `i64::MAX`
    /// stamp and a bad signature, in both merge orders; and bytes that do not
    /// even decode are never ranked at all — the join fails, in both orders
    /// (the decode-or-fail contract: the walk skips the row unaccounted).
    #[test]
    fn a_verifying_wrap_beats_forged_bytes_in_its_cell() {
        let (healer, honest) = signed_v2_wrap(0x21, 7_000);
        let forged = canonical_encode(&GenerationWrapRecordV2::Wrap {
            generation_id: [1u8; 32],
            target_device: [2u8; 32],
            healer,
            at_ms: i64::MAX,
            wrap: vec![0xde; 24],
            healer_sig: vec![0xde; 64],
        })
        .unwrap();
        assert_eq!(
            join_generation_wrap_per_healer(&[1u8; 32], &[2u8; 32], &healer, &honest, &forged)
                .unwrap(),
            honest,
            "a verifying row is never displaced by an unverifiable record"
        );
        assert_eq!(
            join_generation_wrap_per_healer(&[1u8; 32], &[2u8; 32], &healer, &forged, &honest)
                .unwrap(),
            honest,
            "…in either merge order"
        );
        for undecodable in [b"junk".to_vec(), future_variant()] {
            assert!(
                join_generation_wrap_per_healer(
                    &[1u8; 32],
                    &[2u8; 32],
                    &healer,
                    &honest,
                    &undecodable
                )
                .is_err(),
                "an undecodable incoming side fails the join, never ranks lowest"
            );
            assert!(
                join_generation_wrap_per_healer(
                    &[1u8; 32],
                    &[2u8; 32],
                    &healer,
                    &undecodable,
                    &honest
                )
                .is_err(),
                "…and so does an undecodable current side"
            );
        }
    }

    /// A record a later build could write: an enum variant this build's
    /// closed-by-design record types do not know, canonically encoded — the
    /// shape the decode-or-fail joins must refuse rather than rank.
    pub(crate) fn future_variant() -> Vec<u8> {
        #[derive(serde::Serialize)]
        enum Later {
            NotYetInvented { at_ms: i64 },
        }
        canonical_encode(&Later::NotYetInvented { at_ms: i64::MAX }).unwrap()
    }

    #[test]
    fn within_verifying_rows_the_newer_signed_stamp_wins() {
        let (healer, older) = signed_v2_wrap(0x21, 7_000);
        let (_, newer) = signed_v2_wrap(0x21, 8_000);
        assert_eq!(
            join_generation_wrap_per_healer(&[1u8; 32], &[2u8; 32], &healer, &older, &newer)
                .unwrap(),
            newer
        );
        assert_eq!(
            join_generation_wrap_per_healer(&[1u8; 32], &[2u8; 32], &healer, &newer, &older)
                .unwrap(),
            newer
        );
        // Idempotent.
        assert_eq!(
            join_generation_wrap_per_healer(&[1u8; 32], &[2u8; 32], &healer, &newer, &newer)
                .unwrap(),
            newer
        );
    }

    /// The signature covers every field: any echo mismatch with the cell, a
    /// swapped wrap ciphertext, or a bumped stamp un-verifies the record.
    #[test]
    fn verifies_at_refuses_every_tampered_field() {
        let (healer, bytes) = signed_v2_wrap(0x21, 7_000);
        let decode = |b: &[u8]| canonical_decode::<GenerationWrapRecordV2>(b).unwrap();
        assert!(decode(&bytes).verifies_at(&[1u8; 32], &[2u8; 32], &healer));
        // A foreign cell — same bytes filed under another healer's cell.
        assert!(!decode(&bytes).verifies_at(&[1u8; 32], &[2u8; 32], &[9u8; 32]));
        assert!(!decode(&bytes).verifies_at(&[8u8; 32], &[2u8; 32], &healer));
        // A swapped ciphertext under the carried-through signature.
        let GenerationWrapRecordV2::Wrap {
            generation_id,
            target_device,
            healer: h,
            at_ms,
            healer_sig,
            ..
        } = decode(&bytes);
        let swapped = GenerationWrapRecordV2::Wrap {
            generation_id,
            target_device,
            healer: h,
            at_ms,
            wrap: vec![0x66; 24],
            healer_sig: healer_sig.clone(),
        };
        assert!(!swapped.verifies_at(&[1u8; 32], &[2u8; 32], &healer));
        // A bumped stamp under the carried-through signature.
        let bumped = GenerationWrapRecordV2::Wrap {
            generation_id,
            target_device,
            healer: h,
            at_ms: at_ms + 1,
            wrap: vec![0x42u8; 24],
            healer_sig,
        };
        assert!(!bumped.verifies_at(&[1u8; 32], &[2u8; 32], &healer));
    }

    #[test]
    fn wrap_cell_keys_parse_canonically_or_not_at_all() {
        let g = [1u8; 32];
        let t = [2u8; 32];
        let h = [3u8; 32];
        assert_eq!(
            parse_wrap_cell_key(&wrap_cell_key_per_healer(&g, &t, &h)),
            Some(WrapCellKey {
                generation_id: g,
                target_device: t,
                healer: h
            })
        );
        // Non-canonical spellings and wrong arities are not cells.
        assert_eq!(
            parse_wrap_cell_key(&wrap_cell_key_per_healer(&[0xABu8; 32], &t, &h).to_uppercase()),
            None
        );
        assert_eq!(parse_wrap_cell_key("0101"), None);
        // The retired two-segment `"<generation>/<target>"` cell (the v1
        // unattributed wrap shape, removed by the compat-remnant sweep,
        // program 4) is not a cell of this kind.
        assert_eq!(
            parse_wrap_cell_key(&format!(
                "{}/{}",
                crate::hex32::encode(&g),
                crate::hex32::encode(&t)
            )),
            None
        );
        assert_eq!(
            parse_wrap_cell_key(&format!("{}/x/y/z", crate::hex32::encode(&g))),
            None
        );
    }

    // ── The target-authored unkeyable signal ───────────────────────

    /// A signed, verifying record authored by the seed-derived target.
    fn signed_unkeyable(
        target_seed: u8,
        variant: u8,
        at_ms: i64,
        tried: Vec<[u8; 32]>,
    ) -> ([u8; 32], Vec<u8>) {
        let target_key = device_signing_key(target_seed);
        let target = target_key.verifying_key().to_bytes();
        let sig = sign_unkeyable_as_target(&target_key, &[1u8; 32], variant, at_ms, &tried);
        let record = if variant == UNKEYABLE_VARIANT_ASSERTED {
            GenerationUnkeyableRecord::Asserted {
                generation_id: [1u8; 32],
                target_device: target,
                asserted_at_ms: at_ms,
                tried,
                target_sig: sig,
            }
        } else {
            GenerationUnkeyableRecord::Satisfied {
                generation_id: [1u8; 32],
                target_device: target,
                asserted_at_ms: at_ms,
                target_sig: sig,
            }
        };
        (target, canonical_encode(&record).unwrap())
    }

    /// The forgery pin at the join itself: a record verifying under the
    /// cell's target beats a forged assertion with a fabricated `i64::MAX`
    /// stamp, in both merge orders; bytes that do not decode fail the join.
    #[test]
    fn a_verifying_signal_beats_forged_bytes_in_its_cell() {
        let (target, honest) =
            signed_unkeyable(0x31, UNKEYABLE_VARIANT_ASSERTED, 7_000, vec![[7u8; 32]]);
        let forged = canonical_encode(&GenerationUnkeyableRecord::Asserted {
            generation_id: [1u8; 32],
            target_device: target,
            asserted_at_ms: i64::MAX,
            tried: vec![],
            target_sig: vec![0xde; 64],
        })
        .unwrap();
        assert_eq!(
            join_generation_unkeyable(&[1u8; 32], &target, &honest, &forged).unwrap(),
            honest
        );
        assert_eq!(
            join_generation_unkeyable(&[1u8; 32], &target, &forged, &honest).unwrap(),
            honest
        );
        for undecodable in [b"junk".to_vec(), future_variant()] {
            assert!(join_generation_unkeyable(&[1u8; 32], &target, &honest, &undecodable).is_err());
            assert!(join_generation_unkeyable(&[1u8; 32], &target, &undecodable, &honest).is_err());
        }
    }

    /// Retraction and re-assertion are stamp-ranked within the one honest
    /// author's cell: a later `Satisfied` retires the assertion, a later
    /// `Asserted` re-opens it, and a replayed older record displaces neither.
    #[test]
    fn a_later_satisfied_retires_the_assertion_and_a_later_assertion_reopens() {
        let (target, asserted) =
            signed_unkeyable(0x31, UNKEYABLE_VARIANT_ASSERTED, 7_000, vec![[7u8; 32]]);
        let (_, satisfied) = signed_unkeyable(0x31, UNKEYABLE_VARIANT_SATISFIED, 8_000, vec![]);
        let (_, reasserted) =
            signed_unkeyable(0x31, UNKEYABLE_VARIANT_ASSERTED, 9_000, vec![[8u8; 32]]);
        assert_eq!(
            join_generation_unkeyable(&[1u8; 32], &target, &asserted, &satisfied).unwrap(),
            satisfied
        );
        assert_eq!(
            join_generation_unkeyable(&[1u8; 32], &target, &satisfied, &asserted).unwrap(),
            satisfied,
            "a replayed older assertion never displaces the retraction"
        );
        assert_eq!(
            join_generation_unkeyable(&[1u8; 32], &target, &satisfied, &reasserted).unwrap(),
            reasserted
        );
        // Idempotent.
        assert_eq!(
            join_generation_unkeyable(&[1u8; 32], &target, &reasserted, &reasserted).unwrap(),
            reasserted
        );
    }

    /// The signature covers every field of both variants: a foreign cell, a
    /// swapped `tried` list, a bumped stamp, and a variant re-interpretation
    /// (a `Satisfied` signature re-filed as an `Asserted`) all un-verify.
    #[test]
    fn unkeyable_verifies_at_refuses_every_tampered_field() {
        let (target, bytes) =
            signed_unkeyable(0x31, UNKEYABLE_VARIANT_ASSERTED, 7_000, vec![[7u8; 32]]);
        let decode = |b: &[u8]| canonical_decode::<GenerationUnkeyableRecord>(b).unwrap();
        assert!(decode(&bytes).verifies_at(&[1u8; 32], &target));
        // Foreign cells.
        assert!(!decode(&bytes).verifies_at(&[1u8; 32], &[9u8; 32]));
        assert!(!decode(&bytes).verifies_at(&[8u8; 32], &target));
        let GenerationUnkeyableRecord::Asserted {
            generation_id,
            target_device,
            asserted_at_ms,
            target_sig,
            ..
        } = decode(&bytes)
        else {
            unreachable!()
        };
        // A swapped evidence list under the carried-through signature.
        let swapped = GenerationUnkeyableRecord::Asserted {
            generation_id,
            target_device,
            asserted_at_ms,
            tried: vec![[9u8; 32]],
            target_sig: target_sig.clone(),
        };
        assert!(!swapped.verifies_at(&[1u8; 32], &target));
        // A bumped stamp under the carried-through signature.
        let bumped = GenerationUnkeyableRecord::Asserted {
            generation_id,
            target_device,
            asserted_at_ms: asserted_at_ms + 1,
            tried: vec![[7u8; 32]],
            target_sig: target_sig.clone(),
        };
        assert!(!bumped.verifies_at(&[1u8; 32], &target));
        // A variant re-interpretation: the Satisfied signature carried into an
        // Asserted with an empty list (same non-variant fields).
        let (_, sat_bytes) = signed_unkeyable(0x31, UNKEYABLE_VARIANT_SATISFIED, 7_000, vec![]);
        let GenerationUnkeyableRecord::Satisfied {
            target_sig: sat_sig,
            ..
        } = decode(&sat_bytes)
        else {
            unreachable!()
        };
        let refiled = GenerationUnkeyableRecord::Asserted {
            generation_id,
            target_device,
            asserted_at_ms,
            tried: vec![],
            target_sig: sat_sig,
        };
        assert!(!refiled.verifies_at(&[1u8; 32], &target));
    }

    /// The reach kind's laws in one place: the join is a total, verifying-
    /// preferred stamp-ranked max (idempotent, commutative, junk never
    /// displaces the device's own row, a later re-publication supersedes an
    /// earlier one); the signature covers every field; the list is
    /// canonical; the cell key is canonical-or-nothing.
    #[test]
    fn device_reach_joins_by_verification_then_stamp_and_signs_every_field() {
        let key = device_signing_key(0x61);
        let device = key.verifying_key().to_bytes();
        let earlier =
            canonical_encode(&sign_device_reach(&key, 1_000, [[3u8; 32], [1u8; 32]])).unwrap();
        let later = canonical_encode(&sign_device_reach(
            &key,
            2_000,
            [[1u8; 32], [3u8; 32], [1u8; 32]],
        ))
        .unwrap();
        // Canonical: sorted + deduped whatever the caller handed in.
        let rec: DeviceReachRecord = canonical_decode(&later).unwrap();
        assert_eq!(rec.holds, vec![[1u8; 32], [3u8; 32]]);
        assert!(rec.verifies_at(&device));
        assert!(rec.holds(&[3u8; 32]) && !rec.holds(&[2u8; 32]));
        // Later stamp wins, both directions; idempotent.
        assert_eq!(join_device_reach(&device, &earlier, &later), later);
        assert_eq!(join_device_reach(&device, &later, &earlier), later);
        assert_eq!(join_device_reach(&device, &later, &later), later);
        // Junk — and a row signed by ANOTHER device — never displaces the
        // device's own verifying row, whatever the stamp.
        let junk = b"junk".to_vec();
        assert_eq!(join_device_reach(&device, &earlier, &junk), earlier);
        assert_eq!(join_device_reach(&device, &junk, &earlier), earlier);
        let other = device_signing_key(0x62);
        let foreign = canonical_encode(&DeviceReachRecord {
            device_id: device,
            ..sign_device_reach(&other, 9_000, [[5u8; 32]])
        })
        .unwrap();
        assert!(
            !canonical_decode::<DeviceReachRecord>(&foreign)
                .unwrap()
                .verifies_at(&device)
        );
        assert_eq!(join_device_reach(&device, &earlier, &foreign), earlier);
        // Every field is under the signature: a foreign cell, a swapped list,
        // a bumped stamp all un-verify.
        assert!(!rec.verifies_at(&[9u8; 32]));
        let swapped = DeviceReachRecord {
            holds: vec![[1u8; 32]],
            ..rec.clone()
        };
        assert!(!swapped.verifies_at(&device));
        let bumped = DeviceReachRecord {
            at_ms: rec.at_ms + 1,
            ..rec.clone()
        };
        assert!(!bumped.verifies_at(&device));
        // The cell key.
        assert_eq!(parse_reach_cell_key(&reach_cell_key(&device)), Some(device));
        assert_eq!(
            parse_reach_cell_key(&reach_cell_key(&device).to_uppercase()),
            None
        );
        assert_eq!(parse_reach_cell_key("0101"), None);
    }

    #[test]
    fn unkeyable_cell_keys_parse_canonically_or_not_at_all() {
        let g = [1u8; 32];
        let t = [2u8; 32];
        assert_eq!(
            parse_unkeyable_cell_key(&unkeyable_cell_key(&g, &t)),
            Some((g, t))
        );
        assert_eq!(
            parse_unkeyable_cell_key(&unkeyable_cell_key(&[0xABu8; 32], &t).to_uppercase()),
            None
        );
        assert_eq!(parse_unkeyable_cell_key("0101"), None);
        assert_eq!(
            parse_unkeyable_cell_key(&format!(
                "{}/{}/{}",
                crate::hex32::encode(&g),
                crate::hex32::encode(&t),
                crate::hex32::encode(&t)
            )),
            None,
            "three segments are the wrap kind's shape, never this kind's"
        );
    }

    // ── The verified fleet view ─────────────────────────────────────────────

    use crate::data::{Capability, Timestamp};
    use crate::encoding::sign_envelope;
    use crate::identity::ActorKeypair;

    fn root_keypair() -> ActorKeypair {
        ActorKeypair::from_secret([11u8; 32])
    }

    fn prior_keypair() -> ActorKeypair {
        ActorKeypair::from_secret([12u8; 32])
    }

    fn device_signing_key(seed: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
    }

    /// A device id IS the device's Ed25519 public key — real keys since the
    /// ST-007 minter signature verifies at the resolver.
    fn device_id(seed: u8) -> [u8; 32] {
        device_signing_key(seed).verifying_key().to_bytes()
    }

    /// The signing key behind a [`device_id`] — test-only reverse scan over
    /// the one-byte seed space.
    fn signing_key_of(id: &[u8; 32]) -> ed25519_dalek::SigningKey {
        (0..=255u8)
            .map(device_signing_key)
            .find(|k| k.verifying_key().to_bytes() == *id)
            .expect("a fixture id is always seed-derived")
    }

    /// A `DeviceAuthorization` for `id`, signed by `signer`, carried as the
    /// canonical `EmbedAsBytes` bytes the enrollment value embeds.
    fn cert_bytes(signer: &ActorKeypair, id: [u8; 32], expires_at: Option<u64>) -> Vec<u8> {
        let cert = DeviceAuthorization {
            actor_id: signer.actor_id(),
            device_key: id,
            // Deliberately narrow: membership never consults capabilities
            // (bundle-level possession is the proof — module ruling).
            capabilities: vec![Capability::RenewBearer],
            created_at: Timestamp(1_000),
            expires_at: expires_at.map(Timestamp),
        };
        let (bytes, env) = sign_envelope(signer, &cert).expect("sign cert");
        canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).expect("encode carriage")
    }

    /// An enrollment row for `cell`, self-signed by the device behind it —
    /// the one shape production writes; the cert (`authorization`) is what
    /// the view tests below vary.
    fn enrolled_row(cell: [u8; 32], authorization: Vec<u8>, enrolled_at_ms: i64) -> Vec<u8> {
        canonical_encode(&sign_device_enrollment(
            &signing_key_of(&cell),
            authorization,
            enrolled_at_ms,
        ))
        .expect("encode enrolled")
    }

    /// The KEM public key [`enrolled_row`] files for `cell`.
    fn kem_of(cell: &[u8; 32]) -> Vec<u8> {
        derive_device_xwing_keypair(&signing_key_of(cell).to_bytes())
            .public
            .to_bytes()
            .to_vec()
    }

    /// A self-signed enrollment carrying a good cert: what production writes
    /// since the ruling.
    #[test]
    fn a_self_signed_enrollment_is_a_member_with_its_own_kem_key() {
        let id = device_id(1);
        let device = signing_key_of(&id);
        let record = sign_device_enrollment(&device, cert_bytes(&root_keypair(), id, None), 5_000);
        let rows = vec![(
            crate::hex32::encode(&id),
            canonical_encode(&record).unwrap(),
        )];
        let view = view_of(&rows);
        assert!(view.is_verified_member(&id));
        assert!(view.invalid().is_empty());
        let targets: Vec<_> = view.wrap_targets().collect();
        assert_eq!(
            targets[0].xwing_pubkey,
            derive_device_xwing_keypair(&device.to_bytes())
                .public
                .to_bytes()
                .to_vec(),
            "the wrap target is the KEM the device's own secret derives"
        );
    }

    /// The unsigned `Enrolled` shape pre-ruling binaries wrote was admitted as
    /// a cert-verified member until the compat-remnant sweep retired it
    /// (program 4). Now a valid root-signed cert without the device's own
    /// signature is flagged, never a member, never a wrap target — so a
    /// `BackupKey` holder re-filing a member's cert beside its own KEM key
    /// gains nothing at the view either.
    #[test]
    fn an_unsigned_enrollment_is_flagged_not_a_member() {
        let id = device_id(1);
        let unsigned = canonical_encode(&DeviceSetRecord::Enrolled {
            xwing_pubkey: vec![0xE0; 8],
            authorization: cert_bytes(&root_keypair(), id, None),
            enrolled_at_ms: 5_000,
            device_sig: Vec::new(),
        })
        .unwrap();
        let view = view_of(&[(crate::hex32::encode(&id), unsigned)]);
        assert!(!view.is_verified_member(&id));
        assert!(view.wrap_targets().next().is_none());
        assert_eq!(view.invalid().len(), 1);
        assert!(view.invalid()[0].1.contains("self-signature"));
    }

    /// A row CLAIMING the device's binding and failing it is a forgery —
    /// flagged, never a member.
    #[test]
    fn a_bad_self_signature_is_flagged_not_a_member() {
        let id = device_id(1);
        let device = signing_key_of(&id);
        let good = sign_device_enrollment(&device, cert_bytes(&root_keypair(), id, None), 5_000);
        let DeviceSetRecord::Enrolled {
            authorization,
            enrolled_at_ms,
            device_sig,
            ..
        } = good
        else {
            unreachable!()
        };
        let forged = DeviceSetRecord::Enrolled {
            xwing_pubkey: vec![0xFF; 8], // the attacker's key under a carried signature
            authorization,
            enrolled_at_ms,
            device_sig,
        };
        let rows = vec![(
            crate::hex32::encode(&id),
            canonical_encode(&forged).unwrap(),
        )];
        let view = view_of(&rows);
        assert!(!view.is_verified_member(&id));
        assert!(view.wrap_targets().next().is_none());
        assert_eq!(view.invalid().len(), 1);
        assert!(view.invalid()[0].1.contains("self-signature"));
    }

    fn removed_row(removed_by: [u8; 32]) -> Vec<u8> {
        canonical_encode(&DeviceSetRecord::Removed {
            removed_at_ms: 9_000,
            removed_by,
        })
        .expect("encode removed")
    }

    fn view_of(rows: &[(String, Vec<u8>)]) -> FleetView {
        FleetView::build(
            &root_keypair().actor_id(),
            rows.iter().map(|(k, v)| (k.as_str(), v.as_slice())),
        )
    }

    #[test]
    fn a_root_signed_enrollment_is_a_member_and_wrap_target() {
        let id = device_id(1);
        let rows = vec![(
            crate::hex32::encode(&id),
            enrolled_row(id, cert_bytes(&root_keypair(), id, None), 5_000),
        )];
        let view = view_of(&rows);
        assert!(view.is_verified_member(&id));
        assert!(!view.is_excluded(&id));
        let targets: Vec<_> = view.wrap_targets().collect();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].device_id, id);
        assert_eq!(targets[0].xwing_pubkey, kem_of(&id));
        assert!(view.invalid().is_empty());
    }

    #[test]
    fn a_predecessor_signed_enrollment_is_flagged_and_no_member() {
        // The fleet view verifies an enrollment cert against the account root
        // alone (`account-data-taxonomy.md` § The generation machinery →
        // *The source of `prior`*, ruled 2026-10-01): the device set does not
        // cross a succession, so a cert a retired root signed — whatever else
        // this replica attests about that identity — enrolls nothing. Were it
        // admitted, opening a predecessor's device-set row would seat every
        // device the retired root ever enrolled, a seed thief's included.
        let id = device_id(2);
        let rows = vec![(
            crate::hex32::encode(&id),
            enrolled_row(id, cert_bytes(&prior_keypair(), id, None), 5_000),
        )];
        let view = view_of(&rows);
        assert!(!view.is_verified_member(&id));
        assert!(view.wrap_targets().next().is_none());
        assert_eq!(view.invalid().len(), 1);
        assert!(view.invalid()[0].1.contains("account root"));
    }

    #[test]
    fn an_enrollment_signed_by_a_non_root_key_is_refused() {
        // A device key signing its own authorization — "a device cannot
        // authorize a device" — must never mint membership, even though the
        // envelope itself verifies under its signer.
        let id = device_id(3);
        let rogue = ActorKeypair::from_secret([99u8; 32]);
        let rows = vec![(
            crate::hex32::encode(&id),
            enrolled_row(id, cert_bytes(&rogue, id, None), 5_000),
        )];
        let view = view_of(&rows);
        assert!(!view.is_verified_member(&id));
        assert!(view.wrap_targets().next().is_none());
        assert_eq!(view.invalid().len(), 1);
        assert!(
            view.invalid()[0]
                .1
                .contains("not signed by the account root")
        );
    }

    #[test]
    fn an_enrollment_whose_cert_covers_another_id_is_refused() {
        let id = device_id(4);
        let other = device_id(5);
        let rows = vec![(
            crate::hex32::encode(&id),
            enrolled_row(id, cert_bytes(&root_keypair(), other, None), 5_000),
        )];
        let view = view_of(&rows);
        assert!(!view.is_verified_member(&id));
        assert!(
            !view.is_verified_member(&other),
            "the cert's own id gains nothing either"
        );
        assert!(view.invalid()[0].1.contains("different device id"));
    }

    #[test]
    fn cert_expiry_anchors_to_the_enrollment_instant_never_a_clock() {
        let id = device_id(6);
        // Cert expires at t=10s; enrollment asserted at t=5s — valid, and
        // stays valid forever (membership ends by removal, not decay).
        let rows = vec![(
            crate::hex32::encode(&id),
            enrolled_row(id, cert_bytes(&root_keypair(), id, Some(10)), 5_000),
        )];
        assert!(view_of(&rows).is_verified_member(&id));
        // Enrollment asserted after expiry — refused.
        let rows = vec![(
            crate::hex32::encode(&id),
            enrolled_row(id, cert_bytes(&root_keypair(), id, Some(10)), 11_000),
        )];
        let view = view_of(&rows);
        assert!(!view.is_verified_member(&id));
        assert!(view.invalid()[0].1.contains("expired"));
    }

    #[test]
    fn a_removed_row_excludes_unconditionally_whatever_the_attribution() {
        let remover = device_id(7);
        let target = device_id(8);
        let rows = vec![
            (
                crate::hex32::encode(&remover),
                enrolled_row(remover, cert_bytes(&root_keypair(), remover, None), 5_000),
            ),
            // Removal claimed by a verified member.
            (crate::hex32::encode(&target), removed_row(remover)),
            // Removal claimed by the root itself.
            (
                crate::hex32::encode(&device_id(9)),
                removed_row(root_keypair().actor_id().0),
            ),
            // Removal claimed by nobody verifiable — STILL excludes.
            (
                crate::hex32::encode(&device_id(10)),
                removed_row([0xCC; 32]),
            ),
        ];
        let view = view_of(&rows);
        for id in [target, device_id(9), device_id(10)] {
            assert!(view.is_excluded(&id));
            assert!(!view.is_verified_member(&id));
        }
        let attributions: BTreeMap<[u8; 32], RemovalAttribution> =
            view.removed().map(|(id, a)| (*id, a)).collect();
        assert_eq!(attributions[&target], RemovalAttribution::Member);
        assert_eq!(attributions[&device_id(9)], RemovalAttribution::Root);
        assert_eq!(attributions[&device_id(10)], RemovalAttribution::Unverified);
    }

    #[test]
    fn a_removed_member_is_neither_member_nor_wrap_target() {
        let id = device_id(12);
        let rows = vec![(crate::hex32::encode(&id), removed_row([0xCC; 32]))];
        let view = view_of(&rows);
        assert!(view.is_excluded(&id));
        assert!(!view.is_verified_member(&id));
        assert!(view.wrap_targets().next().is_none());
    }

    /// The belt-and-braces conjunct in [`FleetView::is_verified_member`] and
    /// the [`FleetView::wrap_targets`] filter, exercised directly: `build`'s
    /// canonical-key rule makes an id land in `members` OR `removed`, never
    /// both — but the answering methods must not silently depend on that
    /// constructor invariant (a future second constructor, or a refactor of
    /// the fold, would then turn a removed id back into a wrap target with
    /// no test going red). This test constructs the both-present state the
    /// constructor refuses to build, which is only possible here because the
    /// tests live beside the private fields.
    #[test]
    fn membership_answers_hold_even_if_an_id_were_in_both_maps() {
        let id = device_id(17);
        let mut view = FleetView::default();
        view.members.insert(
            id,
            FleetMember {
                device_id: id,
                xwing_pubkey: vec![0xE0; 8],
                enrolled_at_ms: 5_000,
            },
        );
        view.removed.insert(id, RemovalAttribution::Unverified);
        assert!(!view.is_verified_member(&id), "removed must beat member");
        assert!(view.is_excluded(&id));
        assert!(
            view.wrap_targets().next().is_none(),
            "a removed id must never be wrapped to"
        );
    }

    #[test]
    fn non_canonical_keys_and_junk_values_are_flagged_never_members() {
        let id = device_id(13);
        let cert = cert_bytes(&root_keypair(), id, None);
        let uppercase = crate::hex32::encode(&id).to_uppercase();
        let rows = vec![
            (uppercase, enrolled_row(id, cert.clone(), 5_000)),
            ("zz".to_string(), enrolled_row(id, cert, 5_000)),
            (crate::hex32::encode(&device_id(14)), b"junk".to_vec()),
        ];
        let view = view_of(&rows);
        assert!(!view.is_verified_member(&id));
        assert_eq!(view.invalid().len(), 3);
        assert!(view.wrap_targets().next().is_none());
    }

    // ── The escrow receipt ──────────────────────────────────────────────────

    #[test]
    fn a_signed_receipt_verifies_and_any_field_tamper_fails() {
        let holder = ed25519_dalek::SigningKey::from_bytes(&[0x44; 32]);
        let receipt = sign_escrow_receipt(&holder, [1u8; 32], [2u8; 32], "identity/test", 7_000);
        assert_eq!(receipt.holder_id, holder.verifying_key().to_bytes());
        verify_escrow_receipt(&receipt).expect("a fresh receipt verifies");
        // Every signed field is load-bearing: tampering any one fails.
        for tamper in [
            |r: &mut EscrowReceiptRecord| r.generation_id[0] ^= 1,
            |r: &mut EscrowReceiptRecord| r.wrap_hash[0] ^= 1,
            |r: &mut EscrowReceiptRecord| r.target_key.push('x'),
            |r: &mut EscrowReceiptRecord| r.stamped_at_ms += 1,
            |r: &mut EscrowReceiptRecord| r.holder_sig[0] ^= 1,
        ] {
            let mut t = receipt.clone();
            tamper(&mut t);
            assert!(verify_escrow_receipt(&t).is_err());
        }
        // A different holder's id on the same signature fails too — the
        // signature binds the holder identity, not just the deposit.
        let mut t = receipt.clone();
        t.holder_id = ed25519_dalek::SigningKey::from_bytes(&[0x45; 32])
            .verifying_key()
            .to_bytes();
        assert!(verify_escrow_receipt(&t).is_err());
    }

    #[test]
    fn receipt_verification_is_holder_generic_not_deployment_coupled() {
        // Two unrelated holders each sign; both verify purely off their own
        // holder_id — nothing in the verify path privileges any fixed key,
        // which is the step-4 holder-generic constraint made executable.
        for seed in [[0x50u8; 32], [0x51u8; 32]] {
            let holder = ed25519_dalek::SigningKey::from_bytes(&seed);
            let receipt = sign_escrow_receipt(&holder, [9u8; 32], [8u8; 32], "identity/test", 1);
            verify_escrow_receipt(&receipt).expect("any holder's receipt verifies");
        }
    }

    // ── Content-derived ids and the machinery keypairs (build step 5) ───────

    #[test]
    fn generation_id_is_content_derived_and_covers_the_key_commitment() {
        let a = core();
        let id_a = generation_id(&a).unwrap();
        // Deterministic — the same core always names the same generation.
        assert_eq!(id_a, generation_id(&a).unwrap());
        // The commitment is covered: two same-shaped mints (same parents,
        // members, minter, stamp) whose random keys differ fork into distinct
        // ids — the Key↔id-binding property that stops one id from carrying
        // two different keys.
        let mut b = core();
        b.key_commitment = [0x0Au8; 32];
        assert_ne!(id_a, generation_id(&b).unwrap());
        // Every other asserted field is covered too.
        let mut c = core();
        c.minter = [0x0Bu8; 32];
        assert_ne!(id_a, generation_id(&c).unwrap());
        let mut d = core();
        d.parents = vec![[0x0Cu8; 32]];
        assert_ne!(id_a, generation_id(&d).unwrap());
    }

    #[test]
    fn the_machinery_keypairs_pin_known_vectors() {
        // KATs over BLAKE3 of the 1216-byte X-Wing public keys (pinning the
        // full key is unwieldy; the hash pins it just as hard). Both contexts
        // are frozen: the device public half rests in every enrollment row,
        // the escrow public half in the published escrow target, and escrow
        // wraps rest with every holder forever. Filled from a first run; a
        // failure means a context string or the keygen path moved — do not
        // update the vector.
        let seed = [0x01u8; 32];
        let device = derive_device_xwing_keypair(&seed);
        let escrow = derive_escrow_xwing_keypair(&seed);
        // hex: 29872b01b937beec80e9b9f344a5b01048638ba90136357e87265a66d4788625
        let expected_device: [u8; 32] = [
            0x29, 0x87, 0x2b, 0x01, 0xb9, 0x37, 0xbe, 0xec, 0x80, 0xe9, 0xb9, 0xf3, 0x44, 0xa5,
            0xb0, 0x10, 0x48, 0x63, 0x8b, 0xa9, 0x01, 0x36, 0x35, 0x7e, 0x87, 0x26, 0x5a, 0x66,
            0xd4, 0x78, 0x86, 0x25,
        ];
        // hex: 587455dd8bff0589723a965675c0e8af267fb90281b98dc4ef2199d5ceaea458
        let expected_escrow: [u8; 32] = [
            0x58, 0x74, 0x55, 0xdd, 0x8b, 0xff, 0x05, 0x89, 0x72, 0x3a, 0x96, 0x56, 0x75, 0xc0,
            0xe8, 0xaf, 0x26, 0x7f, 0xb9, 0x02, 0x81, 0xb9, 0x8d, 0xc4, 0xef, 0x21, 0x99, 0xd5,
            0xce, 0xae, 0xa4, 0x58,
        ];
        assert_eq!(
            <[u8; 32]>::from(blake3::hash(&device.public.to_bytes())),
            expected_device
        );
        assert_eq!(
            <[u8; 32]>::from(blake3::hash(&escrow.public.to_bytes())),
            expected_escrow
        );
    }

    #[test]
    fn the_two_machinery_keypairs_are_deterministic_and_domain_separated() {
        let seed = [0x09u8; 32];
        // Deterministic: a device re-derives its secret half on demand; a
        // seed ceremony re-derives the escrow secret — nothing stored.
        assert_eq!(
            derive_device_xwing_keypair(&seed).public.to_bytes(),
            derive_device_xwing_keypair(&seed).public.to_bytes()
        );
        assert_eq!(
            derive_escrow_xwing_keypair(&seed).public.to_bytes(),
            derive_escrow_xwing_keypair(&seed).public.to_bytes()
        );
        // Domain-separated: the same 32 bytes as device secret vs. identity
        // seed yield unrelated keypairs (distinct frozen contexts).
        assert_ne!(
            derive_device_xwing_keypair(&seed).public.to_bytes(),
            derive_escrow_xwing_keypair(&seed).public.to_bytes()
        );
        // And distinct inputs yield distinct keys.
        assert_ne!(
            derive_device_xwing_keypair(&[0x09u8; 32]).public.to_bytes(),
            derive_device_xwing_keypair(&[0x0Au8; 32]).public.to_bytes()
        );
    }

    #[test]
    fn escrow_target_record_is_deterministic_and_carries_the_escrow_public_key() {
        let seed = [0x0Du8; 32];
        let record = escrow_target_record(&seed);
        // Byte-identical across concurrent seed-holding writers — what lets
        // the kind register Immutable with no race to arbitrate.
        assert_eq!(record, escrow_target_record(&seed));
        assert_eq!(
            record.xwing_escrow_pubkey,
            derive_escrow_xwing_keypair(&seed)
                .public
                .to_bytes()
                .to_vec()
        );
        assert_eq!(
            record.xwing_escrow_pubkey.len(),
            fauna_pq_kem::XWING_ENCAPS_KEY_LEN
        );
    }

    // ── Tip resolution (build step 6, the pure view) ────────────────────────

    fn holder() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[0x66u8; 32])
    }

    fn holder_id() -> [u8; 32] {
        holder().verifying_key().to_bytes()
    }

    /// A fleet view with the given ids as verified members.
    fn fleet_of(ids: &[[u8; 32]]) -> FleetView {
        let rows: Vec<(String, Vec<u8>)> = ids
            .iter()
            .map(|id| {
                (
                    crate::hex32::encode(id),
                    enrolled_row(*id, cert_bytes(&root_keypair(), *id, None), 5_000),
                )
            })
            .collect();
        view_of(&rows)
    }

    /// One honestly-authored `Minted` row under its content-derived key —
    /// minter = the first member, signature genuine. `salt` differentiates
    /// same-shaped mints (it stands in for the key commitment).
    fn mint_row(
        parents: Vec<[u8; 32]>,
        members: &[[u8; 32]],
        salt: u8,
    ) -> (String, Vec<u8>, [u8; 32]) {
        let core = MintCore {
            parents,
            member_ids: members.to_vec(),
            minter: members[0],
            key_commitment: [salt; 32],
            minted_at_ms: 1_000,
        };
        let id = generation_id(&core).unwrap();
        let minter_sig = sign_mint_as_minter(&signing_key_of(&members[0]), &id);
        let value = canonical_encode(&GenerationMintRecord::Minted {
            core,
            minter_sig,
            wraps: vec![],
        })
        .unwrap();
        (crate::hex32::encode(&id), value, id)
    }

    fn shredded_row(
        parents: Vec<[u8; 32]>,
        members: &[[u8; 32]],
        salt: u8,
    ) -> (String, Vec<u8>, [u8; 32]) {
        let core = MintCore {
            parents,
            member_ids: members.to_vec(),
            minter: members[0],
            key_commitment: [salt; 32],
            minted_at_ms: 1_000,
        };
        let id = generation_id(&core).unwrap();
        let value = canonical_encode(&GenerationMintRecord::Shredded {
            core,
            shredded_at_ms: 2_000,
            shredded_by: members[0],
            shredder_sig: vec![],
        })
        .unwrap();
        (crate::hex32::encode(&id), value, id)
    }

    // ── The authored shred ─────────────────────────────────────
    //
    // Red-verified during development by inverting each check in
    // `shred_is_authored` (the membership test, the signature test, the empty
    // guard): the matching assertion below fails on each inversion.

    fn shred_core(members: &[[u8; 32]], salt: u8) -> MintCore {
        MintCore {
            parents: vec![],
            member_ids: members.to_vec(),
            minter: members[0],
            key_commitment: [salt; 32],
            minted_at_ms: 1_000,
        }
    }

    #[test]
    fn a_shred_signed_by_a_verified_member_is_authored_and_names_its_id() {
        let members = [device_id(1), device_id(2)];
        let core = shred_core(&members, 0x51);
        let id = generation_id(&core).unwrap();
        // The hander, not the minter: any verified member may author.
        let row = sign_shred(&signing_key_of(&members[1]), core, 2_000).unwrap();
        assert_eq!(row.shred_is_authored(&fleet_of(&members)), Ok(id));
    }

    #[test]
    fn a_sig_less_shred_is_unauthored_and_encodes_as_it_always_did() {
        let members = [device_id(1), device_id(2)];
        let row = GenerationMintRecord::Shredded {
            core: shred_core(&members, 0x52),
            shredded_at_ms: 2_000,
            shredded_by: members[0],
            shredder_sig: vec![],
        };
        let err = row.shred_is_authored(&fleet_of(&members)).unwrap_err();
        assert!(err.contains("unauthored"), "{err}");
        // Additive: the empty field is skipped, so the pre-ruling bytes are
        // unchanged and a pre-ruling reader decodes a post-ruling sig-less row.
        let bytes = canonical_encode(&row).unwrap();
        assert!(
            !bytes
                .windows(b"shredder_sig".len())
                .any(|w| w == b"shredder_sig"),
            "an empty signature must not be encoded"
        );
        let back: GenerationMintRecord = canonical_decode(&bytes).unwrap();
        assert_eq!(back, row);
    }

    #[test]
    fn a_shred_signed_by_a_non_member_or_a_removed_member_is_unauthored() {
        let members = [device_id(1), device_id(2)];
        let core = shred_core(&members, 0x53);
        // A device secret the fleet never enrolled (a `BackupKey` holder that
        // is no member) signs a self-consistent row — refused at the view.
        let outsider = sign_shred(&device_signing_key(9), core.clone(), 2_000).unwrap();
        let err = outsider.shred_is_authored(&fleet_of(&members)).unwrap_err();
        assert!(err.contains("not a verified"), "{err}");
        // A member removed since: its row is a verified member's shape, but
        // the reader's current view excludes it — the fail-safe direction.
        let by_member = sign_shred(&signing_key_of(&members[1]), core, 2_000).unwrap();
        assert!(by_member.shred_is_authored(&fleet_of(&members)).is_ok());
        let mut rows: Vec<(String, Vec<u8>)> = members
            .iter()
            .map(|id| {
                (
                    crate::hex32::encode(id),
                    enrolled_row(*id, cert_bytes(&root_keypair(), *id, None), 5_000),
                )
            })
            .collect();
        rows[1].1 = removed_row(members[0]);
        let err = by_member.shred_is_authored(&view_of(&rows)).unwrap_err();
        assert!(err.contains("not a verified"), "{err}");
    }

    #[test]
    fn a_shred_signature_binds_the_generation_the_shredder_and_the_stamp() {
        let members = [device_id(1), device_id(2)];
        let view = fleet_of(&members);
        let signed = sign_shred(
            &signing_key_of(&members[1]),
            shred_core(&members, 0x54),
            2_000,
        )
        .unwrap();
        let GenerationMintRecord::Shredded {
            shredded_at_ms,
            shredded_by,
            shredder_sig,
            ..
        } = signed.clone()
        else {
            unreachable!()
        };
        // Transplanted onto another generation's core: the recomputed id differs.
        let moved = GenerationMintRecord::Shredded {
            core: shred_core(&members, 0x55),
            shredded_at_ms,
            shredded_by,
            shredder_sig: shredder_sig.clone(),
        };
        assert!(
            moved
                .shred_is_authored(&view)
                .unwrap_err()
                .contains("signature")
        );
        // The stamp moved under the signature.
        let restamped = GenerationMintRecord::Shredded {
            core: shred_core(&members, 0x54),
            shredded_at_ms: shredded_at_ms + 1,
            shredded_by,
            shredder_sig: shredder_sig.clone(),
        };
        assert!(
            restamped
                .shred_is_authored(&view)
                .unwrap_err()
                .contains("signature")
        );
        // Another member's id claimed over this member's signature.
        let reattributed = GenerationMintRecord::Shredded {
            core: shred_core(&members, 0x54),
            shredded_at_ms,
            shredded_by: members[0],
            shredder_sig,
        };
        assert!(
            reattributed
                .shred_is_authored(&view)
                .unwrap_err()
                .contains("signature")
        );
        // A Minted row shreds nothing, whoever asks.
        let (_, minted, _) = mint_row(vec![], &members, 0x56);
        let minted: GenerationMintRecord = canonical_decode(&minted).unwrap();
        assert!(
            minted
                .shred_is_authored(&view)
                .unwrap_err()
                .contains("Minted")
        );
    }

    #[test]
    fn an_authored_shred_absorbs_in_the_join_exactly_as_an_unauthored_one() {
        // The join is the one consumer that stays blind to authorship (the
        // availability-only act): a signed and an unsigned shred both absorb
        // a Minted row, in both orders.
        let members = [device_id(1), device_id(2)];
        let (_, minted, _) = mint_row(vec![], &members, 0x57);
        let (_, unsigned, _) = shredded_row(vec![], &members, 0x57);
        let signed = canonical_encode(
            &sign_shred(
                &signing_key_of(&members[0]),
                shred_core(&members, 0x57),
                2_000,
            )
            .unwrap(),
        )
        .unwrap();
        for shred in [&unsigned, &signed] {
            assert_eq!(&join_generation_mint(&minted, shred).unwrap(), shred);
            assert_eq!(&join_generation_mint(shred, &minted).unwrap(), shred);
        }
    }

    /// The observer's own escrow-target key — the one the resolver filters
    /// receipts by.
    fn tk() -> String {
        escrow_target_identity_key(&root_keypair().actor_id())
    }

    fn ack_row(generation: [u8; 32]) -> (String, Vec<u8>) {
        let receipt = sign_escrow_receipt(&holder(), generation, [0xAB; 32], &tk(), 7_000);
        (
            format!(
                "{}/{}",
                crate::hex32::encode(&generation),
                crate::hex32::encode(&holder_id())
            ),
            canonical_encode(&receipt).unwrap(),
        )
    }

    /// Resolution with a fully-permissive keyability closure — for tests of
    /// the observer-independent clauses (structure, authorship, view, acks,
    /// retirement); keyability itself is exercised via [`resolve_keyable`].
    fn resolve(
        view: &FleetView,
        mints: &[(String, Vec<u8>, [u8; 32])],
        acks: &[(String, Vec<u8>)],
    ) -> TipResolution {
        resolve_keyable(view, mints, acks, |_, _, _| true)
    }

    fn resolve_keyable(
        view: &FleetView,
        mints: &[(String, Vec<u8>, [u8; 32])],
        acks: &[(String, Vec<u8>)],
        keyable: impl Fn(&[u8; 32], &MintCore, &[MemberWrap]) -> bool,
    ) -> TipResolution {
        resolve_closed(view, mints, acks, &[], keyable)
    }

    /// Resolution with `closed` as the generations merged closed rows name.
    fn resolve_closed(
        view: &FleetView,
        mints: &[(String, Vec<u8>, [u8; 32])],
        acks: &[(String, Vec<u8>)],
        closed: &[[u8; 32]],
        keyable: impl Fn(&[u8; 32], &MintCore, &[MemberWrap]) -> bool,
    ) -> TipResolution {
        resolve_admissible_tip(
            view,
            mints.iter().map(|(k, v, _)| (k.as_str(), v.as_slice())),
            acks.iter().map(|(k, v)| (k.as_str(), v.as_slice())),
            &closed.iter().copied().collect(),
            &[holder_id()],
            &tk(),
            keyable,
        )
    }

    fn mint_refs(mints: &[(String, Vec<u8>, [u8; 32])]) -> impl Iterator<Item = (&str, &[u8])> {
        mints.iter().map(|(k, v, _)| (k.as_str(), v.as_slice()))
    }

    /// A view in which `members` are enrolled and `removed` carries a
    /// `Removed` row over its enrollment.
    fn fleet_with_removed(members: &[[u8; 32]], removed: [u8; 32]) -> FleetView {
        let mut rows: Vec<(String, Vec<u8>)> = members
            .iter()
            .map(|id| {
                (
                    crate::hex32::encode(id),
                    enrolled_row(*id, cert_bytes(&root_keypair(), *id, None), 5_000),
                )
            })
            .collect();
        rows.push((crate::hex32::encode(&removed), removed_row([0xCC; 32])));
        view_of(&rows)
    }

    /// **Ruling (4).** A closed generation is no candidate: it does not win,
    /// and — a non-candidate like any other — it retires no ancestor. A
    /// closed id no mint row carries changes nothing.
    #[test]
    fn a_closed_generation_is_no_candidate_and_retires_no_ancestor() {
        let a = device_id(1);
        let view = fleet_of(&[a]);
        let old = mint_row(vec![], &[a], 1);
        let tip = mint_row(vec![old.2], &[a], 2);
        let mints = [old.clone(), tip.clone()];
        let acks = [ack_row(old.2), ack_row(tip.2)];
        let open = |_: &[u8; 32], _: &MintCore, _: &[MemberWrap]| true;

        let r = resolve_closed(&view, &mints, &acks, &[], open);
        assert_eq!(r.tip.unwrap().generation_id, tip.2);

        // The tip closed: its ancestor is a candidate again, never retired
        // by a descendant that is no candidate itself.
        let r = resolve_closed(&view, &mints, &acks, &[tip.2], open);
        assert_eq!(r.tip.unwrap().generation_id, old.2);
        // The closed row is still the DAG's leaf: the mint that follows
        // names it as a parent and so supersedes both.
        assert_eq!(r.leaf_ids, vec![tip.2]);

        // Both closed: nothing resolves — first-need.
        let r = resolve_closed(&view, &mints, &acks, &[old.2, tip.2], open);
        assert!(r.tip.is_none());
        assert!(r.invalid.is_empty() && r.unkeyable.is_empty());

        // A closed row for a generation this replica holds no mint of.
        let r = resolve_closed(&view, &mints, &acks, &[[0xEE; 32]], open);
        assert_eq!(r.tip.unwrap().generation_id, tip.2);
    }

    /// **Trigger (b), the first hole.** The tip names `a` alone; `late`
    /// enrolled afterwards and keyed it by top-up. Removing `late` leaves the
    /// tip admissible under the member rule, and the closure set is what
    /// names it.
    #[test]
    fn the_closure_set_names_a_tip_the_removed_device_is_no_member_of() {
        let (a, late) = (device_id(1), device_id(2));
        let view = fleet_of(&[a, late]);
        let tip = mint_row(vec![], &[a], 1);
        let mints = [tip.clone()];
        assert_eq!(closure_set(&view, mint_refs(&mints), &late), vec![tip.2]);

        // Closed, the tip no longer resolves once the removal has merged.
        let after = fleet_with_removed(&[a], late);
        let acks = [ack_row(tip.2)];
        let open = |_: &[u8; 32], _: &MintCore, _: &[MemberWrap]| true;
        assert!(
            resolve_closed(&after, &mints, &acks, &[], open)
                .tip
                .is_some(),
            "the member rule alone leaves it a candidate"
        );
        assert!(
            resolve_closed(&after, &mints, &acks, &[tip.2], open)
                .tip
                .is_none()
        );
    }

    /// **Trigger (b), the second hole.** The tip names the removed device, so
    /// the member rule unseats it — and ruling (2) then lets its live
    /// ancestor, which does not name the removed device, resolve in its
    /// place. The closure set names that ancestor (never the tip, which the
    /// member rule already covers), and with it closed nothing resolves.
    #[test]
    fn the_closure_set_names_a_live_ancestor_the_removed_tip_would_fall_back_to() {
        let (a, b) = (device_id(1), device_id(2));
        let both = fleet_of(&[a, b]);
        let ancestor = mint_row(vec![], &[a], 1);
        let tip = mint_row(vec![ancestor.2], &[a, b], 2);
        let mints = [ancestor.clone(), tip.clone()];
        let acks = [ack_row(ancestor.2), ack_row(tip.2)];
        let open = |_: &[u8; 32], _: &MintCore, _: &[MemberWrap]| true;
        assert_eq!(
            resolve_closed(&both, &mints, &acks, &[], open)
                .tip
                .unwrap()
                .generation_id,
            tip.2
        );

        let closing = closure_set(&both, mint_refs(&mints), &b);
        assert_eq!(closing, vec![ancestor.2]);

        let after = fleet_with_removed(&[a], b);
        assert_eq!(
            resolve_closed(&after, &mints, &acks, &[], open)
                .tip
                .unwrap()
                .generation_id,
            ancestor.2,
            "without the closure the remover seals under the ancestor"
        );
        assert!(
            resolve_closed(&after, &mints, &acks, &closing, open)
                .tip
                .is_none()
        );
    }

    /// The closure set draws nothing from a row that could never be a
    /// candidate: an invented mint (a member the view does not verify), a
    /// forged minter signature, a row squatting a foreign key, a shredded
    /// generation.
    #[test]
    fn the_closure_set_leaves_out_every_row_that_is_no_candidate_anywhere() {
        let (a, late, stranger) = (device_id(1), device_id(2), device_id(9));
        let view = fleet_of(&[a, late]);
        let honest = mint_row(vec![], &[a], 1);
        let invented = mint_row(vec![], &[a, stranger], 2);
        let forged = raw_mint_row(MintCore {
            parents: vec![],
            member_ids: vec![a],
            minter: a,
            key_commitment: [3; 32],
            minted_at_ms: 1_000,
        });
        let mut squatter = mint_row(vec![], &[a], 4);
        squatter.0 = crate::hex32::encode(&[0x5A; 32]);
        let shredded = shredded_row(vec![], &[a], 5);
        let mints = [honest.clone(), invented, forged, squatter, shredded];
        assert_eq!(closure_set(&view, mint_refs(&mints), &late), vec![honest.2]);
    }

    /// The closed kind's cell grammar and join: one canonical hex32 segment,
    /// byte-order max, and the reader's set built from keys alone.
    #[test]
    fn the_closed_cell_is_one_canonical_generation_id_and_joins_by_byte_order() {
        let g = [0xA7u8; 32];
        let key = closed_cell_key(&g);
        assert_eq!(parse_closed_cell_key(&key), Some(g));
        assert_eq!(parse_closed_cell_key(&key.to_uppercase()), None);
        assert_eq!(parse_closed_cell_key("not-a-generation"), None);

        let row = |by: u8| {
            canonical_encode(&GenerationClosedRecord {
                closed_by: [by; 32],
                answers: [0x0B; 32],
                closed_at_ms: 9_000,
            })
            .unwrap()
        };
        let (x, y) = (row(1), row(2));
        assert_eq!(join_generation_closed(&x, &y), y.as_slice());
        assert_eq!(join_generation_closed(&y, &x), y.as_slice());
        assert_eq!(join_generation_closed(&x, &x), x.as_slice());

        let closed = closed_generations([(key.as_str(), b"".as_slice()), ("junk", x.as_slice())]);
        assert_eq!(closed.into_iter().collect::<Vec<_>>(), vec![g]);
    }

    #[test]
    fn a_single_acked_admissible_mint_resolves_as_the_tip() {
        let (a, b) = (device_id(1), device_id(2));
        let view = fleet_of(&[a, b]);
        let mint = mint_row(vec![], &[a, b], 1);
        let tip = resolve(&view, std::slice::from_ref(&mint), &[ack_row(mint.2)])
            .tip
            .expect("resolves");
        assert_eq!(tip.generation_id, mint.2);
        assert_eq!(tip.core.member_ids, vec![a, b]);
        // No mints at all is simply the R14 gate: no tip.
        assert!(resolve(&view, &[], &[]).tip.is_none());
    }

    /// Escrow-before-first-seal, read back at the resolver: a minted-but-
    /// unacked generation resolves to nothing (the prior tip, when one
    /// exists), exactly the ratified wait-window behavior.
    #[test]
    fn an_unacked_mint_never_resolves_and_writers_stay_on_the_prior_tip() {
        let a = device_id(1);
        let view = fleet_of(&[a]);
        let old = mint_row(vec![], &[a], 1);
        let new = mint_row(vec![old.2], &[a], 2);
        // New mint deposited nothing yet: the acked old tip stands.
        let r = resolve(&view, &[old.clone(), new.clone()], &[ack_row(old.2)]);
        assert_eq!(r.tip.unwrap().generation_id, old.2);
        // Once the new mint's receipt lands, it retires its parent.
        let r = resolve(
            &view,
            &[old.clone(), new.clone()],
            &[ack_row(old.2), ack_row(new.2)],
        );
        assert_eq!(r.tip.unwrap().generation_id, new.2);
        // An unacked FIRST mint resolves nothing at all.
        assert!(resolve(&view, &[new], &[]).tip.is_none());
    }

    /// The ratified admissibility clause: a tip whose member set includes a
    /// device the observer excludes OR cannot verify is inadmissible.
    #[test]
    fn a_mint_with_an_excluded_or_unverifiable_member_is_inadmissible() {
        let (a, stranger) = (device_id(1), device_id(9));
        let view = fleet_of(&[a]); // stranger never enrolled
        let m = mint_row(vec![], &[a, stranger], 1);
        assert!(resolve(&view, &[m], &[]).tip.is_none());

        // Removal flips an admissible tip inadmissible, monotonically: the
        // same rows, re-read after b's removal merges, stop resolving.
        let b = device_id(2);
        let both = fleet_of(&[a, b]);
        let m = mint_row(vec![], &[a, b], 2);
        let acks = vec![ack_row(m.2)];
        assert!(
            resolve(&both, std::slice::from_ref(&m), &acks)
                .tip
                .is_some()
        );
        let mut rows: Vec<(String, Vec<u8>)> = vec![
            (
                crate::hex32::encode(&a),
                enrolled_row(a, cert_bytes(&root_keypair(), a, None), 5_000),
            ),
            (crate::hex32::encode(&b), removed_row([0xCC; 32])),
        ];
        rows.rotate_left(1); // order irrelevant, as everywhere
        let after_removal = view_of(&rows);
        assert!(resolve(&after_removal, &[m], &acks).tip.is_none());
    }

    /// Supersession is transitive and crosses non-candidate intermediates —
    /// but ONLY a candidate descendant retires: an inadmissible or unacked
    /// child must not wedge fleet sealing by retiring the admissible tip.
    #[test]
    fn only_a_candidate_descendant_retires_an_ancestor() {
        let (a, stranger) = (device_id(1), device_id(9));
        let view = fleet_of(&[a]);
        let x = mint_row(vec![], &[a], 1);
        // A hostile/junk mint naming x as parent, with an unverifiable member.
        let hostile = mint_row(vec![x.2], &[a, stranger], 2);
        let r = resolve(
            &view,
            &[x.clone(), hostile.clone()],
            &[ack_row(x.2), ack_row(hostile.2)],
        );
        assert_eq!(
            r.tip.unwrap().generation_id,
            x.2,
            "an inadmissible descendant retires nothing"
        );

        // Transitive: w (candidate) ← z (shredded, non-candidate) ← x
        // (candidate): w retires x through z's edges.
        let z = shredded_row(vec![x.2], &[a], 3);
        let w = mint_row(vec![z.2], &[a], 4);
        let r = resolve(
            &view,
            &[x.clone(), z, w.clone()],
            &[ack_row(x.2), ack_row(w.2)],
        );
        assert_eq!(r.tip.unwrap().generation_id, w.2);
    }

    /// A `Minted` row under its own content-derived key, from a caller-crafted
    /// core with a garbage signature — the attacker's construction path
    /// (`build_mint`'s guards are builder-side; the resolver is the trust
    /// boundary — ST-007).
    fn raw_mint_row(core: MintCore) -> (String, Vec<u8>, [u8; 32]) {
        raw_mint_row_with_sig(core, vec![0xBB; 64])
    }

    fn raw_mint_row_with_sig(core: MintCore, minter_sig: Vec<u8>) -> (String, Vec<u8>, [u8; 32]) {
        let id = generation_id(&core).unwrap();
        let value = canonical_encode(&GenerationMintRecord::Minted {
            core,
            minter_sig,
            wraps: vec![],
        })
        .unwrap();
        (crate::hex32::encode(&id), value, id)
    }

    /// **ST-007 pin (probe red 1).** An empty member set must never satisfy
    /// admissibility: `all()` over nothing is vacuously true, and an acked
    /// empty-member mint resolved as the tip for EVERY observer — a mint
    /// nobody can key, wedging all fleet-only sealing. The empty set is a
    /// forgery by construction (`build_mint` refuses it), so the resolver
    /// must count it invalid, not adopt it.
    #[test]
    fn an_empty_member_set_mint_never_resolves() {
        let wedge = raw_mint_row(MintCore {
            parents: vec![],
            member_ids: vec![],
            minter: [0xEE; 32],
            key_commitment: [0x01; 32],
            minted_at_ms: 1_000,
        });
        // Even the zero-knowledge view (no verified members at all) must not
        // admit it — vacuity is exactly the hole.
        let r = resolve(
            &FleetView::default(),
            std::slice::from_ref(&wedge),
            &[ack_row(wedge.2)],
        );
        assert!(
            r.tip.is_none(),
            "an empty-member mint resolved as the tip: {r:?}"
        );

        // Probe red 2's companion shape: a second empty-member mint naming
        // the first as parent must not "retire" its way to winning either.
        let child = raw_mint_row(MintCore {
            parents: vec![wedge.2],
            member_ids: vec![],
            minter: [0xEE; 32],
            key_commitment: [0x02; 32],
            minted_at_ms: 1_001,
        });
        let r = resolve(
            &FleetView::default(),
            &[wedge.clone(), child.clone()],
            &[ack_row(wedge.2), ack_row(child.2)],
        );
        assert!(
            r.tip.is_none(),
            "an empty-member descendant won the resolution: {r:?}"
        );
    }

    /// **ST-007 pin (ruling 2, re-armed).** Ruling 2 says an inadmissible
    /// descendant retires nothing "else any fleet-key holder could wedge all
    /// fleet sealing" — but an empty-member descendant WAS admissible, so it
    /// slipped the ruling's own defense and retired the honest tip. It must
    /// not: the honest tip stands.
    #[test]
    fn an_empty_member_descendant_does_not_retire_the_honest_tip() {
        let a = device_id(1);
        let view = fleet_of(&[a]);
        let honest = mint_row(vec![], &[a], 1);
        let wedge = raw_mint_row(MintCore {
            parents: vec![honest.2],
            member_ids: vec![],
            minter: [0xEE; 32],
            key_commitment: [0x02; 32],
            minted_at_ms: 1_001,
        });
        let r = resolve(
            &view,
            &[honest.clone(), wedge.clone()],
            &[ack_row(honest.2), ack_row(wedge.2)],
        );
        assert_eq!(
            r.tip.as_ref().map(|t| t.generation_id),
            Some(honest.2),
            "the empty-member descendant retired the honest tip: {r:?}"
        );
    }

    /// **ST-007 pin.** A minter outside its own member set is a forgery
    /// (`build_mint` refuses it at authoring); the resolver never checked.
    /// The author here signs GENUINELY with its own key — authorship is not
    /// in question — so this isolates the membership clause: an authenticated
    /// author who excludes itself from the member set escapes the removal
    /// severance its mints must stay accountable to.
    #[test]
    fn a_minter_outside_the_member_set_never_resolves() {
        let a = device_id(1);
        let outside = device_id(9); // enrolled nowhere near this mint's members
        let view = fleet_of(&[a]);
        let core = MintCore {
            parents: vec![],
            member_ids: vec![a],
            minter: outside,
            key_commitment: [0x03; 32],
            minted_at_ms: 1_000,
        };
        let id = generation_id(&core).unwrap();
        let genuine_sig = sign_mint_as_minter(&device_signing_key(9), &id);
        let forged = raw_mint_row_with_sig(core, genuine_sig);
        let r = resolve(&view, std::slice::from_ref(&forged), &[ack_row(forged.2)]);
        assert!(
            r.tip.is_none(),
            "a mint whose minter is outside its member set resolved: {r:?}"
        );
    }

    /// **ST-007.** Authorship is authenticated: a crafted mint naming an
    /// honest device as minter — with a signature that is not that device's —
    /// is a forgery the resolver refuses. Consequence B's enabler: without
    /// this, `minter ∈ member_ids` is satisfied by writing any verified id
    /// into the field.
    #[test]
    fn a_forged_minter_signature_never_resolves() {
        let a = device_id(1);
        let view = fleet_of(&[a]);
        let core = MintCore {
            parents: vec![],
            member_ids: vec![a],
            minter: a,
            key_commitment: [0x04; 32],
            minted_at_ms: 1_000,
        };
        let id = generation_id(&core).unwrap();
        // Signed by device 9, claiming device 1 as minter.
        let forged_sig = sign_mint_as_minter(&device_signing_key(9), &id);
        let forged = raw_mint_row_with_sig(core.clone(), forged_sig);
        let r = resolve(&view, std::slice::from_ref(&forged), &[ack_row(forged.2)]);
        assert!(r.tip.is_none(), "a forged-authorship mint resolved: {r:?}");
        assert!(
            r.invalid.iter().any(|(_, why)| why.contains("authorship")),
            "the forgery is counted, with the authorship reason: {r:?}"
        );

        // And the genuine article resolves — the guard is authorship, not
        // the existence of a signature field.
        let honest = raw_mint_row_with_sig(core, sign_mint_as_minter(&device_signing_key(1), &id));
        let r = resolve(&view, std::slice::from_ref(&honest), &[ack_row(honest.2)]);
        assert_eq!(r.tip.map(|t| t.generation_id), Some(id));
    }

    /// **ST-007, the keyability clause.** A mint this observer cannot key is
    /// no candidate here: it neither wins (first shape) nor retires the tip
    /// the observer can key (second shape) — and it is reported `unkeyable`,
    /// not `invalid`, because a legitimate mint that raced this device's
    /// enrollment looks identical until a top-up lands.
    #[test]
    fn an_unkeyable_mint_neither_wins_nor_retires() {
        let a = device_id(1);
        let view = fleet_of(&[a]);
        let old = mint_row(vec![], &[a], 1);
        let new = mint_row(vec![old.2], &[a], 2);
        let acks = vec![ack_row(old.2), ack_row(new.2)];

        // Alone and unkeyable: no tip, and the diagnostic names the row.
        let r = resolve_keyable(&view, std::slice::from_ref(&new), &acks, |_, _, _| false);
        assert!(r.tip.is_none());
        assert_eq!(r.unkeyable, vec![new.0.clone()]);
        assert!(r.invalid.is_empty(), "unkeyable is not invalid: {r:?}");

        // A keyable ancestor stands against an unkeyable descendant.
        let keyable_old = |id: &[u8; 32], _: &MintCore, _: &[MemberWrap]| *id == old.2;
        let r = resolve_keyable(&view, &[old.clone(), new.clone()], &acks, keyable_old);
        assert_eq!(
            r.tip.map(|t| t.generation_id),
            Some(old.2),
            "the unkeyable descendant must not retire the keyable tip"
        );

        // Fully keyable, the descendant wins as ever.
        let r = resolve_keyable(&view, &[old, new.clone()], &acks, |_, _, _| true);
        assert_eq!(r.tip.map(|t| t.generation_id), Some(new.2));
    }

    /// The leaf set: every id-bound row nobody names as a parent — candidates,
    /// non-candidates and shredded rows alike — so the first-need heal-mint
    /// supersedes attacker and orphan rows instead of rooting a second DAG.
    #[test]
    fn leaf_ids_cover_every_unreferenced_id_bound_row() {
        let a = device_id(1);
        let view = fleet_of(&[a]);
        // parent ← child (candidate chain); a shredded leaf; an unkeyable-ish
        // forged leaf (empty members — invalid, still id-bound).
        let parent = mint_row(vec![], &[a], 1);
        let child = mint_row(vec![parent.2], &[a], 2);
        let shredded = shredded_row(vec![parent.2], &[a], 3);
        let forged = raw_mint_row(MintCore {
            parents: vec![child.2],
            member_ids: vec![],
            minter: [0xEE; 32],
            key_commitment: [0x05; 32],
            minted_at_ms: 1_002,
        });
        let r = resolve(
            &view,
            &[
                parent.clone(),
                child.clone(),
                shredded.clone(),
                forged.clone(),
            ],
            &[ack_row(parent.2), ack_row(child.2)],
        );
        let mut expected = vec![shredded.2, forged.2];
        expected.sort();
        assert_eq!(r.leaf_ids, expected, "leaves: {r:?}");
    }

    /// Concurrent forks converge with no communication: the winner is the
    /// byte-order max of the content-derived ids, whatever the row order —
    /// the plane's no-local-preference discipline.
    #[test]
    fn concurrent_forks_resolve_to_the_byte_max_id_in_any_order() {
        let a = device_id(1);
        let view = fleet_of(&[a]);
        let parent = mint_row(vec![], &[a], 1);
        let f1 = mint_row(vec![parent.2], &[a], 2);
        let f2 = mint_row(vec![parent.2], &[a], 3);
        let expected = if f1.2 >= f2.2 { f1.2 } else { f2.2 };
        let acks = vec![ack_row(parent.2), ack_row(f1.2), ack_row(f2.2)];
        let forward = resolve(&view, &[parent.clone(), f1.clone(), f2.clone()], &acks);
        let reverse = resolve(&view, &[f2, f1, parent], &acks);
        assert_eq!(forward, reverse);
        assert_eq!(forward.tip.unwrap().generation_id, expected);
    }

    /// A shredded mint is retired by definition; alone it resolves nothing.
    #[test]
    fn a_shredded_mint_is_never_a_candidate() {
        let a = device_id(1);
        let view = fleet_of(&[a]);
        let s = shredded_row(vec![], &[a], 1);
        let r = resolve(&view, std::slice::from_ref(&s), &[ack_row(s.2)]);
        assert!(r.tip.is_none());
    }

    /// The Key↔id binding read back at the resolver: a row whose key is not
    /// its own core's content-derived id verifies as nothing — neither a
    /// candidate nor a DAG edge.
    #[test]
    fn a_mint_row_squatting_a_foreign_key_verifies_as_nothing() {
        let a = device_id(1);
        let view = fleet_of(&[a]);
        let honest = mint_row(vec![], &[a], 1);
        let (_, squat_value, squat_id) = mint_row(vec![], &[a], 2);
        // The squatter's core under the honest row's key.
        let squat = (honest.0.clone(), squat_value, squat_id);
        let r = resolve(&view, &[squat], &[ack_row(squat_id), ack_row(honest.2)]);
        assert!(r.tip.is_none());
        assert_eq!(r.invalid.len(), 1);
        assert!(r.invalid[0].1.contains("content-derived id"));
    }

    /// Acks are load-bearing: an untrusted holder's receipt, a tampered
    /// receipt, and junk bytes each ack nothing (and are counted).
    #[test]
    fn untrusted_or_tampered_receipts_ack_nothing() {
        let a = device_id(1);
        let view = fleet_of(&[a]);
        let m = mint_row(vec![], &[a], 1);

        let rogue = ed25519_dalek::SigningKey::from_bytes(&[0x99u8; 32]);
        let untrusted = (
            "k1".to_string(),
            canonical_encode(&sign_escrow_receipt(&rogue, m.2, [0xAB; 32], &tk(), 1)).unwrap(),
        );
        let mut tampered_receipt = sign_escrow_receipt(&holder(), m.2, [0xAB; 32], &tk(), 1);
        tampered_receipt.stamped_at_ms += 1;
        let tampered = (
            "k2".to_string(),
            canonical_encode(&tampered_receipt).unwrap(),
        );
        let junk = ("k3".to_string(), b"junk".to_vec());

        let r = resolve(&view, &[m], &[untrusted, tampered, junk]);
        assert!(r.tip.is_none());
        assert_eq!(r.invalid.len(), 3);
    }

    #[test]
    fn the_view_is_a_pure_function_of_rows_and_identity_line() {
        // Same rows, either order → identical views (no arrival-order state).
        let a = device_id(15);
        let b = device_id(16);
        let mut rows = vec![
            (
                crate::hex32::encode(&a),
                enrolled_row(a, cert_bytes(&root_keypair(), a, None), 5_000),
            ),
            (crate::hex32::encode(&b), removed_row(a)),
        ];
        let forward = view_of(&rows);
        rows.reverse();
        let reverse = view_of(&rows);
        assert_eq!(forward, reverse);
    }

    // ── The bounded mint's inline set ────────────────────────────────────────

    /// The minter is inline whatever its id, and the rest of the set is the
    /// `MAX_INLINE_MEMBER_WRAPS - 1` smallest other ids — pure over the
    /// inputs, so the same list in any order names the same set.
    #[test]
    fn the_inline_set_is_the_minter_plus_the_smallest_ids_whatever_the_order() {
        let minter = [0xF0u8; 32]; // byte-order LAST, still inline
        let mut ids: Vec<[u8; 32]> = (1..=20u8).map(|i| [i; 32]).collect();
        ids.push(minter);
        let forward = inline_wrap_members(&ids, &minter);
        ids.reverse();
        let reverse = inline_wrap_members(&ids, &minter);
        assert_eq!(forward, reverse, "order-independent");
        assert_eq!(forward.len(), MAX_INLINE_MEMBER_WRAPS);
        assert!(forward.contains(&minter), "the minter is always inline");
        for i in 1..MAX_INLINE_MEMBER_WRAPS as u8 {
            assert!(forward.contains(&[i; 32]), "id {i} is among the smallest");
        }
        assert!(
            !forward.contains(&[MAX_INLINE_MEMBER_WRAPS as u8; 32]),
            "the first id past the cap is spilled"
        );
    }

    /// A fleet that fits is all-inline, and a duplicated id counts once.
    #[test]
    fn a_small_fleet_is_all_inline_and_duplicates_count_once() {
        let minter = [1u8; 32];
        let ids = vec![[3u8; 32], [2u8; 32], minter, [3u8; 32]];
        let inline = inline_wrap_members(&ids, &minter);
        assert_eq!(inline.len(), 3);
        assert!(inline.contains(&[2u8; 32]) && inline.contains(&[3u8; 32]));
    }
}
