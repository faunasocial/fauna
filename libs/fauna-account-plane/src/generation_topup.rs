//! The **top-up self-heal pass** — the production `fauna.state.generation-wrap`
//! writer (`account-data-plane.md` § The generation machinery, the top-up
//! kind; the W5 (account-data-plane.md § Workstreams)-era pass the charter's § Implementation status has owed since
//! the R14 (account-data-plane.md § The ratified decisions) build: "un-partitions reads under subset mints generally — the
//! device-endpoints writer's re-seal covers only its own kind").
//!
//! # The partition this closes
//!
//! A mint wraps its generation key inline for the member set it saw. A device
//! that enrolled after that mint — or that a subset mint simply left out — has
//! no wrap for it, so **every** row any writer ever sealed under that
//! generation is unreadable there, for good, no matter how long it syncs.
//! Reads walk every retained generation (§ The generation machinery: reads are
//! integrity-only), so the missing wrap is the whole failure.
//!
//! The charter's remedy is the top-up kind, and it is deliberately not a
//! handshake: "any device holding the key can write one" — since the hardening
//! into the healer's own per-healer cell
//! `(generation id, target device id, healer device id)`, healer-signed —
//! and the receiver verifies the key commitment on open. This module is the
//! writer nobody had built — until W5.8 the kind was read-only in practice
//! (`generation_tip` opens top-ups; nothing produced any).
//!
//! `device_endpoints_writer`'s re-seal is the neighbouring, narrower mechanism:
//! it re-publishes *this device's own* row under a newer tip. It cannot help a
//! row some other writer sealed, and it cannot help at all for a generation
//! that is still the tip. This pass is the general half.
//!
//! # What publishes when
//!
//! Once per full pump pass, for every **live** (non-shredded) mint row this
//! device can key, and every verified non-removed fleet member with no
//! **authenticated** coverage: one heal through the door. Coverage evidence
//! is authenticated or it is nothing (*the healer
//! that believes the vandal*: both of the original skip grounds were
//! attacker-writable, and the skip is absorbing, so one forged row
//! permanently partitioned a chosen device from a chosen generation):
//!
//! - **Inline coverage** is the triple: the core BINDS to its own key (the
//!   content-derived generation id recomputes — checked here, not assumed
//!   from "inside the signature"; a forged whole
//!   `Minted` value squatting an honest key wins the mint join and its
//!   `member_ids` claim is nobody's evidence) ∧ (target ∈
//!   `MintCore::member_ids`) ∧ (a `wraps` entry present). `wraps` alone sits
//!   outside both the id and `minter_sig`, and a forged appended
//!   `MemberWrap` wins the mint join with the honest signature carried
//!   through.
//! - **Top-up coverage** is a per-healer v2 row that verifies at its cell
//!   (the healer's own signature over every field) AND whose healer is a
//!   currently verified, non-removed member — a removed device's
//!   validly-signed row never counts.
//! - **A verifying "cannot key" assertion by the target clears BOTH grounds**
//!   (`fauna.state.generation-unkeyable`, the cure for in-place
//!   corruption of a member's own inline wrap, which wears coverage's exact
//!   costume; and a malicious *verified member* can mint verifying garbage
//!   top-ups, so the top-up ground must be clearable too). The response is
//!   bounded per assertion: publish iff this healer's own cell holds no
//!   verifying row or one whose wrap hash the assertion names as
//!   tried-and-failed — one fresh wrap per healer per assertion, then
//!   byte-quiet until the target re-asserts against the new evidence
//!   ([`crate::generation_unkeyable`] owns the target half).
//!
//! Everything else is a quiet skip:
//!
//! - **A generation this device cannot key.** Not an error and not a warning:
//!   the observing device is simply not one of the ones that can heal this
//!   generation, and some device that holds the key will (the charter's
//!   "any device holding the key"). A pass that treated it as a failure would
//!   turn the normal state of a freshly-enrolled device into a loud one.
//! - **A `Shredded` mint.** Handing out a shredded generation's key is exactly
//!   the crypto-shred defeat the retained-key custody contract exists to
//!   prevent ("deleting a generation = devices drop it"): a top-up would
//!   re-deliver, to a device that had correctly dropped it, key material the
//!   user deleted. `Shredded` rows carry no wraps anyway, so this is
//!   belt-and-braces — but it is the belt that matters, because the key can
//!   still be sitting in *our* retained bundle.
//! - **A removed device.** The kind's contract is explicit — "never targets a
//!   removed id (writers check merged device-set state)" — and
//!   [`FleetView::wrap_targets`] is that check: `Removed` is absorbing in the
//!   device-set lattice, so a target that is removed anywhere is removed here.
//!   It is asked **again at each heal's write** ([`put_heal`], through
//!   [`no_longer_wrap_targets`]): the devices page's removal is a local
//!   command served at this pass's own yields — each generation's breath,
//!   each heal's publish — so the target list read at the pass's start can
//!   be stale by the time a wrap is written.
//! - **Ourselves.** We can key it; a self-wrap is pure noise on the plane.
//!
//! Idempotence is structural rather than stateful: the pass republishes
//! nothing it can already see, and each healer's re-publications rank by
//! their signed stamp inside its own cell, so a duplicate that races in from
//! a sibling healer converges without ceremony.
//!
//! # Why it is safe to run on every replica at once
//!
//! Two devices holding the same key may both top up the same third device.
//! They produce *different ciphertexts* (X-Wing is randomized) for the same
//! plaintext key, each resting in its author's own cell — any of them opens,
//! because the receiver's check is the mint's key commitment, not the wrap's
//! identity, and the read path tries every candidate. Convergence costs a
//! redundant row per healer, which is why this needs no election even though
//! the engine-singleton is the one that runs it in practice.

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use fauna_account_store::{backend::StoreBackend, store::AccountStore};
use fauna_core::generation::{FleetMember, FleetView, GenerationMintRecord};
use fauna_mls::wrapped_blob::generation_wraps::build_topup_wrap_v2;
use fauna_protocol::RpcRequester;
use fauna_protocol::merge_policy::{
    KIND_DEVICE_REACH, KIND_DEVICE_SET, KIND_GENERATION_MINT, KIND_GENERATION_UNKEYABLE,
    KIND_GENERATION_WRAP, LwwStamp,
};

use crate::account_state_plane::{AccountStatePlane, ItemId};
use crate::generation_store::{live_rows, row_ref};
use crate::generation_tip::{self, GenerationTrust};

/// What one top-up pass did (the pump's `generation_topup` report slot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopupPass {
    /// Every verified member already reaches every generation this device can
    /// key — including the trivial cases (a one-device fleet, nothing minted).
    Current,
    /// This many `fauna.state.generation-wrap` rows went through the door.
    Published(usize),
}

/// Ensure every verified fleet member can key every live generation **this**
/// device can key. Module docs own the decision table.
pub async fn ensure_topped_up<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
) -> Result<TopupPass>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let device_id = writer_key.verifying_key().to_bytes();

    // Who may be targeted at all. `wrap_targets` is the charter's
    // "writers check merged device-set state" — verified enrollment certs
    // only, `Removed` absorbing, invalid rows non-members.
    let device_rows = live_rows(store, KIND_DEVICE_SET).await?;
    let view = FleetView::build(&trust.root, device_rows.iter().map(row_ref));
    let targets: Vec<FleetMember> = view
        .wrap_targets()
        .filter(|m| m.device_id != device_id)
        .cloned()
        .collect();
    if targets.is_empty() {
        return Ok(TopupPass::Current);
    }

    let coverage = wrap_coverage(store, &view).await?;
    let signals = unkeyable_signals(store).await?;
    let mut published = 0usize;

    for entry in live_rows(store, KIND_GENERATION_MINT).await? {
        // One generation is one unit of local work (`pass_breath` module docs).
        crate::pass_breath::pass_breath().await;
        // Canonical-or-skip, the same injectivity rule `FleetView::build`
        // applies to device-set keys: a row whose key is not the canonical hex
        // of its generation id is not one we can name a wrap for.
        let Ok(generation_id) = fauna_core::hex32::decode(&entry.key) else {
            continue;
        };
        if fauna_core::hex32::encode(&generation_id) != entry.key {
            continue;
        }
        let record: GenerationMintRecord =
            match fauna_core::encoding::canonical_decode(&entry.value) {
                Ok(r) => r,
                // Attacker-suppliable content; the read path warns about these
                // where it matters. Here it just means "no members to serve".
                Err(_) => continue,
            };
        let GenerationMintRecord::Minted { core, wraps, .. } = record else {
            // `Shredded` — module docs, the crypto-shred clause.
            continue;
        };
        // the binding is what authenticates `member_ids` — the
        // generation id is content-derived from the core, so a forged core
        // squatting an honest key never binds, and its member list is
        // nobody's coverage evidence. The same check the read path applies
        // (`generation_tip`'s Key↔id gate); `minter_sig` would add
        // authorship, which this decision does not need. Deliberately NOT a
        // whole-row skip: a healer holding the key in its retained bundle
        // must still heal the generation the forged row denies.
        let core_binds = fauna_core::generation::generation_id(&core).ok() == Some(generation_id);

        let missing: Vec<&FleetMember> = targets
            .iter()
            .filter(|m| match signals.get(&(generation_id, m.device_id)) {
                // A verifying "cannot key" assertion by the target CLEARS BOTH
                // suppression grounds: apparent coverage is exactly
                // what the durable in-member corrupted-wrap case wears, and a
                // malicious verified member can mint verifying garbage
                // top-ups, so neither ground survives the target's own signed
                // testimony. The response is bounded per assertion by the
                // per-healer gate: publish iff my own cell holds no verifying
                // row, or holds one whose wrap hash the assertion names as
                // tried-and-failed — after one response my fresh row's hash
                // is outside the assertion's evidence and I go quiet until
                // the target re-asserts against the NEW evidence.
                Some(tried) => match coverage.rows.get(&(generation_id, m.device_id, device_id)) {
                    None => true,
                    Some(row) => tried.contains(&row.wrap_hash),
                },
                // Inline coverage is believed only when
                // AUTHENTICATED: the target must be in `core.member_ids`
                // — inside the content-derived id and therefore inside
                // `minter_sig`'s coverage — AND have a wrap present. `wraps`
                // alone sits outside both, so a forged appended `MemberWrap`
                // won the mint join carrying the honest signature through;
                // and a member the minter listed but forgot to wrap now heals
                // instead of staying partitioned.
                None => {
                    (!core_binds
                        || !core.member_ids.contains(&m.device_id)
                        || !wraps.iter().any(|w| w.device_id == m.device_id))
                        && !coverage.covered.contains(&(generation_id, m.device_id))
                }
            })
            .collect();
        if missing.is_empty() {
            continue;
        }

        // Only now do we pay for the unwrap — the common steady state is
        // "everyone already has one", and this is the expensive step.
        let Some(gen_key) = generation_tip::generation_key_for(
            store,
            &generation_id,
            writer_key,
            fleet.generation_custody(),
            Some(&view),
        )
        .await?
        else {
            // Not ours to heal (module docs). Quiet by design.
            continue;
        };

        for target in missing {
            // The stamp floor: this healer's own current row's signed stamp,
            // so a re-publication rises past it structurally — a clock
            // regression must not make the join keep the failed row, which
            // under the signal gate would be an infinite republish loop.
            let floor = coverage
                .rows
                .get(&(generation_id, target.device_id, device_id))
                .map(|row| row.at_ms);
            if put_heal(fleet, &generation_id, target, &gen_key, writer_key, floor).await? {
                published += 1;
            }
        }
    }

    if published == 0 {
        Ok(TopupPass::Current)
    } else {
        tracing::info!(
            rows = published,
            "generation top-up: published wraps for members that could not key a live generation"
        );
        Ok(TopupPass::Published(published))
    }
}

/// One cell-verifying per-healer row's facts, as [`WrapCoverage`] carries
/// them: what was wrapped (the ciphertext's BLAKE3 — the same hash the healer
/// signature covers and the evidence lists name) and when (the signed
/// stamp — the re-publication floor).
#[derive(Debug, Clone, Copy)]
pub struct WrapRowFacts {
    /// BLAKE3 of the wrap ciphertext.
    pub wrap_hash: [u8; 32],
    /// The signed `at_ms`.
    pub at_ms: i64,
}

/// One per-healer cell's coordinates: (generation id, target device, healer).
pub type WrapCellCoords = ([u8; 32], [u8; 32], [u8; 32]);

/// The authenticated top-up surface, one whole-kind scan (
/// the healer gate here and the target pass in [`crate::generation_unkeyable`]).
pub struct WrapCoverage {
    /// The `(generation, target)` pairs whose suppression evidence is
    /// **authenticated**: a per-healer v2 row that verifies at its cell — the
    /// healer's own Ed25519 signature over every field, which a `BackupKey`
    /// holder cannot forge — AND whose healer is a currently verified,
    /// non-removed fleet member; or — since the reclamation ruling
    /// (2026-09-16) — the **target's own verifying reach row** listing the
    /// generation (`fauna.state.device-reach`, by a currently verified
    /// non-removed member: the target says it holds the key, which is what
    /// lets a healer retire its cell without re-triggering a heal). A removed
    /// device's own validly-signed rows therefore stop counting the moment
    /// its removal merges. Anything less than this pair of checks is evidence the vandal
    /// can mint.
    pub covered: std::collections::BTreeSet<([u8; 32], [u8; 32])>,
    /// Every currently verified non-removed member's verifying reach row,
    /// by device id — the possession evidence itself, for the reclamation
    /// pass's retirement decisions.
    pub reach: std::collections::BTreeMap<[u8; 32], fauna_core::generation::DeviceReachRecord>,
    /// Every **cell-verifying** per-healer row, keyed
    /// `(generation, target, healer)` — membership deliberately NOT consulted
    /// here: the healer gate wants "what is in my own cell" whatever my
    /// current view says about me, and the target pass filters by membership
    /// itself when it assembles evidence.
    pub rows: std::collections::BTreeMap<WrapCellCoords, WrapRowFacts>,
}

/// Build [`WrapCoverage`] from the merged wrap rows.
pub async fn wrap_coverage<B: StoreBackend>(
    store: &AccountStore<B>,
    view: &FleetView,
) -> Result<WrapCoverage> {
    use fauna_core::generation::{
        DeviceReachRecord, GenerationWrapRecordV2, WrapCellKey, parse_reach_cell_key,
        parse_wrap_cell_key,
    };
    let mut coverage = WrapCoverage {
        covered: std::collections::BTreeSet::new(),
        rows: std::collections::BTreeMap::new(),
        reach: std::collections::BTreeMap::new(),
    };
    for entry in live_rows(store, KIND_DEVICE_REACH).await? {
        let Some(device) = parse_reach_cell_key(&entry.key) else {
            continue;
        };
        let Ok(record) = fauna_core::encoding::canonical_decode::<DeviceReachRecord>(&entry.value)
        else {
            continue;
        };
        if !record.verifies_at(&device) || !view.is_verified_member(&device) {
            continue;
        }
        for generation in &record.holds {
            coverage.covered.insert((*generation, device));
        }
        coverage.reach.insert(device, record);
    }
    for entry in live_rows(store, KIND_GENERATION_WRAP).await? {
        let Some(WrapCellKey {
            generation_id,
            target_device,
            healer,
        }) = parse_wrap_cell_key(&entry.key)
        else {
            continue;
        };
        let Ok(record) =
            fauna_core::encoding::canonical_decode::<GenerationWrapRecordV2>(&entry.value)
        else {
            continue;
        };
        if !record.verifies_at(&generation_id, &target_device, &healer) {
            continue;
        }
        let GenerationWrapRecordV2::Wrap { at_ms, wrap, .. } = &record;
        coverage.rows.insert(
            (generation_id, target_device, healer),
            WrapRowFacts {
                wrap_hash: *blake3::hash(wrap).as_bytes(),
                at_ms: *at_ms,
            },
        );
        if view.is_verified_member(&healer) {
            coverage.covered.insert((generation_id, target_device));
        }
    }
    Ok(coverage)
}

/// The verifying, un-retracted "cannot key" signals:
/// `(generation, target)` → the assertion's tried-and-failed wrap-hash set.
/// A cell whose join winner is `Satisfied`, fails to verify at its cell, or
/// fails to decode contributes nothing — forged or junk testimony clears no
/// suppression.
async fn unkeyable_signals<B: StoreBackend>(
    store: &AccountStore<B>,
) -> Result<std::collections::BTreeMap<([u8; 32], [u8; 32]), std::collections::BTreeSet<[u8; 32]>>>
{
    use fauna_core::generation::{GenerationUnkeyableRecord, parse_unkeyable_cell_key};
    let mut out = std::collections::BTreeMap::new();
    for entry in live_rows(store, KIND_GENERATION_UNKEYABLE).await? {
        let Some((generation_id, target_device)) = parse_unkeyable_cell_key(&entry.key) else {
            continue;
        };
        let Ok(record) =
            fauna_core::encoding::canonical_decode::<GenerationUnkeyableRecord>(&entry.value)
        else {
            continue;
        };
        if !record.verifies_at(&generation_id, &target_device) {
            continue;
        }
        if let GenerationUnkeyableRecord::Asserted { tried, .. } = record {
            out.insert(
                (generation_id, target_device),
                tried.into_iter().collect::<std::collections::BTreeSet<_>>(),
            );
        }
    }
    Ok(out)
}

/// Which of `ids` merged device-set state no longer lists as a wrap target
/// **now** — the kind's "never targets a removed id", asked at a write
/// instead of through a view some await has since made stale. One
/// predicate, [`FleetView::wrap_targets`], so the healer and the mint cannot
/// drift into two readings of "still a member".
///
/// The devices page's removal quartet is a local command served at any
/// yield of a running pass (`account_runtime`'s `Cmd::is_local`), so a
/// writer that read its targets before an await — a breath, a publish, an
/// escrow deposit — must re-ask here before the wrap exists. Store calls are synchronous underneath, so the answer
/// still holds at a write that follows it with no await between.
pub async fn no_longer_wrap_targets<B: StoreBackend>(
    store: &AccountStore<B>,
    root: &fauna_core::identity::ActorId,
    ids: impl IntoIterator<Item = [u8; 32]>,
) -> Result<Vec<[u8; 32]>> {
    let device_rows = live_rows(store, KIND_DEVICE_SET).await?;
    let view = FleetView::build(root, device_rows.iter().map(row_ref));
    let current: std::collections::BTreeSet<[u8; 32]> =
        view.wrap_targets().map(|m| m.device_id).collect();
    Ok(ids.into_iter().filter(|id| !current.contains(id)).collect())
}

/// Publish one heal: the healer-attributed row into this device's own
/// per-healer cell. The row carries the outer [`LwwStamp`] the plane's put
/// requires; its real freshness is its **signed** `at_ms`, which rises past
/// `stamp_floor` (the current own-cell row) structurally.
///
/// Returns `false`, writing nothing, when `target` is no longer a wrap
/// target under merged device-set state — checked here, immediately before
/// the write, because every caller chose `target` before an await
/// ([`no_longer_wrap_targets`]).
pub async fn put_heal<B, R>(
    fleet: &AccountStatePlane<'_, B, R>,
    generation_id: &[u8; 32],
    target: &FleetMember,
    gen_key: &fauna_core::crypto::GenerationKey,
    writer_key: &SigningKey,
    stamp_floor: Option<i64>,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let Some(row) = owed_heal_row(
        fleet,
        generation_id,
        target,
        gen_key,
        writer_key,
        stamp_floor,
    )
    .await?
    else {
        return Ok(false);
    };
    fleet
        .put(&row.item, row.value, Some(row.merge_meta))
        .await?;
    Ok(true)
}

/// [`put_heal`]'s local half alone ([`AccountStatePlane::put_local`]): the
/// heal is durable and **nothing is sent** — the first-need mint's spill,
/// whose rows go out with the mint's in one ordered publish.
pub(crate) async fn put_heal_local<B, R>(
    fleet: &AccountStatePlane<'_, B, R>,
    generation_id: &[u8; 32],
    target: &FleetMember,
    gen_key: &fauna_core::crypto::GenerationKey,
    writer_key: &SigningKey,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let Some(row) = owed_heal_row(fleet, generation_id, target, gen_key, writer_key, None).await?
    else {
        return Ok(false);
    };
    fleet
        .put_local(&row.item, row.value, Some(row.merge_meta))
        .await?;
    Ok(true)
}

/// The row [`put_heal`] writes, or `None` when `target` is no longer a wrap
/// target — the re-check both of its doors make immediately before the write.
async fn owed_heal_row<B, R>(
    fleet: &AccountStatePlane<'_, B, R>,
    generation_id: &[u8; 32],
    target: &FleetMember,
    gen_key: &fauna_core::crypto::GenerationKey,
    writer_key: &SigningKey,
    stamp_floor: Option<i64>,
) -> Result<Option<HealRow>>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let (store, trust) = fleet.store_and_trust();
    if !no_longer_wrap_targets(store, &trust.root, [target.device_id])
        .await?
        .is_empty()
    {
        return Ok(None);
    }
    heal_row(generation_id, target, gen_key, writer_key, stamp_floor).map(Some)
}

/// One row of a heal, as [`AccountStatePlane::put`] takes it.
pub struct HealRow {
    pub item: ItemId,
    pub value: Vec<u8>,
    /// The outer [`LwwStamp`], encoded.
    pub merge_meta: Vec<u8>,
}

/// The row one heal writes — the per-healer cell — built without touching
/// any plane; [`put_heal`] (the pass and the first-need mint's spill)
/// writes it through the door. The signed `at_ms` rises past `stamp_floor`
/// structurally (`put_heal`'s contract).
pub fn heal_row(
    generation_id: &[u8; 32],
    target: &FleetMember,
    gen_key: &fauna_core::crypto::GenerationKey,
    writer_key: &SigningKey,
    stamp_floor: Option<i64>,
) -> Result<HealRow> {
    use fauna_core::generation::wrap_cell_key_per_healer;
    let device_id = writer_key.verifying_key().to_bytes();
    let at_ms = (fauna_core::data::Timestamp::now_millis_or_zero() as i64)
        .max(stamp_floor.map_or(i64::MIN, |floor| floor.saturating_add(1)));
    let stamp = LwwStamp {
        at_ms,
        writer: device_id,
    };

    let v2 = build_topup_wrap_v2(gen_key, generation_id, target, writer_key, at_ms).with_context(
        || {
            format!(
                "building a generation-wrap top-up for {} → {}",
                fauna_core::hex32::encode(generation_id),
                fauna_core::hex32::encode(&target.device_id)
            )
        },
    )?;
    Ok(HealRow {
        item: ItemId {
            kind: KIND_GENERATION_WRAP.into(),
            key: wrap_cell_key_per_healer(generation_id, &target.device_id, &device_id),
        },
        value: fauna_core::encoding::canonical_encode(&v2)?,
        merge_meta: stamp.encode()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation_fixture_test_support::*;
    use fauna_account_store::types::StateEntry;
    use fauna_core::crypto::GenerationKey;
    use fauna_core::generation::{
        DeviceSetRecord, EscrowTargetRecord, derive_device_xwing_keypair,
        derive_escrow_xwing_keypair,
    };
    use fauna_mls::wrapped_blob::generation_wraps::{build_mint, open_generation_key_as_device};

    // `NoNest`, the `ROOT_SEED`/`US`/`THEM`/`ESCROW_SEED` key set, `root`/
    // `device_key`/`device_id_of`/`member_of`/`enrollment_row`/`machinery_row`,
    // and `Fixture`/`fixture`/`Fixture::{plane,put,mint_over}` all come from
    // `generation_fixture_test_support` now — this suite's own twin in
    // `generation_unkeyable.rs` hand-copied the identical scaffolding.

    fn removal_row(seed: [u8; 32]) -> StateEntry {
        machinery_row(
            KIND_DEVICE_SET,
            fauna_core::hex32::encode(&device_id_of(seed)),
            &DeviceSetRecord::Removed {
                removed_at_ms: 9_000,
                removed_by: device_id_of(US),
            },
        )
    }

    impl Fixture {
        async fn topup_rows(&self) -> Vec<StateEntry> {
            live_rows(&self.store, KIND_GENERATION_WRAP).await.unwrap()
        }
    }

    /// **The pin this pass exists for.** A device the mint left out gets a
    /// top-up whose wrap actually opens under *its* KEM secret — the whole
    /// point being that reads of every row sealed under that generation stop
    /// being partitioned there. The heal is one row in the
    /// healer's own cell, verifying under the healer — and nothing else (the
    /// unattributed two-segment courtesy row for pre-hardening readers was
    /// retired by the compat-remnant sweep, program 4).
    #[tokio::test]
    async fn a_member_the_mint_left_out_is_topped_up_with_a_wrap_that_opens() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        // The subset mint: only we are a member.
        let (generation_id, _key, core) = f.mint_over(&[member_of(US)]).await;
        // ...and only afterwards does the second device enroll.
        f.put(enrollment_row(THEM)).await;

        let plane = f.plane();
        let outcome = ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
            .await
            .expect("pass");
        assert_eq!(
            outcome,
            TopupPass::Published(1),
            "one heal, whatever its row count"
        );

        let rows = f.topup_rows().await;
        assert_eq!(rows.len(), 1, "the per-healer cell row alone");
        let v2_key = fauna_core::generation::wrap_cell_key_per_healer(
            &generation_id,
            &device_id_of(THEM),
            &device_id_of(US),
        );
        let v2_row = rows.iter().find(|r| r.key == v2_key).expect("the v2 row");
        let record: fauna_core::generation::GenerationWrapRecordV2 =
            fauna_core::encoding::canonical_decode(&v2_row.value).unwrap();
        assert!(
            record.verifies_at(&generation_id, &device_id_of(THEM), &device_id_of(US)),
            "the published row verifies at its own cell"
        );
        let fauna_core::generation::GenerationWrapRecordV2::Wrap { wrap, .. } = &record;
        // The assertion that matters: the target can actually open it.
        open_generation_key_as_device(
            wrap,
            &derive_device_xwing_keypair(&THEM).secret,
            &generation_id,
            &device_id_of(THEM),
            &core.key_commitment,
        )
        .expect("the topped-up device opens the generation key");
    }

    /// Idempotent against the mint's own carriage: a member already wrapped
    /// inline needs nothing, and a pass that re-published would churn the plane
    /// on every pump for the entire life of the account.
    #[tokio::test]
    async fn a_member_wrapped_inline_on_the_mint_is_not_topped_up() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        f.mint_over(&[member_of(US), member_of(THEM)]).await;

        let plane = f.plane();
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .expect("pass"),
            TopupPass::Current
        );
        assert!(f.topup_rows().await.is_empty());
    }

    /// Idempotent against itself: the second pass over an unchanged fleet
    /// publishes nothing.
    #[tokio::test]
    async fn a_second_pass_republishes_nothing() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.mint_over(&[member_of(US)]).await;
        f.put(enrollment_row(THEM)).await;

        let plane = f.plane();
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            TopupPass::Published(1)
        );
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            TopupPass::Current,
            "our own verifying v2 row suppresses a second heal"
        );
        assert_eq!(f.topup_rows().await.len(), 1, "one per-healer row, once");
    }

    /// The kind's own contract — "never targets a removed id". `Removed` is
    /// absorbing in the device-set lattice, so a removal anywhere means the
    /// severance holds here: topping a removed device back up would hand the
    /// current fleet key to the device the user just cut off.
    #[tokio::test]
    async fn a_removed_device_is_never_topped_up() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.mint_over(&[member_of(US)]).await;
        f.put(enrollment_row(THEM)).await;
        f.put(removal_row(THEM)).await;

        let plane = f.plane();
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            TopupPass::Current
        );
        assert!(f.topup_rows().await.is_empty());
    }

    /// **a removal served INSIDE the pass holds too.** The
    /// fleet-removal quartet is a local command, served at any yield point of
    /// a running pass — each generation's breath among them — so the
    /// `Removed` row can land after the pass read its targets. Polled by
    /// hand over two generations THEM lacks: the row lands at the first
    /// breath, and not one wrap may reach THEM after it (the severance the
    /// healer would otherwise undo, one generation key at a time).
    #[tokio::test]
    async fn a_device_removed_mid_pass_is_not_topped_up_after_the_removal() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.mint_over(&[member_of(US)]).await;
        f.mint_over(&[member_of(US)]).await;
        f.put(enrollment_row(THEM)).await;

        let plane = f.plane();
        let mut pass = std::pin::pin!(ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key));
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        let mut breaths = 0usize;
        let outcome = loop {
            match pass.as_mut().poll(&mut cx) {
                std::task::Poll::Ready(out) => break out.expect("pass"),
                std::task::Poll::Pending => {
                    breaths += 1;
                    if breaths == 1 {
                        // What `fleet_removal::write_removed` writes.
                        f.put(removal_row(THEM)).await;
                    }
                }
            }
        };
        assert_eq!(
            breaths, 2,
            "one breath per generation, the removal at the first"
        );
        assert_eq!(outcome, TopupPass::Current);
        assert!(
            f.topup_rows().await.is_empty(),
            "no wrap reaches a device removed while the pass ran"
        );
    }

    /// The crypto-shred clause. A shredded generation's key must not be
    /// re-delivered to anyone — including a device that had correctly dropped
    /// it — or "deleting a generation = devices drop it" becomes a suggestion.
    /// The pass must refuse from the *row's phase*, not from the absence of
    /// inline wraps, because our own retained bundle can still hold the key.
    #[tokio::test]
    async fn a_shredded_generation_is_never_topped_up() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        let (generation_id, _key, core) = f.mint_over(&[member_of(US)]).await;
        f.put(enrollment_row(THEM)).await;
        // The shred lands (absorbing phase — no wraps survive it).
        f.put(machinery_row(
            KIND_GENERATION_MINT,
            fauna_core::hex32::encode(&generation_id),
            &GenerationMintRecord::Shredded {
                core,
                shredded_at_ms: 9_000,
                shredded_by: device_id_of(US),
                shredder_sig: vec![],
            },
        ))
        .await;

        let plane = f.plane();
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            TopupPass::Current
        );
        assert!(
            f.topup_rows().await.is_empty(),
            "a shredded generation's key is never re-delivered"
        );
    }

    /// A generation this device cannot key is somebody else's to heal — a
    /// quiet skip, never an error. This is the ordinary state of a
    /// freshly-enrolled device, so treating it as a failure would make the
    /// common case loud and, worse, abort the pass before the generations we
    /// *can* heal.
    #[tokio::test]
    async fn a_generation_we_cannot_key_is_skipped_without_failing_the_pass() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        // A third device mints over itself alone: neither we nor THEM can key
        // it. (`build_mint` requires the minter in its own member set.)
        let stranger = [0xC3u8; 32];
        let escrow = EscrowTargetRecord {
            xwing_escrow_pubkey: derive_escrow_xwing_keypair(&ESCROW_SEED)
                .public
                .to_bytes()
                .to_vec(),
        };
        let built = build_mint(
            &[member_of(stranger)],
            &escrow,
            &crate::generation_fixture_test_support::target_key(),
            Vec::new(),
            &device_key(stranger),
            7_000,
        )
        .expect("mint");
        f.put(machinery_row(
            KIND_GENERATION_MINT,
            fauna_core::hex32::encode(&built.generation_id),
            &built.record,
        ))
        .await;

        let plane = f.plane();
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .expect("an unkeyable generation is not an error"),
            TopupPass::Current
        );
        assert!(f.topup_rows().await.is_empty());
    }

    /// A one-device fleet has nobody to serve — the cheapest exit, and the
    /// state every account is in until its second device enrolls.
    #[tokio::test]
    async fn a_lone_device_publishes_nothing() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.mint_over(&[member_of(US)]).await;

        let plane = f.plane();
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            TopupPass::Current
        );
        assert!(f.topup_rows().await.is_empty());
    }

    // ── The pins — forged coverage evidence must not suppress ─────
    // The fixture writes rows straight into the store, which is exactly the
    // adversarial position: rows a pre-hardening binary merged opaquely, or
    // that a `BackupKey` holder sealed before its removal propagated. The
    // hardened merge seam refuses most of these at first contact
    // (`merge_policy` pins that); the pass must not believe them even when
    // they are already sitting in the store.

    /// PROBE-366-A, landed — the forged inline wrap. The vandal keeps the
    /// honest `core` (same content-derived id, same logical key) AND the
    /// honest `minter_sig` (it signs the id alone, so it still verifies),
    /// appends one unopenable wrap naming the victim, and wins the mint join
    /// with no padding. Arm A: inline coverage is believed only for members
    /// of the SIGNED `member_ids` set, so the victim still heals.
    #[tokio::test]
    async fn probe_366_a_a_forged_inline_wrap_does_not_suppress_the_heal() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        let (generation_id, _key, _core) = f.mint_over(&[member_of(US)]).await;
        f.put(enrollment_row(THEM)).await;

        let honest = live_rows(&f.store, KIND_GENERATION_MINT).await.unwrap();
        let honest_bytes = honest[0].value.clone();
        let mut rec: GenerationMintRecord =
            fauna_core::encoding::canonical_decode(&honest_bytes).unwrap();
        let GenerationMintRecord::Minted { wraps, .. } = &mut rec else {
            panic!("minted")
        };
        wraps.push(fauna_core::generation::MemberWrap {
            device_id: device_id_of(THEM),
            wrap: vec![0xde; 1200],
        });
        let poisoned_bytes = fauna_core::encoding::canonical_encode(&rec).unwrap();

        // It wins the kind's own CRDT join with no padding: both variants are
        // `Minted`, so `two_phase_winner` falls to byte-order max, and the
        // longer `wraps` array is the larger byte string.
        assert_eq!(
            fauna_core::generation::join_generation_mint(&honest_bytes, &poisoned_bytes).unwrap(),
            poisoned_bytes,
            "PROBE-366-A: the poisoned variant wins the mint join"
        );

        f.put(machinery_row(
            KIND_GENERATION_MINT,
            fauna_core::hex32::encode(&generation_id),
            &rec,
        ))
        .await;

        let plane = f.plane();
        let outcome = ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
            .await
            .expect("pass");
        assert_eq!(
            outcome,
            TopupPass::Published(1),
            "PROBE-366-A: an unopenable inline wrap must not count as coverage"
        );
    }

    /// A v2-shaped row whose signature is garbage is not coverage — however
    /// plausible its fields and however large its stamp.
    #[tokio::test]
    async fn a_v2_row_with_a_bad_signature_is_not_coverage() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        let (generation_id, _key, _core) = f.mint_over(&[member_of(US)]).await;
        f.put(enrollment_row(THEM)).await;

        let forged = fauna_core::generation::GenerationWrapRecordV2::Wrap {
            generation_id,
            target_device: device_id_of(THEM),
            healer: device_id_of(US),
            at_ms: i64::MAX,
            wrap: vec![0xde; 1200],
            healer_sig: vec![0xde; 64],
        };
        f.put(machinery_row(
            KIND_GENERATION_WRAP,
            fauna_core::generation::wrap_cell_key_per_healer(
                &generation_id,
                &device_id_of(THEM),
                &device_id_of(US),
            ),
            &forged,
        ))
        .await;

        let plane = f.plane();
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .expect("pass"),
            TopupPass::Published(1),
            "an unverifiable v2 row must not count as coverage"
        );
    }

    /// A REMOVED device's own validly-signed row is not coverage: the vandal
    /// holds a real device key, so its signatures verify — membership is the
    /// second, non-negotiable half of the coverage predicate.
    #[tokio::test]
    async fn a_removed_healers_valid_row_is_not_coverage() {
        let f = fixture().await;
        let vandal = [0xC7u8; 32];
        f.put(enrollment_row(US)).await;
        let (generation_id, gen_key, _core) = f.mint_over(&[member_of(US)]).await;
        f.put(enrollment_row(THEM)).await;
        f.put(enrollment_row(vandal)).await;
        f.put(removal_row(vandal)).await;

        // The vandal's row is REAL — properly sealed, properly signed by its
        // own key — just authored by a device the user severed. (Sealed
        // before removal propagated, or pushed through a lingering bearer.)
        let row = build_topup_wrap_v2(
            &gen_key,
            &generation_id,
            &member_of(THEM),
            &device_key(vandal),
            9_000,
        )
        .unwrap();
        f.put(machinery_row(
            KIND_GENERATION_WRAP,
            fauna_core::generation::wrap_cell_key_per_healer(
                &generation_id,
                &device_id_of(THEM),
                &device_id_of(vandal),
            ),
            &row,
        ))
        .await;

        let plane = f.plane();
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .expect("pass"),
            TopupPass::Published(1),
            "a removed healer's row suppresses nothing, valid signature or not"
        );
    }

    /// The pre-hardening two-segment `"<generation>/<target>"` cell and its
    /// unattributed record were retired by the compat-remnant sweep
    /// (program 4): an honestly sealed wrap filed in that shape no longer
    /// serves the read path — `generation_key_for` finds no wrap for the
    /// target (it was the pin that such a row still opened).
    #[tokio::test]
    async fn a_two_segment_topup_row_no_longer_opens_on_the_read_path() {
        /// The retired v1 wrap record's field set, spelled locally.
        #[derive(serde::Serialize)]
        struct PreSweepWrap {
            #[serde(with = "serde_bytes")]
            generation_id: [u8; 32],
            #[serde(with = "serde_bytes")]
            target_device: [u8; 32],
            #[serde(with = "serde_bytes")]
            wrap: Vec<u8>,
        }
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        let (generation_id, gen_key, _core) = f.mint_over(&[member_of(US)]).await;
        f.put(enrollment_row(THEM)).await;

        let wrap = fauna_mls::wrapped_blob::generation_wraps::seal_generation_key_to_device(
            &gen_key,
            &member_of(THEM).xwing_pubkey,
            &generation_id,
            &device_id_of(THEM),
        )
        .unwrap();
        f.put(machinery_row(
            KIND_GENERATION_WRAP,
            format!(
                "{}/{}",
                fauna_core::hex32::encode(&generation_id),
                fauna_core::hex32::encode(&device_id_of(THEM))
            ),
            &PreSweepWrap {
                generation_id,
                target_device: device_id_of(THEM),
                wrap,
            },
        ))
        .await;

        let opened = crate::generation_tip::generation_key_for(
            &f.store,
            &generation_id,
            &device_key(THEM),
            None,
            None,
        )
        .await
        .expect("store read");
        assert!(
            opened.is_none(),
            "a two-segment wrap row keys nothing for its target"
        );
    }

    /// The converse pin, so the membership check cannot rot into "nobody
    /// suppresses": a LIVE sibling healer's verifying row IS coverage, which
    /// is what keeps a converged fleet quiet.
    #[tokio::test]
    async fn a_live_siblings_verifying_row_is_coverage() {
        let f = fixture().await;
        let sibling = [0xC9u8; 32];
        f.put(enrollment_row(US)).await;
        let (generation_id, gen_key, _core) = f.mint_over(&[member_of(US)]).await;
        f.put(enrollment_row(THEM)).await;
        f.put(enrollment_row(sibling)).await;

        for target in [THEM, sibling] {
            let row = build_topup_wrap_v2(
                &gen_key,
                &generation_id,
                &member_of(target),
                &device_key(sibling),
                9_000,
            )
            .unwrap();
            f.put(machinery_row(
                KIND_GENERATION_WRAP,
                fauna_core::generation::wrap_cell_key_per_healer(
                    &generation_id,
                    &device_id_of(target),
                    &device_id_of(sibling),
                ),
                &row,
            ))
            .await;
        }

        let plane = f.plane();
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .expect("pass"),
            TopupPass::Current,
            "a live member's verifying rows cover the fleet — nothing republishes"
        );
    }

    // ── The signal clearing ──────────────────────────────────────────

    /// Corrupt THEM's inline wrap in place on the merged mint row — the
    /// durable case as seen from the HEALER: the victim stays in the signed
    /// member set, a wrap stays present, the poisoned variant wins the join.
    async fn corrupt_them_inline_wrap(f: &Fixture, generation_id: &[u8; 32]) -> [u8; 32] {
        let honest = live_rows(&f.store, KIND_GENERATION_MINT).await.unwrap();
        let honest_bytes = honest
            .iter()
            .find(|r| r.key == fauna_core::hex32::encode(generation_id))
            .expect("mint row")
            .value
            .clone();
        let mut rec: GenerationMintRecord =
            fauna_core::encoding::canonical_decode(&honest_bytes).unwrap();
        let GenerationMintRecord::Minted { wraps, .. } = &mut rec else {
            panic!("minted")
        };
        let victim = wraps
            .iter_mut()
            .find(|w| w.device_id == device_id_of(THEM))
            .expect("the victim's inline wrap");
        victim.wrap = vec![0xFF; victim.wrap.len()];
        let corrupted_hash = *blake3::hash(&victim.wrap).as_bytes();
        let poisoned = fauna_core::encoding::canonical_encode(&rec).unwrap();
        assert_eq!(
            fauna_core::generation::join_generation_mint(&honest_bytes, &poisoned).unwrap(),
            poisoned,
            "the poisoned variant wins the mint join"
        );
        f.put(machinery_row(
            KIND_GENERATION_MINT,
            fauna_core::hex32::encode(generation_id),
            &rec,
        ))
        .await;
        corrupted_hash
    }

    /// THEM's signal row: a target-signed `Asserted` naming `tried`.
    fn them_assertion(generation_id: &[u8; 32], at_ms: i64, tried: Vec<[u8; 32]>) -> StateEntry {
        use fauna_core::generation::{
            GenerationUnkeyableRecord, UNKEYABLE_VARIANT_ASSERTED, sign_unkeyable_as_target,
            unkeyable_cell_key,
        };
        let target_sig = sign_unkeyable_as_target(
            &device_key(THEM),
            generation_id,
            UNKEYABLE_VARIANT_ASSERTED,
            at_ms,
            &tried,
        );
        machinery_row(
            KIND_GENERATION_UNKEYABLE,
            unkeyable_cell_key(generation_id, &device_id_of(THEM)),
            &GenerationUnkeyableRecord::Asserted {
                generation_id: *generation_id,
                target_device: device_id_of(THEM),
                asserted_at_ms: at_ms,
                tried,
                target_sig,
            },
        )
    }

    /// **The clearing pin.** The durable case first — an in-place
    /// corrupted member wrap suppresses the heal (that IS the
    /// residual, asserted before the fix) — then the victim's verifying
    /// assertion clears BOTH grounds and one heal that actually opens goes
    /// through the door.
    #[tokio::test]
    async fn a_verifying_assertion_clears_the_corrupted_wrap_suppression() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let (generation_id, _key, core) = f.mint_over(&[member_of(US), member_of(THEM)]).await;
        let corrupted_hash = corrupt_them_inline_wrap(&f, &generation_id).await;

        let plane = f.plane();
        // The partition, asserted before the fix: covered-looking, no heal.
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            TopupPass::Current,
            "without the signal, the corrupted inline wrap suppresses the heal"
        );

        f.put(them_assertion(&generation_id, 8_000, vec![corrupted_hash]))
            .await;
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            TopupPass::Published(1),
            "the victim's testimony clears the suppression"
        );
        // The heal opens for the victim.
        let v2_key = fauna_core::generation::wrap_cell_key_per_healer(
            &generation_id,
            &device_id_of(THEM),
            &device_id_of(US),
        );
        let rows = f.topup_rows().await;
        let row = rows.iter().find(|r| r.key == v2_key).expect("the heal");
        let fauna_core::generation::GenerationWrapRecordV2::Wrap { wrap, .. } =
            fauna_core::encoding::canonical_decode(&row.value).unwrap();
        open_generation_key_as_device(
            &wrap,
            &derive_device_xwing_keypair(&THEM).secret,
            &generation_id,
            &device_id_of(THEM),
            &core.key_commitment,
        )
        .expect("the victim opens the heal");
    }

    /// **The anti-churn pin.** One assertion extracts at most one wrap from
    /// this healer: after the heal, the same standing assertion republishes
    /// nothing (the fresh row's hash is outside the assertion's evidence).
    #[tokio::test]
    async fn one_assertion_extracts_at_most_one_heal_from_this_healer() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let (generation_id, _key, _core) = f.mint_over(&[member_of(US), member_of(THEM)]).await;
        let corrupted_hash = corrupt_them_inline_wrap(&f, &generation_id).await;
        f.put(them_assertion(&generation_id, 8_000, vec![corrupted_hash]))
            .await;

        let plane = f.plane();
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            TopupPass::Published(1)
        );
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            TopupPass::Current,
            "the standing assertion is answered exactly once"
        );
    }

    /// A signal that does not verify under the cell's target clears nothing —
    /// forged testimony is not testimony.
    #[tokio::test]
    async fn a_forged_assertion_clears_nothing() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let (generation_id, _key, _core) = f.mint_over(&[member_of(US), member_of(THEM)]).await;
        let corrupted_hash = corrupt_them_inline_wrap(&f, &generation_id).await;

        // The assertion's fields, but signed by the WRONG key (us, not THEM).
        use fauna_core::generation::{
            GenerationUnkeyableRecord, UNKEYABLE_VARIANT_ASSERTED, sign_unkeyable_as_target,
            unkeyable_cell_key,
        };
        let forged_sig = sign_unkeyable_as_target(
            &device_key(US),
            &generation_id,
            UNKEYABLE_VARIANT_ASSERTED,
            8_000,
            &[corrupted_hash],
        );
        f.put(machinery_row(
            KIND_GENERATION_UNKEYABLE,
            unkeyable_cell_key(&generation_id, &device_id_of(THEM)),
            &GenerationUnkeyableRecord::Asserted {
                generation_id,
                target_device: device_id_of(THEM),
                asserted_at_ms: 8_000,
                tried: vec![corrupted_hash],
                target_sig: forged_sig,
            },
        ))
        .await;

        let plane = f.plane();
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            TopupPass::Current,
            "a signal that fails to verify under the target clears no suppression"
        );
    }

    /// A re-assertion naming THIS healer's current row as tried-and-failed
    /// earns exactly one fresh wrap, with a signed stamp rising past the
    /// failed row's — the join must replace, whatever the clock says.
    #[tokio::test]
    async fn a_reassertion_naming_our_row_earns_one_fresh_wrap_with_a_rising_stamp() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let (generation_id, _key, _core) = f.mint_over(&[member_of(US), member_of(THEM)]).await;
        let corrupted_hash = corrupt_them_inline_wrap(&f, &generation_id).await;
        f.put(them_assertion(&generation_id, 8_000, vec![corrupted_hash]))
            .await;

        let plane = f.plane();
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            TopupPass::Published(1)
        );
        let v2_key = fauna_core::generation::wrap_cell_key_per_healer(
            &generation_id,
            &device_id_of(THEM),
            &device_id_of(US),
        );
        let facts_of = |rows: &[StateEntry]| {
            let row = rows.iter().find(|r| r.key == v2_key).expect("row").clone();
            let fauna_core::generation::GenerationWrapRecordV2::Wrap { at_ms, wrap, .. } =
                fauna_core::encoding::canonical_decode(&row.value).unwrap();
            (at_ms, *blake3::hash(&wrap).as_bytes())
        };
        let (first_at, first_hash) = facts_of(&f.topup_rows().await);

        // The victim tried our wrap too and still cannot key (say, we are the
        // malicious healer — or our wrap was corrupted in transit): it
        // re-asserts naming BOTH.
        f.put(them_assertion(
            &generation_id,
            9_000,
            vec![corrupted_hash, first_hash],
        ))
        .await;
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            TopupPass::Published(1),
            "a re-assertion naming our current row earns one fresh wrap"
        );
        let (second_at, second_hash) = facts_of(&f.topup_rows().await);
        assert_ne!(second_hash, first_hash, "a FRESH ciphertext, not a replay");
        assert!(
            second_at > first_at,
            "the signed stamp rises past the failed row's"
        );
        // ...and once per assertion still holds.
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            TopupPass::Current
        );
    }

    /// A tiny in-memory retained-bundle custody — the production
    /// `PrincipalSlot` shape for tests that need a healer to hold a key the
    /// plane's merged rows no longer serve it.
    #[derive(Default)]
    struct MapCustody(std::sync::Mutex<std::collections::BTreeMap<[u8; 32], [u8; 32]>>);

    impl crate::generation_tip::RetainedKeyCustody for MapCustody {
        fn retained_generation_key(&self, generation: &[u8; 32]) -> Option<GenerationKey> {
            self.0
                .lock()
                .unwrap()
                .get(generation)
                .map(|b| GenerationKey::from_bytes(*b))
        }
        fn record_generation_key(&self, generation: &[u8; 32], key: &GenerationKey) {
            self.0.lock().unwrap().insert(*generation, *key.as_bytes());
        }
        fn drop_generation_key(&self, generation: &[u8; 32]) {
            self.0.lock().unwrap().remove(generation);
        }
    }

    /// A `BackupKey` vandal
    /// writes a whole forged `Minted` value at the honest key: the core names
    /// a victim the honest mint never listed (so the core does NOT derive the
    /// key it sits at) with a garbage wrap, and wins the mint join. A healer
    /// holding the generation in its retained bundle (the production custody
    /// shape) must still heal — the forged core's `member_ids` claim is
    /// nobody's coverage evidence, because nothing authenticates it.
    #[tokio::test]
    async fn probe_368_b_a_forged_core_does_not_suppress_a_custody_holding_healer() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        // The honest mint lists only US — THEM's heal would ordinarily come
        // from this pass.
        let (generation_id, gen_key, core) = f.mint_over(&[member_of(US)]).await;

        // The forgery: a whole `Minted` value at the honest key whose core
        // adds THEM to `member_ids` (different member set ⇒ different derived
        // id ⇒ the binding fails) with a garbage wrap, keeping the honest key
        // commitment so an eventual honest heal still opens at THEM.
        let honest = live_rows(&f.store, KIND_GENERATION_MINT).await.unwrap();
        let honest_bytes = honest
            .iter()
            .find(|r| r.key == fauna_core::hex32::encode(&generation_id))
            .unwrap()
            .value
            .clone();
        let mut forged_core = core.clone();
        forged_core.member_ids.push(device_id_of(THEM));
        let forged = GenerationMintRecord::Minted {
            core: forged_core,
            minter_sig: vec![0xde; 64],
            wraps: vec![fauna_core::generation::MemberWrap {
                device_id: device_id_of(THEM),
                wrap: vec![0xde; 1200],
            }],
        };
        let forged_bytes = fauna_core::encoding::canonical_encode(&forged).unwrap();
        assert_eq!(
            fauna_core::generation::join_generation_mint(&honest_bytes, &forged_bytes).unwrap(),
            forged_bytes,
            "prerequisite: the forged variant wins the mint join"
        );
        assert_ne!(
            fauna_core::generation::generation_id(match &forged {
                GenerationMintRecord::Minted { core, .. } => core,
                GenerationMintRecord::Shredded { core, .. } => core,
            })
            .unwrap(),
            generation_id,
            "prerequisite: the forged core does NOT derive the key it sits at"
        );
        f.put(machinery_row(
            KIND_GENERATION_MINT,
            fauna_core::hex32::encode(&generation_id),
            &forged,
        ))
        .await;

        // The healer holds G in its retained bundle — the production shape
        // (`account_runtime` attaches `PrincipalSlot` to the fleet plane).
        let custody = MapCustody::default();
        crate::generation_tip::RetainedKeyCustody::record_generation_key(
            &custody,
            &generation_id,
            &gen_key,
        );
        let plane = f.plane().with_generation_custody(&custody);
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            TopupPass::Published(1),
            "PROBE-368-B: a non-binding core's member list suppresses nothing — \
             the custody-holding healer heals the victim"
        );
        // ...and the heal actually opens at the victim (the forged row kept
        // the honest commitment).
        let v2_key = fauna_core::generation::wrap_cell_key_per_healer(
            &generation_id,
            &device_id_of(THEM),
            &device_id_of(US),
        );
        let rows = f.topup_rows().await;
        let row = rows.iter().find(|r| r.key == v2_key).expect("the heal");
        let fauna_core::generation::GenerationWrapRecordV2::Wrap { wrap, .. } =
            fauna_core::encoding::canonical_decode(&row.value).unwrap();
        open_generation_key_as_device(
            &wrap,
            &derive_device_xwing_keypair(&THEM).secret,
            &generation_id,
            &device_id_of(THEM),
            &core.key_commitment,
        )
        .expect("the victim opens the heal");
    }

    /// **The stamp-floor pin, timing-independent (convention 14).** Our own
    /// cell holds a verifying row stamped in the far future (a skewed clock's
    /// leftover); the victim names it tried-and-failed. The fresh heal must
    /// outrank it in the join — without the floor, the wall clock loses to
    /// the old stamp and the failed row is frozen in place forever, an
    /// infinite republish loop under the gate.
    #[tokio::test]
    async fn a_clock_regression_cannot_freeze_a_failed_heal_in_place() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let (generation_id, gen_key, _core) = f.mint_over(&[member_of(US), member_of(THEM)]).await;
        corrupt_them_inline_wrap(&f, &generation_id).await;

        // Our own failed heal, stamped far in the future.
        let far_future = i64::MAX - 10;
        let stale = build_topup_wrap_v2(
            &gen_key,
            &generation_id,
            &member_of(THEM),
            &f.writer_key,
            far_future,
        )
        .unwrap();
        let fauna_core::generation::GenerationWrapRecordV2::Wrap { wrap, .. } = &stale;
        let stale_hash = *blake3::hash(wrap).as_bytes();
        let cell = fauna_core::generation::wrap_cell_key_per_healer(
            &generation_id,
            &device_id_of(THEM),
            &device_id_of(US),
        );
        f.put(machinery_row(KIND_GENERATION_WRAP, cell.clone(), &stale))
            .await;
        f.put(them_assertion(&generation_id, 8_000, vec![stale_hash]))
            .await;

        let plane = f.plane();
        assert_eq!(
            ensure_topped_up(&f.store, &plane, &f.trust, &f.writer_key)
                .await
                .unwrap(),
            TopupPass::Published(1)
        );
        let rows = f.topup_rows().await;
        let row = rows.iter().find(|r| r.key == cell).expect("our cell");
        let fauna_core::generation::GenerationWrapRecordV2::Wrap { at_ms, wrap, .. } =
            fauna_core::encoding::canonical_decode(&row.value).unwrap();
        assert_ne!(
            *blake3::hash(&wrap).as_bytes(),
            stale_hash,
            "the fresh heal must actually displace the failed row"
        );
        assert!(
            at_ms > far_future,
            "the fresh stamp rises past the failed row's, whatever the clock says"
        );
    }
}
