//! Engine-side R14 (account-data-plane.md § The ratified decisions) **tip resolution + the writer's own generation key** —
//! build step 6's store seam (`account-data-plane.md` § The generation
//! machinery → *The sealing-epoch axis, and the gate's real shape*).
//!
//! [`fauna_core::generation::resolve_admissible_tip`] is the pure view; this
//! module is what feeds it **merged plane rows** from an [`AccountStore`] and
//! what turns a resolved tip into sealing capability for this device: find our
//! wrap (inline on the mint, else the top-up kind), open it under the device
//! KEM secret derived from the plane's own writer key, verify the key
//! commitment, and hand back the [`GenerationKey`] the per-generation schedule
//! derives from ([`fauna_core::crypto::FleetOnlySchedule::derive_for_generation`]).
//!
//! # Admissibility gates sealing, never reading
//!
//! [`resolve_tip`] (trust-consulting, [`GenerationTrust`]) answers the writer
//! door's question: *which tip do I seal under, if any*. [`generation_key_for`]
//! (integrity-only) answers the reader's: *can I key this row's named
//! generation*. The reader deliberately consults no [`FleetView`] and no
//! receipts — historical generations legitimately name devices that are
//! removed **now**, and a fresh device bootstrapping from the feed must read
//! rows sealed under every retained generation ("readers walk every retained
//! generation, so nothing written in the window is lost"). What reading *does*
//! verify is integrity: the mint row's logical key must be the content-derived
//! id of its own core (a squatted or forged row keys nothing), and every
//! unwrap re-computes the key commitment against that core
//! (`fauna_mls::wrapped_blob::generation_wraps` refuses the mismatch).

use anyhow::{Result, bail};
use ed25519_dalek::SigningKey;
use fauna_account_store::{backend::StoreBackend, store::AccountStore, types::StateEntry};

use crate::generation_store::{live_rows, row_ref};
use fauna_core::crypto::GenerationKey;
use fauna_core::generation::{
    AdmissibleTip, FleetView, GenerationMintRecord, MemberWrap, MintCore, TipResolution,
    closed_generations, derive_device_xwing_keypair, escrow_target_identity_key,
    resolve_admissible_tip,
};
use fauna_core::identity::ActorId;
use fauna_mls::wrapped_blob::generation_wraps::open_generation_key_as_device;
use fauna_protocol::merge_policy::{
    KIND_DEVICE_SET, KIND_ESCROW_RECEIPT, KIND_GENERATION_CLOSED, KIND_GENERATION_MINT,
    KIND_GENERATION_WRAP,
};

/// The bundle's custody face for **retained generation keys** (T10 — the
/// `<hex>/generation-keys` slot attribute; implemented by
/// `principal_bundle::PrincipalSlot` under the `account-runtime` feature; a
/// trait so this module never grows the credential-store graph).
///
/// Three duties, all W5 (account-data-plane.md § Workstreams).4a's carriage contract:
///
/// - **Record after every obtain.** A key unwrapped from the plane or minted
///   here rides the bundle from then on — bridging the windows where the
///   plane cannot answer (a store re-syncing from scratch, a sealed row
///   walked before its writer's mint row, a top-up that raced enrollment).
/// - **Consult where the plane answers "no key" for a live mint.** Entries
///   were commitment-verified when recorded and the AEAD tag stays the
///   arbiter at every open; still, wherever a live, id-bound mint row is in
///   hand its commitment is re-checked, and a hit that fails it is never
///   served (read and seal half alike). The consult is deliberately **not**
///   blind-cache-first on the read path: the mint row must be looked at
///   first, because of the duty below.
/// - **Drop on shred.** "Deleting a generation = devices drop it + escrow
///   holders delete the wrap" (charter § The account data plane, the
///   generation-axis ruling) — real crypto-shredding at generation
///   granularity. A retained key that survived its generation's shred would
///   quietly defeat that deletion on every machine whose slot carries it, so
///   any observation of a `Shredded` mint drops the entry. (The shred
///   *calling surface* — user-gated, not yet built — owes the same drop on
///   the device that originates the shred.)
pub trait RetainedKeyCustody: Send + Sync {
    /// The retained key for `generation`, when one rode the bundle.
    fn retained_generation_key(&self, generation: &[u8; 32]) -> Option<GenerationKey>;
    /// Record a key this device just obtained (verified by its caller).
    fn record_generation_key(&self, generation: &[u8; 32], key: &GenerationKey);
    /// Drop a shredded generation's key — the crypto-shred contract's
    /// device-side half. Idempotent; dropping an absent entry is a no-op.
    fn drop_generation_key(&self, generation: &[u8; 32]);
}

/// The account identity line + this account's escrow-holder trust — everything
/// the writer door's tip resolution needs beyond the store itself.
///
/// The same three facts [`crate::generation_mint::MintContext`] carries for a
/// mint, minus the minter id (a plane already knows its own writer). `root`
/// alone anchors the [`FleetView`]'s cert verification; `prior` rides along
/// for the group authority view only; `trusted_holders` is
/// the *trust* half of the holder-generic receipt contract
/// (`fauna_core::generation::verify_escrow_receipt` checks integrity
/// holder-generically, and membership here decides whose receipts count —
/// v1: the nest deployment identity the client already pins).
#[derive(Debug, Clone)]
pub struct GenerationTrust {
    /// The account's current identity — enrollment certs verify against this.
    pub root: ActorId,
    /// Succeeded-from identities — the device's **attested** predecessor set
    /// (`AccountRuntimeParams::attested_predecessors`), never a writer-asserted
    /// `prior_actor_ids` list.
    ///
    /// Read by the group authority view alone
    /// (`group_authority_revocation::severance_work` →
    /// `GroupAuthority::build`). It signs nothing in the [`FleetView`]: the
    /// device set does not cross a succession, so an enrollment cert verifies
    /// against `root` only (`account-data-taxonomy.md` § The generation
    /// machinery → *The source of `prior`*, ruled 2026-10-01).
    pub prior: Vec<ActorId>,
    /// Holder identities whose escrow receipts this account accepts.
    ///
    /// Empty is honest and fail-safe: no receipt is ever trusted, so no tip
    /// ever resolves and `GenerationTip` sealing stays refused with the
    /// precise no-tip error — exactly the R14 gate, per replica.
    ///
    /// A shared cell, not a value: the set follows the pin, read at the start
    /// of every pass (`account-data-taxonomy.md` § The generation machinery →
    /// *A holder change re-receipts and never mints*, (1)), and every plane
    /// of the assembly borrows this one trust.
    pub trusted_holders: TrustedHolders,
}

/// [`GenerationTrust::trusted_holders`] — the holder set, replaceable under the
/// planes that borrow it, so a rotation the app has accepted moves it at the
/// next pass with no reassembly (a frozen set refuses every successor-signed
/// receipt until the runtime is rebuilt). Clones share the cell.
#[derive(Debug, Clone, Default)]
pub struct TrustedHolders(std::sync::Arc<std::sync::RwLock<Vec<[u8; 32]>>>);

impl TrustedHolders {
    /// A cell holding `holders`.
    #[must_use]
    pub fn new(holders: Vec<[u8; 32]>) -> Self {
        Self(std::sync::Arc::new(std::sync::RwLock::new(holders)))
    }

    /// The set as it stands now — what one step reads for its whole run.
    #[must_use]
    pub fn get(&self) -> Vec<[u8; 32]> {
        self.0.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Whether `holder` is trusted now.
    #[must_use]
    pub fn contains(&self, holder: &[u8; 32]) -> bool {
        self.0
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .contains(holder)
    }

    /// Whether no holder is trusted now.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.read().unwrap_or_else(|p| p.into_inner()).is_empty()
    }

    /// Replace the set; `true` when it changed.
    pub fn replace(&self, holders: Vec<[u8; 32]>) -> bool {
        let mut cell = self.0.write().unwrap_or_else(|p| p.into_inner());
        if *cell == holders {
            return false;
        }
        *cell = holders;
        true
    }
}

impl From<Vec<[u8; 32]>> for TrustedHolders {
    fn from(holders: Vec<[u8; 32]>) -> Self {
        Self::new(holders)
    }
}

/// Resolve this device's current candidate generation tip from this store's
/// **merged plane rows** — the writer door's question, and therefore
/// trust-consulting. Pure over the store's current state and this device's
/// own key material: no clock, no network; escrow status is the merged
/// `fauna.state.escrow-receipt` rows, never a live holder query, so the
/// answer is the same offline and in the no-nest profile (charter § The
/// generation machinery, the escrow-receipt kind's contract).
///
/// `writer_key` feeds the ST-007 **observer-keyability** clause: a mint whose
/// key this device cannot actually obtain — no inline wrap, no merged top-up,
/// or one that fails to open/verify — is no candidate here, so it neither
/// wins nor retires the tip this device seals under. Keyability is what the
/// plane distributes to this device **plus its own retained bundle**
/// (`custody`, since the fleet-scope reclamation ruling of 2026-09-16): a
/// consumed top-up cell is retired from the feed once the target's reach
/// says it holds the key, so the bundle — which only ever holds a key that
/// opened against the mint's commitment, and is re-checked against it here —
/// is the one source left for a spilled member's candidacy. Before that
/// ruling the check was deliberately plane-native (the charter's resolver
/// rulings record why that was then the safer reading, and why it no longer
/// holds once cells are reclaimed).
///
/// The live `fauna.state.generation-closed` rows are the resolver's `closed`
/// set (ruling (4)): a generation a remover closed is no candidate here, so
/// the door's next tip-sealed write finds none and mints.
pub async fn resolve_tip<B: StoreBackend>(
    store: &AccountStore<B>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
    custody: Option<&dyn RetainedKeyCustody>,
) -> Result<TipResolution> {
    let device_rows = live_rows(store, KIND_DEVICE_SET).await?;
    let view = FleetView::build(&trust.root, device_rows.iter().map(row_ref));
    let mint_rows = live_rows(store, KIND_GENERATION_MINT).await?;
    let receipt_rows = live_rows(store, KIND_ESCROW_RECEIPT).await?;
    let closed_rows = live_rows(store, KIND_GENERATION_CLOSED).await?;
    let device_id = writer_key.verifying_key().to_bytes();
    let device_kem = derive_device_xwing_keypair(&writer_key.to_bytes());
    // The merged top-up rows targeting THIS device, prefetched once so the
    // keyability check inside the pure resolver stays synchronous.
    let topups = own_topup_wraps(store, &device_id).await?;
    let keyable = |id: &[u8; 32], core: &MintCore, wraps: &[MemberWrap]| {
        // The retained bundle first — a hash compare against the mint's
        // commitment, no KEM — then inline, then every merged top-up:
        // fall-through, not either/or: a wrap that
        // refuses (tampered, substituted, garbage targeting us) keys nothing,
        // but it must not mask a sibling wrap that opens — every candidate is
        // commitment-checked, so trying all of them is pure gain.
        if custody.is_some_and(|c| {
            c.retained_generation_key(id)
                .is_some_and(|key| key.commitment() == core.key_commitment)
        }) {
            return true;
        }
        let inline = wraps
            .iter()
            .filter(|w| w.device_id == device_id)
            .map(|w| w.wrap.as_slice());
        let topup = topups.get(id).into_iter().flatten().map(Vec::as_slice);
        inline.chain(topup).any(|wrap_bytes| {
            open_generation_key_as_device(
                wrap_bytes,
                &device_kem.secret,
                id,
                &device_id,
                &core.key_commitment,
            )
            .is_ok()
        })
    };
    Ok(resolve_admissible_tip(
        &view,
        mint_rows.iter().map(row_ref),
        receipt_rows.iter().map(row_ref),
        &closed_generations(closed_rows.iter().map(row_ref)),
        &trust.trusted_holders.get(),
        // Only receipts naming THIS identity's target ack a generation here
        // — a predecessor's receipt vouches for a wrap the succession burned.
        &escrow_target_identity_key(&trust.root),
        keyable,
    ))
}

/// Every usable merged `fauna.state.generation-wrap` wrap targeting `device`,
/// grouped by generation id — the per-healer cells,
/// one per healer. The
/// same per-row validation as [`topup_wraps`] (self-inconsistent or
/// undecodable rows serve nobody), collected in one scan for the resolver's
/// synchronous keyability check. Healer signatures are deliberately NOT
/// consulted on the read side: the open below is checked against the mint's
/// key commitment, which is the ground truth a signature could only
/// approximate.
/// Which device PRINCIPALS hold the generation key at this observer's
/// resolved tip — the winning mint's inline `MemberWrap` targets plus every
/// echo-consistent generation-wrap row for that generation (any target; the
/// same single validation site the resolver's own keyability scan uses).
/// The keyless-posture badge's derivation: posture is bundle key reach,
/// derived, never stored or asked (`ui/devices.md` § Custody facet piece 1).
///
/// `Ok(None)` when no tip resolves for this observer (no trusted escrow
/// holder, an empty plane): the caller renders NOTHING — a badge claiming
/// "holds no keys" must never rest on an unresolved world (the escrow-holder
/// badge's no-pin arm, fail-safe by construction).
pub async fn keyed_principals_at_tip<B: StoreBackend>(
    store: &AccountStore<B>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
    custody: Option<&dyn RetainedKeyCustody>,
) -> Result<Option<std::collections::BTreeSet<[u8; 32]>>> {
    let resolution = resolve_tip(store, trust, writer_key, custody).await?;
    let Some(tip) = resolution.tip else {
        return Ok(None);
    };
    let mut keyed: std::collections::BTreeSet<[u8; 32]> =
        tip.wraps.iter().map(|w| w.device_id).collect();
    for entry in live_rows(store, KIND_GENERATION_WRAP).await? {
        // Parse the cell for its target, then run the shared validation for
        // exactly that (generation, target) — reusing `usable_wrap_for`'s one
        // site rather than a second, drift-prone row check.
        let Some(cell) = fauna_core::generation::parse_wrap_cell_key(&entry.key) else {
            continue;
        };
        let target = cell.target_device;
        if usable_wrap_for(&entry, Some(&tip.generation_id), &target).is_some() {
            keyed.insert(target);
        }
    }
    Ok(Some(keyed))
}

async fn own_topup_wraps<B: StoreBackend>(
    store: &AccountStore<B>,
    device_id: &[u8; 32],
) -> Result<std::collections::BTreeMap<[u8; 32], Vec<Vec<u8>>>> {
    let mut out: std::collections::BTreeMap<[u8; 32], Vec<Vec<u8>>> =
        std::collections::BTreeMap::new();
    for entry in live_rows(store, KIND_GENERATION_WRAP).await? {
        if let Some((generation_id, wrap)) = usable_wrap_for(&entry, None, device_id) {
            out.entry(generation_id).or_default().push(wrap);
        }
    }
    Ok(out)
}

/// One row's usable wrap for `(generation, device)` — or `None` for a row
/// that is another cell's, malformed, self-inconsistent, or not this shape.
/// `generation_id: None` accepts any generation (the resolver's whole-kind
/// scan); `Some` filters to one (the read path's per-generation question).
/// Single validation site, shared by both consumers.
fn usable_wrap_for(
    entry: &StateEntry,
    generation_id: Option<&[u8; 32]>,
    device_id: &[u8; 32],
) -> Option<([u8; 32], Vec<u8>)> {
    use fauna_core::generation::{GenerationWrapRecordV2, WrapCellKey, parse_wrap_cell_key};
    let cell = parse_wrap_cell_key(&entry.key)?;
    let WrapCellKey {
        generation_id: cell_generation,
        target_device: cell_target,
        healer,
    } = cell;
    if cell_target != *device_id {
        return None;
    }
    if let Some(wanted) = generation_id
        && cell_generation != *wanted
    {
        return None;
    }
    // The value echoes the logical key so it is self-describing; a mismatch
    // is a malformed writer and the row serves nobody.
    let record: GenerationWrapRecordV2 =
        fauna_core::encoding::canonical_decode(&entry.value).ok()?;
    let GenerationWrapRecordV2::Wrap {
        generation_id: rec_generation,
        target_device: rec_target,
        healer: rec_healer,
        wrap,
        ..
    } = record;
    (rec_generation == cell_generation && rec_target == cell_target && rec_healer == healer)
        .then_some((cell_generation, wrap))
}

/// This device's key for the **resolved tip** — the seal half of step 6.
///
/// Wrap sources, in the charter's order: the tip's own inline member wrap
/// ("its wrap is inline when it was in the mint's member set"), else the
/// top-up kind (`fauna.state.generation-wrap`, the self-healing path for the
/// mint that raced this device's enrollment). No wrap at all is a refusal
/// that names the fix — a top-up from any key-holding device — rather than a
/// silent fallback to gen-0 keys, which is exactly what R14 forbids.
pub async fn key_for_tip<B: StoreBackend>(
    store: &AccountStore<B>,
    tip: &AdmissibleTip,
    writer_key: &SigningKey,
    custody: Option<&dyn RetainedKeyCustody>,
) -> Result<GenerationKey> {
    // The retained bundle first (T10 carriage). Safe cache-first HERE, unlike
    // the read path: a resolved tip is a live `Minted` row by construction
    // (a shredded mint has no wraps, so it is never keyable, never a
    // candidate), and the hit is re-checked against the tip's own commitment
    // — sealing under a key the mint did not commit to would write rows
    // every sibling refuses.
    if let Some(custody) = custody
        && let Some(key) = custody.retained_generation_key(&tip.generation_id)
    {
        if key.commitment() == tip.core.key_commitment {
            return Ok(key);
        }
        tracing::warn!(
            generation = %fauna_core::hex32::encode(&tip.generation_id),
            "a retained generation key fails the resolved tip's commitment — \
             ignoring it and falling back to the plane's wraps"
        );
    }
    match open_with_wraps(store, &tip.generation_id, &tip.core, &tip.wraps, writer_key).await? {
        WrapOpen::Key(key) => {
            if let Some(custody) = custody {
                custody.record_generation_key(&tip.generation_id, &key);
            }
            Ok(key)
        }
        WrapOpen::NoWrap => bail!(
            "no generation wrap reaches this device for the resolved tip {} — neither inline \
             on the mint nor as a `{KIND_GENERATION_WRAP}` top-up row; any device holding the \
             key can write one (charter § The generation machinery, the top-up kind)",
            fauna_core::hex32::encode(&tip.generation_id)
        ),
        WrapOpen::Refused(reason) => bail!(
            "the generation wrap for this device fails to open (tip {}): {reason}",
            fauna_core::hex32::encode(&tip.generation_id)
        ),
    }
}

/// This device's key for an **arbitrary named generation** — the read half:
/// integrity-only, deliberately no admissibility (module docs). `Ok(None)`
/// means "this generation keys nothing here": no mint row, a shredded or
/// forged one, no wrap reaching this device, or a wrap that refuses to open —
/// every one of those is attacker-suppliable row content, so the caller skips
/// the row (a warn line is the witness for the refusal cases) and a later
/// reconcile re-presents it; `Err` is a store failure only.
pub async fn generation_key_for<B: StoreBackend>(
    store: &AccountStore<B>,
    generation_id: &[u8; 32],
    writer_key: &SigningKey,
    custody: Option<&dyn RetainedKeyCustody>,
) -> Result<Option<GenerationKey>> {
    // The retained bundle (T10 carriage) answers wherever the *plane* cannot
    // — deliberately consulted only after the mint row has been looked at,
    // never blind-cache-first, so a `Shredded` mint reaches the drop below
    // instead of being served from custody forever
    // ([`RetainedKeyCustody`] owns the three duties).
    let consult = || custody.and_then(|c| c.retained_generation_key(generation_id));
    let key_hex = fauna_core::hex32::encode(generation_id);
    let Some(entry) = store.state(KIND_GENERATION_MINT, &key_hex).await? else {
        // No mint row merged (yet): another writer's sealed row can arrive
        // ahead of the minter's own rows — per-writer frontiers interleave —
        // and the bundle bridges exactly that window.
        return Ok(consult());
    };
    if entry.tombstone {
        // Anomalous — the mint lattice's only transition is to `Shredded`,
        // never a tombstone (charter § The generation machinery). Treated
        // like an absent row.
        return Ok(consult());
    }
    let record: GenerationMintRecord = match fauna_core::encoding::canonical_decode(&entry.value) {
        Ok(r) => r,
        // A mint row that does not decode keys nothing; the row itself is the
        // resolver's `invalid` business, not the reader's. A bundle key
        // recorded from the real mint still answers.
        Err(_) => return Ok(consult()),
    };
    // `minter_sig` deliberately unverified here: reading is integrity-only
    // (key↔id + commitment at the unwrap) — authorship gates *candidacy*, and
    // refusing to read rows the fleet historically sealed would lose data.
    let GenerationMintRecord::Minted {
        core,
        minter_sig: _,
        wraps,
    } = record
    else {
        // Shredded: the crypto-shred contract — the wraps are gone from the
        // plane by design, AND the device-side half is to drop the retained
        // key, or every slot that carried it would quietly defeat the
        // deletion ([`RetainedKeyCustody`], duty three).
        if let Some(custody) = custody {
            custody.drop_generation_key(generation_id);
        }
        return Ok(None);
    };
    // The Key↔id binding, read back exactly as the resolver reads it: a row
    // squatting a foreign key (or a forged id over someone's core) keys
    // nothing — but a bundle key recorded from the real mint of this id
    // still answers (the id is content-derived, so the squatter cannot have
    // been its source).
    if fauna_core::generation::generation_id(&core).ok() != Some(*generation_id) {
        return Ok(consult());
    }
    // The row has been looked at (live, id-bound): a bundle key that matches
    // its commitment answers without a KEM open — the same order
    // `key_for_tip` uses, and what keeps every per-pass "can I key this"
    // question (reach, the unkeyable pass) a hash compare at steady state.
    // Past this compare the bundle has nothing left to offer: its key is
    // KNOWN to fail the commitment of this live, id-bound mint, so it is
    // never served — the read half refuses exactly what `key_for_tip`
    // refuses (a key that slipped the escrow door's commitment check would
    // otherwise reach every reader of this generation).
    if let Some(key) = consult() {
        if key.commitment() == core.key_commitment {
            return Ok(Some(key));
        }
        tracing::warn!(
            generation = %key_hex,
            "a retained generation key fails its live mint's commitment — not serving it"
        );
    }
    match open_with_wraps(store, generation_id, &core, &wraps, writer_key).await? {
        WrapOpen::Key(key) => {
            if let Some(custody) = custody {
                custody.record_generation_key(generation_id, &key);
            }
            Ok(Some(key))
        }
        // No wrap reaches this device (a mint that raced our enrollment,
        // pre-top-up), and a bundle key that rode along already answered
        // above.
        WrapOpen::NoWrap => Ok(None),
        WrapOpen::Refused(reason) => {
            // Tampering or substitution — witnessed, never fatal: any
            // fleet-key holder can plant the bytes, and a walk that aborts
            // on them is a plane-wide wedge (the `unmergeable` reasoning).
            tracing::warn!(
                generation = %key_hex,
                "a generation wrap refused to open during the walk: {reason}"
            );
            Ok(None)
        }
    }
}

/// One attempted wrap-open for this device. `Err` at the call site is store
/// I/O only; everything an attacker could plant lands in a variant.
enum WrapOpen {
    Key(GenerationKey),
    /// Nothing targets this device — no inline member wrap, no top-up row.
    NoWrap,
    /// A wrap exists but refuses: HPKE failure, or a key that fails the
    /// mint's commitment (substitution).
    Refused(String),
}

async fn open_with_wraps<B: StoreBackend>(
    store: &AccountStore<B>,
    generation_id: &[u8; 32],
    core: &MintCore,
    inline: &[MemberWrap],
    writer_key: &SigningKey,
) -> Result<WrapOpen> {
    let device_id = writer_key.verifying_key().to_bytes();
    let device_kem = derive_device_xwing_keypair(&writer_key.to_bytes());

    // Inline first, then every merged top-up — fall-through, not either/or: a refused inline wrap must not mask a top-up
    // that opens (a vandal-corrupted inline wrap was previously a durable
    // partition even with a good top-up sitting right there). Every candidate
    // is checked against the mint's key commitment, so trying all of them is
    // pure gain; the refusals are still witnessed below.
    let mut candidates: Vec<Vec<u8>> = inline
        .iter()
        .filter(|w| w.device_id == device_id)
        .map(|w| w.wrap.clone())
        .collect();
    candidates.extend(topup_wraps(store, generation_id, &device_id).await?);
    if candidates.is_empty() {
        return Ok(WrapOpen::NoWrap);
    }
    let mut refusals = Vec::new();
    for wrap_bytes in &candidates {
        match open_generation_key_as_device(
            wrap_bytes,
            &device_kem.secret,
            generation_id,
            &device_id,
            &core.key_commitment,
        ) {
            Ok(key) => return Ok(WrapOpen::Key(key)),
            Err(e) => refusals.push(e.to_string()),
        }
    }
    Ok(WrapOpen::Refused(refusals.join("; ")))
}

/// Every usable top-up wrap for `(generation, device)` — the per-healer
/// cells (a whole-kind scan,
/// since the healer segment of a v2 cell is unknowable in advance).
///
/// A malformed or self-inconsistent row is simply not among the candidates —
/// row content is attacker-suppliable, so it must never become a hard error
/// at a read site, and the seal-path refusal an empty answer produces ("no
/// wrap reaches this device") still names the fix. Tombstones never enter
/// via [`live_rows`]. `Err` is store I/O only.
async fn topup_wraps<B: StoreBackend>(
    store: &AccountStore<B>,
    generation_id: &[u8; 32],
    device_id: &[u8; 32],
) -> Result<Vec<Vec<u8>>> {
    Ok(live_rows(store, KIND_GENERATION_WRAP)
        .await?
        .iter()
        .filter_map(|entry| usable_wrap_for(entry, Some(generation_id), device_id))
        .map(|(_, wrap)| wrap)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation_fixture_test_support::{
        Bundle, ESCROW_SEED, THEM, US, device_id_of, device_key, fixture, machinery_row, member_of,
    };
    use fauna_core::generation::{EscrowTargetRecord, derive_escrow_xwing_keypair};
    use fauna_mls::wrapped_blob::generation_wraps::build_mint;

    /// A key no mint committed to — what a retained bundle holds after a
    /// substituted key slipped in.
    fn foreign_key() -> GenerationKey {
        GenerationKey::from_bytes([0x42u8; 32])
    }

    /// **The read half refuses what the seal half refuses — no wrap arm.** A
    /// live, id-bound mint this device holds no wrap of: a bundle key that
    /// matches its commitment answers (the seedless host's heal), and one
    /// that fails it is never served. Red-verified: with the `NoWrap` arm
    /// serving the bundle again, the foreign key is returned.
    #[tokio::test]
    async fn a_bundle_key_failing_a_live_mints_commitment_is_not_served_when_no_wrap_reaches() {
        let f = fixture().await;
        // Minted by THEM alone, so no wrap reaches this device.
        let built = build_mint(
            &[member_of(THEM)],
            &EscrowTargetRecord {
                xwing_escrow_pubkey: derive_escrow_xwing_keypair(&ESCROW_SEED)
                    .public
                    .to_bytes()
                    .to_vec(),
            },
            &crate::generation_fixture_test_support::target_key(),
            Vec::new(),
            &device_key(THEM),
            7_000,
        )
        .expect("mint");
        let (g, key) = (built.generation_id, built.gen_key);
        f.put(machinery_row(
            KIND_GENERATION_MINT,
            fauna_core::hex32::encode(&g),
            &built.record,
        ))
        .await;

        let good = Bundle::default();
        good.record_generation_key(&g, &key);
        let served = generation_key_for(&f.store, &g, &f.writer_key, Some(&good))
            .await
            .unwrap();
        assert_eq!(served.map(|k| *k.as_bytes()), Some(*key.as_bytes()));

        let bad = Bundle::default();
        bad.record_generation_key(&g, &foreign_key());
        assert!(
            generation_key_for(&f.store, &g, &f.writer_key, Some(&bad))
                .await
                .unwrap()
                .is_none()
        );
    }

    /// **The same, refused-wrap arm.** This device's inline wrap is replaced
    /// by another member's (it fails to open here), and the bundle holds a
    /// key that fails the mint's commitment: nothing is served. Red-verified:
    /// with the `Refused` arm serving the bundle again, the foreign key is
    /// returned.
    #[tokio::test]
    async fn a_bundle_key_failing_a_live_mints_commitment_is_not_served_when_the_wrap_refuses() {
        let f = fixture().await;
        let (g, _, _) = f.mint_over(&[member_of(US), member_of(THEM)]).await;
        let key_hex = fauna_core::hex32::encode(&g);
        let entry = f
            .store
            .state(KIND_GENERATION_MINT, &key_hex)
            .await
            .unwrap()
            .expect("mint row");
        let GenerationMintRecord::Minted {
            core,
            minter_sig,
            mut wraps,
        } = fauna_core::encoding::canonical_decode(&entry.value).unwrap()
        else {
            unreachable!()
        };
        let us = device_id_of(US);
        let theirs = wraps
            .iter()
            .find(|w| w.device_id != us)
            .expect("THEM's wrap")
            .wrap
            .clone();
        for w in wraps.iter_mut().filter(|w| w.device_id == us) {
            w.wrap = theirs.clone();
        }
        f.put(machinery_row(
            KIND_GENERATION_MINT,
            key_hex,
            &GenerationMintRecord::Minted {
                core,
                minter_sig,
                wraps,
            },
        ))
        .await;

        let bad = Bundle::default();
        bad.record_generation_key(&g, &foreign_key());
        assert!(
            generation_key_for(&f.store, &g, &f.writer_key, Some(&bad))
                .await
                .unwrap()
                .is_none()
        );
    }
}
