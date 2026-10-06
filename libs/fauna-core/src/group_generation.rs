//! The group generation machinery — [`crate::generation`] (R14 (account-data-plane.md § The ratified decisions)) re-targeted
//! at storage groups (T20).
//!
//! Owner: `docs/goal/architecture/account-data-plane.md` § The audience ladder
//! → *The recipient-set scheme* (the generations bullet: "Generations ride the
//! R14 machinery, re-targeted"); key material: `key-material-hierarchy.md`
//! § Audience: a storage group. A transplant, never a third invention, with
//! the section's **two ruled deltas**:
//!
//! * **(i) Wrap targets are member reception keys, cross-account** — a
//!   [`GroupMemberWrap`] seals the group generation key to the reception key
//!   recorded on one **roster entry** ([`crate::group_scope::RosterMember`]),
//!   never to a device KEM key.
//! * **(ii) Admissibility is ROSTER COVERAGE, replacing the escrow-ack arm** —
//!   a mint is admissible when its inline wrap set plus merged member top-ups
//!   cover the merged `Enrolled` roster. There is **no group escrow door**:
//!   durability is each member's own account custody of their reception
//!   secret plus their held wraps, so no escrow kinds exist on this plane.
//!
//! Two shapes differ from R14 for reasons the transplant makes explicit:
//!
//! * **Mints are authority-device-signed, and the minter is not a member.**
//!   Membership authority is the authority seam (v1: the initiating account's
//!   device fleet), so a `Minted` row carries the minting device's
//!   `DeviceAuthorization` inline — the roster-entry carriage, verified at
//!   the resolver against the authority root — where R14's minter proves
//!   itself by being in its own member set.
//! * **Cell identities.** Top-up cells are
//!   `"<generation>/<target-ENTRY>/<healer-ACTOR>"` — wraps are per-entry
//!   because reception keys hang off roster entries (a re-admitted member is
//!   a fresh entry with a fresh key). The unkeyable signal's cells are
//!   `"<generation>/<target-ACTOR>"` — "I cannot key G" is a fact about the
//!   member, and an actor id IS an Ed25519 verifying key, which is what keeps
//!   the join pure (the R14 rule: a merge may never consult external state;
//!   an entry-keyed cell would need the roster to find its verification key).
//!
//! The wrap ciphertexts (and the minter's carried authorization) sit
//! **outside** the content-derived id, exactly as in R14: the id must be a
//! deterministic function of what the mint *asserts*, and same-key variants
//! differing only in those siblings converge byte-level like any lattice
//! value. The stated consequence carries over too: a machinery-root holder
//! can publish a wrap-corrupted variant of an honest mint; the cure is the
//! transplanted self-heal loop (coverage admissibility forces the re-heal,
//! member top-ups restore it, the unkeyable signal reports the one case
//! healers cannot see), never a trusted writer.

use serde::{Deserialize, Serialize};

use crate::crypto::GenerationKey;
use crate::encoding::canonical_decode;
use crate::error::Error;
use crate::group_scope::{GroupAuthority, RosterView};
use crate::identity::ActorId;

// ── Contexts (all frozen — the replay discipline of the R14 originals, and a
// new group-named string per axis: a signature or id made in one plane must
// never verify in the other) ────────────────────────────────────────────────

/// Domain-separation context for the **content-derived group generation id**
/// — `BLAKE3` over the mint's canonical [`GroupMintCore`] bytes. Frozen: the
/// id is the mint row's logical key on every member forever.
pub const GROUP_GENERATION_ID_CONTEXT: &str = "fauna.group.generation.id.v1 2026-08-17";

/// Domain-separation context for the group **key↔id commitment** —
/// `commit = BLAKE3::derive_key(this, gen_key)`, carried in
/// [`GroupMintCore::key_commitment`] and covered by the content-derived id,
/// so every unwrap can refuse a substituted key. A group-named sibling of
/// [`crate::crypto::GENERATION_KEY_COMMIT_CONTEXT`], frozen on the same
/// terms.
pub const GROUP_GENERATION_KEY_COMMIT_CONTEXT: &str =
    "fauna.group.generation.key-commit.v1 2026-08-17";

/// Domain-separation tag for the group mint's minter signature (frozen).
pub const GROUP_MINT_MINTER_SIG_CONTEXT: &[u8] = b"fauna.group.generation.mint-minter.v1\0";

/// Domain-separation tag for the group top-up healer signature (frozen).
pub const GROUP_TOPUP_HEALER_SIG_CONTEXT: &[u8] = b"fauna.group.generation.topup-healer.v1\0";

/// Domain-separation tag for the group unkeyable-signal target signature
/// (frozen).
pub const GROUP_UNKEYABLE_TARGET_SIG_CONTEXT: &[u8] =
    b"fauna.group.generation.unkeyable-target.v1\0";

/// The group generation key's commitment — the value
/// [`GroupMintCore::key_commitment`] carries. The key type is
/// [`GenerationKey`] itself (32 random bytes under `Zeroizing` custody — the
/// same custody, so the same type, never a twin); only the commitment context
/// is group-named, which is what stops a commitment made in one plane from
/// verifying in the other.
#[must_use]
pub fn group_generation_key_commitment(key: &GenerationKey) -> [u8; 32] {
    blake3::derive_key(GROUP_GENERATION_KEY_COMMIT_CONTEXT, key.as_bytes())
}

// ── The mint kind ───────────────────────────────────────────────────────────

/// The DAG core of one group generation mint — everything the mint asserts,
/// and exactly what its content-derived id hashes over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupMintCore {
    /// Parent tip id(s) — one for an ordinary mint, several when a mint
    /// merges concurrent forks.
    #[serde(with = "crate::byte_array::vec")]
    pub parents: Vec<[u8; 32]>,
    /// The member set at mint: **roster entry ids** from merged roster state
    /// (delta (i): entries, never device ids). Admissibility is evaluated
    /// against the *observer's* merged roster, never trusted from this list
    /// alone.
    #[serde(with = "crate::byte_array::vec")]
    pub member_entries: Vec<[u8; 32]>,
    /// The minting **authority device** id — the Ed25519 verification key for
    /// the minter signature. Not a member: its license is the carried
    /// authorization (module header).
    #[serde(with = "serde_bytes")]
    pub minter: [u8; 32],
    /// `BLAKE3(gen_key)` under [`GROUP_GENERATION_KEY_COMMIT_CONTEXT`]. The
    /// content-derived generation id hashes over this, so every unwrap can
    /// refuse a substituted key — and two same-shaped concurrent mints still
    /// fork into distinct ids, because their random keys differ.
    #[serde(with = "serde_bytes")]
    pub key_commitment: [u8; 32],
    /// Mint stamp, unix ms. Advisory.
    pub minted_at_ms: i64,
    /// **The authority devices this mint was minted PAST** — the revoked set
    /// of the minter's authority line, ascending and duplicate-free (a
    /// non-sorted or repeated list is a forged shape, refused at the
    /// resolver). Inside the content-derived id, so the minter signs it and
    /// nobody edits it. **Line-committed admissibility** (ruled 2026-09-27
    /// with the per-writer roster cell — `account-data-taxonomy.md` § The
    /// recipient-set scheme → *Mint triggers*): a mint is admissible only if
    /// this set covers the reader's merged authority line, so a generation
    /// minted before a revocation names a smaller set, resolves inadmissible
    /// at every reader with the line, and is the debt the next severance pass
    /// pays with a mint past it. The set only grows, so the predicate is
    /// monotone and needs no order; authorship under the line stays a
    /// separate conjunct (a revoked device may list itself). The additive
    /// field discipline: an empty set encodes as absence, so a mint minted
    /// past nothing has the id it always had.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde(with = "crate::byte_array::vec")]
    pub revoked_past: Vec<[u8; 32]>,
}

/// One roster entry's inline X-Wing wrap of the group generation key, riding
/// the mint entry itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupMemberWrap {
    /// The target **roster entry id** (delta (i): the reception key that
    /// unwraps this hangs off the entry, so a re-admitted member's fresh
    /// entry gets fresh wraps).
    #[serde(with = "serde_bytes")]
    pub entry_id: [u8; 32],
    /// The X-Wing envelope sealing the generation key to that entry's
    /// reception public key (`fauna_mls::wrapped_blob::xwing_envelope` — one
    /// wrap format, never a fork).
    #[serde(with = "serde_bytes")]
    pub wrap: Vec<u8>,
}

/// One generation's row in `fauna.group.generation-mint`. Logical key = the
/// content-derived generation id hex. Immutable-by-lattice: the only change a
/// mint entry ever undergoes is the one-way step to [`Self::Shredded`].
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `consensus` ground: every reader must reach the same
/// decision from the value, so there is no unknown arm and a reader that cannot
/// decode one fails rather than guess). A new variant is an edit to
/// `tools/check-additive-evolution/enum_ledger.txt`, made in the same change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupGenerationMintRecord {
    /// A live generation: DAG core + the minter's proof + the per-entry
    /// inline wraps.
    Minted {
        core: GroupMintCore,
        /// The minter's Ed25519 signature over
        /// [`group_mint_minter_signing_bytes`] of the content-derived id —
        /// the ST-007 rule transplanted: authorship must verify at the
        /// reader, because any machinery-root holder can craft this row
        /// directly.
        #[serde(with = "serde_bytes")]
        minter_sig: Vec<u8>,
        /// The authority-device `DeviceAuthorization` covering
        /// [`GroupMintCore::minter`], in the [`crate::encoding::EmbedAsBytes`]
        /// carriage every witness surface uses — verified at the resolver
        /// against the authority root (the roster-entry pattern). A sibling
        /// of the core, deliberately outside the id, for the same
        /// convergence reason as `minter_sig`.
        #[serde(with = "serde_bytes")]
        authorization: Vec<u8>,
        wraps: Vec<GroupMemberWrap>,
    },
    /// The absorbing shred marker; this row's surviving bytes carry no wrap
    /// ciphertext.
    ///
    /// **What a reader does with it, stated as built** (ruled
    /// 2026-09-19 — `account-data-taxonomy.md` § The recipient-set
    /// scheme → *Severance, per axis*, the shred clause). The join absorbs and
    /// the resolver drops the generation's candidacy on ANY decodable
    /// `Shredded`, authored or not: that is availability only (the group
    /// needs a re-mint), the same class any machinery-root holder already
    /// reaches by same-key `Minted`-variant suppression. **Dropping a key is
    /// a different act and never rides an unauthored row**: the machinery
    /// root is held by every member ever admitted, so R14's `BackupKey`-gated
    /// acceptance does not transplant — a consumer may discard a generation
    /// key only when [`Self::shred_is_authored`] answers `Ok`.
    Shredded {
        core: GroupMintCore,
        /// Shred stamp, unix ms. Advisory; the instant the shredder's cert
        /// expiry anchors to.
        shredded_at_ms: i64,
        /// The shredding writer's 32-byte device id. **Attribution only
        /// unless the row is authored** — no reader consults it on its own.
        #[serde(with = "serde_bytes")]
        shredded_by: [u8; 32],
        /// `shredded_by`'s `DeviceAuthorization` in the
        /// [`crate::encoding::EmbedAsBytes`] carriage. Additive: empty on a
        /// row written before the ruling (which is then unauthored), skipped
        /// when empty so such a row encodes exactly as it always did.
        #[serde(default, skip_serializing_if = "Vec::is_empty", with = "serde_bytes")]
        authorization: Vec<u8>,
        /// Ed25519 by `shredded_by` over [`group_shred_signing_bytes`].
        #[serde(default, skip_serializing_if = "Vec::is_empty", with = "serde_bytes")]
        shredder_sig: Vec<u8>,
    },
}

/// Domain-separation tag for an authored shred's signature (frozen — the
/// [`GROUP_MINT_MINTER_SIG_CONTEXT`] replay discipline).
pub const GROUP_SHRED_SIG_CONTEXT: &[u8] = b"fauna.group.generation-shred.v1\0";

/// The exact bytes a shredder signs: the domain tag, the content-derived
/// generation id (which commits to the whole core), the shredder's device id
/// and the big-endian stamp — fixed-width throughout.
#[must_use]
pub fn group_shred_signing_bytes(
    generation_id: &[u8; 32],
    shredded_by: &[u8; 32],
    shredded_at_ms: i64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(GROUP_SHRED_SIG_CONTEXT.len() + 32 * 2 + 8);
    out.extend_from_slice(GROUP_SHRED_SIG_CONTEXT);
    out.extend_from_slice(generation_id);
    out.extend_from_slice(shredded_by);
    out.extend_from_slice(&shredded_at_ms.to_be_bytes());
    out
}

/// Build one **authored** shred as an authority device — the ONE production
/// shape of a `Shredded` row since the ruling. `authorization` is the
/// canonical `EmbedAsBytes` carriage of that device's `DeviceAuthorization`.
///
/// # Errors
/// Only on a core that fails canonical encoding.
pub fn sign_group_shred(
    authority_device: &ed25519_dalek::SigningKey,
    core: GroupMintCore,
    authorization: Vec<u8>,
    shredded_at_ms: i64,
) -> Result<GroupGenerationMintRecord, Error> {
    use ed25519_dalek::Signer;
    let id = group_generation_id(&core)?;
    let shredded_by = authority_device.verifying_key().to_bytes();
    let shredder_sig = authority_device
        .sign(&group_shred_signing_bytes(
            &id,
            &shredded_by,
            shredded_at_ms,
        ))
        .to_bytes()
        .to_vec();
    Ok(GroupGenerationMintRecord::Shredded {
        core,
        shredded_at_ms,
        shredded_by,
        authorization,
        shredder_sig,
    })
}

impl GroupGenerationMintRecord {
    /// Is this a `Shredded` row **authored by a live authority device** —
    /// the only shape a consumer may drop a generation key on? The carried
    /// authorization must pass the plane's one authority-device check
    /// ([`GroupAuthority::verify_device_cert`]: chain, expiry, not learned
    /// revoked), cover exactly `shredded_by`, and `shredder_sig` must verify
    /// under it over the **recomputed** generation id. Returns that id.
    ///
    /// # Errors
    /// A `Minted` row, an unauthored (pre-ruling or forged) shred, or any
    /// failure of the authority check or the signature.
    pub fn shred_is_authored(&self, authority: &GroupAuthority) -> Result<[u8; 32], String> {
        let GroupGenerationMintRecord::Shredded {
            core,
            shredded_at_ms,
            shredded_by,
            authorization,
            shredder_sig,
        } = self
        else {
            return Err("a Minted row shreds nothing".to_string());
        };
        if authorization.is_empty() || shredder_sig.is_empty() {
            return Err("shred is unauthored — it carries no authorization".to_string());
        }
        let id =
            group_generation_id(core).map_err(|e| format!("shred core does not encode: {e}"))?;
        let device_key =
            authority.verify_device_cert("shred authorization", authorization, *shredded_at_ms)?;
        if device_key != *shredded_by {
            return Err("shred authorization covers a different device id".to_string());
        }
        if shredder_sig.len() != 64
            || !crate::identity::verify_detached(
                shredded_by,
                &group_shred_signing_bytes(&id, shredded_by, *shredded_at_ms),
                shredder_sig,
            )
        {
            return Err("shred signature does not verify under the authorized device".to_string());
        }
        Ok(id)
    }
}

/// The content-derived group generation id of a mint: `BLAKE3` of the
/// canonical encoding of its DAG core under [`GROUP_GENERATION_ID_CONTEXT`].
///
/// # Errors
/// Only on a core that fails canonical encoding — unreachable for honestly
/// constructed values.
pub fn group_generation_id(core: &GroupMintCore) -> Result<[u8; 32], Error> {
    let bytes = crate::encoding::canonical_encode(core)?;
    Ok(blake3::derive_key(GROUP_GENERATION_ID_CONTEXT, &bytes))
}

/// The exact bytes a minter signs for one group mint — the domain-separated
/// content-derived id, which already commits to the whole core under its own
/// frozen context (the [`crate::generation::mint_minter_signing_bytes`]
/// shape).
pub fn group_mint_minter_signing_bytes(generation_id: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(GROUP_MINT_MINTER_SIG_CONTEXT.len() + 32);
    out.extend_from_slice(GROUP_MINT_MINTER_SIG_CONTEXT);
    out.extend_from_slice(generation_id);
    out
}

/// Sign one group mint as its minting authority device.
pub fn sign_group_mint_as_minter(
    minter: &ed25519_dalek::SigningKey,
    generation_id: &[u8; 32],
) -> Vec<u8> {
    use ed25519_dalek::Signer;
    minter
        .sign(&group_mint_minter_signing_bytes(generation_id))
        .to_bytes()
        .to_vec()
}

/// Verify a group mint's minter signature against the **recomputed**
/// content-derived id (never the row key an attacker chose) and the core's
/// own claimed minter.
///
/// # Errors
/// A minter id that is not a valid Ed25519 key, a signature of the wrong
/// width, or one that does not verify.
pub fn verify_group_mint_minter_sig(
    minter: &[u8; 32],
    generation_id: &[u8; 32],
    sig: &[u8],
) -> Result<(), Error> {
    if sig.len() != 64 {
        return Err(Error::Encoding(
            "group minter signature must be 64 bytes".into(),
        ));
    }
    if !crate::identity::verify_detached(
        minter,
        &group_mint_minter_signing_bytes(generation_id),
        sig,
    ) {
        return Err(Error::Encoding(
            "group minter signature does not verify".into(),
        ));
    }
    Ok(())
}

/// The `fauna.group.generation-mint` join, byte-level: `Shredded` absorbs,
/// byte-order max within a phase — the exact contract of
/// [`crate::generation::join_generation_mint`], at the group record type.
///
/// # Errors
/// Either side failing to decode as a [`GroupGenerationMintRecord`].
pub fn join_group_generation_mint(current: &[u8], incoming: &[u8]) -> Result<Vec<u8>, Error> {
    let cur: GroupGenerationMintRecord = canonical_decode(current)?;
    let inc: GroupGenerationMintRecord = canonical_decode(incoming)?;
    let cur_shredded = matches!(cur, GroupGenerationMintRecord::Shredded { .. });
    let inc_shredded = matches!(inc, GroupGenerationMintRecord::Shredded { .. });
    Ok(crate::generation::two_phase_winner(current, cur_shredded, incoming, inc_shredded).to_vec())
}

// ── The member top-up kind ──────────────────────────────────────────────────

/// One member-authored top-up wrap in `fauna.group.generation-wrap`, at the
/// three-segment logical key
/// `"<generation-id-hex>/<target-entry-id-hex>/<healer-actor-hex>"` — the
/// per-healer hardening's cell shape from birth (this plane never
/// had a legacy two-segment cell, so none exists here).
///
/// Each healer owns its own cell and the record is **self-authenticating**:
/// [`Self::verifies_at`] checks the in-value Ed25519 signature by the healer
/// ACTOR key named in the cell key, over every field including the wrap
/// bytes' hash. A machinery-root holder without that actor's secret can
/// neither displace a healer's row nor mint suppressing coverage (the
/// coverage arm counts only verifying rows authored by currently-enrolled
/// members). An enum from birth so an authenticated absorbing variant can
/// land additively, as R14's wrap kind planned.
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `consensus` ground: every reader must reach the same
/// decision from the value, so there is no unknown arm and a reader that cannot
/// decode one fails rather than guess). A new variant is an edit to
/// `tools/check-additive-evolution/enum_ledger.txt`, made in the same change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupTopupRecord {
    /// A live top-up wrap.
    Wrap {
        /// The generation whose key this wraps — echoes cell segment 1.
        #[serde(with = "serde_bytes")]
        generation_id: [u8; 32],
        /// The target roster entry id — echoes cell segment 2 (wraps are
        /// per-entry, module header).
        #[serde(with = "serde_bytes")]
        target_entry: [u8; 32],
        /// The authoring healer's actor id — echoes cell segment 3, and IS
        /// the Ed25519 verification key for `healer_sig` (delta (i): members
        /// are actors here, where R14's healers are devices).
        #[serde(with = "serde_bytes")]
        healer: [u8; 32],
        /// The healer's clock at authoring, unix ms. Signed, so a replayed
        /// old row cannot claim freshness.
        at_ms: i64,
        /// The X-Wing envelope sealing the generation key to the target
        /// entry's reception public key.
        #[serde(with = "serde_bytes")]
        wrap: Vec<u8>,
        /// Ed25519 by `healer` over [`group_topup_healer_signing_bytes`] —
        /// every field covered, the wrap bytes' hash included.
        #[serde(with = "serde_bytes")]
        healer_sig: Vec<u8>,
    },
}

impl GroupTopupRecord {
    /// Does this record verify **at its cell** — every field echoing the cell
    /// key's three segments, and `healer_sig` verifying under the cell's
    /// healer actor over exactly these bytes? Pure over the record and the
    /// cell coordinates (a merge may never consult external state);
    /// enrollment of the healer is the coverage arm's second,
    /// view-consulting half.
    #[must_use]
    pub fn verifies_at(
        &self,
        cell_generation: &[u8; 32],
        cell_target: &[u8; 32],
        cell_healer: &[u8; 32],
    ) -> bool {
        let GroupTopupRecord::Wrap {
            generation_id,
            target_entry,
            healer,
            at_ms,
            wrap,
            healer_sig,
        } = self;
        if generation_id != cell_generation || target_entry != cell_target || healer != cell_healer
        {
            return false;
        }
        crate::identity::verify_detached(
            healer,
            &group_topup_healer_signing_bytes(generation_id, target_entry, healer, *at_ms, wrap),
            healer_sig,
        )
    }
}

/// The exact bytes a healer signs for one [`GroupTopupRecord`] wrap — the
/// [`crate::generation::topup_healer_signing_bytes`] shape under the
/// group-named tag: fixed-width ids, the big-endian stamp, and the BLAKE3
/// hash of the wrap ciphertext, so no field can be swapped under a
/// carried-through signature.
#[must_use]
pub fn group_topup_healer_signing_bytes(
    generation_id: &[u8; 32],
    target_entry: &[u8; 32],
    healer: &[u8; 32],
    at_ms: i64,
    wrap: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(GROUP_TOPUP_HEALER_SIG_CONTEXT.len() + 32 * 4 + 8);
    out.extend_from_slice(GROUP_TOPUP_HEALER_SIG_CONTEXT);
    out.extend_from_slice(generation_id);
    out.extend_from_slice(target_entry);
    out.extend_from_slice(healer);
    out.extend_from_slice(&at_ms.to_be_bytes());
    out.extend_from_slice(blake3::hash(wrap).as_bytes());
    out
}

/// Sign one top-up wrap as its healing member. The verification key is the
/// healer's actor id itself (pass `actor.signing_key()`), so no key
/// distribution rides this.
pub fn sign_group_topup_as_healer(
    healer: &ed25519_dalek::SigningKey,
    generation_id: &[u8; 32],
    target_entry: &[u8; 32],
    at_ms: i64,
    wrap: &[u8],
) -> Vec<u8> {
    use ed25519_dalek::Signer;
    healer
        .sign(&group_topup_healer_signing_bytes(
            generation_id,
            target_entry,
            &healer.verifying_key().to_bytes(),
            at_ms,
            wrap,
        ))
        .to_bytes()
        .to_vec()
}

/// The three-segment top-up cell key. One derivation, shared by the writer,
/// the coverage arm, and the tests — never re-formatted ad hoc.
#[must_use]
pub fn group_topup_cell_key(
    generation_id: &[u8; 32],
    target_entry: &[u8; 32],
    healer: &[u8; 32],
) -> String {
    format!(
        "{}/{}/{}",
        crate::hex32::encode(generation_id),
        crate::hex32::encode(target_entry),
        crate::hex32::encode(healer)
    )
}

/// Parse a group top-up logical key — canonical-or-nothing (the
/// [`crate::generation::parse_wrap_cell_key`] injectivity rule), and exactly
/// three segments: this plane has no legacy cell shape.
#[must_use]
pub fn parse_group_topup_cell_key(key: &str) -> Option<([u8; 32], [u8; 32], [u8; 32])> {
    let canonical = |segment: &str| -> Option<[u8; 32]> {
        let bytes = crate::hex32::decode(segment).ok()?;
        (crate::hex32::encode(&bytes) == segment).then_some(bytes)
    };
    let segments: Vec<&str> = key.split('/').collect();
    match segments.as_slice() {
        [g, t, h] => Some((canonical(g)?, canonical(t)?, canonical(h)?)),
        _ => None,
    }
}

/// The `fauna.group.generation-wrap` join — byte-level, decode-or-fail: rank
/// = (verifies-at-this-cell, signed stamp, bytes), lexicographic max, winner
/// returned verbatim. The
/// [`crate::generation::join_generation_wrap_per_healer`] contract at the
/// group record type: a side that does not decode is an `Err`, never ranked;
/// first-contact strictness lives in the adoption arm.
pub fn join_group_topup<'a>(
    generation_id: &[u8; 32],
    target_entry: &[u8; 32],
    healer: &[u8; 32],
    current: &'a [u8],
    incoming: &'a [u8],
) -> Result<&'a [u8], Error> {
    let rank = |bytes: &[u8]| -> Result<(bool, i64), Error> {
        let rec = canonical_decode::<GroupTopupRecord>(bytes)?;
        let verifies = rec.verifies_at(generation_id, target_entry, healer);
        let GroupTopupRecord::Wrap { at_ms, .. } = rec;
        Ok((verifies, at_ms))
    };
    let (cur, inc) = (rank(current)?, rank(incoming)?);
    Ok(if (cur, current) >= (inc, incoming) {
        current
    } else {
        incoming
    })
}

// ── The target-authored "cannot key generation G" signal ────────────────────

/// One target-authored signal in `fauna.group.generation-unkeyable`, at the
/// two-segment logical key `"<generation-id-hex>/<target-actor-hex>"` — one
/// cell per (generation, member actor), authored ONLY by that member.
///
/// The [`crate::generation::GenerationUnkeyableRecord`] contract transplanted
/// — the cure for in-place corruption of a member's own inline wrap, with the
/// same anti-churn evidence rules — keyed by ACTOR where R14 keys by device:
/// the actor id is the member's Ed25519 verifying key, which is what keeps
/// the join pure (module header, cell identities).
///
/// **Closed by design** (`transport.md` § Schema and forward-compat discipline
/// → *Rule 3 in full*, the `consensus` ground: every reader must reach the same
/// decision from the value, so there is no unknown arm and a reader that cannot
/// decode one fails rather than guess). A new variant is an edit to
/// `tools/check-additive-evolution/enum_ledger.txt`, made in the same change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GroupUnkeyableRecord {
    /// "I cannot key this generation despite apparent coverage" — with the
    /// evidence already tried and failed.
    Asserted {
        /// The generation this member cannot key — echoes cell segment 1.
        #[serde(with = "serde_bytes")]
        generation_id: [u8; 32],
        /// The asserting member's actor id — echoes cell segment 2, and IS
        /// the Ed25519 verification key for `target_sig`.
        #[serde(with = "serde_bytes")]
        target_actor: [u8; 32],
        /// The member's clock at assertion, unix ms. Signed and rising.
        asserted_at_ms: i64,
        /// BLAKE3 hashes of the wrap ciphertexts tried and refused — the
        /// member's own inline wraps (any standing entry of theirs) plus
        /// every verifying top-up row targeting those entries by a
        /// currently-enrolled member — sorted, deduped, capped at
        /// [`crate::generation::MAX_UNKEYABLE_TRIED`] by byte order.
        #[serde(with = "crate::byte_array::vec")]
        tried: Vec<[u8; 32]>,
        /// Ed25519 by `target_actor` over
        /// [`group_unkeyable_target_signing_bytes`] — every field covered.
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
        target_actor: [u8; 32],
        /// Rises past the retired assertion's stamp.
        asserted_at_ms: i64,
        /// Ed25519 by `target_actor`, empty `tried` domain.
        #[serde(with = "serde_bytes")]
        target_sig: Vec<u8>,
    },
}

impl GroupUnkeyableRecord {
    /// Does this record verify **at its cell**? Pure over the record and the
    /// cell coordinates, exactly the top-up contract.
    #[must_use]
    pub fn verifies_at(&self, cell_generation: &[u8; 32], cell_target: &[u8; 32]) -> bool {
        let (generation_id, target_actor, variant, at_ms, tried, sig) = match self {
            GroupUnkeyableRecord::Asserted {
                generation_id,
                target_actor,
                asserted_at_ms,
                tried,
                target_sig,
            } => (
                generation_id,
                target_actor,
                crate::generation::UNKEYABLE_VARIANT_ASSERTED,
                *asserted_at_ms,
                tried.as_slice(),
                target_sig,
            ),
            GroupUnkeyableRecord::Satisfied {
                generation_id,
                target_actor,
                asserted_at_ms,
                target_sig,
            } => (
                generation_id,
                target_actor,
                crate::generation::UNKEYABLE_VARIANT_SATISFIED,
                *asserted_at_ms,
                [].as_slice(),
                target_sig,
            ),
        };
        if generation_id != cell_generation || target_actor != cell_target {
            return false;
        }
        crate::identity::verify_detached(
            target_actor,
            &group_unkeyable_target_signing_bytes(
                generation_id,
                target_actor,
                variant,
                at_ms,
                tried,
            ),
            sig,
        )
    }

    /// The signed stamp, whichever variant.
    #[must_use]
    pub fn asserted_at_ms(&self) -> i64 {
        match self {
            GroupUnkeyableRecord::Asserted { asserted_at_ms, .. }
            | GroupUnkeyableRecord::Satisfied { asserted_at_ms, .. } => *asserted_at_ms,
        }
    }
}

/// The exact bytes a member signs for one [`GroupUnkeyableRecord`] — the
/// [`crate::generation::unkeyable_target_signing_bytes`] layout under the
/// group-named tag (variant byte, fixed-width ids, big-endian stamp, hash
/// count, concatenated hashes).
#[must_use]
pub fn group_unkeyable_target_signing_bytes(
    generation_id: &[u8; 32],
    target_actor: &[u8; 32],
    variant: u8,
    asserted_at_ms: i64,
    tried: &[[u8; 32]],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        GROUP_UNKEYABLE_TARGET_SIG_CONTEXT.len() + 1 + 32 * 2 + 8 + 4 + 32 * tried.len(),
    );
    out.extend_from_slice(GROUP_UNKEYABLE_TARGET_SIG_CONTEXT);
    out.push(variant);
    out.extend_from_slice(generation_id);
    out.extend_from_slice(target_actor);
    out.extend_from_slice(&asserted_at_ms.to_be_bytes());
    out.extend_from_slice(&u32::try_from(tried.len()).unwrap_or(u32::MAX).to_be_bytes());
    for hash in tried {
        out.extend_from_slice(hash);
    }
    out
}

/// Sign one unkeyable record as its member (pass `actor.signing_key()`; an
/// empty `tried` for a `Satisfied`).
pub fn sign_group_unkeyable_as_target(
    target: &ed25519_dalek::SigningKey,
    generation_id: &[u8; 32],
    variant: u8,
    asserted_at_ms: i64,
    tried: &[[u8; 32]],
) -> Vec<u8> {
    use ed25519_dalek::Signer;
    target
        .sign(&group_unkeyable_target_signing_bytes(
            generation_id,
            &target.verifying_key().to_bytes(),
            variant,
            asserted_at_ms,
            tried,
        ))
        .to_bytes()
        .to_vec()
}

/// The `fauna.group.generation-unkeyable` cell key.
#[must_use]
pub fn group_unkeyable_cell_key(generation_id: &[u8; 32], target_actor: &[u8; 32]) -> String {
    format!(
        "{}/{}",
        crate::hex32::encode(generation_id),
        crate::hex32::encode(target_actor)
    )
}

/// Parse a group unkeyable logical key — canonical-or-nothing, two segments.
#[must_use]
pub fn parse_group_unkeyable_cell_key(key: &str) -> Option<([u8; 32], [u8; 32])> {
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

/// The `fauna.group.generation-unkeyable` join — byte-level, decode-or-fail,
/// rank = (verifies-at-this-cell, signed stamp, bytes); the
/// [`crate::generation::join_generation_unkeyable`] contract at the group
/// record type.
pub fn join_group_unkeyable<'a>(
    generation_id: &[u8; 32],
    target_actor: &[u8; 32],
    current: &'a [u8],
    incoming: &'a [u8],
) -> Result<&'a [u8], Error> {
    let rank = |bytes: &[u8]| -> Result<(bool, i64), Error> {
        let rec = canonical_decode::<GroupUnkeyableRecord>(bytes)?;
        Ok((
            rec.verifies_at(generation_id, target_actor),
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

// ── Tip resolution — roster-coverage admissibility (delta (ii)) ─────────────

/// The resolved group sealing tip: the winning mint's id, core, and inline
/// wraps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissibleGroupTip {
    /// The content-derived group generation id.
    pub generation_id: [u8; 32],
    /// The winning mint's DAG core (its `key_commitment` is what every
    /// unwrap verifies against).
    pub core: GroupMintCore,
    /// The winning mint's inline per-entry wraps.
    pub wraps: Vec<GroupMemberWrap>,
}

/// The outcome of one resolution pass over merged group mint + top-up rows —
/// the [`crate::generation::TipResolution`] shape.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroupTipResolution {
    /// The current candidate tip **for this observer** — admissible (both
    /// coverage arms) and keyable here. `None` before any admissible mint
    /// exists is the gate, and — the candidate-aware first-need ruling
    /// transplanted — also the mint/heal trigger.
    pub tip: Option<AdmissibleGroupTip>,
    /// Rows that verified as nothing: `(row key, reason)` — countable, never
    /// candidates. Structural forgeries land here (empty member set, a
    /// foreign-rooted or missing authorization, a minter signature that does
    /// not verify): an honest builder cannot produce them.
    pub invalid: Vec<(String, String)>,
    /// Rows that pass every observer-independent check but carry no wrap
    /// this observer can open — a legitimate mint that raced this member's
    /// admission looks exactly like this until a top-up lands; diagnostics,
    /// never `invalid`.
    pub unkeyable: Vec<String>,
    /// The mint DAG's current leaves (shredded rows included — their edges
    /// and ids stay). Ascending byte order; the heal-mint's parent source,
    /// capped by the consumer at [`crate::generation::MAX_MINT_PARENTS`].
    pub leaf_ids: Vec<[u8; 32]>,
}

/// Resolve this observer's current candidate group generation tip from
/// merged `fauna.group.generation-mint` rows, merged top-up rows, the
/// observer's verified [`RosterView`], the group's authority line
/// ([`GroupAuthority`] — revoked authority devices included), and the
/// observer's own key reach.
///
/// Candidacy is the R14 resolver predicate with the two ruled deltas:
/// key↔id binding; authenticated authorship (non-empty member-entry set, a
/// carried authorization chaining the minter to the authority root and
/// covering a device not learned revoked, a verifying minter signature, a
/// sorted duplicate-free `revoked_past`); **admissibility = both coverage
/// arms plus the line-committed arm** — (1) every listed entry is enrolled in
/// the observer's merged roster (an excluded, unknown, or unverifiable entry
/// makes the mint inadmissible: the severance-forcing arm), (2) every
/// enrolled roster entry is covered by an inline wrap or a verifying top-up
/// authored by a currently-enrolled member (the late-add arm — no escrow ack
/// exists on this plane), and (3) the mint's [`GroupMintCore::revoked_past`]
/// covers every authority device the observer's line revokes (a tip minted
/// before a revocation is nobody's tip once the revocation merges — the
/// severance mint's second trigger); and
/// observer-keyability via the injected closure. The supersession rules are
/// unchanged: only a *candidate* descendant retires an ancestor (any
/// machinery-root holder could otherwise wedge the group with one junk
/// mint), and the winner among survivors is the byte-order max id.
/// `authority` is the same authority line the caller built `roster` with
/// ([`RosterView::build`]'s parameter) — pass the same value, or minter
/// verification and roster verification would answer to different
/// authorities. A `Shredded` row drops its generation's candidacy whether or
/// not it is authored (availability only — the variant's doc owns the
/// ruling); nothing here drops a key.
pub fn resolve_admissible_group_tip<'a, M, T, K>(
    roster: &RosterView,
    authority: &GroupAuthority,
    mint_rows: M,
    topup_rows: T,
    observer_keyable: K,
) -> GroupTipResolution
where
    M: IntoIterator<Item = (&'a str, &'a [u8])>,
    T: IntoIterator<Item = (&'a str, &'a [u8])>,
    K: Fn(&[u8; 32], &GroupMintCore, &[GroupMemberWrap]) -> bool,
{
    use std::collections::{BTreeMap, BTreeSet};

    let mut resolution = GroupTipResolution::default();

    // The enrolled entry set — both coverage arms range over it.
    let enrolled: Vec<[u8; 32]> = roster.wrap_targets().map(|m| m.entry_id).collect();

    // Verified top-up coverage: (generation id, target entry) pairs some
    // currently-enrolled member's verifying row covers. Rows that fail to
    // parse or verify are flagged; rows by non-members are silently not
    // coverage (a removal is not a forgery).
    let mut topup_covered: BTreeSet<([u8; 32], [u8; 32])> = BTreeSet::new();
    for (key, value) in topup_rows {
        let Some((generation, target, healer)) = parse_group_topup_cell_key(key) else {
            resolution.invalid.push((
                key.to_string(),
                "top-up key is not three canonical hex segments".to_string(),
            ));
            continue;
        };
        let record: GroupTopupRecord = match canonical_decode(value) {
            Ok(r) => r,
            Err(e) => {
                resolution
                    .invalid
                    .push((key.to_string(), format!("top-up does not decode: {e}")));
                continue;
            }
        };
        if !record.verifies_at(&generation, &target, &healer) {
            resolution.invalid.push((
                key.to_string(),
                "top-up does not verify at its cell".to_string(),
            ));
            continue;
        }
        if !roster.is_verified_member(&ActorId(healer)) {
            continue;
        }
        topup_covered.insert((generation, target));
    }

    // Decode every mint row; keep the whole DAG (shredded included) for the
    // ancestor walk, and select candidates.
    let mut parents: BTreeMap<[u8; 32], Vec<[u8; 32]>> = BTreeMap::new();
    let mut candidates: BTreeMap<[u8; 32], AdmissibleGroupTip> = BTreeMap::new();
    for (key, value) in mint_rows {
        let record: GroupGenerationMintRecord = match canonical_decode(value) {
            Ok(r) => r,
            Err(e) => {
                resolution
                    .invalid
                    .push((key.to_string(), format!("mint does not decode: {e}")));
                continue;
            }
        };
        let (core, minter_sig, authorization, wraps) = match record {
            GroupGenerationMintRecord::Minted {
                core,
                minter_sig,
                authorization,
                wraps,
            } => (core, minter_sig, authorization, wraps),
            GroupGenerationMintRecord::Shredded { core, .. } => {
                if let Ok(id) = group_generation_id(&core) {
                    parents.insert(id, core.parents.clone());
                }
                continue;
            }
        };
        // Key↔id binding, read back at the resolver.
        let id = match group_generation_id(&core) {
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
        // Authenticated authorship: an honest builder cannot emit these
        // shapes, so each is a forgery — counted, never a candidate.
        if core.member_entries.is_empty() {
            resolution.invalid.push((
                key.to_string(),
                "mint member-entry set is empty — a forged row".to_string(),
            ));
            continue;
        }
        if let Err(reason) =
            verify_minter_authorization(&authorization, &core.minter, core.minted_at_ms, authority)
        {
            resolution.invalid.push((key.to_string(), reason));
            continue;
        }
        if let Err(e) = verify_group_mint_minter_sig(&core.minter, &id, &minter_sig) {
            resolution
                .invalid
                .push((key.to_string(), format!("mint authorship fails: {e}")));
            continue;
        }
        if !core.revoked_past.windows(2).all(|w| w[0] < w[1]) {
            resolution.invalid.push((
                key.to_string(),
                "mint revoked-past set is not ascending and duplicate-free — a forged row"
                    .to_string(),
            ));
            continue;
        }
        // Admissibility arm 1 — severance-forcing: every listed entry must
        // be enrolled at this observer.
        if !core
            .member_entries
            .iter()
            .all(|e| roster.is_enrolled_entry(e))
        {
            continue;
        }
        // Admissibility arm 2 — coverage: every enrolled entry has an inline
        // wrap or a verified member top-up for this generation.
        if !enrolled.iter().all(|entry| {
            wraps.iter().any(|w| w.entry_id == *entry) || topup_covered.contains(&(id, *entry))
        }) {
            continue;
        }
        // Admissibility arm 3 — line-committed: the mint was minted past
        // every authority device this observer's line revokes. A skip, never
        // a forgery: an honest mint that predates a revocation looks exactly
        // like this, and it is the debt the next severance pass pays.
        if !authority
            .revoked()
            .all(|device| core.revoked_past.binary_search(device).is_ok())
        {
            continue;
        }
        // Observer-keyability: a mint this observer cannot key must neither
        // win nor retire — it is some other member's candidate, not ours.
        if !observer_keyable(&id, &core, &wraps) {
            resolution.unkeyable.push(key.to_string());
            continue;
        }
        candidates.insert(
            id,
            AdmissibleGroupTip {
                generation_id: id,
                core,
                wraps,
            },
        );
    }

    // Leaves: every id-bound row nobody names as a parent.
    let referenced: BTreeSet<[u8; 32]> = parents.values().flatten().copied().collect();
    resolution.leaf_ids = parents
        .keys()
        .filter(|id| !referenced.contains(*id))
        .copied()
        .collect();

    // Supersession: retire every candidate that is a transitive ancestor of
    // another candidate; the walk crosses non-candidate intermediates.
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

    resolution.tip = candidates
        .into_iter()
        .filter(|(id, _)| !retired.contains(id))
        .map(|(_, tip)| tip)
        .next_back();
    resolution
}

/// Verify a mint's carried authority-device authorization — the plane's one
/// authority-device check ([`GroupAuthority::verify_device_cert`]: chain to
/// the authority root or a prior identity, not expired before the mint's
/// asserted instant, not learned revoked) at the mint surface, plus the one
/// mint-specific clause: the cert covers exactly the core's claimed minter.
fn verify_minter_authorization(
    authorization: &[u8],
    minter: &[u8; 32],
    minted_at_ms: i64,
    authority: &GroupAuthority,
) -> Result<(), String> {
    let device_key =
        authority.verify_device_cert("minter authorization", authorization, minted_at_ms)?;
    if device_key != *minter {
        return Err("minter authorization covers a different device id".to_string());
    }
    Ok(())
}

/// One retained generation inside an admission bundle: the id names the
/// mint whose `key_commitment` the joiner verifies the key against once it
/// can read the mint DAG.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupRetainedGeneration {
    /// The content-derived group generation id.
    #[serde(with = "serde_bytes")]
    pub generation_id: [u8; 32],
    /// The generation's 32-byte key ([`crate::secret::SecretByteBuf`] —
    /// zeroizing, redacted `Debug`, CBOR byte string).
    pub key: crate::secret::SecretByteBuf,
}

/// The admission wrap's plaintext — what one X-Wing envelope hands a joiner
/// at admission (`key-material-hierarchy.md` § Audience: a storage group,
/// the machinery-root bullet: the root travels "inside the same X-Wing wrap
/// bundle as the retained generation bundle"): the scope's machinery root
/// plus the tip *and* the generations still covering live content.
///
/// Sealed/opened only by
/// `fauna_mls::wrapped_blob::group_generation_wraps::{seal,open}_group_admission_bundle`
/// — the open door verifies the ROOT's commitment against the birth record
/// in-door (root substitution is a crisp refusal at the joiner), and returns
/// the retained keys for the caller's per-mint commitment checks (which need
/// the mint DAG, readable only once the root is in hand — the bootstrap
/// order the ruling states).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupAdmissionBundle {
    /// The scope's machinery root bytes.
    pub machinery_root: crate::secret::SecretByteBuf,
    /// The retained generation bundle, tip included.
    pub retained: Vec<GroupRetainedGeneration>,
}

// ── The group-reception keypair (the member-side kind) ──────────────────────
//
// The wrap target's account-side half (`key-material-hierarchy.md`
// § Audience: a storage group, the group-reception bullet): the secret half
// is a fleet-only, `GenerationTip`-sealed kind in the member's OWN account
// plane (`fauna_protocol::merge_policy::KIND_GROUP_RECEPTION_KEY`) — so its
// custody, device-removal severance, and escrow ride the member's own R14
// machinery, nothing group-specific — and the public half is published
// actor-signed. It ROTATES on the member's own fleet mint (a fresh record,
// a fresh published half), and that rotation is the member's re-mint signal
// to every group holding them — the rotation coupling that closes the
// static-per-account-target rule-6 hole. The rotation call rides the same
// engine surface that completes a fleet mint (the R14 step-7 trigger wiring
// `fauna_sync_engine::generation_mint` queues); this module owns the pure
// halves it will call.

/// Domain-separation context for the ML-KEM-768 half of a member's X-Wing
/// group-reception keypair ([`GroupReceptionKeyRecord::keypair`]). Frozen —
/// the public half rests in roster entries and published records.
pub const GROUP_RECEPTION_KEM_MLKEM_CONTEXT: &str = "fauna.group.reception-kem.mlkem.v1 2026-08-17";

/// Domain-separation context for the X25519 half. A distinct scalar from the
/// member's Ed25519 identity and from every other KEM derivation here — no
/// cross-protocol key reuse (the `derive_keypair_from_ikm` discipline).
pub const GROUP_RECEPTION_KEM_X25519_CONTEXT: &str =
    "fauna.group.reception-kem.x25519.v1 2026-08-17";

/// One reception keypair's row in `fauna.state.group-reception-key` — the
/// member's account plane, fleet-only, `GenerationTip`-sealed. Logical key =
/// [`Self::logical_key`] (a digest of the public half), one **Immutable** row
/// per keypair: rotation writes a fresh row and old rows are retained, because
/// old generations' wraps still target old reception keys — severance of a
/// stolen device comes from the member's own R14 fleet mint re-sealing this
/// kind under a tip the stolen device cannot reach, never from deleting rows.
///
/// Stores the 32-byte **ikm**, not the expanded keypair: X-Wing keygen is
/// deterministic from it ([`Self::keypair`]), so nothing key-sized rests
/// beyond the seed. The ikm is random per rotation
/// ([`Self::mint`]) — never derived from a stable secret, or rotation would
/// be a fiction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupReceptionKeyRecord {
    /// The keypair's 32-byte input key material ([`crate::secret::SecretByteBuf`]
    /// — zeroizing, redacted `Debug`, CBOR byte string).
    pub ikm: crate::secret::SecretByteBuf,
    /// Mint stamp, unix ms. Advisory.
    pub minted_at_ms: i64,
}

impl GroupReceptionKeyRecord {
    /// Mint a fresh reception keypair record from the OS CSPRNG — called at
    /// first group use and on every fleet-mint rotation.
    #[must_use]
    pub fn mint(minted_at_ms: i64) -> GroupReceptionKeyRecord {
        GroupReceptionKeyRecord {
            ikm: crate::secret::SecretByteBuf::from(
                crate::crypto::GenerationKey::mint().as_bytes().to_vec(),
            ),
            minted_at_ms,
        }
    }

    /// The X-Wing keypair, re-derived from the stored ikm under the two
    /// frozen contexts above.
    ///
    /// # Errors
    /// An ikm that is not exactly 32 bytes (a corrupted row — honest writers
    /// only ever store [`Self::mint`]'s output).
    pub fn keypair(&self) -> Result<fauna_pq_kem::XWingKeyPair, Error> {
        let ikm: &[u8] = &self.ikm;
        if ikm.len() != 32 {
            return Err(Error::Encoding(format!(
                "group-reception ikm must be 32 bytes, got {}",
                ikm.len()
            )));
        }
        Ok(fauna_pq_kem::derive_keypair_from_ikm(
            ikm,
            GROUP_RECEPTION_KEM_MLKEM_CONTEXT,
            GROUP_RECEPTION_KEM_X25519_CONTEXT,
        ))
    }

    /// The published/roster-carried public half, in the wire form every wrap
    /// target field carries.
    ///
    /// # Errors
    /// As [`Self::keypair`].
    pub fn reception_pubkey(&self) -> Result<Vec<u8>, Error> {
        Ok(self.keypair()?.public.to_bytes().to_vec())
    }

    /// The row's logical key: the lowercase hex of `BLAKE3(public half)` —
    /// compact (a raw X-Wing public key is 1216 bytes), deterministic, and
    /// exactly what a member holding a wrap's target key needs to find its
    /// secret. Naming only — the plane's item blind is the opacity layer;
    /// this needs no keyed context.
    ///
    /// # Errors
    /// As [`Self::keypair`].
    pub fn logical_key(&self) -> Result<String, Error> {
        Ok(reception_pubkey_row_key(&self.reception_pubkey()?))
    }
}

/// The `fauna.state.group-reception-key` logical key for a given public half
/// — one derivation, shared by the writer and every reader that starts from
/// a wrap target.
#[must_use]
pub fn reception_pubkey_row_key(reception_pubkey: &[u8]) -> String {
    crate::hex32::encode(blake3::hash(reception_pubkey).as_bytes())
}

/// The actor-signed **published half** — what a nest serves (key-package
/// style) and what peers carry over authenticated channels in the no-nest
/// profile. Verification is by [`crate::encoding::verify_envelope`] against
/// the embedded `member_actor`, so a consumer additionally checks that actor
/// is the member it meant (the [`verify_group_reception_published`] door does
/// both).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupReceptionPublished {
    /// The publishing member — the Ed25519 key the envelope verifies under.
    pub member_actor: ActorId,
    /// The current reception public key (X-Wing wire form).
    #[serde(with = "serde_bytes")]
    pub reception_pubkey: Vec<u8>,
    /// Publication stamp, unix ms — signed, so consumers can prefer the
    /// newest of several carried copies; advisory beyond that.
    pub rotated_at_ms: i64,
}

impl crate::encoding::Signed for GroupReceptionPublished {
    fn signer_public_key(&self) -> &[u8; 32] {
        &self.member_actor.0
    }
}

/// Sign one published half as the member, in the [`crate::encoding::EmbedAsBytes`]
/// carriage every witness surface uses.
///
/// # Errors
/// Only on encoding failure — unreachable for honest values.
pub fn sign_group_reception_published(
    member: &crate::identity::ActorKeypair,
    reception_pubkey: Vec<u8>,
    rotated_at_ms: i64,
) -> Result<Vec<u8>, Error> {
    let record = GroupReceptionPublished {
        member_actor: member.actor_id(),
        reception_pubkey,
        rotated_at_ms,
    };
    let (bytes, env) = crate::encoding::sign_envelope(member, &record)?;
    crate::encoding::canonical_encode(&crate::encoding::EmbedAsBytes::from_signed(bytes, env))
}

/// Open and verify one carried published half: the carriage decodes, the
/// envelope verifies under the embedded actor, and that actor is
/// `expected_member` — a valid record from ANOTHER member never serves.
///
/// # Errors
/// A malformed carriage, a signature that does not verify, or a record
/// published by a different actor than expected.
pub fn verify_group_reception_published(
    carriage: &[u8],
    expected_member: &ActorId,
) -> Result<GroupReceptionPublished, String> {
    use crate::encoding::{EmbedAsBytes, decode_signed_bytes, verify_envelope};
    let wire: EmbedAsBytes = canonical_decode(carriage)
        .map_err(|e| format!("published reception key is not an EmbedAsBytes carriage: {e}"))?;
    let (bytes, env) = wire
        .into_signed()
        .map_err(|e| format!("published reception key envelope malformed: {e}"))?;
    let record: GroupReceptionPublished = decode_signed_bytes(&bytes)
        .map_err(|e| format!("published reception key bytes do not decode: {e}"))?;
    verify_envelope(&record, &bytes, &env)
        .map_err(|_| "published reception key signature invalid".to_string())?;
    if record.member_actor != *expected_member {
        return Err("published reception key names a different member".to_string());
    }
    Ok(record)
}

// ── The held machinery root (the member-side custody kind) ──────────────────

/// One held machinery root's row in `fauna.state.group-machinery-root` — the
/// member's own account plane, fleet-only, `GenerationTip`-sealed. Logical
/// key = the scope id hex; one **Immutable** row per scope (the root never
/// rotates, so a second value under the key is a different scope's root or a
/// forgery — never a newer one).
///
/// **Why an account-plane kind at all** (the custody gap the ceremony build
/// surfaced, ruled with it 2026-08-17 — owner:
/// `key-material-hierarchy.md` § Audience: a storage group, the
/// machinery-root bullet's custody sentence): the admission bundle is a
/// one-shot ceremony artifact and the root is random — underivable — so
/// "held forever from admission" requires a durable home, and it must be
/// **fleet-synced** (the member's other devices read the group too) and
/// **tip-sealed** (a stolen device falls out of reach at the member's next
/// fleet mint — the reception-key kind's argument, verbatim). Held group
/// GENERATION keys deliberately get no such kind: the member's wraps rest in
/// the group plane itself and re-open with the reception secret on demand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupHeldRootRecord {
    /// The scope this root belongs to — echoed from the logical key so the
    /// value is self-describing; readers refuse a mismatch and verify the
    /// commitment against the scope's birth record before first use.
    #[serde(with = "serde_bytes")]
    pub scope_id: [u8; 32],
    /// The 32-byte machinery root ([`crate::secret::SecretByteBuf`] —
    /// zeroizing, redacted `Debug`, CBOR byte string).
    pub root: crate::secret::SecretByteBuf,
    /// When this member first held the root (scope birth for the initiator,
    /// admission for a joiner), unix ms. Advisory.
    pub held_since_ms: i64,
}

impl GroupHeldRootRecord {
    /// The row's logical key for a scope. One derivation, shared by writer
    /// and readers.
    #[must_use]
    pub fn logical_key_for(scope_id: &[u8; 32]) -> String {
        crate::hex32::encode(scope_id)
    }

    /// The held root, in its custody type.
    ///
    /// # Errors
    /// A root that is not exactly 32 bytes (a corrupted row).
    pub fn machinery_root(&self) -> Result<crate::crypto::GroupMachineryRoot, Error> {
        let bytes: [u8; 32] = self.root.as_ref().try_into().map_err(|_| {
            Error::Encoding(format!(
                "held machinery root must be 32 bytes, got {}",
                self.root.len()
            ))
        })?;
        Ok(crate::crypto::GroupMachineryRoot::from_bytes(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{Capability, DeviceAuthorization, Timestamp};
    use crate::encoding::{EmbedAsBytes, canonical_encode, sign_envelope};
    use crate::group_scope::{GroupBirthRecord, RosterEntryCore, group_scope_id, roster_entry_id};
    use crate::identity::ActorKeypair;

    fn authority() -> ActorKeypair {
        ActorKeypair::from_secret([21u8; 32])
    }

    fn prior() -> ActorKeypair {
        ActorKeypair::from_secret([22u8; 32])
    }

    fn foreign() -> ActorKeypair {
        ActorKeypair::from_secret([23u8; 32])
    }

    fn member(seed: u8) -> ActorKeypair {
        ActorKeypair::from_secret([seed; 32])
    }

    fn device(seed: u8) -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[seed; 32])
    }

    fn scope() -> [u8; 32] {
        group_scope_id(&GroupBirthRecord {
            authority_actor: authority().actor_id(),
            salt: [0xB1; 32],
            machinery_root_commit: crate::crypto::GroupMachineryRoot::from_bytes([0xD7; 32])
                .commitment(),
            created_at_ms: 1_700_000_000_000,
        })
        .expect("scope id")
    }

    /// A `DeviceAuthorization` for `device`, signed by `signer`, in the
    /// `EmbedAsBytes` carriage mints and roster rows both embed.
    fn cert_bytes(signer: &ActorKeypair, device: [u8; 32], expires_at: Option<u64>) -> Vec<u8> {
        let cert = DeviceAuthorization {
            actor_id: signer.actor_id(),
            device_key: device,
            capabilities: vec![Capability::RenewBearer],
            created_at: Timestamp(1_000),
            expires_at: expires_at.map(Timestamp),
        };
        let (bytes, env) = sign_envelope(signer, &cert).expect("sign cert");
        canonical_encode(&EmbedAsBytes::from_signed(bytes, env)).expect("encode carriage")
    }

    fn entry_core(member_seed: u8) -> RosterEntryCore {
        RosterEntryCore {
            scope_id: scope(),
            member_actor: member(member_seed).actor_id(),
            admission_salt: [member_seed; 32],
        }
    }

    fn enrolled_row(member_seed: u8) -> (String, Vec<u8>) {
        let core = entry_core(member_seed);
        let authority_device = device(0x41);
        let (entry_id, record) = crate::group_scope::sign_roster_enrollment(
            &authority_device,
            core,
            vec![member_seed; 8],
            cert_bytes(
                &authority(),
                authority_device.verifying_key().to_bytes(),
                None,
            ),
            2_000,
        )
        .expect("entry");
        (
            crate::group_scope::roster_cell_key(
                &entry_id,
                &authority_device.verifying_key().to_bytes(),
            ),
            canonical_encode(&record).expect("encode row"),
        )
    }

    /// An authored removal of `entry_id` by authority device 0x41 — honored
    /// under a line that does not revoke it.
    fn removed_row(entry_id: &[u8; 32]) -> (String, Vec<u8>) {
        let remover = device(0x41);
        let (key, record) = crate::group_scope::sign_roster_removal(
            &remover,
            *entry_id,
            cert_bytes(&authority(), remover.verifying_key().to_bytes(), None),
            3_000,
        );
        (key, canonical_encode(&record).expect("encode row"))
    }

    /// The authority line as a reader holding `revocations` sees it.
    fn line(revocations: &[(String, Vec<u8>)]) -> GroupAuthority {
        GroupAuthority::build(
            &scope(),
            &authority().actor_id(),
            &[prior().actor_id()],
            revocations.iter().map(|(k, v)| (k.as_str(), v.as_slice())),
        )
    }

    /// Authority device `revoker_seed` revokes authority device `revoked_seed`.
    fn revocation_row(revoker_seed: u8, revoked_seed: u8) -> (String, Vec<u8>) {
        let revoker = device(revoker_seed);
        let (key, record) = crate::group_scope::sign_authority_revocation(
            &revoker,
            cert_bytes(&authority(), revoker.verifying_key().to_bytes(), None),
            scope(),
            device(revoked_seed).verifying_key().to_bytes(),
            8_000,
        );
        (key, canonical_encode(&record).expect("encode revocation"))
    }

    fn roster(rows: &[(String, Vec<u8>)]) -> RosterView {
        roster_under(&line(&[]), rows)
    }

    fn roster_under(authority: &GroupAuthority, rows: &[(String, Vec<u8>)]) -> RosterView {
        RosterView::build(
            &scope(),
            authority,
            rows.iter().map(|(k, v)| (k.as_str(), v.as_slice())),
        )
    }

    fn entry_id_of(member_seed: u8) -> [u8; 32] {
        roster_entry_id(&entry_core(member_seed)).expect("entry id")
    }

    /// A well-formed `Minted` row over the given entries, wrapped for
    /// `wrapped` (dummy wrap bytes — the resolver never opens them), minted
    /// by authority device 0x41 with its cert carried inline.
    fn mint_row(
        parents: Vec<[u8; 32]>,
        entries: &[[u8; 32]],
        wrapped: &[[u8; 32]],
        key_seed: u8,
    ) -> (String, Vec<u8>) {
        mint_row_with(parents, entries, wrapped, key_seed, &authority(), 0x41)
    }

    fn mint_row_with(
        parents: Vec<[u8; 32]>,
        entries: &[[u8; 32]],
        wrapped: &[[u8; 32]],
        key_seed: u8,
        cert_signer: &ActorKeypair,
        device_seed: u8,
    ) -> (String, Vec<u8>) {
        mint_row_past(
            parents,
            entries,
            wrapped,
            key_seed,
            cert_signer,
            device_seed,
            Vec::new(),
        )
    }

    /// [`mint_row_with`] minted PAST `revoked_past` (the line-committed arm).
    fn mint_row_past(
        parents: Vec<[u8; 32]>,
        entries: &[[u8; 32]],
        wrapped: &[[u8; 32]],
        key_seed: u8,
        cert_signer: &ActorKeypair,
        device_seed: u8,
        revoked_past: Vec<[u8; 32]>,
    ) -> (String, Vec<u8>) {
        let minter = device(device_seed);
        let core = GroupMintCore {
            parents,
            member_entries: entries.to_vec(),
            minter: minter.verifying_key().to_bytes(),
            key_commitment: group_generation_key_commitment(&GenerationKey::from_bytes(
                [key_seed; 32],
            )),
            minted_at_ms: 5_000,
            revoked_past,
        };
        let id = group_generation_id(&core).expect("generation id");
        let minter_sig = sign_group_mint_as_minter(&minter, &id);
        let wraps = wrapped
            .iter()
            .map(|entry| GroupMemberWrap {
                entry_id: *entry,
                wrap: vec![0xEE; 16],
            })
            .collect();
        let value = canonical_encode(&GroupGenerationMintRecord::Minted {
            core,
            minter_sig,
            authorization: cert_bytes(cert_signer, minter.verifying_key().to_bytes(), None),
            wraps,
        })
        .expect("encode mint");
        (crate::hex32::encode(&id), value)
    }

    fn topup_row(
        generation_id: &[u8; 32],
        target_entry: &[u8; 32],
        healer_seed: u8,
    ) -> (String, Vec<u8>) {
        let healer = member(healer_seed);
        let wrap = vec![0xAB; 16];
        let sig = sign_group_topup_as_healer(
            healer.signing_key(),
            generation_id,
            target_entry,
            7_000,
            &wrap,
        );
        let record = GroupTopupRecord::Wrap {
            generation_id: *generation_id,
            target_entry: *target_entry,
            healer: healer.actor_id().0,
            at_ms: 7_000,
            wrap,
            healer_sig: sig,
        };
        (
            group_topup_cell_key(generation_id, target_entry, &healer.actor_id().0),
            canonical_encode(&record).expect("encode top-up"),
        )
    }

    fn resolve(
        view: &RosterView,
        mints: &[(String, Vec<u8>)],
        topups: &[(String, Vec<u8>)],
    ) -> GroupTipResolution {
        resolve_under(&line(&[]), view, mints, topups)
    }

    fn resolve_under(
        authority: &GroupAuthority,
        view: &RosterView,
        mints: &[(String, Vec<u8>)],
        topups: &[(String, Vec<u8>)],
    ) -> GroupTipResolution {
        resolve_admissible_group_tip(
            view,
            authority,
            mints.iter().map(|(k, v)| (k.as_str(), v.as_slice())),
            topups.iter().map(|(k, v)| (k.as_str(), v.as_slice())),
            |_, _, _| true,
        )
    }

    // ── ids and commitments ─────────────────────────────────────────────────

    #[test]
    fn every_core_field_moves_the_group_generation_id() {
        let base_core = GroupMintCore {
            parents: vec![[0x01; 32]],
            member_entries: vec![[0x02; 32]],
            minter: [0x03; 32],
            key_commitment: [0x04; 32],
            minted_at_ms: 5_000,
            revoked_past: Vec::new(),
        };
        let base = group_generation_id(&base_core).unwrap();
        let mut m = base_core.clone();
        m.parents = vec![[0x11; 32]];
        assert_ne!(group_generation_id(&m).unwrap(), base, "parents");
        let mut m = base_core.clone();
        m.member_entries = vec![[0x12; 32]];
        assert_ne!(group_generation_id(&m).unwrap(), base, "entries");
        let mut m = base_core.clone();
        m.minter = [0x13; 32];
        assert_ne!(group_generation_id(&m).unwrap(), base, "minter");
        let mut m = base_core.clone();
        m.key_commitment = [0x14; 32];
        assert_ne!(group_generation_id(&m).unwrap(), base, "commitment");
        let mut m = base_core.clone();
        m.minted_at_ms += 1;
        assert_ne!(group_generation_id(&m).unwrap(), base, "stamp");
        let mut m = base_core;
        m.revoked_past = vec![[0x15; 32]];
        assert_ne!(group_generation_id(&m).unwrap(), base, "revoked past");
    }

    /// **Line-committed admissibility's pin: a tip
    /// minted before a revocation resolves inadmissible at every reader with
    /// the line, and only a mint past it is the tip.** Before the revocation
    /// merges the earlier mint is the tip; after, it names a smaller set than
    /// the line and is nobody's (a skip, never a forgery); a mint by a live
    /// device past the revoked device supersedes it. A `revoked_past` that is
    /// not ascending and duplicate-free is a forged shape, counted invalid.
    #[test]
    fn a_tip_minted_before_a_revocation_resolves_inadmissible() {
        let e31 = entry_id_of(0x31);
        let rows = vec![enrolled_row(0x31)]; // enrolled by device 0x41
        // Device 0x42 also vouches for the entry, in its own cell, so the
        // roster survives 0x41's revocation.
        let live = device(0x42);
        let (_, by_live) = crate::group_scope::sign_roster_enrollment(
            &live,
            entry_core(0x31),
            vec![0x31; 8],
            cert_bytes(&authority(), live.verifying_key().to_bytes(), None),
            2_500,
        )
        .unwrap();
        let mut rows = rows;
        rows.push((
            crate::group_scope::roster_cell_key(&e31, &live.verifying_key().to_bytes()),
            canonical_encode(&by_live).unwrap(),
        ));
        let before = mint_row_with(vec![], &[e31], &[e31], 0x01, &authority(), 0x42);
        assert!(
            resolve(&roster(&rows), std::slice::from_ref(&before), &[])
                .tip
                .is_some(),
            "before the revocation merges, the earlier mint is the tip"
        );

        let aware = line(&[revocation_row(0x42, 0x41)]);
        let view = roster_under(&aware, &rows);
        assert_eq!(view.wrap_targets().count(), 1, "the live cell stands");
        let after = resolve_under(&aware, &view, std::slice::from_ref(&before), &[]);
        assert!(
            after.tip.is_none(),
            "minted before the revocation: nobody's tip"
        );
        assert!(
            after.invalid.is_empty(),
            "a skip, not a forgery: {:?}",
            after.invalid
        );

        let revoked = device(0x41).verifying_key().to_bytes();
        let before_id = crate::hex32::decode(&before.0).unwrap();
        let past = mint_row_past(
            vec![before_id],
            &[e31],
            &[e31],
            0x02,
            &authority(),
            0x42,
            vec![revoked],
        );
        let res = resolve_under(&aware, &view, &[before.clone(), past.clone()], &[]);
        assert_eq!(
            crate::hex32::encode(&res.tip.expect("the mint past the line").generation_id),
            past.0
        );
        // The set is monotone: a mint past MORE than the line revokes is
        // still admissible.
        let wider = mint_row_past(vec![before_id], &[e31], &[e31], 0x03, &authority(), 0x42, {
            let mut set = vec![revoked, [0xFF; 32]];
            set.sort();
            set
        });
        assert!(
            resolve_under(&aware, &view, std::slice::from_ref(&wider), &[])
                .tip
                .is_some()
        );
        // A revoked device may list itself; authorship under the line is the
        // conjunct that refuses it.
        let by_revoked = mint_row_past(
            vec![before_id],
            &[e31],
            &[e31],
            0x04,
            &authority(),
            0x41,
            vec![revoked],
        );
        let res = resolve_under(&aware, &view, std::slice::from_ref(&by_revoked), &[]);
        assert!(res.tip.is_none());
        assert!(res.invalid[0].1.contains("revoked authority device"));
        // A non-sorted or repeated set is a forged shape.
        for forged in [vec![[0xFF; 32], revoked], vec![revoked, revoked]] {
            let row = mint_row_past(vec![], &[e31], &[e31], 0x05, &authority(), 0x42, forged);
            let res = resolve_under(&aware, &view, std::slice::from_ref(&row), &[]);
            assert!(res.tip.is_none());
            assert!(
                res.invalid[0].1.contains("revoked-past"),
                "{:?}",
                res.invalid
            );
        }
    }

    /// A commitment (and an id) made in one plane must never verify in the
    /// other — the group-named contexts are what buy this.
    #[test]
    fn group_commitments_and_ids_are_domain_separated_from_r14() {
        let key = GenerationKey::from_bytes([0x5A; 32]);
        assert_ne!(group_generation_key_commitment(&key), key.commitment());

        let group_core = GroupMintCore {
            parents: vec![],
            member_entries: vec![[0x02; 32]],
            minter: [0x03; 32],
            key_commitment: [0x04; 32],
            minted_at_ms: 5_000,
            revoked_past: Vec::new(),
        };
        let r14_core = crate::generation::MintCore {
            parents: vec![],
            member_ids: vec![[0x02; 32]],
            minter: [0x03; 32],
            key_commitment: [0x04; 32],
            minted_at_ms: 5_000,
        };
        assert_ne!(
            group_generation_id(&group_core).unwrap(),
            crate::generation::generation_id(&r14_core).unwrap()
        );
    }

    // ── the lattices ────────────────────────────────────────────────────────

    #[test]
    fn the_mint_join_is_commutative_idempotent_and_shredded_absorbs() {
        let (_, minted) = mint_row(vec![], &[entry_id_of(0x31)], &[entry_id_of(0x31)], 0x01);
        let core = GroupMintCore {
            parents: vec![],
            member_entries: vec![entry_id_of(0x31)],
            minter: device(0x41).verifying_key().to_bytes(),
            key_commitment: [0x04; 32],
            minted_at_ms: 5_000,
            revoked_past: Vec::new(),
        };
        let shredded = canonical_encode(&GroupGenerationMintRecord::Shredded {
            core,
            shredded_at_ms: 9_000,
            shredded_by: [0x08; 32],
            authorization: Vec::new(),
            shredder_sig: Vec::new(),
        })
        .unwrap();
        assert_eq!(
            join_group_generation_mint(&minted, &minted).unwrap(),
            minted
        );
        assert_eq!(
            join_group_generation_mint(&minted, &shredded).unwrap(),
            join_group_generation_mint(&shredded, &minted).unwrap()
        );
        assert_eq!(
            join_group_generation_mint(&minted, &shredded).unwrap(),
            shredded
        );
    }

    /// The displacement-resistance property the per-healer cells buy: bytes
    /// that do not verify at the cell never displace a verifying row,
    /// whatever stamp they invent.
    #[test]
    fn a_forged_topup_never_displaces_the_healers_row() {
        let generation = [0x51; 32];
        let target = entry_id_of(0x31);
        let healer = member(0x32);
        let (_, honest) = topup_row(&generation, &target, 0x32);
        // A forged row at the same cell with a huge stamp — signed by the
        // WRONG actor, so it cannot verify under the cell's healer.
        let forged_sig = sign_group_topup_as_healer(
            member(0x33).signing_key(),
            &generation,
            &target,
            i64::MAX,
            &[0xFF; 16],
        );
        let forged = canonical_encode(&GroupTopupRecord::Wrap {
            generation_id: generation,
            target_entry: target,
            healer: healer.actor_id().0,
            at_ms: i64::MAX,
            wrap: vec![0xFF; 16],
            healer_sig: forged_sig,
        })
        .unwrap();
        let healer_id = healer.actor_id().0;
        assert_eq!(
            join_group_topup(&generation, &target, &healer_id, &honest, &forged).unwrap(),
            honest.as_slice()
        );
        assert_eq!(
            join_group_topup(&generation, &target, &healer_id, &forged, &honest).unwrap(),
            honest.as_slice()
        );
        // Decode-or-fail: bytes this build cannot read are never ranked — the
        // join fails in both orders and the walk skips the row unaccounted.
        for undecodable in [b"junk".to_vec(), crate::generation::tests::future_variant()] {
            assert!(
                join_group_topup(&generation, &target, &healer_id, &honest, &undecodable).is_err()
            );
            assert!(
                join_group_topup(&generation, &target, &healer_id, &undecodable, &honest).is_err()
            );
        }
    }

    #[test]
    fn a_satisfied_retraction_outranks_the_assertion_it_retires() {
        let generation = [0x51; 32];
        let target = member(0x31);
        let target_id = target.actor_id().0;
        let asserted = canonical_encode(&GroupUnkeyableRecord::Asserted {
            generation_id: generation,
            target_actor: target_id,
            asserted_at_ms: 1_000,
            tried: vec![[0xAA; 32]],
            target_sig: sign_group_unkeyable_as_target(
                target.signing_key(),
                &generation,
                crate::generation::UNKEYABLE_VARIANT_ASSERTED,
                1_000,
                &[[0xAA; 32]],
            ),
        })
        .unwrap();
        let satisfied = canonical_encode(&GroupUnkeyableRecord::Satisfied {
            generation_id: generation,
            target_actor: target_id,
            asserted_at_ms: 2_000,
            target_sig: sign_group_unkeyable_as_target(
                target.signing_key(),
                &generation,
                crate::generation::UNKEYABLE_VARIANT_SATISFIED,
                2_000,
                &[],
            ),
        })
        .unwrap();
        assert_eq!(
            join_group_unkeyable(&generation, &target_id, &asserted, &satisfied).unwrap(),
            satisfied.as_slice()
        );
        assert_eq!(
            join_group_unkeyable(&generation, &target_id, &satisfied, &asserted).unwrap(),
            satisfied.as_slice()
        );
        for undecodable in [b"junk".to_vec(), crate::generation::tests::future_variant()] {
            assert!(
                join_group_unkeyable(&generation, &target_id, &satisfied, &undecodable).is_err()
            );
            assert!(
                join_group_unkeyable(&generation, &target_id, &undecodable, &satisfied).is_err()
            );
        }
    }

    #[test]
    fn cell_keys_parse_canonical_or_nothing() {
        let g = [0x5A; 32];
        let t = [0x5B; 32];
        let h = [0x5C; 32];
        assert_eq!(
            parse_group_topup_cell_key(&group_topup_cell_key(&g, &t, &h)),
            Some((g, t, h))
        );
        assert_eq!(
            parse_group_unkeyable_cell_key(&group_unkeyable_cell_key(&g, &t)),
            Some((g, t))
        );
        // Wrong segment count (this plane has no legacy two-segment top-up).
        assert!(parse_group_topup_cell_key(&group_unkeyable_cell_key(&g, &t)).is_none());
        // Non-canonical spelling of the same cell is not a second cell.
        let upper = group_topup_cell_key(&g, &t, &h).to_uppercase();
        assert!(parse_group_topup_cell_key(&upper).is_none());
    }

    // ── the reception keypair ───────────────────────────────────────────────

    #[test]
    fn the_reception_keypair_is_deterministic_from_its_stored_ikm() {
        let record = GroupReceptionKeyRecord::mint(1_000);
        let a = record.keypair().unwrap();
        let b = record.keypair().unwrap();
        assert_eq!(a.public.to_bytes(), b.public.to_bytes());
        assert_eq!(record.logical_key().unwrap(), record.logical_key().unwrap());
        assert_eq!(
            record.logical_key().unwrap(),
            reception_pubkey_row_key(&record.reception_pubkey().unwrap())
        );
        // Two mints are two keypairs, two rows.
        let other = GroupReceptionKeyRecord::mint(1_000);
        assert_ne!(record.logical_key().unwrap(), other.logical_key().unwrap());
        // A corrupted row is a refusal, never a truncated derivation.
        let corrupt = GroupReceptionKeyRecord {
            ikm: crate::secret::SecretByteBuf::from(vec![1u8; 7]),
            minted_at_ms: 1_000,
        };
        assert!(corrupt.keypair().is_err());
    }

    #[test]
    fn a_published_reception_half_verifies_only_as_its_own_member() {
        let alice = member(0x31);
        let record = GroupReceptionKeyRecord::mint(1_000);
        let pubkey = record.reception_pubkey().unwrap();
        let carriage = sign_group_reception_published(&alice, pubkey.clone(), 2_000).unwrap();
        let opened =
            verify_group_reception_published(&carriage, &alice.actor_id()).expect("verifies");
        assert_eq!(opened.reception_pubkey, pubkey);
        assert_eq!(opened.rotated_at_ms, 2_000);
        // A valid record from another member never serves.
        assert!(
            verify_group_reception_published(&carriage, &member(0x32).actor_id())
                .unwrap_err()
                .contains("different member")
        );
        // Tampered bytes fail the envelope.
        let mut tampered = carriage.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        assert!(verify_group_reception_published(&tampered, &alice.actor_id()).is_err());
    }

    // ── the resolver ────────────────────────────────────────────────────────

    #[test]
    fn a_covered_authority_signed_mint_resolves_as_the_tip() {
        let rows = vec![enrolled_row(0x31), enrolled_row(0x32)];
        let view = roster(&rows);
        let entries = [entry_id_of(0x31), entry_id_of(0x32)];
        let mints = vec![mint_row(vec![], &entries, &entries, 0x01)];
        let res = resolve(&view, &mints, &[]);
        assert!(res.invalid.is_empty(), "unexpected: {:?}", res.invalid);
        let tip = res.tip.expect("tip resolves");
        assert_eq!(crate::hex32::encode(&tip.generation_id), mints[0].0);
        assert_eq!(res.leaf_ids, vec![tip.generation_id]);
    }

    #[test]
    fn a_row_squatting_another_key_is_invalid_never_a_candidate() {
        let rows = vec![enrolled_row(0x31)];
        let view = roster(&rows);
        let entries = [entry_id_of(0x31)];
        let (_, value) = mint_row(vec![], &entries, &entries, 0x01);
        let squatted = vec![(crate::hex32::encode(&[0xAA; 32]), value)];
        let res = resolve(&view, &squatted, &[]);
        assert!(res.tip.is_none());
        assert!(res.invalid[0].1.contains("content-derived id"));
    }

    #[test]
    fn a_minter_chained_to_a_foreign_root_is_invalid() {
        let rows = vec![enrolled_row(0x31)];
        let view = roster(&rows);
        let entries = [entry_id_of(0x31)];
        let mints = vec![mint_row_with(
            vec![],
            &entries,
            &entries,
            0x01,
            &foreign(),
            0x41,
        )];
        let res = resolve(&view, &mints, &[]);
        assert!(res.tip.is_none());
        assert!(
            res.invalid[0]
                .1
                .contains("neither the authority root nor a prior")
        );
    }

    #[test]
    fn a_minter_chained_to_a_prior_identity_still_verifies() {
        let rows = vec![enrolled_row(0x31)];
        let view = roster(&rows);
        let entries = [entry_id_of(0x31)];
        let mints = vec![mint_row_with(
            vec![],
            &entries,
            &entries,
            0x01,
            &prior(),
            0x41,
        )];
        let res = resolve(&view, &mints, &[]);
        assert!(res.invalid.is_empty(), "unexpected: {:?}", res.invalid);
        assert!(res.tip.is_some());
    }

    /// The cert names one device; a different device's signature over the
    /// mint does not ride it.
    #[test]
    fn the_mint_signature_must_be_the_authorized_devices() {
        let rows = vec![enrolled_row(0x31)];
        let view = roster(&rows);
        let entries = [entry_id_of(0x31)];
        let minter = device(0x41);
        let core = GroupMintCore {
            parents: vec![],
            member_entries: entries.to_vec(),
            minter: minter.verifying_key().to_bytes(),
            key_commitment: [0x04; 32],
            minted_at_ms: 5_000,
            revoked_past: Vec::new(),
        };
        let id = group_generation_id(&core).unwrap();
        let value = canonical_encode(&GroupGenerationMintRecord::Minted {
            core,
            // Signed by a DIFFERENT device than the core names.
            minter_sig: sign_group_mint_as_minter(&device(0x42), &id),
            authorization: cert_bytes(&authority(), minter.verifying_key().to_bytes(), None),
            wraps: entries
                .iter()
                .map(|e| GroupMemberWrap {
                    entry_id: *e,
                    wrap: vec![0xEE; 16],
                })
                .collect(),
        })
        .unwrap();
        let res = resolve(&view, &[(crate::hex32::encode(&id), value)], &[]);
        assert!(res.tip.is_none());
        assert!(res.invalid[0].1.contains("does not verify"));
    }

    /// Admissibility arm 1 — the severance-forcing arm: a mint listing a
    /// removed entry is inadmissible at every observer that merged the
    /// removal (a skip, never `invalid` — it was honest when minted).
    #[test]
    fn a_mint_listing_a_removed_entry_is_inadmissible() {
        let e31 = entry_id_of(0x31);
        let rows = vec![enrolled_row(0x31), enrolled_row(0x32)];
        let entries = [e31, entry_id_of(0x32)];
        let mints = vec![mint_row(vec![], &entries, &entries, 0x01)];
        // Before the removal merges: admissible.
        assert!(resolve(&roster(&rows), &mints, &[]).tip.is_some());
        // After: the same mint is nobody's tip — the severance mint is due.
        let mut removed = rows;
        removed.push(removed_row(&e31));
        let res = resolve(&roster(&removed), &mints, &[]);
        assert!(res.tip.is_none());
        assert!(
            res.invalid.is_empty(),
            "a skip, not a forgery: {:?}",
            res.invalid
        );
    }

    /// Admissibility arm 2 — the late-add arm: a mint that predates an add
    /// is uncovered (no inline wrap for the new entry) until a verifying
    /// member top-up covers it. There is no escrow arm on this plane.
    #[test]
    fn an_uncovered_add_gates_the_tip_until_a_member_topup_covers_it() {
        let e31 = entry_id_of(0x31);
        let e32 = entry_id_of(0x32);
        let rows = vec![enrolled_row(0x31), enrolled_row(0x32)];
        let view = roster(&rows);
        // The mint predates 0x32's admission: lists and wraps only 0x31.
        let mints = vec![mint_row(vec![], &[e31], &[e31], 0x01)];
        assert!(resolve(&view, &mints, &[]).tip.is_none());
        // A top-up for the new entry by an enrolled member restores coverage.
        let generation = crate::hex32::decode(&mints[0].0).unwrap();
        let topups = vec![topup_row(&generation, &e32, 0x31)];
        assert!(resolve(&view, &mints, &topups).tip.is_some());
    }

    /// A top-up authored by a non-member (or an actor never enrolled) is not
    /// coverage, however well it verifies at its cell.
    #[test]
    fn a_non_member_topup_is_not_coverage() {
        let e31 = entry_id_of(0x31);
        let e32 = entry_id_of(0x32);
        let view = roster(&[enrolled_row(0x31), enrolled_row(0x32)]);
        let mints = vec![mint_row(vec![], &[e31], &[e31], 0x01)];
        let generation = crate::hex32::decode(&mints[0].0).unwrap();
        // 0x66 is nobody: its row verifies at its cell but covers nothing.
        let topups = vec![topup_row(&generation, &e32, 0x66)];
        let res = resolve(&view, &mints, &topups);
        assert!(res.tip.is_none());
        assert!(res.invalid.is_empty(), "not a forgery: {:?}", res.invalid);
    }

    /// The supersession transplant: a candidate descendant retires its
    /// ancestor — across a shredded intermediate — while an
    /// observer-unkeyable descendant retires nothing (the wedge argument).
    #[test]
    fn supersession_crosses_shredded_rows_and_unkeyable_mints_retire_nothing() {
        let e31 = entry_id_of(0x31);
        let rows = vec![enrolled_row(0x31)];
        let view = roster(&rows);
        let (a_key, a_val) = mint_row(vec![], &[e31], &[e31], 0x01);
        let a_id: [u8; 32] = crate::hex32::decode(&a_key).unwrap();
        // A shredded middle generation whose edges must still count.
        let b_core = GroupMintCore {
            parents: vec![a_id],
            member_entries: vec![e31],
            minter: device(0x41).verifying_key().to_bytes(),
            key_commitment: [0x0B; 32],
            minted_at_ms: 6_000,
            revoked_past: Vec::new(),
        };
        let b_id = group_generation_id(&b_core).unwrap();
        let b_val = canonical_encode(&GroupGenerationMintRecord::Shredded {
            core: b_core,
            shredded_at_ms: 9_000,
            shredded_by: [0x08; 32],
            authorization: Vec::new(),
            shredder_sig: Vec::new(),
        })
        .unwrap();
        let (c_key, c_val) = mint_row(vec![b_id], &[e31], &[e31], 0x03);
        let mints = vec![
            (a_key.clone(), a_val.clone()),
            (crate::hex32::encode(&b_id), b_val),
            (c_key.clone(), c_val.clone()),
        ];
        let res = resolve(&view, &mints, &[]);
        assert_eq!(crate::hex32::encode(&res.tip.unwrap().generation_id), c_key);

        // Same DAG, but the descendant is unkeyable at this observer: it
        // must neither win nor retire A.
        let c_id: [u8; 32] = crate::hex32::decode(&c_key).unwrap();
        let res = resolve_admissible_group_tip(
            &view,
            &line(&[]),
            mints.iter().map(|(k, v)| (k.as_str(), v.as_slice())),
            std::iter::empty(),
            |id, _, _| *id != c_id,
        );
        assert_eq!(crate::hex32::encode(&res.tip.unwrap().generation_id), a_key);
        assert_eq!(res.unkeyable, vec![c_key]);
    }

    // ── authority-device revocation ───────────────────────────────

    /// **The pin at the mint resolver.** Device 0x41 was removed from the
    /// authority's own account; it keeps its secret, its root-signed cert and
    /// the never-rotated machinery root, so its mint is chain-valid and wins
    /// at a reader that never learned. A reader that has learned refuses it
    /// — and the live sibling's mint over the live sibling's roster is the
    /// tip, which is the re-publication the ruling prescribes.
    #[test]
    fn a_mint_by_a_revoked_authority_device_is_never_a_candidate() {
        let e31 = entry_id_of(0x31);
        let rows = vec![enrolled_row(0x31)]; // enrolled by device 0x41
        let (rogue_key, rogue_val) = mint_row(vec![], &[e31], &[e31], 0x01); // minted by 0x41

        let unaware = resolve(
            &roster(&rows),
            &[(rogue_key.clone(), rogue_val.clone())],
            &[],
        );
        assert!(unaware.tip.is_some(), "the gap, before learning");

        let aware = line(&[revocation_row(0x42, 0x41)]);
        assert!(
            aware.invalid().is_empty(),
            "unexpected: {:?}",
            aware.invalid()
        );
        // The revoked device's roster cell is dead too, so the live device
        // re-publishes the SAME entry under its own cell, as the per-writer
        // cell ruling says.
        let live = device(0x42);
        let (fresh_id, fresh) = crate::group_scope::sign_roster_enrollment(
            &live,
            entry_core(0x31),
            vec![0x31; 8],
            cert_bytes(&authority(), live.verifying_key().to_bytes(), None),
            9_000,
        )
        .unwrap();
        assert_eq!(fresh_id, e31, "the same entry id");
        let mut rows = rows;
        rows.push((
            crate::group_scope::roster_cell_key(&fresh_id, &live.verifying_key().to_bytes()),
            canonical_encode(&fresh).unwrap(),
        ));
        let view = roster_under(&aware, &rows);
        assert_eq!(
            view.wrap_targets().count(),
            1,
            "only the live device's cell enrols"
        );

        let past = vec![device(0x41).verifying_key().to_bytes()];
        let rogue_again = mint_row_past(
            vec![],
            &[fresh_id],
            &[fresh_id],
            0x02,
            &authority(),
            0x41, // 0x41 again, listing itself
            past.clone(),
        );
        let honest = mint_row_past(
            vec![],
            &[fresh_id],
            &[fresh_id],
            0x03,
            &authority(),
            0x42,
            past,
        );
        let res = resolve_under(
            &aware,
            &view,
            &[(rogue_key, rogue_val), rogue_again, honest.clone()],
            &[],
        );
        assert_eq!(
            crate::hex32::encode(&res.tip.expect("the live mint").generation_id),
            honest.0
        );
        assert_eq!(
            res.invalid
                .iter()
                .filter(|(_, why)| why.contains("revoked authority device"))
                .count(),
            2,
            "both rogue mints are refused for their author, before any roster arm runs"
        );
    }

    /// The shred ruling: an unauthored `Shredded` still drops candidacy
    /// (availability — nothing new to a root holder), but it is never a
    /// licence to drop a key. Only a live authority device's signed shred is.
    #[test]
    fn only_an_authored_shred_by_a_live_authority_device_may_drop_a_key() {
        let e31 = entry_id_of(0x31);
        let shredder = device(0x41);
        let core = GroupMintCore {
            parents: vec![],
            member_entries: vec![e31],
            minter: shredder.verifying_key().to_bytes(),
            key_commitment: [0x04; 32],
            minted_at_ms: 5_000,
            revoked_past: Vec::new(),
        };
        let id = group_generation_id(&core).unwrap();
        let cert = cert_bytes(&authority(), shredder.verifying_key().to_bytes(), None);

        // What any machinery-root holder — a FORMER MEMBER included — can write.
        let unauthored = GroupGenerationMintRecord::Shredded {
            core: core.clone(),
            shredded_at_ms: 9_000,
            shredded_by: shredder.verifying_key().to_bytes(),
            authorization: Vec::new(),
            shredder_sig: Vec::new(),
        };
        assert!(
            unauthored
                .shred_is_authored(&line(&[]))
                .unwrap_err()
                .contains("unauthored")
        );
        // ...even carrying the authority device's public cert verbatim.
        let cert_only = GroupGenerationMintRecord::Shredded {
            core: core.clone(),
            shredded_at_ms: 9_000,
            shredded_by: shredder.verifying_key().to_bytes(),
            authorization: cert.clone(),
            shredder_sig: vec![0u8; 64],
        };
        assert!(
            cert_only
                .shred_is_authored(&line(&[]))
                .unwrap_err()
                .contains("shred signature")
        );

        let authored = sign_group_shred(&shredder, core, cert, 9_000).unwrap();
        assert_eq!(authored.shred_is_authored(&line(&[])), Ok(id));
        // A revoked authority device shreds nothing.
        assert!(
            authored
                .shred_is_authored(&line(&[revocation_row(0x42, 0x41)]))
                .unwrap_err()
                .contains("revoked authority device")
        );

        // Compat: the unauthored shape encodes exactly as before the ruling.
        let bytes = canonical_encode(&unauthored).unwrap();
        assert!(!bytes.windows(12).any(|w| w == b"shredder_sig"));
        assert!(!bytes.windows(13).any(|w| w == b"authorization"));
    }

    #[test]
    fn concurrent_candidates_resolve_to_the_byte_order_max() {
        let e31 = entry_id_of(0x31);
        let view = roster(&[enrolled_row(0x31)]);
        let (a_key, a_val) = mint_row(vec![], &[e31], &[e31], 0x01);
        let (b_key, b_val) = mint_row(vec![], &[e31], &[e31], 0x02);
        let mints = vec![(a_key.clone(), a_val), (b_key.clone(), b_val)];
        let res = resolve(&view, &mints, &[]);
        let winner = crate::hex32::encode(&res.tip.unwrap().generation_id);
        assert_eq!(winner, std::cmp::max(a_key.clone(), b_key.clone()));
        assert_eq!(res.leaf_ids.len(), 2);
    }

    #[test]
    fn an_expired_minter_authorization_is_invalid() {
        let e31 = entry_id_of(0x31);
        let view = roster(&[enrolled_row(0x31)]);
        let minter = device(0x41);
        let core = GroupMintCore {
            parents: vec![],
            member_entries: vec![e31],
            minter: minter.verifying_key().to_bytes(),
            key_commitment: [0x04; 32],
            minted_at_ms: 5_000,
            revoked_past: Vec::new(),
        };
        let id = group_generation_id(&core).unwrap();
        let value = canonical_encode(&GroupGenerationMintRecord::Minted {
            core,
            minter_sig: sign_group_mint_as_minter(&minter, &id),
            // Expires at second 1 = ms 1_000, before the mint's 5_000 stamp.
            authorization: cert_bytes(&authority(), minter.verifying_key().to_bytes(), Some(1)),
            wraps: vec![GroupMemberWrap {
                entry_id: e31,
                wrap: vec![0xEE; 16],
            }],
        })
        .unwrap();
        let res = resolve(&view, &[(crate::hex32::encode(&id), value)], &[]);
        assert!(res.tip.is_none());
        assert!(res.invalid[0].1.contains("expired"));
    }
}
