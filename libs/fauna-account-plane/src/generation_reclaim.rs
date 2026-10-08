//! The **fleet-scope reclamation pass** — the production caller of
//! `fauna.account.state.retire` and the production writer of
//! `fauna.state.device-reach` (`account-data-taxonomy.md` § The generation
//! machinery → *Fleet-scope reclamation*, clauses (2) and (3); the nest's
//! retention gate is clause (1) and lives in `bins/fauna-nest`).
//!
//! # Why it exists
//!
//! The fleet scope's live-entry cap counts one row per `(item, writer)`, and
//! only the same writer re-putting the same item ever collapses one. Every
//! machinery row of a superseded generation, every row a removed device ever
//! wrote, and every top-up cell a device has long since consumed was therefore
//! permanent — the scope spent its cap at one generation per removal and one
//! device-set row per enrollment, and a whole-suite sweep's sign-out cycles
//! reached the wall in a few dozen tests.
//!
//! # What one pass does, in order
//!
//! Throughout, a generation reads `Shredded` only when its shred is
//! **authored** at this replica's view (`GenerationMintRecord::shred_is_authored`;
//! `account-data-taxonomy.md` § *Fleet-scope reclamation* → *the authored
//! shred*): an unauthored one is out of candidacy, the resolver's act, and no
//! step below retires, sweeps, forgets or drops anything because of it.
//!
//! 1. **Reach.** Publish this device's `fauna.state.device-reach` row when the
//!    set of live `Minted` generations it can key has changed — the possession
//!    evidence every retirement below rests on. Never on a cadence: a row
//!    republished per pass would ping-pong across the fleet forever.
//! 2. **Wrap cells** whose target's verifying reach
//!    lists the generation, whose target is no longer a verified member, or
//!    whose generation is `Shredded` — this device's own cells, and cells of
//!    writers that are no longer members. A live sibling's cells are its own
//!    pass's business.
//! 3. **Unkeyable cells** of this device once `Satisfied` is merged or the
//!    generation is shredded, and any cell whose target is removed. **Closed
//!    rows** (`fauna.state.generation-closed`) once merged state reads their
//!    generation `Shredded` — never merely while its mint row is absent,
//!    which a closed row arriving ahead of its mint over another leg would
//!    be: until the shred the row is what keeps the generation from being
//!    sealed under.
//! 4. **A removed device's rows**: its `Enrolled` rows, then its reach and
//!    its **device-scoped** generation-sealed rows. **Never the `Removed`
//!    evidence rows**: a nest's gate counts only the walkers bound to that
//!    nest, so evidence retired here would leave before a device bound to a
//!    linked nest had read it. They stay ordinary live rows, at the bound
//!    nest and in the relay plane, until a secondary-leg run has found them
//!    at every linked replica and retires them (`account-sync-plane.md`
//!    § The bind leg, ruling 7; [`removal_evidence`] is that arm's listing,
//!    `crate::linked_leg::retire_carried_evidence` the arm) — so the
//!    enrollment still goes first, and no reader sees one without its
//!    removal. The device-scoped rows are device-endpoints, the kinds
//!    `fauna_protocol::merge_policy::TipSealedKind::scope` classifies
//!    `TipRowScope::Device`. An account-level row the device wrote (the
//!    reception keypair, a held group root, a custody registry row, a share
//!    member's endpoints) is the account's and stays (`account-data-taxonomy.md`
//!    clause (3)(d)): retired with its writer it would be lost, so it goes
//!    only through step 5's hand-over. Only a generation no verified member
//!    can key stays pinned by such a row, with its escrow wrap: the one way
//!    back to it. A device-scoped row of a writer that is no member and was
//!    never removed — a predecessor's device after a succession — is step
//!    5's let-go arm's, under a superseded generation.
//! 5. **The re-seal pass** (clause (3)(g)), over the generations step 7
//!    would shred but for being in use — `Minted` proper ancestors of the
//!    resolved tip, every verified member's reach holding that tip. For a row
//!    sealed under one that this device can open, this replica's MERGED entry
//!    for the item (value, `merge_meta`, tombstone marker verbatim) goes
//!    back through the writer door, which seals it under the tip. **Own
//!    arm:** our row, and no row of ours at the tip's item key yet → put;
//!    step 6 retires the old row. **Hand-over arm:** a writer that is no
//!    longer a verified member, an account-level kind
//!    (`TipRowScope::Account`), and this device the generation's hander (the
//!    byte-order-min verified member whose reach lists it) → put, unless our
//!    row at the tip's item key already carries the entry; the departed
//!    writer's row is retired only once that row of ours is published, and
//!    forgotten locally when the nest confirms it gone. **Let-go arm:** the
//!    same departed writer and hander, a device-scoped kind
//!    (`TipRowScope::Device`) → no put: the row is retired outright, no
//!    removal evidence needed, and forgotten locally when the nest confirms
//!    it gone — so a generation carried across a succession is not pinned by
//!    the predecessor device's endpoints row.
//! 6. **This device's own superseded generation-sealed rows.** A re-seal
//!    under a newer tip is a NEW wire row (the v2 item key derives from the
//!    per-generation schedule, so the nest cannot collapse the old row); only
//!    the writer knows the two are one logical item — its journal holds the
//!    `(kind, key)` at the seq its relay row carries. The newest row per item
//!    stays; every older one of ours is retired once the newest is published.
//!    Without this no superseded generation ever reads as dataless (ruling
//!    clause (3f)).
//! 7. **Generations.** A `Minted` generation that is a proper ancestor of this
//!    observer's resolved tip, when every verified member's reach holds that
//!    tip (so no member still seals under the ancestor) and the nest serves no
//!    live row sealed under it, is shredded — by its minter into its own row
//!    (count-neutral), or, if the minter is removed, by the byte-order-min
//!    verified member whose reach lists it, else the byte-order-min verified
//!    member (one transient row). The nest's answer is necessary, never
//!    sufficient: **the shredder's veto** refuses while this replica holds a
//!    row sealed under the generation that it can open and that is uncovered
//!    — neither device-scoped with a departed writer, nor carried by a
//!    verified member's row at the tip's item key. Every row of a `Shredded` generation
//!    this device may compact — its mint row(s), receipts, cells — is then
//!    retired, the mint and receipt rows carrying the nest-side belt
//!    (`no_rows_sealed_under`), and the receipt retire the escrow sweep
//!    beside it (`delete_escrow_wraps`): the nest deletes the wraps it holds
//!    for the generation in the same transaction, so the holder's escrow
//!    table stays O(retained generations) rather than O(minted ever).
//! 8. **A shredded generation's relay residue** (clause (3)(h)): every relay
//!    row whose cleartext header names a generation merged state reads
//!    `Shredded` — a live sibling's superseded row, a departed writer's own
//!    retired one — is forgotten, found by the plane's generation index.
//! 9. **The predecessor arm** (clause (3)(i)), on a plane handed the retired
//!    machinery keys alone (`AccountStatePlane::with_predecessor_machinery_keys`
//!    — a successor's bound fleet plane whose host attests a predecessor):
//!    every form-v1 relay row of a writer that is no verified member, which
//!    the plane's own keys do not open and a retired machinery pair does, is
//!    that retired identity's generation-0 machinery. Any kind but the mint
//!    record is retired outright. A mint record is retired only when this
//!    device's own row for that generation carries the merged record and is
//!    published as the pass saw it (the walk's carry wrote it), or when the
//!    record or merged state reads the generation `Shredded` (that retire
//!    carries the belt); otherwise it is kept — the one row a device that
//!    brings the generation's key later still needs. Nothing opened here is
//!    merged, re-authored, vouched or pushed: the retire names the row's own
//!    blinded item key, and the relay copy is forgotten once the nest
//!    confirms it gone.
//!
//! Every retire is a verdict, never an abort: `not_yet_stable` and
//! `generation_in_use` are deferred to the next pass, an `Unsupported`
//! answer (the peer leg) stops the pass's retires (today's growth), and a transport fault
//! is reported like any other step's. A row the nest confirms retired (or
//! already gone) is also **forgotten locally** — its merged state where the
//! ruling allows (a removed device's device-set row stays: it is the removal
//! evidence), and its relay row always; a gen-0 item forgotten as dead
//! everywhere takes its relay row under EVERY writer with it, a live
//! sibling's included (clause (3)(h)), and step 8 sweeps what the plane is
//! never told about — so a long-lived replica's relay plane and per-pass
//! scans stay O(live rows) rather than O(rows ever walked), and no pass asks
//! the nest about a row it has already confirmed off the feed; a deferred
//! row is kept and asked about again.
//!
//! # What a pass may skip — the gate's watermark (ruled 2026-09-22)
//!
//! The retention gate refuses a retire while any counted walker — a marked
//! walker whose key still holds a live grant — has not walked past the row,
//! and a walker that died without signing out holds it for ever, until the
//! user removes it. Asked one row at a time, that is one refused RPC per
//! removed device's row per pass, for as long as the stranded walker stays:
//! some 240 per pass by the middle of a whole-suite sweep, on every pass, a
//! fresh sign-in's prologue included. So the feed tells the walker where the
//! gate stands — `retirable_through_seq`, the lowest counted mark, on every
//! class-2 page — the walk keeps each relay row's own feed coordinate
//! (`RelayRow::feed_seq`), and a pass **withholds** the retire of any row
//! above the watermark without asking (`ReclaimPass::withheld`). The rule is
//! exact, not a backoff: it predicts the nest's own verdict from the state
//! the walk just merged, so it costs nothing on the first pass of a fresh
//! runtime, never delays a retire the gate would accept, and changes no
//! safety argument — the nest still gates every retire it receives. A row
//! whose coordinate is unknown (a store-served leg's, one recorded before
//! the column, an own row the reply never stamped) is asked about as
//! before; a walk that carries no watermark (the peer leg) withholds nothing. The
//! sign-out's [`sever_self`] leg never withholds: it runs once, the nest's
//! answer is its only witness, and a row it leaves is the fleet's to retire
//! as a removed writer's.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use fauna_account_store::types::{RelayRow, StateEntry, WriterId};
use fauna_account_store::{backend::StoreBackend, store::AccountStore};
use fauna_core::crypto::{FleetOnlySchedule, GenerationKey};
use fauna_core::generation::{
    DeviceReachRecord, DeviceSetRecord, FleetView, GenerationMintRecord, GenerationUnkeyableRecord,
    MintCore, WrapCellKey, parse_closed_cell_key, parse_reach_cell_key, parse_unkeyable_cell_key,
    parse_wrap_cell_key, reach_cell_key, sign_device_reach,
};
use fauna_protocol::account_state::ItemClass;
use fauna_protocol::merge_policy::{
    KIND_DEVICE_REACH, KIND_DEVICE_SET, KIND_ESCROW_RECEIPT, KIND_GENERATION_CLOSED,
    KIND_GENERATION_MINT, KIND_GENERATION_UNKEYABLE, KIND_GENERATION_WRAP, LwwStamp, TipRowScope,
    TipSealedKind, retired_with_its_writer,
};
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::account_state_plane::{AccountStatePlane, ItemId, RetireOutcome};
use crate::generation_store::{live_rows, row_ref};
use crate::generation_tip::{self, GenerationTrust};
use crate::generation_topup::{WrapCoverage, wrap_coverage};

/// What one reclamation pass did (the pump's `generation_reclaim` slot).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReclaimPass {
    /// This device's reach row went through the door (the set changed).
    pub reach_published: bool,
    /// Rows the nest confirmed retired.
    pub retired: usize,
    /// Rows the nest deferred (`not_yet_stable` / `generation_in_use`) —
    /// asked again next pass.
    pub deferred: usize,
    /// Rows this pass did not ask about because their feed coordinate sits
    /// above the gate's watermark (module docs → *What a pass may skip*): the
    /// nest would have deferred them `not_yet_stable`. Kept, like a deferred
    /// row, and reconsidered next pass against the watermark the next walk
    /// reads.
    pub withheld: usize,
    /// `Shredded` markers this device wrote.
    pub shredded: usize,
    /// Relay rows this pass forgot locally as a shredded generation's residue
    /// (step 8) — nothing a peer can open, whoever wrote them.
    pub residue_forgotten: usize,
    /// The nest does not serve the retire kind: retirement was skipped this
    /// pass (the row stays — today's growth).
    pub unsupported: bool,
}

impl ReclaimPass {
    /// Nothing changed hands this pass.
    #[must_use]
    pub fn is_quiet(&self) -> bool {
        !self.reach_published
            && self.retired == 0
            && self.shredded == 0
            && self.residue_forgotten == 0
    }
}

/// Run one reclamation pass. Module docs own the decision table.
pub async fn ensure_reclaimed<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    trust: &GenerationTrust,
    writer_key: &SigningKey,
) -> Result<ReclaimPass>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let me = writer_key.verifying_key().to_bytes();
    // The fleet view, the possession evidence and the mint DAG as this
    // replica holds them — and the dead predicate over them.
    let state = ReclaimState::read(store, trust, me).await?;
    let ReclaimState {
        view,
        coverage,
        minted,
        shredded,
        parents,
        ..
    } = &state;
    let mut pass = ReclaimPass::default();

    // ── 1. Reach ────────────────────────────────────────────────────────────
    // What this device keys NOW — its reach as the rest of the fleet will read
    // it once the row below lands (`coverage` was read before it).
    let mut my_holds: BTreeSet<[u8; 32]> = BTreeSet::new();
    if view.is_verified_member(&me) {
        let mut holds: Vec<[u8; 32]> = Vec::new();
        for (id, core) in minted {
            // One generation is one unit of local work (`pass_breath` module
            // docs) — a KEM decap when the bundle does not carry it.
            crate::pass_breath::pass_breath().await;
            // The bundle first (a commitment compare, no KEM — the same order
            // the resolver's candidacy check uses), the plane's wraps behind
            // it for a generation the bundle does not carry yet.
            let retained = fleet.generation_custody().is_some_and(|c| {
                c.retained_generation_key(id)
                    .is_some_and(|key| key.commitment() == core.key_commitment)
            });
            if retained
                || generation_tip::generation_key_for(
                    store,
                    id,
                    writer_key,
                    fleet.generation_custody(),
                    Some(view),
                )
                .await?
                .is_some()
            {
                holds.push(*id);
            }
        }
        holds.sort_unstable();
        my_holds.extend(holds.iter().copied());
        let own = own_reach(store, &me).await?;
        if own.as_ref().map(|r| r.holds.as_slice()) != Some(holds.as_slice()) {
            let at_ms = (fauna_core::data::Timestamp::now_millis_or_zero() as i64)
                .max(own.map_or(i64::MIN, |r| r.at_ms.saturating_add(1)));
            let record = sign_device_reach(writer_key, at_ms, holds);
            let stamp = LwwStamp { at_ms, writer: me };
            fleet
                .put(
                    &ItemId {
                        kind: KIND_DEVICE_REACH.into(),
                        key: reach_cell_key(&me),
                    },
                    fauna_core::encoding::canonical_encode(&record)?,
                    Some(stamp.encode()?),
                )
                .await
                .context("publishing this device's reach")?;
            pass.reach_published = true;
        }
    }

    // Every retire names a row by the coordinates this replica's relay plane
    // holds for it (`publish` records our own rows there too), looked up per
    // item — never by paging the whole plane, which grows with every row
    // ever walked.
    // The gate's watermark as this pass's own walk read it (module docs →
    // *What a pass may skip*).
    let retirer_watermark = fleet.retirable_through_seq();
    let mut retirer = Retirer {
        fleet,
        me,
        view,
        pass: &mut pass,
        withhold_above: retirer_watermark,
    };

    // ── 2. Wrap cells, 3. unkeyable cells and closed rows ───────────────────
    // A dead cell ([`ReclaimState::dead_item`]) is retired at the nest where
    // this device may (its own, a removed writer's) AND forgotten locally
    // whoever wrote it, merged state and relay row alike
    // ([`Retirer::forget_dead_item`]): dead everywhere by the ruling's own
    // rule, and a live sibling's copy would otherwise sit in this replica's
    // merged state for ever, re-verified by every pass that scans the kind
    // (the top-up, unkeyable and reclaim passes all do) — the quadratic term
    // a long-lived replica cannot afford. Local only: no journal row, nothing
    // published; the feed re-presents it on a reconcile if the nest still
    // serves it, and the next pass forgets it again.
    for kind in [
        KIND_GENERATION_WRAP,
        KIND_GENERATION_UNKEYABLE,
        KIND_GENERATION_CLOSED,
    ] {
        for entry in live_rows(store, kind).await? {
            let Some(only_writer) = state.dead_item(&entry) else {
                continue;
            };
            if retirer
                .retire_gen0(kind, &entry.key, only_writer.as_ref(), Belt::None)
                .await?
            {
                retirer
                    .forget_dead_item(store, kind, &entry.key, only_writer.as_ref())
                    .await?;
            }
        }
    }

    // ── 4. Removed devices ──────────────────────────────────────────────────
    let removed: Vec<[u8; 32]> = view.removed().map(|(id, _)| *id).collect();
    for id in removed {
        // One removed device is one unit (`pass_breath` module docs): by the
        // middle of a whole-suite sweep this loop used to refuse some 240
        // retires per pass as `not_yet_stable` — the rows a stranded walker
        // holds, now withheld against the gate's watermark instead of asked.
        crate::pass_breath::pass_breath().await;
        // Its reach is nobody's coverage now (the pass only reads a verified
        // member's); forgotten locally like a dead cell once it is off the
        // feed. The device-set row itself stays: it IS the removal evidence,
        // and the lattice needs it.
        if retirer.retire_removed_device(&id).await? {
            retirer
                .forget_dead_item(store, KIND_DEVICE_REACH, &reach_cell_key(&id), Some(&id))
                .await?;
        }
    }

    // The tip, and the generations it supersedes — one value for the re-seal
    // (step 5) and the shred (step 7). A generation is reclaimable when it is
    // a `Minted` proper ancestor of the tip AND every verified member's reach
    // holds that tip, so no member still seals under it and every member can
    // open a row re-sealed under the tip.
    let resolution =
        generation_tip::resolve_tip(store, trust, writer_key, fleet.generation_custody()).await?;
    let tip = resolution.tip.as_ref();
    let everyone_holds_tip = tip.is_some_and(|t| {
        view.wrap_targets().all(|m| {
            m.device_id == me
                || coverage
                    .reach
                    .get(&m.device_id)
                    .is_some_and(|r| r.holds(&t.generation_id))
        })
    });
    let reclaimable: BTreeSet<[u8; 32]> = match tip {
        Some(t) if everyone_holds_tip => ancestors_of(&t.generation_id, parents)
            .into_iter()
            .filter(|g| minted.contains_key(g))
            .collect(),
        _ => BTreeSet::new(),
    };
    // The key the tip seals under — what names a row "at the tip's item key".
    // Only asked for when something is reclaimable (it can cost a KEM).
    let tip_key = match tip {
        Some(t) if !reclaimable.is_empty() => {
            match generation_tip::key_for_tip(store, t, writer_key, fleet.generation_custody())
                .await
            {
                Ok(k) => Some(k),
                Err(e) => {
                    // The resolver only names a tip this observer keys, so
                    // this is unexpected; nothing is re-sealed and nothing
                    // shreds this pass (the veto cannot be evaluated).
                    tracing::warn!("generation reclaim: cannot key the resolved tip ({e:#})");
                    None
                }
            }
        }
        _ => None,
    };
    // The byte-order-min verified member whose verifying reach lists `g` —
    // `g`'s hander (clause (3)(g)) and first choice of stand-in shredder
    // (clause (3)(e)). This device's own reach is read as step 1 just stated
    // it.
    let reach_lists = |m: &[u8; 32], g: &[u8; 32]| {
        if *m == me {
            my_holds.contains(g)
        } else {
            coverage.reach.get(m).is_some_and(|r| r.holds(g))
        }
    };
    let hander_of = |g: &[u8; 32]| {
        view.wrap_targets()
            .map(|m| m.device_id)
            .filter(|m| reach_lists(m, g))
            .min()
    };

    // ── 5. The re-seal pass ─────────────────────────────────────────────────
    if let Some(tip_key) = &tip_key {
        let handing: BTreeSet<[u8; 32]> = reclaimable
            .iter()
            .filter(|g| hander_of(g) == Some(me))
            .copied()
            .collect();
        reseal_superseded(store, &mut retirer, tip_key, &reclaimable, &handing).await?;
    }

    // ── 6. Own generation-sealed rows under a superseded generation ─────────
    // A re-seal under a newer tip (device-endpoints on every tip change, and
    // every GenerationTip kind on its own re-publish or step 5's own arm) is
    // a NEW wire row: the
    // v2 item key derives from the per-generation schedule, so the nest
    // cannot collapse the old row under the new one — only its writer knows
    // they are one logical item. Our journal does: it holds `(kind, key)` at
    // the writer seq our relay row carries. The newest seq per item stays;
    // every older generation-sealed row of ours is retired (and, once off the
    // feed, forgotten from our relay plane) — once the newest is PUBLISHED as
    // this pass saw it (in its listing, or acked since — `account-sync-plane.md`
    // § The bind leg, ruling 1(d); never our frontier slot alone, which says
    // only that SOME nest once acked it): a newer row this nest does not hold
    // carries the item to nobody here, and retiring the older one then would
    // leave the nest serving the item to no one. Without this, every
    // superseded generation stays "in use" for ever and nothing below ever
    // shreds.
    {
        let own = fleet.relay_rows_of_writer(&WriterId(me)).await?;
        let mut newest: BTreeMap<ItemId, (u64, [u8; 32])> = BTreeMap::new();
        let mut v2_rows: Vec<(ItemId, u64, [u8; 32], Option<u64>)> = Vec::new();
        for row in &own {
            let is_v2 = row
                .entry
                .as_deref()
                .and_then(fauna_core::account_entry_crypto::peek_generation_id)
                .is_some();
            if !is_v2 {
                continue;
            }
            let (Some(item), Ok(item_key)) = (
                fleet.own_journal_item(row.writer_seq).await?,
                <[u8; 32]>::try_from(row.item_key.as_slice()),
            ) else {
                continue;
            };
            let slot = newest
                .entry(item.clone())
                .or_insert((row.writer_seq, item_key));
            if row.writer_seq > slot.0 {
                *slot = (row.writer_seq, item_key);
            }
            v2_rows.push((item, row.writer_seq, item_key, row.feed_seq));
        }
        for (item, seq, item_key, feed_seq) in v2_rows {
            if newest.get(&item).is_some_and(|(n, newest_key)| {
                *n > seq && fleet.listed_at_or_above(&WriterId(me), newest_key, *n)
            }) {
                match retirer
                    .retire(&item_key, &WriterId(me), seq, feed_seq, Belt::None)
                    .await?
                {
                    RetireOutcome::Retired | RetireOutcome::Gone => {
                        fleet.relay_forget(&WriterId(me), &item_key).await?;
                    }
                    _ => {}
                }
            }
        }
    }

    // ── 7. Generations ──────────────────────────────────────────────────────
    if let Some(tip_key) = &tip_key {
        let minimum_member = view.wrap_targets().map(|m| m.device_id).min();
        for (g, core) in minted {
            if !reclaimable.contains(g) {
                continue;
            }
            let shredder = if core.minter == me {
                true
            } else if !view.is_verified_member(&core.minter) {
                // The minter is gone: exactly one member writes the marker,
                // deterministically — the byte-order-min member that keys
                // G (so its veto below can open G's rows), else the
                // byte-order-min member.
                hander_of(g).or(minimum_member) == Some(me)
            } else {
                false // the minter's own pass shreds its own row
            };
            if !shredder {
                continue;
            }
            // The nest's answer is necessary…
            if fleet.any_row_sealed_under(g).await? {
                continue;
            }
            // …never sufficient: the shredder's veto (clause (3)(e)).
            if let Some(uncovered) =
                uncovered_row_under(store, retirer.fleet, retirer.view, tip_key, g).await?
            {
                tracing::info!(
                    generation = %fauna_core::hex32::encode(g),
                    kind = %uncovered,
                    "generation reclaim: shred vetoed — this replica still holds an uncovered \
                     row sealed under the generation"
                );
                continue;
            }
            {
                // Authored: signed by this device over the recomputed id, the
                // ONE production shape since the ruling — a
                // consumer drops a key or retires a row only on a row
                // `shred_is_authored` admits (`GenerationMintRecord::Shredded`
                // docs).
                let record = fauna_core::generation::sign_shred(
                    writer_key,
                    core.clone(),
                    fauna_core::data::Timestamp::now_millis_or_zero() as i64,
                )?;
                fleet
                    .put(
                        &ItemId {
                            kind: KIND_GENERATION_MINT.into(),
                            key: fauna_core::hex32::encode(g),
                        },
                        fauna_core::encoding::canonical_encode(&record)?,
                        None,
                    )
                    .await
                    .with_context(|| {
                        format!(
                            "shredding dataless superseded generation {}",
                            fauna_core::hex32::encode(g)
                        )
                    })?;
                tracing::info!(
                    generation = %fauna_core::hex32::encode(g),
                    "generation reclaim: shredded a dataless superseded generation"
                );
                retirer.pass.shredded += 1;
            }
        }
    }
    let receipts = live_rows(store, KIND_ESCROW_RECEIPT).await?;
    for g in shredded {
        // The device-side half of the crypto-shred, eventual: the walk's hook
        // drops the key as the shred merges, but may have judged it before
        // the shredder's own enrollment merged (per-writer frontiers
        // interleave). Idempotent.
        if let Some(custody) = fleet.generation_custody() {
            custody.drop_generation_key(g);
        }
        let g_hex = fauna_core::hex32::encode(g);
        retirer
            .retire_gen0(KIND_GENERATION_MINT, &g_hex, None, Belt::Dataless(g))
            .await?;
        // Exactly the receipts [`ReclaimState::dead_item`] calls dead.
        for receipt in receipts
            .iter()
            .filter(|r| receipt_generation(&r.key) == Some(*g))
        {
            // A shredded generation's receipt acks nothing any more; the
            // resolver verifies every receipt row it holds on every
            // resolution, so the dead ones go the way of dead cells — and
            // the holder's escrow wrap goes with the receipt that named it
            // (clause (3e)'s sweep), the belt proving the generation dataless
            // once more in the same transaction.
            if retirer
                .retire_gen0(KIND_ESCROW_RECEIPT, &receipt.key, None, Belt::Sweep(g))
                .await?
            {
                retirer
                    .forget_dead_item(store, KIND_ESCROW_RECEIPT, &receipt.key, None)
                    .await?;
            }
        }
    }

    // ── 8. A shredded generation's relay residue ────────────────────────────
    // Every relay row whose cleartext header names a generation merged state
    // reads `Shredded` is dead everywhere: every device dropped the key on
    // merging the marker, so no reader opens it and nothing a peer is served
    // from it is worth a byte. A live sibling's superseded rows land here —
    // this plane is never told about their retirement (clause (3)(f) retires
    // them at the nest) — and so does every row a departed writer retired
    // itself. Forgotten locally, no journal row, nothing published, and
    // looked up by index over the generations this plane still holds rows
    // under: the probe follows the live plane, never the fleet's history.
    // Idempotent, so a row a reconcile or a lagging peer serves again is
    // re-recorded (the record point stays unconditional) and goes again here.
    for g in store.relay_generations(fleet.scope()).await? {
        if state.generation_dead(&g) {
            let n = store.relay_forget_sealed_under(fleet.scope(), &g).await?;
            pass.residue_forgotten += usize::try_from(n).unwrap_or(usize::MAX);
        }
    }

    // ── 9. The predecessor arm ──────────────────────────────────────────────
    if fleet.has_predecessor_machinery_keys() {
        let mut retirer = Retirer {
            fleet,
            me,
            view,
            pass: &mut pass,
            withhold_above: retirer_watermark,
        };
        retire_predecessor_machinery(store, &mut retirer, shredded).await?;
    }

    if !pass.is_quiet() || pass.withheld > 0 || pass.deferred > 0 {
        tracing::info!(
            reach_published = pass.reach_published,
            retired = pass.retired,
            deferred = pass.deferred,
            withheld = pass.withheld,
            retirable_through_seq = ?retirer_watermark,
            shredded = pass.shredded,
            residue_forgotten = pass.residue_forgotten,
            unsupported = pass.unsupported,
            "generation reclaim: pass"
        );
    }
    Ok(pass)
}

/// What the reclamation pass reads from this replica's merged state before it
/// acts — the verified fleet view, the possession evidence, the mint DAG —
/// and the ONE home of its **dead** predicate: the set
/// `account-data-taxonomy.md` § The generation machinery → *Fleet-scope
/// reclamation*, clause (3)(h) owns, which the pass forgets and the bind
/// leg's publish diff declines to push (`account-sync-plane.md` § The bind
/// leg, ruling 1(b); `crate::publish_diff`). Both callers ask the same
/// methods, so the skipped set is exactly the forgotten set.
pub(crate) struct ReclaimState {
    me: [u8; 32],
    view: FleetView,
    coverage: WrapCoverage,
    /// Live generations: canonical, id-bound cores.
    minted: BTreeMap<[u8; 32], MintCore>,
    /// Generations merged state reads **authored** `Shredded` at `view`.
    shredded: BTreeSet<[u8; 32]>,
    /// Every edge of the mint DAG, shredded generations' included.
    parents: BTreeMap<[u8; 32], Vec<[u8; 32]>>,
}

impl ReclaimState {
    /// Read it for the device `me`.
    pub(crate) async fn read<B: StoreBackend>(
        store: &AccountStore<B>,
        trust: &GenerationTrust,
        me: [u8; 32],
    ) -> Result<Self> {
        let device_rows = live_rows(store, KIND_DEVICE_SET).await?;
        let view = FleetView::build(&trust.root, device_rows.iter().map(row_ref));
        let coverage = wrap_coverage(store, &view).await?;
        let mut minted: BTreeMap<[u8; 32], MintCore> = BTreeMap::new();
        let mut shredded: BTreeSet<[u8; 32]> = BTreeSet::new();
        let mut parents: BTreeMap<[u8; 32], Vec<[u8; 32]>> = BTreeMap::new();
        for entry in live_rows(store, KIND_GENERATION_MINT).await? {
            let Ok(id) = fauna_core::hex32::decode(&entry.key) else {
                continue;
            };
            if fauna_core::hex32::encode(&id) != entry.key {
                continue;
            }
            let Ok(record) =
                fauna_core::encoding::canonical_decode::<GenerationMintRecord>(&entry.value)
            else {
                continue;
            };
            match record {
                GenerationMintRecord::Minted { core, .. } => {
                    if fauna_core::generation::generation_id(&core).ok() == Some(id) {
                        parents.insert(id, core.parents.clone());
                        minted.insert(id, core);
                    }
                }
                ref shred @ GenerationMintRecord::Shredded { ref core, .. } => {
                    parents.insert(id, core.parents.clone());
                    // Only an AUTHORED shred makes a generation dead here
                    // (`account-data-taxonomy.md` § *Fleet-scope
                    // reclamation* → *the authored shred*): this one set is
                    // every destructive arm's — step 7's retires, receipt
                    // sweep and key drop, step 8's relay residue, the
                    // publish diff's dead rows. An unauthored one is out of
                    // candidacy (the resolver's act) and nothing more.
                    if shred.shred_is_authored(&view) == Ok(id) {
                        shredded.insert(id);
                    }
                }
            }
        }
        Ok(Self {
            me,
            view,
            coverage,
            minted,
            shredded,
            parents,
        })
    }

    /// The verified fleet view the state was read under.
    pub(crate) fn view(&self) -> &FleetView {
        &self.view
    }

    /// Whether merged state reads `g` `Shredded` (clause (3)(h)'s first arm).
    pub(crate) fn is_shredded(&self, g: &[u8; 32]) -> bool {
        self.generation_dead(g)
    }

    /// `g`'s live, id-bound `Minted` core, when merged state carries one.
    pub(crate) fn minted_core(&self, g: &[u8; 32]) -> Option<&MintCore> {
        self.minted.get(g)
    }

    /// Whether some verified member other than `self.me` lists `g` in its
    /// verifying reach row — the possession evidence every arm of the pass
    /// reads, and what keeps a generation from reading dead (clause (3)(j)).
    pub(crate) fn a_sibling_reach_lists(&self, g: &[u8; 32]) -> bool {
        self.view.wrap_targets().any(|m| {
            m.device_id != self.me
                && self
                    .coverage
                    .reach
                    .get(&m.device_id)
                    .is_some_and(|r| r.holds(g))
        })
    }

    /// The first arm of clause (3)(h): merged state reads `g` `Shredded`, so
    /// every row sealed under it is dead (step 8).
    fn generation_dead(&self, g: &[u8; 32]) -> bool {
        self.shredded.contains(g)
    }

    /// The second arm of clause (3)(h): is the live merged gen-0 entry
    /// `entry` an item the pass forgets as dead everywhere? `Some(writer)`
    /// names the one writer whose row of it goes (the cell's healer or
    /// target, the removed device), `Some(None)` every writer's; `None`: not
    /// dead. A wrap cell whose target's reach lists the generation, whose
    /// target is no longer a verified member, or whose generation is
    /// shredded (step 2); an unkeyable cell of this device once `Satisfied`
    /// or shredded, and any whose target is not a verified member (step 3); a
    /// removed device's reach (step 4); a receipt of a shredded generation
    /// (step 7's sweep); a closed row of a shredded generation, under every
    /// writer (step 3 — live until then, whatever else merged state holds
    /// of the generation). **A `Shredded` generation's own mint row is in
    /// neither arm**: no pass forgets a sibling's copy of it, which leaves by
    /// the diff's refused put (`account-sync-plane.md` § The bind leg,
    /// ruling 1(c)).
    fn dead_item(&self, entry: &StateEntry) -> Option<Option<[u8; 32]>> {
        match entry.kind.as_str() {
            KIND_GENERATION_WRAP => {
                let WrapCellKey {
                    generation_id,
                    target_device,
                    healer,
                } = parse_wrap_cell_key(&entry.key)?;
                let redundant = self.generation_dead(&generation_id)
                    || !self.view.is_verified_member(&target_device)
                    || self
                        .coverage
                        .reach
                        .get(&target_device)
                        .is_some_and(|r| r.holds(&generation_id));
                redundant.then_some(Some(healer))
            }
            KIND_GENERATION_UNKEYABLE => {
                let (generation_id, target) = parse_unkeyable_cell_key(&entry.key)?;
                let dead = if target == self.me {
                    self.generation_dead(&generation_id)
                        || fauna_core::encoding::canonical_decode::<GenerationUnkeyableRecord>(
                            &entry.value,
                        )
                        .is_ok_and(|r| {
                            r.verifies_at(&generation_id, &target)
                                && matches!(r, GenerationUnkeyableRecord::Satisfied { .. })
                        })
                } else {
                    !self.view.is_verified_member(&target) || self.generation_dead(&generation_id)
                };
                dead.then_some(Some(target))
            }
            KIND_DEVICE_REACH => {
                let device = parse_reach_cell_key(&entry.key)?;
                self.view.is_excluded(&device).then_some(Some(device))
            }
            KIND_ESCROW_RECEIPT => receipt_generation(&entry.key)
                .filter(|g| self.generation_dead(g))
                .map(|_| None),
            KIND_GENERATION_CLOSED => parse_closed_cell_key(&entry.key)
                .filter(|g| self.generation_dead(g))
                .map(|_| None),
            _ => None,
        }
    }

    /// Is `row` — a relay row this replica holds, with its `plaintext` when
    /// this device opens it — one the pass forgets as dead from this
    /// replica's merged state alone? By its cleartext header (sealed under a
    /// shredded generation, whose key no device keeps — so it is decided
    /// without opening), or as a copy of a dead gen-0 item this replica's
    /// merged state holds live.
    pub(crate) async fn relay_row_dead<B: StoreBackend>(
        &self,
        store: &AccountStore<B>,
        row: &RelayRow,
        plaintext: Option<&fauna_core::account_entry_crypto::EntryPlaintext>,
    ) -> Result<bool> {
        match (
            row.entry
                .as_deref()
                .and_then(fauna_core::account_entry_crypto::peek_generation_id),
            plaintext,
        ) {
            (Some(g), _) => Ok(self.generation_dead(&g)),
            (None, None) => Ok(false),
            (None, Some(plaintext)) => Ok(store
                .state(&plaintext.kind, &plaintext.key)
                .await?
                .filter(|merged| !merged.tombstone)
                .and_then(|merged| self.dead_item(&merged))
                .is_some_and(|only| only.is_none_or(|w| w == row.writer.0))),
        }
    }
}

/// The generation an escrow receipt's cell key names (`<generation>/…`).
pub(crate) fn receipt_generation(key: &str) -> Option<[u8; 32]> {
    let (g_hex, _) = key.split_once('/')?;
    let g = fauna_core::hex32::decode(g_hex).ok()?;
    (fauna_core::hex32::encode(&g) == g_hex).then_some(g)
}

/// The generation belt a retire carries (ruling clause (3e)).
#[derive(Clone, Copy)]
enum Belt<'a> {
    /// No belt.
    None,
    /// `no_rows_sealed_under: G` — refused `generation_in_use` while the nest
    /// serves a live row sealed under G.
    Dataless(&'a [u8; 32]),
    /// [`Belt::Dataless`], and the retire that lands takes the nest's escrow
    /// wraps of G with it (`delete_escrow_wraps`) — the receipt retire of a
    /// shredded generation, and nothing else.
    Sweep(&'a [u8; 32]),
}

/// The retire side of a pass: which rows this device may compact, and the
/// bookkeeping of what the nest answered.
struct Retirer<'a, 'p, B: StoreBackend, R: RpcRequester> {
    fleet: &'a AccountStatePlane<'p, B, R>,
    me: [u8; 32],
    view: &'a FleetView,
    pass: &'a mut ReclaimPass,
    /// The gate's watermark (module docs → *What a pass may skip*): a row
    /// whose feed coordinate is known and above it is withheld, never sent.
    /// `None` withholds nothing — the peer leg, a walk that carried none,
    /// and the sign-out's [`sever_self`] leg by construction.
    withhold_above: Option<u64>,
}

impl<B, R> Retirer<'_, '_, B, R>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    /// May this device compact a row `writer` authored? Its own, or a
    /// writer that is no longer a verified member (removed, unknown,
    /// unverifiable) — never a live sibling's.
    fn may_compact(&self, writer: &[u8; 32]) -> bool {
        *writer == self.me || !self.view.is_verified_member(writer)
    }

    /// Forget, locally, a gen-0 item the pass knows is dead everywhere and has
    /// settled at the nest ([`Self::retire_gen0`] returned `true`): its merged
    /// state, and its relay row under every writer — `only_writer`'s alone
    /// when given — not only the rows this device retired. A live sibling's
    /// row is off this replica's merged state from here on, so no later pass
    /// would ever name its relay row again: kept, it would sit in the plane
    /// for good, one per cell and receipt the fleet ever wrote. No journal
    /// row, nothing published; a reconcile that re-serves the row brings both
    /// back and the next pass forgets them again.
    async fn forget_dead_item(
        &self,
        store: &AccountStore<B>,
        kind: &str,
        key: &str,
        only_writer: Option<&[u8; 32]>,
    ) -> Result<()> {
        store.forget_state(kind, key).await?;
        let Some(item_key) = self.fleet.gen0_item_key(kind, key) else {
            return Ok(());
        };
        for row in self.fleet.relay_rows_at(&item_key).await? {
            if only_writer.is_none_or(|only| *only == row.writer.0) {
                self.fleet.relay_forget(&row.writer, &item_key).await?;
            }
        }
        Ok(())
    }

    /// Retire every live row of the gen-0 item `(kind, key)` this device may
    /// compact — or, with `only_writer`, that one writer's row alone. Returns
    /// whether every row it named is now off the feed (retired, or already
    /// gone): the caller's licence to forget the item locally. A row the nest
    /// deferred keeps the item in merged state, so the next pass asks again —
    /// forgetting a deferred row would leave it live on the feed for ever.
    async fn retire_gen0(
        &mut self,
        kind: &str,
        key: &str,
        only_writer: Option<&[u8; 32]>,
        belt: Belt<'_>,
    ) -> Result<bool> {
        let Some(item_key) = self.fleet.gen0_item_key(kind, key) else {
            return Ok(true);
        };
        let targets: Vec<(WriterId, u64, Option<u64>)> = self
            .fleet
            .relay_rows_at(&item_key)
            .await?
            .iter()
            .filter(|r| only_writer.is_none_or(|only| *only == r.writer.0))
            .filter(|r| self.may_compact(&r.writer.0))
            .map(|r| (r.writer, r.writer_seq, r.feed_seq))
            .collect();
        let mut settled = true;
        for (writer, seq, feed_seq) in targets {
            match self.retire(&item_key, &writer, seq, feed_seq, belt).await? {
                RetireOutcome::Retired | RetireOutcome::Gone => {
                    // Off the feed: nothing a peer should be served, and
                    // nothing a later pass should ask about again (the
                    // per-pass `Gone` round trip a kept relay row costs).
                    self.fleet.relay_forget(&writer, &item_key).await?;
                }
                _ => settled = false,
            }
        }
        Ok(settled)
    }

    /// One retire, or the verdict the pass already knows: a row whose feed
    /// coordinate sits above the gate's watermark is withheld as
    /// [`RetireOutcome::NotYetStable`] without a request (module docs →
    /// *What a pass may skip*) — the same verdict the nest would return, so
    /// every caller keeps the row exactly as it keeps a deferred one.
    async fn retire(
        &mut self,
        item_key: &[u8; 32],
        writer: &WriterId,
        seq: u64,
        feed_seq: Option<u64>,
        belt: Belt<'_>,
    ) -> Result<RetireOutcome> {
        if self.pass.unsupported {
            return Ok(RetireOutcome::Unsupported);
        }
        if crate::account_state_plane::withheld_by_gate(self.withhold_above, feed_seq) {
            self.pass.withheld += 1;
            return Ok(RetireOutcome::NotYetStable);
        }
        let (no_rows_sealed_under, delete_escrow_wraps) = match belt {
            Belt::None => (None, false),
            Belt::Dataless(g) => (Some(g), false),
            Belt::Sweep(g) => (Some(g), true),
        };
        let outcome = self
            .fleet
            .retire(
                item_key,
                writer,
                seq,
                no_rows_sealed_under,
                delete_escrow_wraps,
            )
            .await?;
        match outcome {
            RetireOutcome::Retired => self.pass.retired += 1,
            RetireOutcome::Gone => {}
            RetireOutcome::NotYetStable | RetireOutcome::GenerationInUse => {
                self.pass.deferred += 1;
            }
            RetireOutcome::Unsupported => self.pass.unsupported = true,
        }
        Ok(outcome)
    }

    /// Clause (3d): a removed device's rows — its `Enrolled` rows, its reach
    /// and its device-scoped rows. **Never the `Removed` evidence rows**
    /// (module docs, step 4): they stay live here and in the relay plane for
    /// the secondary leg to carry and retire. Returns whether its reach row
    /// is off the feed (the caller's licence to forget it locally).
    async fn retire_removed_device(&mut self, id: &[u8; 32]) -> Result<bool> {
        let id_hex = fauna_core::hex32::encode(id);
        // Its device-set item: every writer's row for it, opened to tell an
        // enrollment from the removal evidence.
        if let Some(item_key) = self.fleet.gen0_item_key(KIND_DEVICE_SET, &id_hex) {
            let mut enrolled: Vec<(WriterId, u64, Option<u64>)> = Vec::new();
            for row in &self.fleet.relay_rows_at(&item_key).await? {
                let Some(plaintext) = self.fleet.open_relay_row(row).await? else {
                    continue;
                };
                if let Ok(DeviceSetRecord::Enrolled { .. }) =
                    fauna_core::encoding::canonical_decode::<DeviceSetRecord>(&plaintext.value)
                {
                    enrolled.push((row.writer, row.writer_seq, row.feed_seq));
                }
            }
            for (writer, seq, feed_seq) in enrolled {
                if let RetireOutcome::Retired | RetireOutcome::Gone = self
                    .retire(&item_key, &writer, seq, feed_seq, Belt::None)
                    .await?
                {
                    // The relay row only: the merged device-set row IS
                    // the removal evidence the lattice needs and stays.
                    self.fleet.relay_forget(&writer, &item_key).await?;
                }
            }
        }
        // Its reach row.
        let reach_settled = self
            .retire_gen0(KIND_DEVICE_REACH, &id_hex, Some(id), Belt::None)
            .await?;
        // Its device-scoped generation-sealed rows (device-endpoints) — never
        // an account-level row it happened to write.
        self.retire_device_scoped_rows(id).await?;
        Ok(reach_settled)
    }

    /// Retire `writer`'s live **device-scoped** generation-sealed rows — the
    /// kinds `fauna_protocol::merge_policy::retired_with_its_writer` names
    /// (`TipSealedKind::scope` owns the classification): today its
    /// device-endpoints rows, which clauses (3)(d) and (4) retire with the
    /// device. Each row is opened to learn its kind; a row this replica
    /// cannot open is left alone, never guessed at. An account-level row the
    /// departing device wrote — the reception keypair, a held group root, a
    /// custody registry row, a share member's endpoints — is the account's
    /// and stays: retiring it with its writer would lose it for the whole
    /// account, so only the re-seal pass's hand-over retires it, behind a
    /// published copy of its own ([`reseal_superseded`]). A device-scoped row
    /// of a writer that was never removed is not this arm's: the re-seal
    /// pass's let-go arm retires it under a superseded generation, the
    /// generation's hander asking. A row the nest confirms
    /// off the feed is forgotten from the relay plane, so later passes stop
    /// opening it.
    async fn retire_device_scoped_rows(&mut self, writer: &[u8; 32]) -> Result<()> {
        let writer = WriterId(*writer);
        for row in self.fleet.relay_rows_of_writer(&writer).await? {
            // Only a form-v2 row can be of a `GenerationTip` kind (v1 is the
            // gen-0 form), so a v1 row is skipped before any AEAD.
            if row
                .entry
                .as_deref()
                .and_then(fauna_core::account_entry_crypto::peek_generation_id)
                .is_none()
            {
                continue;
            }
            let Ok(item_key) = <[u8; 32]>::try_from(row.item_key.as_slice()) else {
                continue;
            };
            let Some(plaintext) = self.fleet.open_relay_row(&row).await? else {
                continue;
            };
            if !retired_with_its_writer(&plaintext.kind) {
                continue;
            }
            match self
                .retire(&item_key, &writer, row.writer_seq, row.feed_seq, Belt::None)
                .await?
            {
                RetireOutcome::Retired | RetireOutcome::Gone => {
                    self.fleet.relay_forget(&writer, &item_key).await?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// One row of **removal evidence** as this replica's relay plane holds it
/// (`account-sync-plane.md` § The bind leg, ruling 7): a row in a device-set
/// item that opens as `Removed`, for a device merged state reads removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvidenceRow {
    pub writer: WriterId,
    pub item_key: [u8; 32],
    pub writer_seq: u64,
    /// The bound nest's serve coordinate for the row, once a walk of its feed
    /// or a put it acked has stamped one. `Some` is the removed-device arm's
    /// evidence that the bound nest has held the row — a linked nest's
    /// reconcile records a row and stamps none — and the coordinate a retire
    /// at the bound nest is withheld against (module docs → *What a pass may
    /// skip*).
    pub feed_seq: Option<u64>,
}

/// Every removal-evidence row the relay plane of `fleet`'s scope holds — what
/// the secondary leg's removed-device arm carries and retires
/// (`crate::linked_leg::retire_carried_evidence`). The pass above retires
/// none of them.
///
/// # Errors
///
/// Store I/O.
pub async fn removal_evidence<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    trust: &GenerationTrust,
) -> Result<Vec<EvidenceRow>>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let device_rows = live_rows(store, KIND_DEVICE_SET).await?;
    let view = FleetView::build(&trust.root, device_rows.iter().map(row_ref));
    let mut evidence = Vec::new();
    for (id, _) in view.removed() {
        let Some(item_key) = fleet.gen0_item_key(KIND_DEVICE_SET, &fauna_core::hex32::encode(id))
        else {
            continue;
        };
        for row in &fleet.relay_rows_at(&item_key).await? {
            let Some(plaintext) = fleet.open_relay_row(row).await? else {
                continue;
            };
            if let Ok(DeviceSetRecord::Removed { .. }) =
                fauna_core::encoding::canonical_decode::<DeviceSetRecord>(&plaintext.value)
            {
                evidence.push(EvidenceRow {
                    writer: row.writer,
                    item_key,
                    writer_seq: row.writer_seq,
                    feed_seq: row.feed_seq,
                });
            }
        }
    }
    Ok(evidence)
}

/// Clause (4) — the sign-out's plane leg: this device writes its own
/// `Removed` row (superseding its `Enrolled` row at the nest — same item,
/// same writer — under the kind's "an enrolled non-removed device may remove
/// itself"), then retires its own reach row and its own device-scoped
/// generation-sealed rows (device-endpoints) — never an account-level row it
/// wrote, which the account keeps ([`Retirer::retire_device_scoped_rows`]).
/// Best-effort like the grant retirement that follows it: an
/// answer the nest defers or refuses leaves the row for the fleet's own
/// passes (a removed writer's rows are anyone's to compact), and a transport
/// fault is the `Err` the caller logs. Must run while the runtime still holds
/// the writer key and its session — before the grant revoke.
pub async fn sever_self<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    writer_key: &SigningKey,
) -> Result<ReclaimPass>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let me = writer_key.verifying_key().to_bytes();
    let me_hex = fauna_core::hex32::encode(&me);
    let already_removed = store
        .state(KIND_DEVICE_SET, &me_hex)
        .await?
        .and_then(|e| fauna_core::encoding::canonical_decode::<DeviceSetRecord>(&e.value).ok())
        .is_some_and(|r| matches!(r, DeviceSetRecord::Removed { .. }));
    if !already_removed {
        // The writer's last word: a drain refused ahead of it (a sign-out
        // cut the pass inside a put whose reply died — a replay the nest
        // refuses) must not hold the severance back
        // ([`AccountStatePlane::put_last_word`]).
        fleet
            .put_last_word(
                &ItemId {
                    kind: KIND_DEVICE_SET.into(),
                    key: me_hex.clone(),
                },
                fauna_core::encoding::canonical_encode(&DeviceSetRecord::Removed {
                    removed_at_ms: fauna_core::data::Timestamp::now_millis_or_zero() as i64,
                    removed_by: me,
                })?,
                None,
            )
            .await
            .context("sign-out: writing this device's own fleet removal")?;
    }
    // A view in which this device is already removed, so `may_compact`
    // reads its own rows as a removed writer's — the same rule any sibling
    // applies to them after this.
    let view = FleetView::default();
    let mut pass = ReclaimPass::default();
    let mut retirer = Retirer {
        fleet,
        me,
        view: &view,
        pass: &mut pass,
        // Never withheld (module docs → *What a pass may skip*): this leg
        // runs once, the nest's own answer is its only witness, and a row it
        // leaves behind is the fleet's to retire as a removed writer's.
        withhold_above: None,
    };
    retirer
        .retire_gen0(KIND_DEVICE_REACH, &me_hex, Some(&me), Belt::None)
        .await?;
    retirer.retire_device_scoped_rows(&me).await?;
    Ok(pass)
}

/// This device's own verifying reach record, if it has published one.
async fn own_reach<B: StoreBackend>(
    store: &AccountStore<B>,
    me: &[u8; 32],
) -> Result<Option<DeviceReachRecord>> {
    let Some(entry) = store.state(KIND_DEVICE_REACH, &reach_cell_key(me)).await? else {
        return Ok(None);
    };
    if entry.tombstone || parse_reach_cell_key(&entry.key) != Some(*me) {
        return Ok(None);
    }
    Ok(
        fauna_core::encoding::canonical_decode::<DeviceReachRecord>(&entry.value)
            .ok()
            .filter(|r| r.verifies_at(me)),
    )
}

/// The form-v2 wire item key of `(kind, key)` sealed under the generation
/// `gen_key` keys — the per-generation schedule's blind, so the same logical
/// item has a different item key under every generation. Homed here (ungated,
/// like this whole module) rather than in `device_endpoints_writer` (gated
/// behind `account-runtime`) so both passes can share it without pulling this
/// pass behind that feature.
pub fn item_key_under(gen_key: &GenerationKey, kind: &str, key: &str) -> [u8; 32] {
    FleetOnlySchedule::derive_for_generation(gen_key)
        .for_kind(kind)
        .item_key(key.as_bytes())
}

/// This device's own live relay row of `(kind, key)` sealed under the
/// generation `gen_key` keys, if it holds one — shared with
/// `device_endpoints_writer`'s `published_under` (`account-data-taxonomy.md`
/// clause (3)(g): "holds a row of its own at the tip's item key").
pub async fn own_row_at<B: StoreBackend, R: RpcRequester>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    gen_key: &GenerationKey,
    kind: &str,
    key: &str,
) -> Result<Option<RelayRow>> {
    let writer = store.writer();
    Ok(fleet
        .relay_rows_at(&item_key_under(gen_key, kind, key))
        .await?
        .into_iter()
        .find(|r| r.writer == writer))
}

/// Clause (3)(g), the re-seal pass: for every row this replica holds sealed
/// under a `reclaimable` generation that it can open, re-put the item's
/// MERGED entry — value, `merge_meta` and tombstone marker verbatim — through
/// the ordinary writer door, which seals it under the tip.
///
/// - **Own arm** — the row is ours and we hold no row of our own for the item
///   at the tip's item key: put; step 6 (clause (f)) retires the old row once
///   the new one is published.
/// - **Hand-over arm** — the writer is no longer a verified member, the kind
///   is account-level, and this device is the generation's hander (in
///   `handing`): put, unless our own row at the tip's item key already
///   carries the merged entry; then retire the departed writer's row, but
///   only once that own row is published (at or below our frontier slot) —
///   a covering row that reached us over the peer leg, or one of ours not
///   yet sent, could leave the nest serving the item to no one.
/// - **Let-go arm** — the writer is no longer a verified member, the kind is
///   device-scoped (`TipRowScope::Device`), and this device is the
///   generation's hander: retire the row with no put first, removal evidence
///   or none, and forget the relay copy once the nest confirms it gone. The
///   row describes its writer, not the account, so there is nothing to hand
///   over; a live writer re-publishes its own under the tip. Without this a
///   generation carried across a succession never shreds — the predecessor
///   device's endpoints row pins it, and no removal ever names that device.
///
/// A put the door refuses (`scope_full`, no tip) leaves the old row where it
/// was: the put always precedes the hand-over's retire. A live sibling's row
/// is its own pass's business.
async fn reseal_superseded<B, R>(
    store: &AccountStore<B>,
    retirer: &mut Retirer<'_, '_, B, R>,
    tip_key: &GenerationKey,
    reclaimable: &BTreeSet<[u8; 32]>,
    handing: &BTreeSet<[u8; 32]>,
) -> Result<()>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let fleet = retirer.fleet;
    let me = retirer.me;
    // Only the writers either arm acts on, each looked up by index — never
    // a page of the whole plane: the relay plane is never told about a live
    // sibling's retirements, so it keeps every row a sibling superseded
    // until that row's generation shreds (step 8), and a scan of the whole
    // plane pays for all of them.
    let mut writers = vec![WriterId(me)];
    if !handing.is_empty() {
        for (writer, _) in store
            .relay_high_waters(fleet.scope(), ItemClass::StateEntry.as_wire())
            .await?
        {
            if writer.0 != me && !retirer.view.is_verified_member(&writer.0) {
                writers.push(writer);
            }
        }
    }
    let mut rows: Vec<([u8; 32], RelayRow)> = Vec::new();
    for writer in &writers {
        rows.extend(
            fleet
                .relay_rows_of_writer(writer)
                .await?
                .into_iter()
                .filter_map(|row| sealed_under_one_of(row, reclaimable)),
        );
    }
    for (g, row) in rows {
        let own = row.writer.0 == me;
        let departed = !own && !retirer.view.is_verified_member(&row.writer.0);
        if !own && !(departed && handing.contains(&g)) {
            continue;
        }
        // One row is one unit of local work (`pass_breath` module docs) — an
        // AEAD open, and on a re-seal a seal and a put.
        crate::pass_breath::pass_breath().await;
        let Ok(old_item_key) = <[u8; 32]>::try_from(row.item_key.as_slice()) else {
            continue;
        };
        let Some(plaintext) = fleet.open_relay_row(&row).await? else {
            continue;
        };
        let Some(kind) = TipSealedKind::of(&plaintext.kind) else {
            continue;
        };
        if departed && kind.scope() != TipRowScope::Account {
            // The let-go arm (clause (3)(g)): a device-level row describes
            // its writer, not the account, so nothing is handed over — the
            // superseded generation's hander retires it outright, removal
            // evidence or none. Deferred or withheld: kept, and asked about
            // again next pass.
            if let RetireOutcome::Retired | RetireOutcome::Gone = retirer
                .retire(
                    &old_item_key,
                    &row.writer,
                    row.writer_seq,
                    row.feed_seq,
                    Belt::None,
                )
                .await?
            {
                fleet.relay_forget(&row.writer, &old_item_key).await?;
            }
            continue;
        }
        let Some(merged) = store.state(&plaintext.kind, &plaintext.key).await? else {
            continue;
        };
        let mine = own_row_at(store, fleet, tip_key, &plaintext.kind, &plaintext.key).await?;
        if own {
            if mine.is_none() {
                reseal(fleet, &merged).await;
            }
            continue;
        }
        let carried = match &mine {
            Some(r) => carries(fleet, r, &merged).await?,
            None => false,
        };
        let mine = if carried {
            mine
        } else {
            if !reseal(fleet, &merged).await {
                continue;
            }
            own_row_at(store, fleet, tip_key, &plaintext.kind, &plaintext.key).await?
        };
        // Published as this pass saw it (ruling 1(d)), never by our
        // frontier slot alone.
        if !mine.as_ref().is_some_and(|r| listed(fleet, r)) {
            continue; // not published here yet: the retire waits for the put
        }
        match retirer
            .retire(
                &old_item_key,
                &row.writer,
                row.writer_seq,
                row.feed_seq,
                Belt::None,
            )
            .await?
        {
            RetireOutcome::Retired | RetireOutcome::Gone => {
                fleet.relay_forget(&row.writer, &old_item_key).await?;
            }
            // Deferred or withheld: kept, and asked about again next pass —
            // our own row already carries the item, so the next pass goes
            // straight to the retire.
            _ => {}
        }
    }
    Ok(())
}

/// Step 9, clause (3)(i)'s predecessor arm (module docs). `shredded` is the
/// set of generations merged state reads `Shredded`.
///
/// The writers are looked up by index and filtered to non-members first, as
/// the hand-over arm's are, so a pass costs nothing per live sibling's row;
/// and a row the retired keys never open is remembered by the plane and not
/// tried again ([`AccountStatePlane::open_predecessor_machinery_row`]).
async fn retire_predecessor_machinery<B, R>(
    store: &AccountStore<B>,
    retirer: &mut Retirer<'_, '_, B, R>,
    shredded: &BTreeSet<[u8; 32]>,
) -> Result<()>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let fleet = retirer.fleet;
    let me = retirer.me;
    let mut writers = Vec::new();
    for (writer, _) in store
        .relay_high_waters(fleet.scope(), ItemClass::StateEntry.as_wire())
        .await?
    {
        if writer.0 != me && !retirer.view.is_verified_member(&writer.0) {
            writers.push(writer);
        }
    }
    for writer in writers {
        for row in fleet.relay_rows_of_writer(&writer).await? {
            if retirer.pass.unsupported {
                return Ok(());
            }
            let Ok(item_key) = <[u8; 32]>::try_from(row.item_key.as_slice()) else {
                continue;
            };
            // One row is one unit of local work (`pass_breath` module docs) —
            // at most one AEAD open per retired pair.
            crate::pass_breath::pass_breath().await;
            // The retired keys first: a row they do not open costs nothing
            // more (and is remembered), and one they do is checked against
            // the plane's own keys, which must not open it (the arm acts on
            // what this replica could not otherwise read).
            let Some(plaintext) = fleet.open_predecessor_machinery_row(&row) else {
                continue;
            };
            if fleet.open_relay_row(&row).await?.is_some() {
                continue;
            }
            let belt_generation;
            let belt = if plaintext.kind == KIND_GENERATION_MINT {
                let Some(g) = fauna_core::hex32::decode(&plaintext.key)
                    .ok()
                    .filter(|g| fauna_core::hex32::encode(g) == plaintext.key)
                else {
                    continue;
                };
                belt_generation = g;
                match mint_licence(
                    store,
                    fleet,
                    me,
                    retirer.view,
                    &plaintext,
                    &belt_generation,
                    shredded,
                )
                .await?
                {
                    Some(MintLicence::Carried) => Belt::None,
                    Some(MintLicence::Shredded) => Belt::Dataless(&belt_generation),
                    // Kept: the one row a device that brings the generation's
                    // key later still needs (the rider's residual (iii)).
                    None => continue,
                }
            } else {
                Belt::None
            };
            match retirer
                .retire(&item_key, &row.writer, row.writer_seq, row.feed_seq, belt)
                .await?
            {
                RetireOutcome::Retired | RetireOutcome::Gone => {
                    // The relay copy only: the successor's merged state never
                    // held the row.
                    fleet.relay_forget(&row.writer, &item_key).await?;
                }
                // Deferred or withheld: kept, and asked about again next pass.
                _ => {}
            }
        }
    }
    Ok(())
}

/// Which of clause (3)(i)'s two licences lets the predecessor arm retire a
/// predecessor's mint row for generation `g`.
enum MintLicence {
    /// This device's own row for `g` carries the merged record and is
    /// published as the pass saw it — the row the walk's carry wrote.
    Carried,
    /// The opened record, or this replica's merged state, reads `g`
    /// **authored** `Shredded`.
    Shredded,
}

async fn mint_licence<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    me: [u8; 32],
    view: &FleetView,
    plaintext: &fauna_core::account_entry_crypto::EntryPlaintext,
    g: &[u8; 32],
    shredded: &BTreeSet<[u8; 32]>,
) -> Result<Option<MintLicence>>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    // Authored at this replica's view, over `g` itself — an unauthored shred
    // licenses no retire (the authored shred; `shredded` is filtered alike).
    let record_shredded = !plaintext.tombstone
        && fauna_core::encoding::canonical_decode::<GenerationMintRecord>(&plaintext.value)
            .is_ok_and(|record| record.shred_is_authored(view) == Ok(*g));
    if record_shredded || shredded.contains(g) {
        return Ok(Some(MintLicence::Shredded));
    }
    let g_hex = fauna_core::hex32::encode(g);
    let (Some(merged), Some(own_key)) = (
        store.state(KIND_GENERATION_MINT, &g_hex).await?,
        fleet.gen0_item_key(KIND_GENERATION_MINT, &g_hex),
    ) else {
        return Ok(None);
    };
    for mine in fleet.relay_rows_at(&own_key).await? {
        if mine.writer.0 == me && listed(fleet, &mine) && carries(fleet, &mine, &merged).await? {
            return Ok(Some(MintLicence::Carried));
        }
    }
    Ok(None)
}

/// Put `merged` through the writer door as this device's own row — a
/// tombstone as a tombstone. `false` when the door refused it (logged): the
/// row it would have covered stays where it is.
async fn reseal<B, R>(fleet: &AccountStatePlane<'_, B, R>, merged: &StateEntry) -> bool
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let item = ItemId {
        kind: merged.kind.clone(),
        key: merged.key.clone(),
    };
    let put = if merged.tombstone {
        fleet.tombstone(&item, merged.merge_meta.clone()).await
    } else {
        fleet
            .put(&item, merged.value.clone(), merged.merge_meta.clone())
            .await
    };
    match put {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(
                kind = %merged.kind,
                "generation reclaim: re-seal under the tip refused ({e:#}) — the superseded \
                 row stays"
            );
            false
        }
    }
}

/// Clause (3)(e)'s veto: the kind of the first row this replica holds sealed
/// under `g` that it can open and that is **uncovered**, or `None` when
/// every such row is covered. Covered = device-scoped with a writer that is
/// no longer a verified member (it goes with its device), or the item's
/// merged entry carried by a verified member's row on this plane at the tip's
/// item key. A row this replica cannot open vetoes nothing.
async fn uncovered_row_under<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    view: &FleetView,
    tip_key: &GenerationKey,
    g: &[u8; 32],
) -> Result<Option<String>>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let only = BTreeSet::from([*g]);
    for (_, row) in rows_sealed_under(store, fleet.scope(), &only).await? {
        crate::pass_breath::pass_breath().await;
        let Some(plaintext) = fleet.open_relay_row(&row).await? else {
            continue;
        };
        let device_scoped =
            TipSealedKind::of(&plaintext.kind).is_some_and(|k| k.scope() == TipRowScope::Device);
        if device_scoped && !view.is_verified_member(&row.writer.0) {
            continue;
        }
        if !covered_at_tip(store, fleet, view, tip_key, &plaintext).await? {
            return Ok(Some(plaintext.kind));
        }
    }
    Ok(None)
}

/// **Covered** (`account-data-taxonomy.md` § The generation machinery →
/// *Fleet-scope reclamation*): the merged entry of `plaintext`'s item is
/// carried — value, `merge_meta` and tombstone marker — by a verified
/// member's row at the tip's item key that the bound nest holds as this pass
/// saw it (`account-sync-plane.md` § The bind leg, ruling 1(d)). One
/// predicate for the shredder's veto and the publish diff's skip
/// (`crate::publish_diff`), so the two never disagree about a row.
pub(crate) async fn covered_at_tip<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    view: &FleetView,
    tip_key: &GenerationKey,
    plaintext: &fauna_core::account_entry_crypto::EntryPlaintext,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let Some(merged) = store.state(&plaintext.kind, &plaintext.key).await? else {
        return Ok(false);
    };
    let at_tip = item_key_under(tip_key, &plaintext.kind, &plaintext.key);
    for candidate in fleet.relay_rows_at(&at_tip).await? {
        if view.is_verified_member(&candidate.writer.0)
            && listed(fleet, &candidate)
            && carries(fleet, &candidate, &merged).await?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Is `row` published as this pass saw it — the bound nest's listing holds
/// its `(writer, item)` at its seq or later, or the nest acked it since?
fn listed<B: StoreBackend, R: RpcRequester>(
    fleet: &AccountStatePlane<'_, B, R>,
    row: &RelayRow,
) -> bool {
    <[u8; 32]>::try_from(row.item_key.as_slice())
        .is_ok_and(|key| fleet.listed_at_or_above(&row.writer, &key, row.writer_seq))
}

/// Does `row` open to exactly `merged` — value, `merge_meta` and tombstone
/// marker?
async fn carries<B, R>(
    fleet: &AccountStatePlane<'_, B, R>,
    row: &RelayRow,
    merged: &StateEntry,
) -> Result<bool>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    Ok(fleet.open_relay_row(row).await?.is_some_and(|p| {
        p.kind == merged.kind
            && p.key == merged.key
            && p.tombstone == merged.tombstone
            && p.value.as_slice() == merged.value.as_slice()
            && p.merge_meta.as_ref().map(|m| m.as_slice()) == merged.merge_meta.as_deref()
    }))
}

/// `row` with the generation its cleartext header names, when that is one of
/// `generations` (a form-v1 row names none).
fn sealed_under_one_of(
    row: RelayRow,
    generations: &BTreeSet<[u8; 32]>,
) -> Option<([u8; 32], RelayRow)> {
    let g = row
        .entry
        .as_deref()
        .and_then(fauna_core::account_entry_crypto::peek_generation_id)?;
    generations.contains(&g).then_some((g, row))
}

/// This replica's relay rows of `scope` whose cleartext header names one of
/// `generations`, each with the generation it names. Pages the whole plane,
/// so only the veto asks — once per shred decision, never per row.
async fn rows_sealed_under<B: StoreBackend>(
    store: &AccountStore<B>,
    scope: &str,
    generations: &BTreeSet<[u8; 32]>,
) -> Result<Vec<([u8; 32], RelayRow)>> {
    Ok(store
        .relay_rows(scope, ItemClass::StateEntry.as_wire(), &[], u32::MAX)
        .await?
        .into_iter()
        .filter_map(|row| sealed_under_one_of(row, generations))
        .collect())
}

/// The proper ancestors of `tip` over the mint DAG's parent edges — the
/// generations a member holding `tip` no longer seals under.
pub fn ancestors_of(
    tip: &[u8; 32],
    parents: &BTreeMap<[u8; 32], Vec<[u8; 32]>>,
) -> BTreeSet<[u8; 32]> {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<[u8; 32]> = parents.get(tip).cloned().unwrap_or_default();
    while let Some(id) = stack.pop() {
        if id == *tip || !seen.insert(id) {
            continue;
        }
        if let Some(more) = parents.get(&id) {
            stack.extend(more.iter().copied());
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation_fixture_test_support::*;
    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_core::account_entry_crypto::{EntryCoordinates, EntryPlaintext, seal_entry};
    use fauna_protocol::account_state::ACCOUNT_STATE_FLEET_SCOPE;
    use fauna_protocol::error::RpcError;
    use fauna_protocol::merge_policy::kind_keys;
    use std::sync::Mutex;

    /// One retire the fake nest saw: `(item key, writer, writer seq)`.
    type Retired = ([u8; 32], [u8; 32], u64);

    /// A nest that records every retire and answers every put — the wire
    /// half the pull-only `NoNest` refuses to be. Its feed is empty and
    /// carries the gate's watermark in `.1` (`None`: no
    /// counted mark — nothing withheld). While `.2` is set every form-v2
    /// (tip-sealed) put is refused as a transport-shaped fault, so such a row
    /// stays local-only while gen-0 machinery rows still land. While `.3` is
    /// set every put is refused `scope_full` — the scope at its cap.
    #[derive(Clone, Default)]
    struct Recorder(
        std::sync::Arc<Mutex<Vec<Retired>>>,
        Option<i64>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    );

    #[derive(Debug)]
    struct RecorderErr(RpcError);
    impl std::fmt::Display for RecorderErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0.code)
        }
    }
    impl RpcErrorClass for RecorderErr {
        fn is_rejection(&self) -> bool {
            true
        }
        fn as_rpc_error(&self) -> Option<&RpcError> {
            Some(&self.0)
        }
    }

    impl RpcRequester for Recorder {
        type Error = RecorderErr;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, RecorderErr>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            use fauna_protocol::account_state::{
                AccountStatePutReply, AccountStateRetireReply, AccountStateRetireRequest,
                KIND_STATE_PUT, KIND_STATE_RETIRE,
            };
            let bytes = fauna_core::encoding::canonical_encode(&payload).unwrap();
            let reply = match kind {
                KIND_STATE_PUT if self.3.load(std::sync::atomic::Ordering::SeqCst) => {
                    return Err(RecorderErr(RpcError::new(
                        "fauna.account.state.scope_full",
                        "the scope is at its cap",
                    )));
                }
                KIND_STATE_PUT
                    if self.2.load(std::sync::atomic::Ordering::SeqCst)
                        && fauna_core::account_entry_crypto::peek_generation_id(
                            &fauna_core::encoding::canonical_decode::<
                                fauna_protocol::account_state::AccountStatePutRequest,
                            >(&bytes)
                            .unwrap()
                            .entry,
                        )
                        .is_some() =>
                {
                    return Err(RecorderErr(RpcError::new(
                        "unavailable",
                        "the nest is away",
                    )));
                }
                KIND_STATE_PUT => fauna_core::encoding::canonical_encode(&AccountStatePutReply {
                    seq: 1,
                    ..Default::default()
                })
                .unwrap(),
                KIND_STATE_RETIRE => {
                    let req: AccountStateRetireRequest =
                        fauna_core::encoding::canonical_decode(&bytes).unwrap();
                    let item: [u8; 32] = req.item_key.as_ref().try_into().unwrap();
                    let writer = fauna_core::hex32::decode(&req.writer_id).unwrap();
                    self.0
                        .lock()
                        .unwrap()
                        .push((item, writer, req.writer_seq as u64));
                    fauna_core::encoding::canonical_encode(&AccountStateRetireReply {
                        retired: true,
                        extra: Default::default(),
                    })
                    .unwrap()
                }
                "fauna.sync.changes.list" => fauna_core::encoding::canonical_encode(
                    &fauna_protocol::sync::SyncChangesListReply {
                        retirable_through_seq: self.1,
                        ..Default::default()
                    },
                )
                .unwrap(),
                other => {
                    return Err(RecorderErr(RpcError::new("kind_not_served", other)));
                }
            };
            Ok(fauna_core::encoding::canonical_decode(&reply).unwrap())
        }
    }

    fn plane<'a>(
        f: &'a Fixture,
        nest: &'a Recorder,
    ) -> AccountStatePlane<'a, SqliteBackend, Recorder> {
        AccountStatePlane::new(
            &f.store,
            nest,
            &f.schedule,
            &f.writer_key,
            &f.trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .unwrap()
    }

    /// Stage a row as if walked off the feed from ANOTHER writer: the merged
    /// local state plus the verbatim relay row (sealed under the fleet's
    /// own schedule by that writer), which is what the pass indexes —
    /// carrying the feed coordinate the walk would have recorded, or none
    /// (a row recorded before the column, a store-served leg's).
    async fn walked_row(f: &Fixture, writer_seed: [u8; 32], seq: u64, row: &StateEntry) {
        walked_row_at(f, writer_seed, seq, None, row).await;
    }

    async fn walked_row_at(
        f: &Fixture,
        writer_seed: [u8; 32],
        seq: u64,
        feed_seq: Option<u64>,
        row: &StateEntry,
    ) {
        f.put(row.clone()).await;
        let keys = kind_keys(&f.schedule, &row.kind).unwrap();
        let writer_key = device_key(writer_seed);
        let plaintext = EntryPlaintext {
            kind: row.kind.clone(),
            key: row.key.clone(),
            merge_meta: row.merge_meta.clone().map(Into::into),
            value: row.value.clone().into(),
            tombstone: false,
        };
        let coords = EntryCoordinates {
            writer_id: writer_key.verifying_key().to_bytes(),
            writer_seq: seq,
            scope: ACCOUNT_STATE_FLEET_SCOPE,
        };
        let sealed = seal_entry(&keys, &coords, &plaintext, &writer_key).unwrap();
        f.store
            .record_relay_row(&RelayRow {
                scope: ACCOUNT_STATE_FLEET_SCOPE.into(),
                item_class: "state-entry".into(),
                writer: WriterId(writer_key.verifying_key().to_bytes()),
                writer_seq: seq,
                item_key: sealed.item_key.to_vec(),
                op: "state-put".into(),
                entry: Some(sealed.envelope),
                feed_seq,
            })
            .await
            .unwrap();
    }

    fn reach_row(seed: [u8; 32], at_ms: i64, holds: Vec<[u8; 32]>) -> StateEntry {
        let key = device_key(seed);
        machinery_row(
            KIND_DEVICE_REACH,
            reach_cell_key(&key.verifying_key().to_bytes()),
            &sign_device_reach(&key, at_ms, holds),
        )
    }

    use fauna_account_store::types::{RelayRow, StateEntry};

    /// Clause (2): a healer's own cell is retired once the target's reach
    /// lists the generation — and not before.
    #[tokio::test]
    async fn a_healers_own_cell_is_retired_once_the_targets_reach_covers_it() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        f.put(enrollment_row(THEM)).await;
        let (generation, gen_key, _) = f.mint_over(&[member_of(US)]).await;
        let nest = Recorder::default();
        let p = plane(&f, &nest);

        // No reach for THEM: the heal writes this healer's cell.
        crate::generation_topup::put_heal(
            &p,
            &generation,
            &member_of(THEM),
            &gen_key,
            &f.writer_key,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            live_rows(&f.store, KIND_GENERATION_WRAP)
                .await
                .unwrap()
                .len(),
            1
        );
        // Nothing to reclaim yet: THEM has not said it holds the key.
        let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!(pass.retired, 0);
        assert!(nest.0.lock().unwrap().is_empty());

        // THEM publishes its reach listing the generation.
        walked_row(&f, THEM, 1, &reach_row(THEM, 8_000, vec![generation])).await;
        let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!(pass.retired, 1, "this healer's cell: {pass:?}");
        let retired = nest.0.lock().unwrap().clone();
        assert!(
            retired.iter().all(|(_, w, _)| *w == device_id_of(US)),
            "only this healer's own rows were named: {retired:?}"
        );
        // The retired cells are forgotten locally too — dead everywhere, so
        // no later pass re-verifies them.
        assert!(
            live_rows(&f.store, KIND_GENERATION_WRAP)
                .await
                .unwrap()
                .is_empty(),
            "the retired cells are gone from this replica's merged state"
        );
    }

    /// Clause (1) of the pass: this device publishes its reach once, and
    /// only again when the set changes.
    #[tokio::test]
    async fn the_reach_is_published_on_change_and_byte_quiet_otherwise() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        let nest = Recorder::default();
        let p = plane(&f, &nest);
        let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert!(pass.reach_published, "an empty reach is still a statement");
        let own = own_reach(&f.store, &device_id_of(US))
            .await
            .unwrap()
            .unwrap();
        assert!(own.holds.is_empty());
        let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert!(!pass.reach_published, "unchanged set: quiet");
        // A generation this device can key joins the set.
        let (generation, _, _) = f.mint_over(&[member_of(US)]).await;
        let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert!(pass.reach_published);
        let own = own_reach(&f.store, &device_id_of(US))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(own.holds, vec![generation]);
    }

    /// Clause (3d) and the bind leg's ruling 7: the pass retires a removed
    /// device's `Enrolled` row and leaves its `Removed` evidence live, here
    /// and in the relay plane; the secondary leg's removed-device arm — here a
    /// run with no linked replica, so every row is carried — retires the
    /// evidence behind it, and enters nothing in the store's retire record.
    #[tokio::test]
    async fn a_removed_devices_enrollment_is_retired_before_its_removal_evidence() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        // THEM enrolled itself (seq 1), then US removed it (US's own row,
        // published through the door below).
        walked_row(&f, THEM, 1, &enrollment_row(THEM)).await;
        let nest = Recorder::default();
        let p = plane(&f, &nest);
        p.put(
            &ItemId {
                kind: KIND_DEVICE_SET.into(),
                key: fauna_core::hex32::encode(&device_id_of(THEM)),
            },
            fauna_core::encoding::canonical_encode(&DeviceSetRecord::Removed {
                removed_at_ms: 9_000,
                removed_by: device_id_of(US),
            })
            .unwrap(),
            None,
        )
        .await
        .unwrap();
        let devset_key = p
            .gen0_item_key(
                KIND_DEVICE_SET,
                &fauna_core::hex32::encode(&device_id_of(THEM)),
            )
            .unwrap();
        let named = |nest: &Recorder| -> Vec<[u8; 32]> {
            nest.0
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _, _)| *k == devset_key)
                .map(|(_, w, _)| *w)
                .collect()
        };

        // Two passes: the evidence is never the pass's, however often it runs.
        for _ in 0..2 {
            let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
                .await
                .unwrap();
            assert_eq!(
                named(&nest),
                vec![device_id_of(THEM)],
                "the pass retires the enrollment and never the evidence: {pass:?}"
            );
        }
        let evidence = removal_evidence(&f.store, &p, &f.trust).await.unwrap();
        let [row] = evidence.as_slice() else {
            panic!("one evidence row, US's: {evidence:?}");
        };
        assert_eq!((row.writer.0, row.item_key), (device_id_of(US), devset_key));
        let recorded = f.store.issued_retires().await.unwrap().len();

        // The leg run's arm, with no linked replica to carry it to.
        let ctx = crate::linked_leg::LinkedCtx {
            store: &f.store,
            bound_fleet: &p,
            schedule: &f.schedule,
            trust: &f.trust,
            writer_key: &f.writer_key,
            custody: None,
        };
        let arm =
            crate::linked_leg::retire_carried_evidence(&ctx, &evidence, &[], &BTreeMap::new(), &[])
                .await;
        assert_eq!((arm.retired, arm.deferred, arm.held), (1, 0, 0), "{arm:?}");
        assert_eq!(
            named(&nest),
            vec![device_id_of(THEM), device_id_of(US)],
            "the enrollment strictly before the removal"
        );
        assert!(
            removal_evidence(&f.store, &p, &f.trust)
                .await
                .unwrap()
                .is_empty(),
            "the retired evidence's relay row is forgotten"
        );
        assert_eq!(
            f.store.issued_retires().await.unwrap().len(),
            recorded,
            "the arm's retire at the bound nest is not entered in the retire record"
        );
        assert!(
            ReclaimState::read(&f.store, &f.trust, device_id_of(US))
                .await
                .unwrap()
                .view()
                .is_excluded(&device_id_of(THEM)),
            "merged state keeps the removal: it is absorbing"
        );
    }

    /// Module docs → *What a pass may skip*: with the feed's watermark banked
    /// by the walk, a row whose feed coordinate sits above it is withheld —
    /// no request, counted `withheld`, kept for the next pass — while rows at
    /// or below it are asked about as before; a feed that serves no
    /// watermark (the peer leg) withholds nothing, and the row is asked
    /// about on the very next pass that walks such a feed.
    #[tokio::test]
    async fn a_row_above_the_gates_watermark_is_withheld_without_a_request() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        // THEM enrolled itself at feed seq 10 and published its reach at feed
        // seq 12; the fleet's stranded walker marked 11.
        walked_row_at(&f, THEM, 1, Some(10), &enrollment_row(THEM)).await;
        walked_row_at(&f, THEM, 2, Some(12), &reach_row(THEM, 8_000, vec![])).await;
        let reach_key = p_reach_key(&f, THEM);
        let devset_key = p_devset_key(&f, THEM);

        let nest = Recorder(
            Default::default(),
            Some(11),
            Default::default(),
            Default::default(),
        );
        let p = plane(&f, &nest);
        // The walk banks the watermark (an empty page carrying it).
        p.reconcile().await.unwrap();
        assert_eq!(p.retirable_through_seq(), Some(11));
        // US removes THEM (US's own row, published through the door — the put
        // reply stamps its feed coordinate, seq 1, below the watermark).
        p.put(
            &ItemId {
                kind: KIND_DEVICE_SET.into(),
                key: fauna_core::hex32::encode(&device_id_of(THEM)),
            },
            fauna_core::encoding::canonical_encode(&DeviceSetRecord::Removed {
                removed_at_ms: 9_000,
                removed_by: device_id_of(US),
            })
            .unwrap(),
            None,
        )
        .await
        .unwrap();
        let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        let asked: Vec<[u8; 32]> = nest.0.lock().unwrap().iter().map(|(k, _, _)| *k).collect();
        assert!(
            asked.contains(&devset_key) && !asked.contains(&reach_key),
            "the enrollment (seq 10) and the removal (seq 1) are asked about, the reach (seq \
             12) is withheld: {pass:?}, asked {asked:?}"
        );
        assert_eq!((pass.withheld, pass.deferred), (1, 0), "{pass:?}");

        // A nest that serves no watermark: the withheld row is asked about.
        let unmarked = Recorder(
            Default::default(),
            None,
            Default::default(),
            Default::default(),
        );
        let p = plane(&f, &unmarked);
        p.reconcile().await.unwrap();
        assert_eq!(p.retirable_through_seq(), None);
        let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        let asked: Vec<[u8; 32]> = unmarked
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|(k, _, _)| *k)
            .collect();
        assert!(asked.contains(&reach_key), "{pass:?}, asked {asked:?}");
        assert_eq!(pass.withheld, 0, "{pass:?}");
    }

    /// The sign-out's leg never withholds (module docs → *What a pass may
    /// skip*): with a watermark banked below this device's own reach row,
    /// `sever_self` still sends the retire — the nest's answer is its only
    /// witness, and this device runs no next pass.
    #[tokio::test]
    async fn sever_self_never_withholds_against_the_watermark() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        // This device's own reach row, walked back at feed seq 50.
        walked_row_at(&f, US, 1, Some(50), &reach_row(US, 8_000, vec![])).await;
        let reach_key = p_reach_key(&f, US);
        let nest = Recorder(
            Default::default(),
            Some(10),
            Default::default(),
            Default::default(),
        );
        let p = plane(&f, &nest);
        p.reconcile().await.unwrap();
        assert_eq!(p.retirable_through_seq(), Some(10));
        let pass = sever_self(&f.store, &p, &f.writer_key).await.unwrap();
        let asked: Vec<[u8; 32]> = nest.0.lock().unwrap().iter().map(|(k, _, _)| *k).collect();
        assert!(asked.contains(&reach_key), "{pass:?}, asked {asked:?}");
        assert_eq!(pass.withheld, 0, "{pass:?}");
    }

    /// [`Recorder`] as a nest that names `replica` and echoes `echo` on its
    /// (empty) feed — another box than the one whose log the store's
    /// coordinates were taken from.
    #[derive(Clone)]
    struct OfReplica {
        inner: Recorder,
        replica: [u8; 16],
        echo: i64,
    }

    impl RpcRequester for OfReplica {
        type Error = RecorderErr;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, RecorderErr>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            if kind != "fauna.sync.changes.list" {
                return self.inner.request(kind, payload).await;
            }
            let reply = fauna_protocol::sync::SyncChangesListReply {
                retirable_through_seq: self.inner.1,
                complete_through_seq: Some(self.echo),
                replica_id: Some(fauna_protocol::ByteBuf::from(self.replica.to_vec())),
                ..Default::default()
            };
            Ok(fauna_core::encoding::canonical_decode(
                &fauna_core::encoding::canonical_encode(&reply).unwrap(),
            )
            .unwrap())
        }
    }

    /// **A coordinate from another replica never withholds a retire**
    /// (`account-sync-plane.md` § The bind leg, ruling 2). A removed
    /// device's rows were walked off replica A at feed seqs 10 and 12, and
    /// A's watermark banked. This replica then walks replica B, whose log is
    /// shorter and whose gate stands at 5: the coordinates are A's numbering,
    /// void with A's watermark, so the pass asks B to retire both rows rather
    /// than withholding them against B's gate by A's seqs.
    #[tokio::test]
    async fn a_coordinate_from_another_replica_never_withholds_a_retire() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        walked_row_at(&f, THEM, 1, Some(10), &enrollment_row(THEM)).await;
        walked_row_at(&f, THEM, 2, Some(12), &reach_row(THEM, 8_000, vec![])).await;
        f.store
            .raise_nest_watermark(ACCOUNT_STATE_FLEET_SCOPE, Some(&[0xAA; 16]), 12)
            .await
            .unwrap();
        let reach_key = p_reach_key(&f, THEM);
        let devset_key = p_devset_key(&f, THEM);

        let nest = OfReplica {
            inner: Recorder(
                Default::default(),
                Some(5),
                Default::default(),
                Default::default(),
            ),
            replica: [0xBB; 16],
            echo: 3,
        };
        let p = AccountStatePlane::new(
            &f.store,
            &nest,
            &f.schedule,
            &f.writer_key,
            &f.trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .unwrap();
        p.reconcile().await.unwrap();
        assert_eq!(p.retirable_through_seq(), Some(5));
        assert_eq!(
            f.store
                .nest_watermark_replica(ACCOUNT_STATE_FLEET_SCOPE)
                .await
                .unwrap()
                .as_deref(),
            Some([0xBB; 16].as_slice()),
            "the bank now names B"
        );
        p.put(
            &ItemId {
                kind: KIND_DEVICE_SET.into(),
                key: fauna_core::hex32::encode(&device_id_of(THEM)),
            },
            fauna_core::encoding::canonical_encode(&DeviceSetRecord::Removed {
                removed_at_ms: 9_000,
                removed_by: device_id_of(US),
            })
            .unwrap(),
            None,
        )
        .await
        .unwrap();
        let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        let asked: Vec<[u8; 32]> = nest
            .inner
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|(k, _, _)| *k)
            .collect();
        assert!(
            asked.contains(&devset_key) && asked.contains(&reach_key),
            "{pass:?}, asked {asked:?}"
        );
        assert_eq!(pass.withheld, 0, "{pass:?}");
    }

    /// The gen-0 item key of a device's reach row, as the pass names it.
    fn p_reach_key(f: &Fixture, seed: [u8; 32]) -> [u8; 32] {
        let nest = Recorder::default();
        plane(f, &nest)
            .gen0_item_key(KIND_DEVICE_REACH, &reach_cell_key(&device_id_of(seed)))
            .unwrap()
    }

    /// The gen-0 item key of a device's device-set row, as the pass names it.
    fn p_devset_key(f: &Fixture, seed: [u8; 32]) -> [u8; 32] {
        let nest = Recorder::default();
        plane(f, &nest)
            .gen0_item_key(
                KIND_DEVICE_SET,
                &fauna_core::hex32::encode(&device_id_of(seed)),
            )
            .unwrap()
    }

    // ── Clause (3)(g), the re-seal pass, and clause (3)(e)'s veto ──────────

    use fauna_core::account_entry_crypto::seal_entry_v2;
    use fauna_core::crypto::FleetOnlySchedule;
    use fauna_core::generation::{
        EscrowTargetRecord, FleetMember, derive_escrow_xwing_keypair, sign_escrow_receipt,
    };
    use fauna_protocol::merge_policy::{
        KIND_CUSTODIES_HELD, KIND_DEVICE_ENDPOINTS, KIND_GROUP_RECEPTION_KEY, KIND_SHARE_ENDPOINTS,
    };

    /// The escrow holder whose receipts make a generation admissible.
    fn holder_key() -> SigningKey {
        SigningKey::from_bytes(&[0x44; 32])
    }

    /// Mint a generation over `members`, signed by `minter_seed`, and land the
    /// trusted holder's receipt for it — so it resolves as a tip candidate.
    async fn mint_acked(
        f: &Fixture,
        members: &[FleetMember],
        parents: Vec<[u8; 32]>,
        minter_seed: [u8; 32],
        at_ms: i64,
    ) -> ([u8; 32], GenerationKey) {
        let built = fauna_mls::wrapped_blob::generation_wraps::build_mint(
            members,
            &EscrowTargetRecord {
                xwing_escrow_pubkey: derive_escrow_xwing_keypair(&ESCROW_SEED)
                    .public
                    .to_bytes()
                    .to_vec(),
            },
            &crate::generation_fixture_test_support::target_key(),
            parents,
            &device_key(minter_seed),
            at_ms,
        )
        .expect("mint");
        let id_hex = fauna_core::hex32::encode(&built.generation_id);
        f.put(machinery_row(
            KIND_GENERATION_MINT,
            id_hex.clone(),
            &built.record,
        ))
        .await;
        let receipt = sign_escrow_receipt(
            &holder_key(),
            built.generation_id,
            blake3::hash(&built.escrow_wrap).into(),
            &crate::generation_fixture_test_support::target_key(),
            at_ms,
        );
        f.put(machinery_row(
            KIND_ESCROW_RECEIPT,
            format!("{id_hex}/{}", fauna_core::hex32::encode(&receipt.holder_id)),
            &receipt,
        ))
        .await;
        (built.generation_id, built.gen_key)
    }

    /// A fixture trusting [`holder_key`], with this device (US) enrolled.
    async fn trusting_fixture() -> Fixture {
        let mut f = fixture().await;
        f.trust.trusted_holders = vec![holder_key().verifying_key().to_bytes()].into();
        f.put(enrollment_row(US)).await;
        f
    }

    /// An account-state entry as this replica's merged state holds it.
    fn entry(kind: &str, key: &str, value: &[u8], at_ms: i64, tombstone: bool) -> StateEntry {
        let stamp = LwwStamp {
            at_ms,
            writer: device_id_of(THEM),
        };
        StateEntry {
            kind: kind.into(),
            key: key.into(),
            scope: ACCOUNT_STATE_FLEET_SCOPE.into(),
            value: value.to_vec(),
            // A stamp only where the policy orders by one; an `Immutable`
            // value carries none.
            merge_meta: (kind != KIND_GROUP_RECEPTION_KEY).then(|| stamp.encode().unwrap()),
            entry_version: 0,
            tombstone,
        }
    }

    /// Stage `row` as merged state plus the verbatim form-v2 relay row
    /// `writer_seed` sealed under `generation` at `seq` — a row walked off
    /// the feed.
    async fn sealed_under(
        f: &Fixture,
        writer_seed: [u8; 32],
        seq: u64,
        generation: &[u8; 32],
        gen_key: &GenerationKey,
        row: &StateEntry,
    ) {
        // Merged in under the WALKED writer's coordinates, as the walk lands
        // it — never as this device's own journal row, which the plane's
        // ordered publish would send and seal under the tip itself.
        let journal = fauna_account_store::types::JournalRow {
            writer: WriterId(device_id_of(writer_seed)),
            seq,
            scope: ACCOUNT_STATE_FLEET_SCOPE.into(),
            op: if row.tombstone {
                fauna_account_store::types::JournalOp::Tombstone
            } else {
                fauna_account_store::types::JournalOp::StatePut
            },
            item: fauna_account_store::types::ItemRef::StateKey {
                kind: row.kind.clone(),
                key: row.key.clone(),
                entry_version: 0,
            },
        };
        f.store.ingest_state(&journal, row.clone()).await.unwrap();
        let keys = FleetOnlySchedule::derive_for_generation(gen_key).for_kind(&row.kind);
        let writer_key = device_key(writer_seed);
        let plaintext = EntryPlaintext {
            kind: row.kind.clone(),
            key: row.key.clone(),
            merge_meta: row.merge_meta.clone().map(Into::into),
            value: row.value.clone().into(),
            tombstone: row.tombstone,
        };
        let coords = EntryCoordinates {
            writer_id: writer_key.verifying_key().to_bytes(),
            writer_seq: seq,
            scope: ACCOUNT_STATE_FLEET_SCOPE,
        };
        let sealed = seal_entry_v2(&keys, &coords, generation, &plaintext, &writer_key).unwrap();
        f.store
            .record_relay_row(&RelayRow {
                scope: ACCOUNT_STATE_FLEET_SCOPE.into(),
                item_class: "state-entry".into(),
                writer: WriterId(writer_key.verifying_key().to_bytes()),
                writer_seq: seq,
                item_key: sealed.item_key.to_vec(),
                op: if row.tombstone {
                    "tombstone"
                } else {
                    "state-put"
                }
                .into(),
                entry: Some(sealed.envelope),
                feed_seq: None,
            })
            .await
            .unwrap();
    }

    /// Does this replica still hold `writer_seed`'s relay row under
    /// `gen_key` for `row`'s item?
    async fn holds_relay_row(
        f: &Fixture,
        writer_seed: [u8; 32],
        gen_key: &GenerationKey,
        row: &StateEntry,
    ) -> bool {
        let item_key = item_key_under(gen_key, &row.kind, &row.key);
        f.store
            .relay_rows_at(ACCOUNT_STATE_FLEET_SCOPE, "state-entry", &item_key)
            .await
            .unwrap()
            .iter()
            .any(|r| r.writer.0 == device_id_of(writer_seed))
    }

    /// This device's own row of `row`'s item under `gen_key`, opened.
    async fn own_opened_under(
        f: &Fixture,
        p: &AccountStatePlane<'_, SqliteBackend, Recorder>,
        gen_key: &GenerationKey,
        row: &StateEntry,
    ) -> Option<EntryPlaintext> {
        let mine = own_row_at(&f.store, p, gen_key, &row.kind, &row.key)
            .await
            .unwrap()?;
        p.open_relay_row(&mine).await.unwrap()
    }

    /// The pass order up to reclamation against this fake: the reconcile
    /// (its feed is empty, so this pass's listing starts empty) and the
    /// publish diff behind it, which pushes what the listing lacks — so a row
    /// the nest acked is published as the pass saw it
    /// (`account-sync-plane.md` § The bind leg, ruling 1(d)).
    async fn reconciled(f: &Fixture, p: &AccountStatePlane<'_, SqliteBackend, Recorder>) {
        p.reconcile().await.unwrap();
        crate::publish_diff::publish_diff(&f.store, p, &f.trust, &f.writer_key)
            .await
            .unwrap();
    }

    /// A seed whose device id sorts below this device's — a member that
    /// out-ranks US in every byte-order-min election.
    fn lower_seed() -> [u8; 32] {
        (1u8..=255)
            .map(|b| [b; 32])
            .find(|s| device_id_of(*s) < device_id_of(US))
            .expect("some seed sorts below US")
    }

    /// **The hand-over waits for its own publication.** THEM — no longer a
    /// member — wrote the account's reception key under generation 1; a later
    /// mint superseded it. US, generation 1's hander, re-seals the merged
    /// entry under the tip; while the nest is away that row stays local-only
    /// and THEM's row is NOT retired. Once US's row is published, the next
    /// pass retires THEM's row and forgets it locally. (Generation 1's minter
    /// is a live sibling that never keyed it, so the shred is that sibling's
    /// business and this pin watches the hand-over alone.)
    #[tokio::test]
    async fn the_hand_over_never_retires_before_its_own_row_is_published() {
        let f = trusting_fixture().await;
        let sib = lower_seed();
        f.put(enrollment_row(sib)).await;
        let members = [member_of(US), member_of(sib)];
        let (g1, k1) = mint_acked(&f, &members, vec![], sib, 7_000).await;
        let (g2, k2) = mint_acked(&f, &[member_of(US), member_of(sib)], vec![g1], US, 8_000).await;
        walked_row(&f, sib, 1, &reach_row(sib, 8_000, vec![g2])).await;
        let key = entry(
            KIND_GROUP_RECEPTION_KEY,
            "rk",
            b"the-account-keypair",
            1,
            false,
        );
        sealed_under(&f, THEM, 1, &g1, &k1, &key).await;
        // The nest takes no tip-sealed row: the hand-over put stays local.
        let nest = Recorder::default();
        nest.2.store(true, std::sync::atomic::Ordering::SeqCst);
        let p = plane(&f, &nest);
        for _ in 0..2 {
            ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
                .await
                .unwrap();
        }
        assert!(
            own_opened_under(&f, &p, &k2, &key).await.is_some(),
            "the hand-over put landed locally, sealed under the tip"
        );
        assert!(
            nest.0.lock().unwrap().is_empty(),
            "nothing is retired while the covering row is unpublished"
        );
        assert!(holds_relay_row(&f, THEM, &k1, &key).await);

        // The nest is back: the pump's publish step sends the row. Our
        // frontier slot now says published — but that names no nest, and a
        // pass with no listing of the bound one retires nothing on it
        // (ruling 1(d)).
        nest.2.store(false, std::sync::atomic::Ordering::SeqCst);
        p.publish_pending().await.unwrap();
        ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert!(
            nest.0.lock().unwrap().is_empty(),
            "no retire rests on a covering row this pass has not seen published"
        );
        // A pass that saw it published retires THEM's generation-1 row.
        reconciled(&f, &p).await;
        ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        let old_key = item_key_under(&k1, &key.kind, &key.key);
        assert_eq!(
            nest.0.lock().unwrap().clone(),
            vec![(old_key, device_id_of(THEM), 1)],
            "THEM's row alone is retired, once"
        );
        assert!(
            !holds_relay_row(&f, THEM, &k1, &key).await,
            "the retired row is forgotten locally"
        );
        let opened = own_opened_under(&f, &p, &k2, &key).await.unwrap();
        assert_eq!(opened.value.as_slice(), key.value.as_slice());
    }

    /// **Another member's covering row licenses nothing.** The same hand-over,
    /// but a live sibling's own re-seal of the item under the tip has already
    /// reached this replica — over the peer leg, so the nest may not hold it
    /// yet. US, generation 1's hander, still puts its own row, and retires
    /// THEM's row only once that own row is published: a retire licensed by
    /// the sibling's row could leave the nest serving the item to no one.
    #[tokio::test]
    async fn a_siblings_covering_row_never_licenses_the_hand_over_retire() {
        let f = trusting_fixture().await;
        let sib = lower_seed();
        f.put(enrollment_row(sib)).await;
        let members = [member_of(US), member_of(sib)];
        let (g1, k1) = mint_acked(&f, &members, vec![], sib, 7_000).await;
        let (g2, k2) = mint_acked(&f, &members, vec![g1], US, 8_000).await;
        walked_row(&f, sib, 1, &reach_row(sib, 8_000, vec![g2])).await;
        let key = entry(
            KIND_GROUP_RECEPTION_KEY,
            "rk",
            b"the-account-keypair",
            1,
            false,
        );
        sealed_under(&f, THEM, 1, &g1, &k1, &key).await;
        sealed_under(&f, sib, 2, &g2, &k2, &key).await;

        let nest = Recorder::default();
        let p = plane(&f, &nest);
        for _ in 0..2 {
            reconciled(&f, &p).await;
            ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
                .await
                .unwrap();
            p.publish_pending().await.unwrap();
        }
        let opened = own_opened_under(&f, &p, &k2, &key).await;
        assert!(
            opened.is_some(),
            "US puts its own row, though the sibling's row already carries the item (retired: \
             {:?})",
            nest.0.lock().unwrap()
        );
        assert_eq!(
            nest.0.lock().unwrap().clone(),
            vec![(
                item_key_under(&k1, &key.kind, &key.key),
                device_id_of(THEM),
                1
            )],
            "and THEM's row is retired on the strength of US's own published row"
        );
    }

    /// **A stale own row is no cover.** US's own row at the tip carries an
    /// OLDER value of the item than a departed writer's row under generation
    /// 1 — a writer that had not yet seen the new tip wrote once more before
    /// it was removed (a forged `Removed` at a live sibling is exactly this
    /// shape). The merged entry is the departed writer's newer value, so US
    /// re-puts it under the tip before it retires the old row; retiring on
    /// the strength of the stale row would leave the nest serving the older
    /// value and lose the write.
    #[tokio::test]
    async fn a_stale_own_row_never_licenses_the_hand_over_retire() {
        let f = trusting_fixture().await;
        let (g1, k1) = mint_acked(&f, &[member_of(US)], vec![], US, 7_000).await;
        let (_g2, k2) = mint_acked(&f, &[member_of(US)], vec![g1], US, 8_000).await;
        let nest = Recorder::default();
        let p = plane(&f, &nest);
        let older = LwwStamp {
            at_ms: 5,
            writer: device_id_of(US),
        };
        p.put(
            &ItemId {
                kind: KIND_CUSTODIES_HELD.into(),
                key: "grant".into(),
            },
            b"older".to_vec(),
            Some(older.encode().unwrap()),
        )
        .await
        .unwrap();
        let newer = entry(KIND_CUSTODIES_HELD, "grant", b"newer", 9, false);
        sealed_under(&f, THEM, 1, &g1, &k1, &newer).await;
        assert_eq!(
            own_opened_under(&f, &p, &k2, &newer)
                .await
                .unwrap()
                .value
                .as_slice(),
            b"older",
            "the setup: US's own row at the tip carries the older value"
        );

        for _ in 0..2 {
            reconciled(&f, &p).await;
            ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
                .await
                .unwrap();
            p.publish_pending().await.unwrap();
        }
        let opened = own_opened_under(&f, &p, &k2, &newer).await.unwrap();
        assert_eq!(
            opened.value.as_slice(),
            b"newer",
            "US re-puts the merged (newer) value before THEM's row goes (retired: {:?})",
            nest.0.lock().unwrap()
        );
        assert!(!holds_relay_row(&f, THEM, &k1, &newer).await);
    }

    /// **A row the hander cannot open is kept.** THEM — no longer a member,
    /// and on a newer build — wrote an item of a kind this build predates
    /// under generation 1. US, generation 1's hander, keys generation 1 but
    /// cannot open the row (the trial set is this build's registry), so it
    /// cannot re-seal the item: retiring the row would lose it for the whole
    /// account. It stays at the nest. This fixture's nest also answers
    /// generation 1 dataless and a row US cannot open vetoes nothing, so US
    /// shreds it (the residue clause (3)(e) accepts, resting on the nest
    /// alone); from then on no key anywhere opens THEM's row, and this
    /// replica's relay copy goes as a shredded generation's residue (clause
    /// (3)(h)) — a local forget, never a retire.
    #[tokio::test]
    async fn the_hand_over_keeps_a_row_it_cannot_open() {
        let f = trusting_fixture().await;
        let (g1, k1) = mint_acked(&f, &[member_of(US)], vec![], US, 7_000).await;
        let (_g2, _k2) = mint_acked(&f, &[member_of(US)], vec![g1], US, 8_000).await;
        let unknown = entry(
            "fauna.state.a-kind-this-build-predates",
            "k",
            b"from-a-newer-build",
            5,
            false,
        );
        sealed_under(&f, THEM, 1, &g1, &k1, &unknown).await;

        let nest = Recorder::default();
        let p = plane(&f, &nest);
        let (mut shredded, mut residue) = (0, 0);
        for _ in 0..3 {
            // The hand-over's verdict comes before the shred: THEM's row is
            // still held while generation 1 is live.
            if shredded == 0 {
                assert!(holds_relay_row(&f, THEM, &k1, &unknown).await);
            }
            let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
                .await
                .unwrap();
            shredded += pass.shredded;
            residue += pass.residue_forgotten;
            p.publish_pending().await.unwrap();
        }
        // US's own superseded rows are clause (f)'s to retire; THEM's is not.
        let retired = nest.0.lock().unwrap().clone();
        assert!(
            retired.iter().all(|(_, w, _)| *w != device_id_of(THEM)),
            "THEM's row is never retired: {retired:?}"
        );
        assert_eq!(shredded, 1, "generation 1 shreds on this nest's word");
        assert!(residue > 0, "the shredded generation's relay residue goes");
        assert!(!holds_relay_row(&f, THEM, &k1, &unknown).await);
    }

    /// **Only the hander hands over.** A verified member that sorts below US
    /// also keys generation 1, so it is the hander: US writes nothing for
    /// THEM's row and retires nothing.
    #[tokio::test]
    async fn a_member_that_is_not_the_hander_writes_nothing() {
        let f = trusting_fixture().await;
        let low = lower_seed();
        f.put(enrollment_row(low)).await;
        let members = [member_of(US), member_of(low)];
        let (g1, k1) = mint_acked(&f, &members, vec![], US, 7_000).await;
        let (g2, _k2) = mint_acked(&f, &members, vec![g1], US, 8_000).await;
        walked_row(&f, low, 1, &reach_row(low, 8_000, vec![g1, g2])).await;
        let key = entry(
            KIND_GROUP_RECEPTION_KEY,
            "rk",
            b"the-account-keypair",
            1,
            false,
        );
        sealed_under(&f, THEM, 1, &g1, &k1, &key).await;

        let nest = Recorder::default();
        let p = plane(&f, &nest);
        ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        let own = f
            .store
            .scope_rows(
                ACCOUNT_STATE_FLEET_SCOPE,
                &WriterId(device_id_of(US)),
                0,
                u32::MAX,
            )
            .await
            .unwrap();
        assert!(
            own.iter().all(|r| !matches!(
                &r.item,
                fauna_account_store::types::ItemRef::StateKey { kind, .. }
                    if kind == KIND_GROUP_RECEPTION_KEY
            )),
            "US wrote no reception-key row: {own:?}"
        );
        assert!(nest.0.lock().unwrap().is_empty(), "and retired nothing");
        assert!(holds_relay_row(&f, THEM, &k1, &key).await);
    }

    // ── Clause (3)(g), the let-go arm ───────────────────────────────────────

    /// A device-endpoints entry for `seed`'s device, as its writer seals it.
    fn endpoints_of(seed: [u8; 32]) -> StateEntry {
        entry(
            KIND_DEVICE_ENDPOINTS,
            &fauna_core::hex32::encode(&device_id_of(seed)),
            b"dial-candidates",
            5,
            false,
        )
    }

    /// A seed whose device id sorts above this device's — a member US
    /// out-ranks in every byte-order-min election.
    fn higher_seed() -> [u8; 32] {
        (1u8..=255)
            .map(|b| [b; 32])
            .find(|s| device_id_of(*s) > device_id_of(US))
            .expect("some seed sorts above US")
    }

    /// The retires `nest` received for `seed`'s rows.
    fn retired_of(nest: &Recorder, seed: [u8; 32]) -> Vec<Retired> {
        nest.0
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, w, _)| *w == device_id_of(seed))
            .cloned()
            .collect()
    }

    /// **The let-go arm.** THEM — no member, and no removal names it (a
    /// predecessor's device after a succession) — left its device-endpoints
    /// row under generation 1, which a later mint superseded. US, generation
    /// 1's hander, retires that row with no put before it, and forgets its
    /// relay copy once the nest confirms it gone.
    #[tokio::test]
    async fn the_hander_lets_go_a_departed_writers_device_level_row() {
        let f = trusting_fixture().await;
        let (g1, k1) = mint_acked(&f, &[member_of(US)], vec![], US, 7_000).await;
        let (_g2, k2) = mint_acked(&f, &[member_of(US)], vec![g1], US, 8_000).await;
        let row = endpoints_of(THEM);
        sealed_under(&f, THEM, 1, &g1, &k1, &row).await;

        let nest = Recorder::default();
        let p = plane(&f, &nest);
        ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!(
            retired_of(&nest, THEM),
            vec![(
                item_key_under(&k1, &row.kind, &row.key),
                device_id_of(THEM),
                1
            )],
            "THEM's device-level row is retired, once"
        );
        assert!(
            own_opened_under(&f, &p, &k2, &row).await.is_none(),
            "nothing is put for it under the tip"
        );
        assert!(
            !holds_relay_row(&f, THEM, &k1, &row).await,
            "the retired row is forgotten locally"
        );
    }

    /// **A verified member's device-level row is never let go by a sibling.**
    /// A member that sorts above US left its device-endpoints row under the
    /// superseded generation 1 (it re-publishes under the tip on its own, and
    /// its own clause (f) retires the old row). US, generation 1's hander,
    /// sends no retire for it.
    #[tokio::test]
    async fn a_verified_members_device_level_row_is_not_let_go_by_a_sibling() {
        let f = trusting_fixture().await;
        let high = higher_seed();
        f.put(enrollment_row(high)).await;
        let members = [member_of(US), member_of(high)];
        let (g1, k1) = mint_acked(&f, &members, vec![], US, 7_000).await;
        let (g2, _k2) = mint_acked(&f, &members, vec![g1], US, 8_000).await;
        walked_row(&f, high, 1, &reach_row(high, 8_000, vec![g1, g2])).await;
        let row = endpoints_of(high);
        sealed_under(&f, high, 2, &g1, &k1, &row).await;

        let nest = Recorder::default();
        let p = plane(&f, &nest);
        for _ in 0..2 {
            ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
                .await
                .unwrap();
        }
        assert!(
            retired_of(&nest, high).is_empty(),
            "no retire of a member's row: {:?}",
            nest.0.lock().unwrap()
        );
        assert!(holds_relay_row(&f, high, &k1, &row).await);
    }

    /// **Only the hander lets go.** A verified member that sorts below US also
    /// keys generation 1, so it is the hander: US sends no retire for THEM's
    /// device-level row.
    #[tokio::test]
    async fn a_member_that_is_not_the_hander_lets_go_of_nothing() {
        let f = trusting_fixture().await;
        let low = lower_seed();
        f.put(enrollment_row(low)).await;
        let members = [member_of(US), member_of(low)];
        let (g1, k1) = mint_acked(&f, &members, vec![], US, 7_000).await;
        let (g2, _k2) = mint_acked(&f, &members, vec![g1], US, 8_000).await;
        walked_row(&f, low, 1, &reach_row(low, 8_000, vec![g1, g2])).await;
        let row = endpoints_of(THEM);
        sealed_under(&f, THEM, 1, &g1, &k1, &row).await;

        let nest = Recorder::default();
        let p = plane(&f, &nest);
        ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert!(
            retired_of(&nest, THEM).is_empty(),
            "no retire of THEM's row: {:?}",
            nest.0.lock().unwrap()
        );
        assert!(holds_relay_row(&f, THEM, &k1, &row).await);
    }

    /// **A tombstone is handed over as a tombstone** — retiring one uncovered
    /// could resurrect what it deleted.
    #[tokio::test]
    async fn a_tombstone_is_handed_over_as_a_tombstone() {
        let f = trusting_fixture().await;
        let (g1, k1) = mint_acked(&f, &[member_of(US)], vec![], US, 7_000).await;
        let (_g2, k2) = mint_acked(&f, &[member_of(US)], vec![g1], US, 8_000).await;
        let grave = entry(KIND_SHARE_ENDPOINTS, "set/member", b"", 5, true);
        sealed_under(&f, THEM, 1, &g1, &k1, &grave).await;

        let nest = Recorder::default();
        let p = plane(&f, &nest);
        reconciled(&f, &p).await;
        ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        let opened = own_opened_under(&f, &p, &k2, &grave)
            .await
            .expect("US re-sealed the item under the tip");
        assert!(opened.tombstone, "re-put as a tombstone");
        assert_eq!(
            opened.merge_meta.as_ref().map(|m| m.as_slice()),
            grave.merge_meta.as_deref(),
            "the stamp travels unchanged — a re-seal never outranks a write"
        );
        assert!(!holds_relay_row(&f, THEM, &k1, &grave).await);
    }

    /// **The shredder's veto** (clause (3)(e)): the nest says generation 1
    /// seals nothing (the fake's feed is empty), but this replica holds a live
    /// sibling's openable row under it that no member's row at the tip
    /// covers — the shred waits. Once the sibling's own re-seal under the tip
    /// is walked, the row is covered and the shred lands.
    #[tokio::test]
    async fn the_veto_holds_the_shred_until_the_covering_row_is_walked() {
        let f = trusting_fixture().await;
        let sib = lower_seed();
        f.put(enrollment_row(sib)).await;
        let members = [member_of(US), member_of(sib)];
        let (g1, k1) = mint_acked(&f, &members, vec![], US, 7_000).await;
        let (g2, k2) = mint_acked(&f, &members, vec![g1], US, 8_000).await;
        walked_row(&f, sib, 1, &reach_row(sib, 8_000, vec![g1, g2])).await;
        let custody = entry(KIND_CUSTODIES_HELD, "grant", b"held", 3, false);
        sealed_under(&f, sib, 2, &g1, &k1, &custody).await;

        let nest = Recorder::default();
        let p = plane(&f, &nest);
        let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!(pass.shredded, 0, "vetoed: an uncovered row: {pass:?}");

        // The sibling's own re-seal under the tip arrives — over the peer
        // leg: this pass's listing of the nest does not hold it, so it covers
        // nothing yet (ruling 1(d)).
        sealed_under(&f, sib, 3, &g2, &k2, &custody).await;
        p.set_listing(Some(crate::account_state_plane::Listing::new()));
        let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!(
            pass.shredded, 0,
            "an unlisted covering row: vetoed: {pass:?}"
        );

        // The nest holds it.
        let at_tip = item_key_under(&k2, &custody.kind, &custody.key);
        p.set_listing(Some(crate::account_state_plane::Listing::from([(
            (WriterId(device_id_of(sib)), at_tip),
            3,
        )])));
        let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!(pass.shredded, 1, "covered: the shred lands: {pass:?}");
    }

    /// **The publish diff skips a covered row** (`account-sync-plane.md`
    /// § The bind leg, ruling 1(b)) — the reclamation pass's own predicate: a
    /// sibling's row under a superseded generation whose item a verified
    /// member's row at the tip carries, that row held by the nest. Pushing it
    /// would put generation 1 back in use on a nest that never saw it
    /// retired. With the covering row absent from the listing, nothing covers
    /// it, and both go.
    #[tokio::test]
    async fn the_publish_diff_skips_a_row_reclamation_calls_covered() {
        let f = trusting_fixture().await;
        let sib = lower_seed();
        f.put(enrollment_row(sib)).await;
        let members = [member_of(US), member_of(sib)];
        let (g1, k1) = mint_acked(&f, &members, vec![], US, 7_000).await;
        let (g2, k2) = mint_acked(&f, &members, vec![g1], US, 8_000).await;
        let custody = entry(KIND_CUSTODIES_HELD, "grant", b"held", 3, false);
        sealed_under(&f, sib, 2, &g1, &k1, &custody).await;
        sealed_under(&f, sib, 3, &g2, &k2, &custody).await;
        let nest = Recorder::default();
        let p = plane(&f, &nest);
        let at_tip = item_key_under(&k2, &custody.kind, &custody.key);

        p.set_listing(Some(crate::account_state_plane::Listing::from([(
            (WriterId(device_id_of(sib)), at_tip),
            3,
        )])));
        let diff = crate::publish_diff::publish_diff(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!((diff.covered, diff.pushed), (1, 0), "{diff:?}");

        p.set_listing(Some(crate::account_state_plane::Listing::new()));
        let diff = crate::publish_diff::publish_diff(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!((diff.covered, diff.pushed), (0, 2), "{diff:?}");
    }

    /// **The diff skips a dead row, and the pass behind it forgets that row**
    /// (`account-sync-plane.md` § The bind leg, ruling 1(b); clause (3)(h)'s
    /// first arm): once merged state reads a generation `Shredded`, a
    /// verified sibling's row sealed under it is counted `dead` and never
    /// pushed, the row under the live tip still goes, and the same pass's
    /// step 8 takes the skipped copy — the skipped set is the forgotten set.
    #[tokio::test]
    async fn the_publish_diff_skips_a_row_sealed_under_a_shredded_generation() {
        let f = trusting_fixture().await;
        let sib = lower_seed();
        f.put(enrollment_row(sib)).await;
        let members = [member_of(US), member_of(sib)];
        let (g1, k1) = mint_acked(&f, &members, vec![], US, 7_000).await;
        let (g2, k2) = mint_acked(&f, &members, vec![g1], US, 8_000).await;
        let custody = entry(KIND_CUSTODIES_HELD, "grant", b"held", 3, false);
        sealed_under(&f, sib, 2, &g1, &k1, &custody).await;
        sealed_under(&f, sib, 3, &g2, &k2, &custody).await;
        let g1_hex = fauna_core::hex32::encode(&g1);
        let core = match fauna_core::encoding::canonical_decode::<GenerationMintRecord>(
            &f.store
                .state(KIND_GENERATION_MINT, &g1_hex)
                .await
                .unwrap()
                .unwrap()
                .value,
        )
        .unwrap()
        {
            GenerationMintRecord::Minted { core, .. } => core,
            GenerationMintRecord::Shredded { .. } => unreachable!("minted above"),
        };
        f.put(machinery_row(
            KIND_GENERATION_MINT,
            g1_hex,
            &fauna_core::generation::sign_shred(&device_key(US), core, 9_000).unwrap(),
        ))
        .await;
        let nest = Recorder::default();
        // A retained key the walk's hook never saw dropped (the shred was
        // staged as merged state): the pass drops it, eventually.
        let bundle = Bundle::default();
        crate::generation_tip::RetainedKeyCustody::record_generation_key(&bundle, &g1, &k1);
        let p = plane(&f, &nest).with_generation_custody(&bundle);

        p.set_listing(Some(crate::account_state_plane::Listing::new()));
        let diff = crate::publish_diff::publish_diff(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!((diff.dead, diff.pushed), (1, 1), "{diff:?}");
        assert!(holds_relay_row(&f, sib, &k1, &custody).await);

        let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert!(
            !holds_relay_row(&f, sib, &k1, &custody).await,
            "the pass forgot the copy the diff skipped: {pass:?}"
        );
        assert!(holds_relay_row(&f, sib, &k2, &custody).await);
        assert!(
            crate::generation_tip::RetainedKeyCustody::retained_generation_key(&bundle, &g1)
                .is_none(),
            "the pass drops an authored shred's retained key"
        );
    }

    /// **An unauthored shred is no deletion** (`account-data-taxonomy.md`
    /// § *Fleet-scope reclamation* → *the authored shred*): a
    /// sig-less `Shredded` for generation 1 — what any `BackupKey` holder can
    /// write — or one signed by a device that is no verified member leaves
    /// everything the authored twin above destroys: the publish diff calls
    /// no row dead, the pass retires neither the mint row nor the receipt
    /// (so the holder's escrow wrap stays), forgets no relay residue, and the
    /// retained key stays in custody. Red-verified: with `ReclaimState::read`
    /// inserting on `Shredded { .. }` again, every assertion here fails.
    #[tokio::test]
    async fn an_unauthored_shred_retires_nothing_sweeps_nothing_and_keeps_the_key() {
        use crate::generation_tip::RetainedKeyCustody as _;
        let f = trusting_fixture().await;
        let sib = lower_seed();
        f.put(enrollment_row(sib)).await;
        let members = [member_of(US), member_of(sib)];
        let (g1, k1) = mint_acked(&f, &members, vec![], US, 7_000).await;
        let (g2, k2) = mint_acked(&f, &members, vec![g1], US, 8_000).await;
        let custody = entry(KIND_CUSTODIES_HELD, "grant", b"held", 3, false);
        sealed_under(&f, sib, 2, &g1, &k1, &custody).await;
        sealed_under(&f, sib, 3, &g2, &k2, &custody).await;
        let g1_hex = fauna_core::hex32::encode(&g1);
        let receipt_key = format!(
            "{g1_hex}/{}",
            fauna_core::hex32::encode(&holder_key().verifying_key().to_bytes())
        );
        let core = match fauna_core::encoding::canonical_decode::<GenerationMintRecord>(
            &f.store
                .state(KIND_GENERATION_MINT, &g1_hex)
                .await
                .unwrap()
                .unwrap()
                .value,
        )
        .unwrap()
        {
            GenerationMintRecord::Minted { core, .. } => core,
            GenerationMintRecord::Shredded { .. } => unreachable!("minted above"),
        };
        let nest = Recorder::default();
        let bundle = Bundle::default();
        bundle.record_generation_key(&g1, &k1);
        let p = plane(&f, &nest).with_generation_custody(&bundle);
        let mint_item = p.gen0_item_key(KIND_GENERATION_MINT, &g1_hex).unwrap();
        let receipt_item = p.gen0_item_key(KIND_ESCROW_RECEIPT, &receipt_key).unwrap();

        for forged in [
            GenerationMintRecord::Shredded {
                core: core.clone(),
                shredded_at_ms: 9_000,
                shredded_by: device_id_of(US),
                shredder_sig: vec![],
            },
            // THEM was never enrolled here: no verified member.
            fauna_core::generation::sign_shred(&device_key(THEM), core.clone(), 9_000).unwrap(),
        ] {
            f.put(machinery_row(KIND_GENERATION_MINT, g1_hex.clone(), &forged))
                .await;
            p.set_listing(Some(crate::account_state_plane::Listing::new()));
            let diff = crate::publish_diff::publish_diff(&f.store, &p, &f.trust, &f.writer_key)
                .await
                .unwrap();
            assert_eq!(
                diff.dead, 0,
                "no row is dead under an unauthored shred: {diff:?}"
            );

            let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
                .await
                .unwrap();
            let asked = |item: [u8; 32]| nest.0.lock().unwrap().iter().any(|(i, _, _)| *i == item);
            assert!(
                !asked(mint_item) && !asked(receipt_item),
                "neither the mint row nor its receipt is retired: {pass:?}"
            );
            assert!(
                f.store
                    .state(KIND_ESCROW_RECEIPT, &receipt_key)
                    .await
                    .unwrap()
                    .is_some(),
                "the receipt — and so the holder's escrow wrap — stays"
            );
            assert!(
                holds_relay_row(&f, sib, &k1, &custody).await,
                "no relay residue is forgotten: {pass:?}"
            );
            assert!(
                bundle.retained_generation_key(&g1).is_some(),
                "the retained key stays"
            );
        }
    }

    /// A `fauna.state.generation-closed` row for `generation`, as the remover
    /// writes it.
    fn closed_row(generation: &[u8; 32]) -> StateEntry {
        machinery_row(
            KIND_GENERATION_CLOSED,
            fauna_core::generation::closed_cell_key(generation),
            &fauna_core::generation::GenerationClosedRecord {
                closed_by: device_id_of(US),
                answers: device_id_of(THEM),
                closed_at_ms: 7_500,
            },
        )
    }

    /// **A closed row lives exactly as long as its generation** (the closed
    /// kind's bullet; clause (3)(e)): while generation 1 is `Minted` this
    /// device's closed row for it is kept and nothing is asked of the nest —
    /// it is what keeps the generation from being sealed under. The mint
    /// that follows the closure supersedes generation 1, the pass shreds it,
    /// and the next pass retires the closed row with the generation's other
    /// rows and forgets it locally.
    #[tokio::test]
    async fn a_closed_row_is_retired_when_its_generation_shreds_and_not_before() {
        let f = trusting_fixture().await;
        let (g1, _) = mint_acked(&f, &[member_of(US)], vec![], US, 7_000).await;
        let nest = Recorder::default();
        let p = plane(&f, &nest);
        let closed = closed_row(&g1);
        p.put(
            &ItemId {
                kind: closed.kind.clone(),
                key: closed.key.clone(),
            },
            closed.value.clone(),
            None,
        )
        .await
        .unwrap();
        let item = p
            .gen0_item_key(KIND_GENERATION_CLOSED, &closed.key)
            .unwrap();
        let asked = || nest.0.lock().unwrap().iter().any(|(i, _, _)| *i == item);
        let held = || async {
            f.store
                .state(KIND_GENERATION_CLOSED, &closed.key)
                .await
                .unwrap()
                .is_some()
        };

        // Closed and `Minted`: no tip resolves, nothing is reclaimable.
        let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert!(held().await && !asked(), "{pass:?}");

        // The first-need mint that follows names it as its parent.
        mint_acked(&f, &[member_of(US)], vec![g1], US, 8_000).await;
        let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!(pass.shredded, 1, "{pass:?}");

        let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert!(
            asked() && !held().await,
            "the shredded generation's closed row is retired and forgotten: {pass:?}"
        );
    }

    /// **The publish diff declines a dead closed row** (clause (3)(h), second
    /// arm): a live sibling's closed row for a generation merged state reads
    /// `Shredded` is counted dead and never pushed, and the pass behind the
    /// diff forgets it — under the sibling's writer, with no retire of a row
    /// that is the sibling's own to compact.
    #[tokio::test]
    async fn the_publish_diff_skips_a_closed_row_of_a_shredded_generation() {
        let f = trusting_fixture().await;
        let sib = lower_seed();
        f.put(enrollment_row(sib)).await;
        let members = [member_of(US), member_of(sib)];
        let (g1, _) = mint_acked(&f, &members, vec![], US, 7_000).await;
        mint_acked(&f, &members, vec![g1], US, 8_000).await;
        let closed = closed_row(&g1);
        walked_row(&f, sib, 4, &closed).await;
        let nest = Recorder::default();
        let p = plane(&f, &nest);

        // `Minted`: the row is live, and the diff does not call it dead.
        p.set_listing(Some(crate::account_state_plane::Listing::new()));
        let live = crate::publish_diff::publish_diff(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!(live.dead, 0, "{live:?}");

        let g1_hex = fauna_core::hex32::encode(&g1);
        let core = match fauna_core::encoding::canonical_decode::<GenerationMintRecord>(
            &f.store
                .state(KIND_GENERATION_MINT, &g1_hex)
                .await
                .unwrap()
                .unwrap()
                .value,
        )
        .unwrap()
        {
            GenerationMintRecord::Minted { core, .. } => core,
            GenerationMintRecord::Shredded { .. } => unreachable!("minted above"),
        };
        f.put(machinery_row(
            KIND_GENERATION_MINT,
            g1_hex,
            &fauna_core::generation::sign_shred(&device_key(US), core, 9_000).unwrap(),
        ))
        .await;
        p.set_listing(Some(crate::account_state_plane::Listing::new()));
        let diff = crate::publish_diff::publish_diff(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!(diff.dead, 1, "{diff:?}");

        let item = p
            .gen0_item_key(KIND_GENERATION_CLOSED, &closed.key)
            .unwrap();
        ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert!(
            f.store
                .state(KIND_GENERATION_CLOSED, &closed.key)
                .await
                .unwrap()
                .is_none(),
            "forgotten from merged state"
        );
        assert!(
            p.relay_rows_at(&item).await.unwrap().is_empty(),
            "forgotten under the sibling's writer"
        );
        // (The fixture's `walked_row` also journals the row as this device's
        // own, and that copy is this device's to retire; the sibling's is not.)
        assert!(
            !nest
                .0
                .lock()
                .unwrap()
                .iter()
                .any(|(i, writer, _)| *i == item && *writer == device_id_of(sib)),
            "a live sibling's row is its own pass's to retire"
        );
    }

    /// **Trigger (b), the second hole, through the writer and the store.**
    /// The tip names THEM; its live ancestor names US alone. Removing THEM
    /// makes the tip inadmissible, and the resolver would fall back to the
    /// ancestor — whose key a healer may have handed THEM as well. The
    /// remover closes the ancestor (and not the tip, which the member rule
    /// already unseats), so nothing resolves and the next tip-sealed write
    /// mints. No real pass produces this shape today — a superseded ancestor
    /// is shredded once every member's reach holds the tip — so it is pinned
    /// here over staged rows and in `fauna_core::generation`'s resolver tests.
    #[tokio::test]
    async fn a_removal_closes_the_live_ancestor_its_tip_would_fall_back_to() {
        let f = trusting_fixture().await;
        f.put(enrollment_row(THEM)).await;
        let (ancestor, _) = mint_acked(&f, &[member_of(US)], vec![], US, 7_000).await;
        let (tip, _) = mint_acked(
            &f,
            &[member_of(US), member_of(THEM)],
            vec![ancestor],
            US,
            8_000,
        )
        .await;
        let resolved = || async {
            generation_tip::resolve_tip(&f.store, &f.trust, &f.writer_key, None)
                .await
                .unwrap()
                .tip
                .map(|t| t.generation_id)
        };
        assert_eq!(resolved().await, Some(tip));

        let nest = Recorder::default();
        let p = plane(&f, &nest);
        crate::fleet_removal::write_removed(
            &f.store,
            &p,
            &f.trust,
            &f.writer_key,
            device_id_of(THEM),
        )
        .await
        .unwrap();

        let closed: Vec<String> = live_rows(&f.store, KIND_GENERATION_CLOSED)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.key)
            .collect();
        assert_eq!(closed, vec![fauna_core::hex32::encode(&ancestor)]);
        assert_eq!(
            resolved().await,
            None,
            "neither the tip that names the removed device nor its closed ancestor"
        );
    }

    /// **A sign-out closes nothing** (trigger (b), *A sign-out closes
    /// nothing*): the leaving device writes its own `Removed` row and no
    /// closed row — it erases its keys in the same act, so no holder is left
    /// to mint past.
    #[tokio::test]
    async fn a_sign_out_closes_nothing() {
        let f = trusting_fixture().await;
        f.put(enrollment_row(THEM)).await;
        mint_acked(&f, &[member_of(THEM)], vec![], THEM, 7_000).await;
        let nest = Recorder::default();
        let p = plane(&f, &nest);
        sever_self(&f.store, &p, &f.writer_key).await.unwrap();
        let state = ReclaimState::read(&f.store, &f.trust, device_id_of(US))
            .await
            .unwrap();
        assert!(state.view().is_excluded(&device_id_of(US)));
        assert!(
            live_rows(&f.store, KIND_GENERATION_CLOSED)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// **At the entry cap the pass still retires** (ruling 1(d)): the diff's
    /// push is refused `scope_full` and stops, and reclamation — whose
    /// retires need no headroom and are the only thing that frees any — runs
    /// in full behind it.
    #[tokio::test]
    async fn at_scope_full_the_diff_stops_and_the_pass_still_retires() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        walked_row(&f, THEM, 1, &enrollment_row(THEM)).await;
        let nest = Recorder::default();
        let p = plane(&f, &nest);
        // Our reach and THEM's removal go out while the scope has room.
        let me = device_id_of(US);
        p.put(
            &ItemId {
                kind: KIND_DEVICE_REACH.into(),
                key: reach_cell_key(&me),
            },
            fauna_core::encoding::canonical_encode(&sign_device_reach(
                &f.writer_key,
                9_000,
                Vec::new(),
            ))
            .unwrap(),
            Some(
                LwwStamp {
                    at_ms: 9_000,
                    writer: me,
                }
                .encode()
                .unwrap(),
            ),
        )
        .await
        .unwrap();
        p.put(
            &ItemId {
                kind: KIND_DEVICE_SET.into(),
                key: fauna_core::hex32::encode(&device_id_of(THEM)),
            },
            fauna_core::encoding::canonical_encode(&DeviceSetRecord::Removed {
                removed_at_ms: 9_000,
                removed_by: me,
            })
            .unwrap(),
            None,
        )
        .await
        .unwrap();

        // Then the scope is full, on a nest whose listing lacks our rows.
        nest.3.store(true, std::sync::atomic::Ordering::SeqCst);
        p.set_listing(Some(crate::account_state_plane::Listing::new()));
        let diff = crate::publish_diff::publish_diff(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert!(diff.scope_full && diff.pushed == 0, "{diff:?}");
        let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert!(pass.retired > 0, "the retires run at the cap: {pass:?}");
    }

    /// **A row the shredder cannot open vetoes nothing.** Generation 1 was
    /// minted by a writer now gone, over itself alone: no member keys it, so
    /// US — the byte-order-min member — is the stand-in shredder and cannot
    /// open the row sealed under it. The nest's answer stands.
    #[tokio::test]
    async fn a_shredder_that_cannot_key_the_generation_is_not_vetoed() {
        let f = trusting_fixture().await;
        let (g1, k1) = mint_acked(&f, &[member_of(THEM)], vec![], THEM, 7_000).await;
        let (_g2, k2) = mint_acked(&f, &[member_of(US)], vec![g1], US, 8_000).await;
        let key = entry(
            KIND_GROUP_RECEPTION_KEY,
            "rk",
            b"the-account-keypair",
            1,
            false,
        );
        sealed_under(&f, THEM, 1, &g1, &k1, &key).await;

        let nest = Recorder::default();
        let p = plane(&f, &nest);
        let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert_eq!(pass.shredded, 1, "{pass:?}");
        assert!(
            own_opened_under(&f, &p, &k2, &key).await.is_none(),
            "no member keys generation 1, so nothing is handed over"
        );
    }

    #[test]
    fn ancestors_walk_the_dag_transitively_and_never_include_the_tip() {
        let (a, b, c, d) = ([1u8; 32], [2u8; 32], [3u8; 32], [4u8; 32]);
        let parents: BTreeMap<[u8; 32], Vec<[u8; 32]>> =
            [(a, vec![]), (b, vec![a]), (c, vec![a]), (d, vec![b, c])]
                .into_iter()
                .collect();
        assert_eq!(ancestors_of(&d, &parents), [a, b, c].into_iter().collect());
        assert_eq!(ancestors_of(&b, &parents), [a].into_iter().collect());
        assert!(ancestors_of(&a, &parents).is_empty());
    }

    // ── Clause (3)(i), the predecessor arm ──────────────────────────────────

    /// The retired identity whose generation-0 machinery the arm is handed.
    fn retired_backup_key() -> fauna_core::crypto::BackupKey {
        fauna_core::crypto::BackupKey::from_bytes([0x33; 32])
    }

    /// Record a form-v1 relay row `writer_seed` sealed under `backup_key`'s
    /// generation-0 schedule — a predecessor device's machinery row as the
    /// successor's walk keeps it, never merged. Answers its item key.
    async fn retired_schedule_row(
        f: &Fixture,
        backup_key: &fauna_core::crypto::BackupKey,
        writer_seed: [u8; 32],
        seq: u64,
        kind: &str,
        key: &str,
    ) -> [u8; 32] {
        retired_schedule_row_of(f, backup_key, writer_seed, seq, kind, key, vec![0xA5; 8]).await
    }

    /// [`retired_schedule_row`] carrying `value`.
    async fn retired_schedule_row_of(
        f: &Fixture,
        backup_key: &fauna_core::crypto::BackupKey,
        writer_seed: [u8; 32],
        seq: u64,
        kind: &str,
        key: &str,
        value: Vec<u8>,
    ) -> [u8; 32] {
        let schedule = fauna_core::crypto::AccountStateKeySchedule::derive(backup_key);
        let writer_key = device_key(writer_seed);
        let coords = EntryCoordinates {
            writer_id: writer_key.verifying_key().to_bytes(),
            writer_seq: seq,
            scope: ACCOUNT_STATE_FLEET_SCOPE,
        };
        let sealed = seal_entry(
            &kind_keys(&schedule, kind).unwrap(),
            &coords,
            &EntryPlaintext {
                kind: kind.into(),
                key: key.into(),
                merge_meta: None,
                value: value.into(),
                tombstone: false,
            },
            &writer_key,
        )
        .unwrap();
        f.store
            .record_relay_row(&RelayRow {
                scope: ACCOUNT_STATE_FLEET_SCOPE.into(),
                item_class: "state-entry".into(),
                writer: WriterId(writer_key.verifying_key().to_bytes()),
                writer_seq: seq,
                item_key: sealed.item_key.to_vec(),
                op: "state-put".into(),
                entry: Some(sealed.envelope),
                feed_seq: None,
            })
            .await
            .unwrap();
        sealed.item_key
    }

    const PREDECESSOR_DEVICE: [u8; 32] = [0xD4; 32];

    /// The arm's two refusals and its one act, on one plane: a non-member's
    /// row that opens under the retired keys is retired and its relay copy
    /// forgotten; the same kind of row naming a VERIFIED MEMBER as its writer
    /// is not, whatever it opens under (a live sibling's rows are its own
    /// pass's business); and a row that opens under no key is left alone —
    /// the guess clause (3) refuses. A predecessor's mint row of a generation
    /// this device neither carries nor reads `Shredded` is kept. A plane
    /// handed no retired keys retires nothing.
    #[tokio::test]
    async fn the_predecessor_arm_retires_only_a_non_members_row_the_retired_keys_open() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        walked_row(&f, THEM, 1, &enrollment_row(THEM)).await;
        let retired = retired_backup_key();
        let attested = crate::attested_predecessors::AttestedPredecessors::from_backup_keys([(
            fauna_core::identity::ActorId([0xA1; 32]),
            &retired,
        )]);

        let reach_key = reach_cell_key(&device_id_of(PREDECESSOR_DEVICE));
        let theirs = retired_schedule_row(
            &f,
            &retired,
            PREDECESSOR_DEVICE,
            4,
            KIND_DEVICE_REACH,
            &reach_key,
        )
        .await;
        let members = retired_schedule_row(
            &f,
            &retired,
            THEM,
            7,
            KIND_DEVICE_REACH,
            &reach_cell_key(&device_id_of(THEM)),
        )
        .await;
        let unrelated = retired_schedule_row(
            &f,
            &fauna_core::crypto::BackupKey::from_bytes([0x44; 32]),
            [0xD5; 32],
            2,
            KIND_DEVICE_SET,
            &fauna_core::hex32::encode(&device_id_of([0xD5; 32])),
        )
        .await;
        let kept_mint = retired_schedule_row(
            &f,
            &retired,
            PREDECESSOR_DEVICE,
            5,
            KIND_GENERATION_MINT,
            &fauna_core::hex32::encode(&[0x5E; 32]),
        )
        .await;
        let ours = [theirs, members, unrelated, kept_mint];
        let named = |nest: &Recorder| -> Vec<([u8; 32], [u8; 32])> {
            nest.0
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _, _)| ours.contains(k))
                .map(|(k, w, _)| (*k, *w))
                .collect()
        };

        // No retired keys: the arm does not run.
        let nest = Recorder::default();
        ensure_reclaimed(&f.store, &plane(&f, &nest), &f.trust, &f.writer_key)
            .await
            .unwrap();
        assert!(named(&nest).is_empty(), "a plane handed no retired keys");

        // Handed them, two passes: one retire, then nothing more to ask.
        let nest = Recorder::default();
        let p = plane(&f, &nest).with_predecessor_machinery_keys(attested.retired_machinery_keys());
        for _ in 0..2 {
            let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
                .await
                .unwrap();
            assert_eq!(
                named(&nest),
                vec![(theirs, device_id_of(PREDECESSOR_DEVICE))],
                "only the non-member's row the retired keys open: {pass:?}"
            );
        }
        let relay_at = |key: [u8; 32]| {
            let p = &p;
            async move { p.relay_rows_at(&key).await.unwrap().len() }
        };
        assert_eq!(
            relay_at(theirs).await,
            0,
            "the retired row's relay copy is forgotten"
        );
        assert_eq!(relay_at(members).await, 1, "a member's row stays");
        assert_eq!(relay_at(unrelated).await, 1, "a row nothing opens stays");
        assert_eq!(relay_at(kept_mint).await, 1, "an unlicensed mint row stays");
        assert!(
            f.store
                .state(KIND_DEVICE_REACH, &reach_key)
                .await
                .unwrap()
                .is_none(),
            "and nothing the arm opened was merged"
        );
    }
    /// **The predecessor arm's shred licence is an authored shred only**
    /// (`account-data-taxonomy.md` § *Fleet-scope reclamation* → *the
    /// authored shred*): a predecessor device's mint row that
    /// opens under the retired keys to a sig-less `Shredded` — what any
    /// holder of the retired `BackupKey` can seal — licenses no retire and
    /// stays; one that opens to a shred a verified member signed is retired.
    /// Red-verified: with `mint_licence` back on `Shredded { .. }` alone, the
    /// unauthored row is retired.
    #[tokio::test]
    async fn the_predecessor_arms_shred_licence_is_an_authored_shred_only() {
        let f = fixture().await;
        f.put(enrollment_row(US)).await;
        let retired = retired_backup_key();
        let attested = crate::attested_predecessors::AttestedPredecessors::from_backup_keys([(
            fauna_core::identity::ActorId([0xA1; 32]),
            &retired,
        )]);
        let core_of = |salt: u8| MintCore {
            parents: vec![],
            member_ids: vec![device_id_of(US)],
            minter: device_id_of(PREDECESSOR_DEVICE),
            key_commitment: [salt; 32],
            minted_at_ms: 1_000,
        };
        let (unsigned_core, signed_core) = (core_of(0x5A), core_of(0x5B));
        let unsigned_g = fauna_core::generation::generation_id(&unsigned_core).unwrap();
        let signed_g = fauna_core::generation::generation_id(&signed_core).unwrap();
        let unsigned = retired_schedule_row_of(
            &f,
            &retired,
            PREDECESSOR_DEVICE,
            5,
            KIND_GENERATION_MINT,
            &fauna_core::hex32::encode(&unsigned_g),
            fauna_core::encoding::canonical_encode(&GenerationMintRecord::Shredded {
                core: unsigned_core,
                shredded_at_ms: 9_000,
                shredded_by: device_id_of(US),
                shredder_sig: vec![],
            })
            .unwrap(),
        )
        .await;
        let signed = retired_schedule_row_of(
            &f,
            &retired,
            PREDECESSOR_DEVICE,
            6,
            KIND_GENERATION_MINT,
            &fauna_core::hex32::encode(&signed_g),
            fauna_core::encoding::canonical_encode(
                &fauna_core::generation::sign_shred(&device_key(US), signed_core, 9_000).unwrap(),
            )
            .unwrap(),
        )
        .await;

        let nest = Recorder::default();
        let p = plane(&f, &nest).with_predecessor_machinery_keys(attested.retired_machinery_keys());
        let pass = ensure_reclaimed(&f.store, &p, &f.trust, &f.writer_key)
            .await
            .unwrap();
        let asked = |item: [u8; 32]| nest.0.lock().unwrap().iter().any(|(i, _, _)| *i == item);
        assert!(
            asked(signed),
            "an authored shred licenses the retire: {pass:?}"
        );
        assert!(
            !asked(unsigned),
            "an unauthored shred licenses nothing: {pass:?}"
        );
        assert_eq!(p.relay_rows_at(&unsigned).await.unwrap().len(), 1);
    }
}
