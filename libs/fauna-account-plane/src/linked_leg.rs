//! **The secondary leg** — a linked nest carrying the `account_replica`
//! capability is completed by the device (`account-sync-plane.md` § The bind
//! leg, ruling 4; `account-data-taxonomy.md` § The generation machinery → *A
//! holder change re-receipts and never mints*, the linked-holder clause, and
//! *Fleet-scope reclamation*, the second-holder ruling).
//!
//! A device binds to one nest, and rulings 1 and 2 make that nest whole. A nest
//! the user linked (`nest/private-mode.md` § Pairing Flow) but never bound a
//! device to would hold nothing, so every seed-holding runtime also completes
//! each linked nest whose pairing carries [`ACCOUNT_REPLICA`](fauna_protocol::pair::capability::ACCOUNT_REPLICA), over
//! a second owner-authenticated connection the host opens
//! ([`LinkedConnection`]). No nest talks to another.
//!
//! # What one pass does, per linked nest
//!
//! 1. **The channel binding first.** The connection's bound identity must be
//!    the pairing row's nest id; a connection presenting another is refused
//!    before any account data moves ([`LinkedOutcome::IdentityMismatch`]).
//! 2. **The full-state reconcile** of both state scopes over that connection,
//!    through a linked plane ([`AccountStatePlane::new_linked`]): its rows are
//!    applied as any walk's are, opened and verified — no nest is trusted for
//!    content — and its answer is **that nest's listing**, the evidence every
//!    step below reads. No watermark is sent, banked or voided, no serve
//!    coordinate is stamped, and our own published high-water (the bound
//!    nest's) never moves.
//! 3. **The publish diff** against that listing (`crate::publish_diff`), with
//!    ruling 1's own vouching.
//! 4. **The escrow deposit and holdings check** at that holder
//!    ([`crate::generation_reescrow::ensure_deposited_at_linked`]): the
//!    receipt verified against the pairing row's nest id and written in that
//!    holder's own receipt cell. Such a receipt acks no tip — sealing reads
//!    the bound nest's receipt alone — so nothing is minted here.
//! 5. **A removed member's grant is revoked** at this nest, by key
//!    ([`crate::removed_grants`]; ruling 8): once merged state reads another
//!    device removed, this nest's roster is read and the grant of every row
//!    claiming a removed principal is revoked — after the reconcile, so merged
//!    state holds whatever removal this nest served, and ahead of the retires
//!    below, which wait behind this nest's gate for as long as that gate
//!    counts the removed device's walk mark.
//! 6. **Retires follow the rows.** Every retire in the store's retire record
//!    ([`IssuedRetire`] — what the bound planes sent since the last leg run,
//!    from whichever co-located process pumps; ruling 5) is issued again when
//!    this nest's listing shows that writer's row for the item live at the
//!    retired coordinate or below it, at the seq this nest lists (ruling 6:
//!    the bound nest may have collapsed the row this nest still holds); and
//!    every row this nest lists under
//!    a generation merged state reads `Shredded` is retired whoever wrote it —
//!    the rows sealed under it, then its mint rows behind the dataless belt,
//!    then its receipts behind the belt that sweeps that nest's wraps.
//! 7. **The shredded generations' wraps** the holdings answer still names are
//!    deleted at that holder: `Shredded` is absorbing, and a shred must reach
//!    every holder. So are the wraps of every generation in the store's
//!    let-go set (`crate::generation_let_go`): the user let it go, and the
//!    act owes every holder the delete.
//!
//! # The removed-device arm — what ends a run
//!
//! Removal evidence — a device-set row that opens as `Removed`, for a device
//! merged state reads removed — is retired at a nest only by a leg run that
//! found it at every linked replica of that nest (`account-sync-plane.md`
//! § The bind leg, ruling 7): a device bound to a linked nest reads a removal
//! only from a row in that nest's feed, and the bound nest's gate never
//! waited for it. So the reclamation pass retires none
//! (`crate::generation_reclaim`), step 3's diff carries it — a departing
//! device's own removal row included, the one exception to the member test
//! (`crate::publish_diff`) — and [`retire_carried_evidence`] ends the run:
//! each evidence row the relay plane held when the run began
//! ([`crate::generation_reclaim::removal_evidence`], read before the first
//! linked nest is touched, since a refused push retires the local copy
//! mid-run) that the bound nest has served and every replica now lists at
//! that coordinate, or refused for good ([`Carried`]), is retired at each
//! replica that lists it and at the bound nest, each behind that nest's own
//! gate. A run that missed a replica retires no evidence anywhere, and
//! neither does a process that runs no leg.
//!
//! Every step is attempted; a failure is recorded on the nest's report and
//! the next run retries. The leg runs in the co-located runtime that holds
//! the **seed-leg role** (`account-runtime.md` § Multi-instance concurrency →
//! *The seed-leg role*): inside every full pass when that runtime also holds
//! the engine role — the prologue and the backstop, never a nudge — and in
//! its seed pass, at the same wakes, when a seedless agent pumps beside it.
//! It needs no device principal: every door it uses admits the authenticated
//! account.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use fauna_account_store::types::WriterId;
use fauna_account_store::{backend::StoreBackend, store::AccountStore};
use fauna_client_core::recovery_chain::{
    ChainReconcile, RpcChainDoor, reconcile_registration_chains,
};
use fauna_core::crypto::AccountStateKeySchedule;
use fauna_core::generation::GenerationMintRecord;
use fauna_protocol::account_state::{ACCOUNT_STATE_FLEET_SCOPE, ACCOUNT_STATE_SCOPE};
use fauna_protocol::generation_escrow::{
    EscrowDeleteReply, EscrowDeleteRequest, KIND_ESCROW_DELETE,
};
use fauna_protocol::merge_policy::{KIND_ESCROW_RECEIPT, KIND_GENERATION_MINT};
use fauna_protocol::pair::{PairListReply, PairListRequest};
use fauna_protocol::{ByteBuf, RpcErrorClass, RpcRequester};

use crate::account_state_plane::{AccountStatePlane, IssuedRetire, Listing, RetireOutcome};
use crate::generation_reclaim::EvidenceRow;
use crate::generation_reescrow::ReescrowPass;
use crate::generation_store::live_rows;
use crate::generation_tip::{GenerationTrust, RetainedKeyCustody};
use crate::publish_diff::DiffPush;

/// The pairing-row shape the leg and the seed-alone replacement's fan-out
/// share — the kind it lists them with, a linked nest, and the connection the
/// host opens to one — lives below both (`fauna_client_core::linked_nests`).
pub use fauna_client_core::linked_nests::{
    KIND_PAIR_LIST, LinkedConnection, LinkedNestTarget, linked_targets,
};

/// What one pass did at every linked nest (the pump's `linked` slot).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkedPass {
    /// The account's pairings could not be listed: nothing was attempted.
    pub unlisted: bool,
    pub nests: Vec<LinkedNestPass>,
    /// The RecoveryKey registration chain's reconcile at every linked nest
    /// that carries an address — the replicas in [`Self::nests`] and the
    /// pairings without the capability alike ([`reconcile_linked_chain`]).
    pub chains: Vec<LinkedChainPass>,
    /// What the removed-device arm did when the run ended
    /// ([`retire_carried_evidence`]).
    pub evidence: EvidencePass,
}

/// One linked nest's registration-chain reconcile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedChainPass {
    pub nest_id: [u8; 32],
    pub outcome: LinkedChainOutcome,
}

/// How the chain reconcile went at one linked nest
/// (`identity-succession.md` § Enforcement on the home nest → *Every nest the
/// identity is linked to*, clause (b)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkedChainOutcome {
    /// The host could not open the connection; owed until the next pass.
    Unreachable,
    /// The connection is bound to another identity than the pairing row's:
    /// nothing was read or submitted.
    IdentityMismatch { presented: [u8; 32] },
    /// The reconcile ran. A `Forked` answer is reported and logged, never
    /// repaired: neither chain is submitted over the other.
    Reconciled(ChainReconcile),
    /// A chain could not be read or a nest refused a record; retried next
    /// pass from whatever each nest then holds.
    Failed(String),
}

/// Carry the account's RecoveryKey registration chain between the bound nest
/// and one linked nest, over the connection the leg already holds
/// (`fauna_client_core::recovery_chain`): whichever holds the longer chain
/// extends the other. The channel binding first, as for every step of the
/// leg — a box that merely answers the address is asked nothing. The bound
/// side rides the **owner session**: `fauna.recovery.registration.submit` is
/// USER class, which a store principal's connection is not.
pub async fn reconcile_linked_chain<R: RpcRequester>(
    session_rpc: &R,
    actor_id: [u8; 32],
    target: &LinkedNestTarget,
    conn: &LinkedConnection<R>,
) -> LinkedChainOutcome {
    if conn.bound_identity != target.nest_id {
        return LinkedChainOutcome::IdentityMismatch {
            presented: conn.bound_identity,
        };
    }
    let door = |rpc| RpcChainDoor { rpc, actor_id };
    match reconcile_registration_chains(&door(session_rpc), &door(&conn.rpc)).await {
        Ok(outcome) => LinkedChainOutcome::Reconciled(outcome),
        Err(e) => LinkedChainOutcome::Failed(e.to_string()),
    }
}

/// What the removed-device arm did with the removal evidence the relay plane
/// held when the run began (module docs → *The removed-device arm*).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EvidencePass {
    /// Evidence rows the bound nest confirmed off its feed; their relay rows
    /// are forgotten.
    pub retired: usize,
    /// Retires a nest's gate deferred — the bound nest's, withheld against
    /// its watermark or refused, and a linked nest's — while the walkers
    /// bound there have not read the row. Asked again by a later run.
    pub deferred: usize,
    /// Retires not sent to a linked nest because it lists a replica this run
    /// did not reach: that nest keeps the row for a run that reaches both.
    pub held: usize,
    /// Every retire that failed, in order.
    pub errors: Vec<String>,
}

/// Where one linked nest stands on one evidence row once this run's fleet
/// reconcile and diff are done there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carried {
    /// Its listing shows that writer's row for the item at that coordinate.
    Listed,
    /// It refused the push for good: it held the row and has retired it.
    Refused,
    /// Neither: the row has not been carried there.
    Missing,
}

/// One linked nest's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedNestPass {
    pub nest_id: [u8; 32],
    pub outcome: LinkedOutcome,
}

/// How far the leg got at one linked nest.
// `allow`, not a `Box`: one of these per linked nest per pass, built once and
// read by the report — a cold path, where the wide `Completed` costs a move
// and boxing it would cost every match site an indirection.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkedOutcome {
    /// The host could not open the connection; retried next pass.
    Unreachable,
    /// The connection is bound to another identity than the pairing row's:
    /// refused before any account data moved.
    IdentityMismatch { presented: [u8; 32] },
    /// The leg ran; see what each step did. Boxed: the completion carries
    /// two walk reports per scope, an order of magnitude past the other
    /// variants.
    Completed(Box<LinkedCompletion>),
}

/// What the leg's steps did at one reachable linked nest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkedCompletion {
    /// The delegable scope's diff.
    pub diff: DiffPush,
    /// The fleet scope's diff.
    pub fleet_diff: DiffPush,
    /// The delegable scope's cover step at this nest, retires only
    /// (`crate::delegable_reclaim::reclaim_below_cover`;
    /// `delegable-scope-reclamation.md` § Delegable-scope reclamation, *At a
    /// linked nest*): every row this nest lists below a cover it lists.
    pub cover_reclaim: Option<crate::delegable_reclaim::CoverReclaim>,
    /// The deposit at this holder, when it ran.
    pub deposit: Option<ReescrowPass>,
    /// The removed members' grants revoked at this nest (step 5). Its own
    /// failures are in [`Self::errors`] too.
    pub removed_grants: crate::removed_grants::RemovedGrantsPass,
    /// Rows this nest confirmed retired.
    pub retired: usize,
    /// Retires this nest deferred (its own gate or belt); asked again next pass.
    pub deferred: usize,
    /// Shredded generations whose wraps this holder deleted.
    pub wraps_swept: usize,
    /// Where this nest stands on each evidence row the run began with, in
    /// the run's order — `None` when its fleet scope could not be reconciled
    /// and diffed whole, which the removed-device arm reads as a replica the
    /// run did not reach.
    pub evidence: Option<Vec<Carried>>,
    /// Every step that failed, in order.
    pub errors: Vec<String>,
}

/// The pairings the leg completes: [`linked_targets`] carrying
/// [`ACCOUNT_REPLICA`](fauna_protocol::pair::capability::ACCOUNT_REPLICA).
#[must_use]
pub fn replica_targets(reply: &PairListReply, bound: &[[u8; 32]]) -> Vec<LinkedNestTarget> {
    let mut targets = linked_targets(reply, bound);
    targets.retain(|t| t.replica);
    targets
}

/// List the account's pairings at a nest and pick the replicas it names
/// ([`replica_targets`]) — what the removed-device arm asks a linked nest.
///
/// # Errors
///
/// The list request failed.
pub async fn list_replica_targets<R: RpcRequester>(
    session_rpc: &R,
    bound: &[[u8; 32]],
) -> Result<Vec<LinkedNestTarget>> {
    let mut targets = list_linked_targets(session_rpc, bound).await?;
    targets.retain(|t| t.replica);
    Ok(targets)
}

/// List the account's pairings on the bound nest and pick the run's targets
/// ([`linked_targets`]).
///
/// # Errors
///
/// The list request failed.
pub async fn list_linked_targets<R: RpcRequester>(
    session_rpc: &R,
    bound: &[[u8; 32]],
) -> Result<Vec<LinkedNestTarget>> {
    let reply: PairListReply = session_rpc
        .request(
            KIND_PAIR_LIST,
            PairListRequest {
                extra: Default::default(),
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("{KIND_PAIR_LIST}: {e}"))?;
    Ok(linked_targets(&reply, bound))
}

/// What one linked nest's leg is run with — everything but the connection.
pub struct LinkedCtx<'a, 'p, B: StoreBackend, R: RpcRequester> {
    pub store: &'a AccountStore<B>,
    /// The bound nest's fleet plane: the receipt a deposit earns is written
    /// through it, like every row this device authors.
    pub bound_fleet: &'a AccountStatePlane<'p, B, R>,
    pub schedule: &'a AccountStateKeySchedule,
    pub trust: &'a GenerationTrust,
    pub writer_key: &'a SigningKey,
    pub custody: Option<&'a dyn RetainedKeyCustody>,
}

/// One retire the leg owes a linked nest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwedRetire {
    pub item_key: [u8; 32],
    pub writer: WriterId,
    pub writer_seq: u64,
    pub no_rows_sealed_under: Option<[u8; 32]>,
    pub delete_escrow_wraps: bool,
    /// The bound nest has the row off its feed too: once this nest confirms,
    /// the copy this reconcile re-recorded is forgotten locally.
    pub forget_after: bool,
}

/// The mirrored half of step 6: every retire the bound planes issued on
/// `scope` whose **cell** — that writer's row for that item — `listing` shows
/// live at the retired coordinate or below it, named at the seq this nest
/// lists (`account-sync-plane.md` § The bind leg, ruling 6). A nest collapses
/// a writer's rows per item, so a lower seq here is a row the retired one
/// superseded at the bound nest: a signed-out device's enrollment, under the
/// removal row it wrote over it. A higher seq is a newer word, and stays.
/// One retire per cell, in the record's order: a second entry for a cell
/// already owed only adds its `settled`.
#[must_use]
pub fn mirrored_retires(
    scope: &str,
    listing: &Listing,
    issued: &[IssuedRetire],
) -> Vec<OwedRetire> {
    let mut owed: Vec<OwedRetire> = Vec::new();
    let mut cells: BTreeMap<(WriterId, [u8; 32]), usize> = BTreeMap::new();
    for r in issued.iter().filter(|r| r.scope == scope) {
        let cell = (r.writer, r.item_key);
        let Some(listed) = listing.get(&cell).copied().filter(|l| *l <= r.writer_seq) else {
            continue;
        };
        if let Some(at) = cells.get(&cell) {
            owed[*at].forget_after |= r.settled;
            continue;
        }
        cells.insert(cell, owed.len());
        owed.push(OwedRetire {
            item_key: r.item_key,
            writer: r.writer,
            writer_seq: listed,
            no_rows_sealed_under: r.no_rows_sealed_under,
            delete_escrow_wraps: r.delete_escrow_wraps,
            forget_after: r.settled,
        });
    }
    owed
}

/// The generations merged state reads `Shredded`.
///
/// # Errors
///
/// Store I/O.
pub async fn shredded_generations<B: StoreBackend>(
    store: &AccountStore<B>,
) -> Result<BTreeSet<[u8; 32]>> {
    let mut shredded = BTreeSet::new();
    for entry in live_rows(store, KIND_GENERATION_MINT).await? {
        let Ok(id) = fauna_core::hex32::decode(&entry.key) else {
            continue;
        };
        if fauna_core::hex32::encode(&id) != entry.key {
            continue;
        }
        if let Ok(GenerationMintRecord::Shredded { .. }) =
            fauna_core::encoding::canonical_decode::<GenerationMintRecord>(&entry.value)
        {
            shredded.insert(id);
        }
    }
    Ok(shredded)
}

/// The shred half of step 6 on one linked plane: every row `listing` shows
/// that is sealed under a `shredded` generation (by its cleartext header, read
/// from the relay row this reconcile recorded — never opened: no device keeps
/// the key), whoever wrote it; then, on the fleet scope, each shredded
/// generation's mint rows behind the dataless belt and its receipts behind the
/// sweeping one. `Shredded` is absorbing, so any member retires them
/// (`account-data-taxonomy.md` § *Fleet-scope reclamation*, the second-holder
/// ruling).
///
/// # Errors
///
/// Store I/O.
pub async fn shred_retires<B, R>(
    store: &AccountStore<B>,
    plane: &AccountStatePlane<'_, B, R>,
    listing: &Listing,
    shredded: &BTreeSet<[u8; 32]>,
) -> Result<Vec<OwedRetire>>
where
    B: StoreBackend,
    R: RpcRequester,
{
    let mut owed = Vec::new();
    if shredded.is_empty() {
        return Ok(owed);
    }
    // Rows sealed under a shredded generation — first, so the belts below find
    // each generation dataless.
    let items: BTreeSet<[u8; 32]> = listing.keys().map(|(_, item)| *item).collect();
    for item_key in items {
        for row in plane.relay_rows_at(&item_key).await? {
            if listing.get(&(row.writer, item_key)) != Some(&row.writer_seq) {
                continue;
            }
            let sealed_under = row
                .entry
                .as_deref()
                .and_then(fauna_core::account_entry_crypto::peek_generation_id);
            if sealed_under.is_some_and(|g| shredded.contains(&g)) {
                owed.push(OwedRetire {
                    item_key,
                    writer: row.writer,
                    writer_seq: row.writer_seq,
                    no_rows_sealed_under: None,
                    delete_escrow_wraps: false,
                    forget_after: true,
                });
            }
        }
    }
    if plane.scope() != ACCOUNT_STATE_FLEET_SCOPE {
        return Ok(owed);
    }
    let listed_at = |item_key: [u8; 32]| {
        listing
            .iter()
            .filter(move |((_, item), _)| *item == item_key)
            .map(|((writer, _), seq)| (*writer, *seq))
    };
    for g in shredded {
        if let Some(item_key) =
            plane.gen0_item_key(KIND_GENERATION_MINT, &fauna_core::hex32::encode(g))
        {
            for (writer, writer_seq) in listed_at(item_key) {
                owed.push(OwedRetire {
                    item_key,
                    writer,
                    writer_seq,
                    no_rows_sealed_under: Some(*g),
                    delete_escrow_wraps: false,
                    // The marker stays in merged state: it is what makes the
                    // generation absorbing here.
                    forget_after: false,
                });
            }
        }
    }
    for receipt in live_rows(store, KIND_ESCROW_RECEIPT).await? {
        let Some(g) = crate::generation_reclaim::receipt_generation(&receipt.key) else {
            continue;
        };
        if !shredded.contains(&g) {
            continue;
        }
        let Some(item_key) = plane.gen0_item_key(KIND_ESCROW_RECEIPT, &receipt.key) else {
            continue;
        };
        for (writer, writer_seq) in listed_at(item_key) {
            owed.push(OwedRetire {
                item_key,
                writer,
                writer_seq,
                no_rows_sealed_under: Some(g),
                delete_escrow_wraps: true,
                forget_after: true,
            });
        }
    }
    Ok(owed)
}

/// Run the secondary leg at one linked nest over `conn` (module docs, steps
/// 1–7). `issued` is the store's retire record as this leg run read it, and
/// `evidence` the removal evidence the relay plane held when the run began.
pub async fn complete_linked_nest<B, R>(
    ctx: &LinkedCtx<'_, '_, B, R>,
    target: &LinkedNestTarget,
    conn: &LinkedConnection<R>,
    issued: &[IssuedRetire],
    evidence: &[EvidenceRow],
) -> LinkedOutcome
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    // 1. The channel binding, before anything else is sent.
    if conn.bound_identity != target.nest_id {
        tracing::warn!(
            linked = %fauna_core::hex32::encode(&target.nest_id),
            presented = %fauna_core::hex32::encode(&conn.bound_identity),
            "secondary leg: the connection to a linked nest is bound to another identity than \
             its pairing row — refused before any account data moves"
        );
        return LinkedOutcome::IdentityMismatch {
            presented: conn.bound_identity,
        };
    }
    let mut done = LinkedCompletion::default();
    let shredded = match shredded_generations(ctx.store).await {
        Ok(s) => s,
        Err(e) => {
            done.errors.push(format!("shredded generations: {e:#}"));
            BTreeSet::new()
        }
    };
    let mut owed: BTreeMap<String, Vec<OwedRetire>> = BTreeMap::new();
    let mut planes = Vec::new();
    // A linked plane is handed no predecessor schedule, by rule
    // (`succession-aftermath.md` § Re-key scope → *Which walks carry*): the
    // bound nest's walk alone carries a predecessor identity's delegable
    // rows, and this nest receives them as the carrying device's own rows
    // through the diff below.
    for scope in [ACCOUNT_STATE_SCOPE, ACCOUNT_STATE_FLEET_SCOPE] {
        let plane = match AccountStatePlane::new_linked(
            ctx.store,
            &conn.rpc,
            ctx.schedule,
            ctx.writer_key,
            ctx.trust,
            scope,
        ) {
            Ok(p) => match ctx.custody {
                Some(c) => p.with_generation_custody(c),
                None => p,
            },
            Err(e) => {
                done.errors.push(format!("{scope}: {e:#}"));
                continue;
            }
        };
        planes.push(plane);
    }
    // 2.–3. Reconcile and diff, per scope.
    for plane in &planes {
        let scope = plane.scope().to_string();
        if let Err(e) = plane.reconcile().await {
            done.errors.push(format!("reconcile ({scope}): {e:#}"));
            continue;
        }
        match crate::publish_diff::publish_diff_naming_refusals(
            ctx.store,
            plane,
            ctx.trust,
            ctx.writer_key,
        )
        .await
        {
            Ok((d, refused)) if scope == ACCOUNT_STATE_FLEET_SCOPE => {
                done.fleet_diff = d;
                // Where this nest stands on the run's removal evidence, read
                // off the listing the diff just raised.
                done.evidence = plane.listing().map(|listing| {
                    evidence
                        .iter()
                        .map(|row| {
                            if listing.get(&(row.writer, row.item_key)) == Some(&row.writer_seq) {
                                Carried::Listed
                            } else if refused.contains(&(row.writer, row.item_key, row.writer_seq))
                            {
                                Carried::Refused
                            } else {
                                Carried::Missing
                            }
                        })
                        .collect()
                });
            }
            Ok((d, _)) => {
                done.diff = d;
                // The retire half of the cover step, on this nest's own
                // listing and serve order — no retire record, no relay copy
                // forgotten: the bound nest's step owns those. A linked plane
                // hands nothing over, so it needs no membership answer.
                match crate::delegable_reclaim::reclaim_below_cover(
                    ctx.store,
                    plane,
                    ctx.trust,
                    ctx.writer_key,
                    None,
                )
                .await
                {
                    Ok(r) => done.cover_reclaim = r,
                    Err(e) => done.errors.push(format!("cover step ({scope}): {e:#}")),
                }
            }
            Err(e) => done.errors.push(format!("publish diff ({scope}): {e:#}")),
        }
    }
    // 4. The holdings check and the deposit at this holder.
    let holdings = match crate::bind_leg::holder_holdings(&conn.rpc).await {
        Ok(h) => Some(h),
        Err(e) => {
            done.errors.push(format!("holdings check: {e:#}"));
            None
        }
    };
    match crate::generation_reescrow::ensure_deposited_at_linked(
        ctx.store,
        ctx.bound_fleet,
        &conn.rpc,
        target.nest_id,
        ctx.trust,
        ctx.writer_key,
        holdings.as_ref(),
    )
    .await
    {
        Ok(p) => done.deposit = Some(p),
        Err(e) => done.errors.push(format!("escrow deposit: {e:#}")),
    }
    // 5. The removed members' grants at this nest, ahead of the retires its
    // gate would otherwise hold behind their walk marks.
    done.removed_grants = crate::removed_grants::revoke_removed_grants(
        ctx.store,
        ctx.trust,
        &ctx.writer_key.verifying_key().to_bytes(),
        &conn.rpc,
        crate::removed_grants::Roster::Unread,
    )
    .await;
    done.errors.extend(
        done.removed_grants
            .errors
            .iter()
            .map(|e| format!("removed grants: {e}")),
    );
    // 6. Retires follow the rows. The listing each plane just completed is
    // re-read: the diff raised it with every row it stored.
    for plane in &planes {
        let Some(listing) = plane.listing() else {
            continue;
        };
        let scope = plane.scope().to_string();
        let mut rows = match shred_retires(ctx.store, plane, &listing, &shredded).await {
            Ok(r) => r,
            Err(e) => {
                done.errors.push(format!("shred retires ({scope}): {e:#}"));
                Vec::new()
            }
        };
        for mirrored in mirrored_retires(&scope, &listing, issued) {
            if !rows
                .iter()
                .any(|r| (r.writer, r.item_key) == (mirrored.writer, mirrored.item_key))
            {
                rows.push(mirrored);
            }
        }
        owed.insert(scope, rows);
    }
    for plane in &planes {
        for row in owed.remove(plane.scope()).unwrap_or_default() {
            match plane
                .retire(
                    &row.item_key,
                    &row.writer,
                    row.writer_seq,
                    row.no_rows_sealed_under.as_ref(),
                    row.delete_escrow_wraps,
                )
                .await
            {
                Ok(RetireOutcome::Retired | RetireOutcome::Gone) => {
                    done.retired += 1;
                    if row.forget_after
                        && let Err(e) = plane.relay_forget(&row.writer, &row.item_key).await
                    {
                        done.errors
                            .push(format!("forgetting a retired copy: {e:#}"));
                    }
                }
                Ok(RetireOutcome::NotYetStable | RetireOutcome::GenerationInUse) => {
                    done.deferred += 1;
                }
                Ok(RetireOutcome::Unsupported) => break,
                Err(e) => {
                    done.errors
                        .push(format!("retire ({}): {e:#}", plane.scope()));
                    break;
                }
            }
        }
    }
    // 7. The shredded generations' wraps this holder still holds — and the
    // let-go's (`crate::generation_let_go`): a dead generation the user let go
    // owes its wraps at every holder too.
    if let Some(held) = &holdings {
        let mut owed_sweeps = shredded.clone();
        match ctx.store.let_go().await {
            Ok(let_go) => owed_sweeps.extend(let_go),
            Err(e) => done.errors.push(format!("let-go set: {e:#}")),
        }
        for g in held.intersection(&owed_sweeps) {
            match sweep_wraps(&conn.rpc, g).await {
                Ok(()) => done.wraps_swept += 1,
                Err(e) => done.errors.push(format!("{e:#}")),
            }
        }
    }
    if !done.errors.is_empty() {
        tracing::debug!(
            linked = %fauna_core::hex32::encode(&target.nest_id),
            errors = ?done.errors,
            "secondary leg: steps failed at a linked nest (retried next pass)"
        );
    }
    LinkedOutcome::Completed(Box::new(done))
}

/// **The removed-device arm** — what ends a leg run (module docs;
/// `account-sync-plane.md` § The bind leg, ruling 7(c)). `evidence` is the
/// removal evidence the relay plane held when the run began, `nests` what the
/// run did at each linked replica the bound nest lists, `conns` the
/// connections it opened, by nest id, and `bound` the bound nest's identities.
///
/// The arm runs only when the run reached every one of those replicas — each
/// `Completed`, its fleet scope reconciled and diffed whole; none listed is
/// every one reached. A row every replica lists or refused for good is
/// **carried** — once the bound nest has served it too
/// ([`EvidenceRow::feed_seq`]): the bound nest is a replica of each nest that
/// lists it, and a row this replica knows only from a linked nest's feed has
/// not been found there until the pass's diff has pushed it. A carried row is
/// then retired:
///
/// - at each replica that lists it, behind that nest's own gate — unless that
///   nest's own pairings name a replica that is neither the bound nest nor one
///   this run reached, which keeps the row for a run that reaches both
///   ([`EvidencePass::held`]; a pair list that cannot be read holds too);
/// - at the bound nest, withheld while the row's serve coordinate sits above
///   the gate's watermark as the pass's reclamation withholds, and its relay
///   row forgotten once that nest answers retired or gone. The retire is not
///   entered in the store's retire record
///   ([`AccountStatePlane::retire_unrecorded`]).
///
/// It reads merged state and the run's own listings, and needs no record.
pub async fn retire_carried_evidence<B, R>(
    ctx: &LinkedCtx<'_, '_, B, R>,
    evidence: &[EvidenceRow],
    nests: &[LinkedNestPass],
    conns: &BTreeMap<[u8; 32], LinkedConnection<R>>,
    bound: &[[u8; 32]],
) -> EvidencePass
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let mut pass = EvidencePass::default();
    if evidence.is_empty() {
        return pass;
    }
    // Every replica reached, or no evidence is retired anywhere.
    let mut reached = Vec::new();
    for nest in nests {
        let (LinkedOutcome::Completed(done), Some(conn)) =
            (&nest.outcome, conns.get(&nest.nest_id))
        else {
            return pass;
        };
        let Some(carried) = done
            .evidence
            .as_deref()
            .filter(|c| c.len() == evidence.len())
        else {
            return pass;
        };
        let plane = match AccountStatePlane::new_linked(
            ctx.store,
            &conn.rpc,
            ctx.schedule,
            ctx.writer_key,
            ctx.trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        ) {
            Ok(p) => p,
            Err(e) => {
                pass.errors.push(format!("linked plane: {e:#}"));
                return pass;
            }
        };
        reached.push((nest.nest_id, conn, carried, plane));
    }
    let reached_ids: Vec<[u8; 32]> = reached.iter().map(|(id, ..)| *id).collect();
    // Does a linked nest list a replica this run did not reach? Asked once per
    // nest, and only when a retire is owed there.
    let mut holds: BTreeMap<[u8; 32], bool> = BTreeMap::new();
    let watermark = ctx.bound_fleet.retirable_through_seq();
    for (i, row) in evidence.iter().enumerate() {
        crate::pass_breath::pass_breath().await;
        // Found at the bound nest, and at every replica — or kept everywhere.
        let Some(served_at) = row.feed_seq else {
            continue;
        };
        if reached
            .iter()
            .any(|(_, _, carried, _)| carried[i] == Carried::Missing)
        {
            continue;
        }
        for (nest_id, conn, carried, plane) in &reached {
            if carried[i] != Carried::Listed {
                continue;
            }
            let hold = match holds.get(nest_id) {
                Some(hold) => *hold,
                None => {
                    let hold = match list_replica_targets(&conn.rpc, bound).await {
                        Ok(theirs) => theirs.iter().any(|t| !reached_ids.contains(&t.nest_id)),
                        Err(e) => {
                            pass.errors.push(format!("a linked nest's pairings: {e:#}"));
                            true
                        }
                    };
                    holds.insert(*nest_id, hold);
                    hold
                }
            };
            if hold {
                pass.held += 1;
                continue;
            }
            match plane
                .retire(&row.item_key, &row.writer, row.writer_seq, None, false)
                .await
            {
                Ok(RetireOutcome::Retired | RetireOutcome::Gone | RetireOutcome::Unsupported) => {}
                Ok(RetireOutcome::NotYetStable | RetireOutcome::GenerationInUse) => {
                    pass.deferred += 1;
                }
                Err(e) => pass
                    .errors
                    .push(format!("retiring evidence at a linked nest: {e:#}")),
            }
        }
        if watermark.is_some_and(|watermark| served_at > watermark) {
            pass.deferred += 1;
            continue;
        }
        match ctx
            .bound_fleet
            .retire_unrecorded(&row.item_key, &row.writer, row.writer_seq)
            .await
        {
            Ok(RetireOutcome::Retired | RetireOutcome::Gone) => {
                pass.retired += 1;
                if let Err(e) = ctx
                    .bound_fleet
                    .relay_forget(&row.writer, &row.item_key)
                    .await
                {
                    pass.errors
                        .push(format!("forgetting retired evidence: {e:#}"));
                }
            }
            Ok(RetireOutcome::NotYetStable | RetireOutcome::GenerationInUse) => {
                pass.deferred += 1;
            }
            Ok(RetireOutcome::Unsupported) => {}
            Err(e) => pass
                .errors
                .push(format!("retiring evidence at the bound nest: {e:#}")),
        }
    }
    if !pass.errors.is_empty() {
        tracing::debug!(
            errors = ?pass.errors,
            "secondary leg: the removed-device arm's retires failed (retried next run)"
        );
    }
    pass
}

/// Delete one generation's wraps at a holder (`fauna.generation.escrow.delete`).
///
/// # Errors
///
/// The door failed.
pub async fn sweep_wraps<R: RpcRequester>(rpc: &R, generation: &[u8; 32]) -> Result<()> {
    let _: EscrowDeleteReply = rpc
        .request(
            KIND_ESCROW_DELETE,
            EscrowDeleteRequest {
                generation_id: ByteBuf::from(generation.to_vec()),
                extra: Default::default(),
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("{KIND_ESCROW_DELETE}: {e}"))
        .with_context(|| {
            format!(
                "sweeping shredded generation {}'s wraps at a linked holder",
                fauna_core::hex32::encode(generation)
            )
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_protocol::pair::capability;

    /// A linked nest's plane is handed no predecessor schedule or retired
    /// fleet-only key, by rule
    /// (`succession-aftermath.md` § Re-key scope → *Which walks carry*): the
    /// bound nest's walk alone carries, because a retired key is tried only
    /// on what the home nest serves. If this trips, read that paragraph
    /// before wiring `with_predecessor_schedules` onto a linked plane.
    #[test]
    fn the_linked_leg_hands_its_planes_no_predecessor_schedule() {
        let src = include_str!("linked_leg.rs");
        let production = src
            .split_once("#[cfg(test)]\nmod tests")
            .expect("the test module moved")
            .0;
        assert!(
            !production.contains("with_predecessor_schedules("),
            "the linked leg grew a `with_predecessor_schedules` call: the carry \
             is the bound nest's walk alone (`succession-aftermath.md` § Re-key \
             scope → *Which walks carry*)"
        );
        for retired in [
            "with_predecessor_mint_keys(",
            "with_predecessor_machinery_keys(",
        ] {
            assert!(
                !production.contains(retired),
                "the linked leg grew a `{retired}` call: a retired key is tried only \
                 on what the bound nest serves (`succession-aftermath.md` § Re-key \
                 scope → *Which walks carry*)"
            );
        }
    }

    use crate::generation_fixture_test_support::*;
    use fauna_account_store::sqlite::SqliteBackend;
    use fauna_account_store::types::RelayRow;
    use fauna_core::account_entry_crypto::{EntryCoordinates, EntryPlaintext, seal_entry_v2};
    use fauna_core::crypto::FleetOnlySchedule;
    use fauna_protocol::account_state::{
        AccountStateRetireReply, AccountStateRetireRequest, KIND_STATE_RETIRE,
    };
    use fauna_protocol::error::RpcError;
    use fauna_protocol::merge_policy::KIND_DEVICE_ENDPOINTS;
    use fauna_protocol::pair::PairingRow;
    use std::sync::{Arc, Mutex};

    /// A nest that records every request it is sent, answers every retire
    /// `retired` and every feed request with an empty page — and its pairings
    /// (`.1`) once a test states them ([`Wire::lists`]); until then the pair
    /// list is a kind it does not serve.
    #[derive(Clone, Default)]
    struct Wire(Arc<Mutex<Vec<Sent>>>, Arc<Mutex<Option<Vec<PairingRow>>>>);

    /// One request the [`Wire`] saw: its kind and its canonical payload.
    type Sent = (&'static str, Vec<u8>);

    impl Wire {
        fn lists(&self, pairings: Vec<PairingRow>) {
            *self.1.lock().unwrap() = Some(pairings);
        }
        fn kinds(&self) -> Vec<&'static str> {
            self.0.lock().unwrap().iter().map(|(k, _)| *k).collect()
        }
        fn retires(&self) -> Vec<AccountStateRetireRequest> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter(|(k, _)| *k == KIND_STATE_RETIRE)
                .map(|(_, b)| fauna_core::encoding::canonical_decode(b).unwrap())
                .collect()
        }
    }

    #[derive(Debug)]
    struct WireErr(RpcError);
    impl std::fmt::Display for WireErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0.code)
        }
    }
    impl RpcErrorClass for WireErr {
        fn is_rejection(&self) -> bool {
            true
        }
        fn as_rpc_error(&self) -> Option<&RpcError> {
            Some(&self.0)
        }
    }

    impl RpcRequester for Wire {
        type Error = WireErr;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, WireErr>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_core::encoding::canonical_encode(&payload).unwrap();
            self.0.lock().unwrap().push((kind, bytes));
            let reply = match kind {
                KIND_STATE_RETIRE => {
                    fauna_core::encoding::canonical_encode(&AccountStateRetireReply {
                        retired: true,
                        extra: Default::default(),
                    })
                }
                "fauna.sync.changes.list" => fauna_core::encoding::canonical_encode(
                    &fauna_protocol::sync::SyncChangesListReply::default(),
                ),
                KIND_PAIR_LIST if self.1.lock().unwrap().is_some() => {
                    fauna_core::encoding::canonical_encode(&PairListReply {
                        pairings: self.1.lock().unwrap().clone().unwrap(),
                        forward_queue: Default::default(),
                        extra: Default::default(),
                    })
                }
                other => return Err(WireErr(RpcError::new("kind_not_served", other))),
            }
            .unwrap();
            Ok(fauna_core::encoding::canonical_decode(&reply).unwrap())
        }
    }

    fn pairing(id: u8, capabilities: &[&str], url: Option<&str>) -> PairingRow {
        PairingRow {
            private_nest_id: ByteBuf::from(vec![id; 32]),
            capabilities: capabilities.iter().map(|c| (*c).to_string()).collect(),
            expires_at: None,
            created_at: 1,
            label: None,
            nest_url: url.map(Into::into),
            extra: Default::default(),
        }
    }

    /// The leg runs only for the pairings carrying `account_replica` — the
    /// user's recorded choice — that name an address, never for the bound
    /// nest itself, each once.
    #[test]
    fn the_leg_completes_only_pairings_carrying_the_replica_capability() {
        let reply = PairListReply {
            pairings: vec![
                pairing(0x0B, &[capability::ACCOUNT_REPLICA], Some("https://b.test")),
                pairing(0x0C, &[capability::MAIL_PULL], Some("https://c.test")),
                pairing(0x0D, &[capability::ACCOUNT_REPLICA], None),
                pairing(0x0A, &[capability::ACCOUNT_REPLICA], Some("https://a.test")),
                pairing(0x0B, &[capability::ACCOUNT_REPLICA], Some("https://b.test")),
            ],
            forward_queue: Default::default(),
            extra: Default::default(),
        };
        assert_eq!(
            replica_targets(&reply, &[[0x0A; 32]]),
            vec![LinkedNestTarget {
                nest_id: [0x0B; 32],
                nest_url: "https://b.test".into(),
                replica: true,
            }]
        );
    }

    /// The chain reconcile visits every addressed pairing, whatever its
    /// capabilities (`identity-succession.md` § Enforcement on the home nest →
    /// *Every nest the identity is linked to*, clause (b)); the flag says
    /// which of them the leg also completes. Two rows naming one nest are one
    /// target, a replica when either carries the capability.
    #[test]
    fn a_run_visits_every_addressed_pairing_and_flags_the_replicas() {
        let reply = PairListReply {
            pairings: vec![
                pairing(0x0C, &[capability::MAIL_PULL], Some("https://c.test")),
                pairing(0x0B, &[capability::MAIL_PULL], Some("https://b.test")),
                pairing(0x0D, &[capability::ACCOUNT_REPLICA], None),
                pairing(0x0A, &[capability::ACCOUNT_REPLICA], Some("https://a.test")),
                pairing(0x0B, &[capability::ACCOUNT_REPLICA], Some("https://b.test")),
            ],
            forward_queue: Default::default(),
            extra: Default::default(),
        };
        let target = |id: u8, url: &str, replica| LinkedNestTarget {
            nest_id: [id; 32],
            nest_url: url.into(),
            replica,
        };
        assert_eq!(
            linked_targets(&reply, &[[0x0A; 32]]),
            vec![
                target(0x0C, "https://c.test", false),
                target(0x0B, "https://b.test", true),
            ]
        );
    }

    /// A connection presenting another identity than the pairing row's is
    /// refused before anything is sent over it — no walk, no put, no deposit.
    #[tokio::test]
    async fn a_connection_bound_to_another_identity_is_refused_before_anything_is_sent() {
        let f = fixture().await;
        let bound = Wire::default();
        let bound_fleet = AccountStatePlane::new(
            &f.store,
            &bound,
            &f.schedule,
            &f.writer_key,
            &f.trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .unwrap();
        let ctx = LinkedCtx {
            store: &f.store,
            bound_fleet: &bound_fleet,
            schedule: &f.schedule,
            trust: &f.trust,
            writer_key: &f.writer_key,
            custody: None,
        };
        let linked = Wire::default();
        let outcome = complete_linked_nest(
            &ctx,
            &LinkedNestTarget {
                nest_id: [0x0B; 32],
                nest_url: "https://b.test".into(),
                replica: true,
            },
            &LinkedConnection {
                rpc: linked.clone(),
                bound_identity: [0x0E; 32],
            },
            &[],
            &[],
        )
        .await;
        assert_eq!(
            outcome,
            LinkedOutcome::IdentityMismatch {
                presented: [0x0E; 32]
            }
        );
        assert!(linked.kinds().is_empty(), "sent: {:?}", linked.kinds());
        assert!(bound.kinds().is_empty(), "sent: {:?}", bound.kinds());
    }

    /// A retire issued at the bound nest is recorded with its exact
    /// coordinates and belt, and owed to a linked nest exactly when that
    /// nest's listing shows the writer's row for the item live at them or
    /// below them — never above.
    #[tokio::test]
    async fn a_retire_issued_at_the_bound_nest_is_issued_at_a_linked_nest_listing_the_row() {
        let f = fixture().await;
        let bound = Wire::default();
        let bound_fleet = AccountStatePlane::new(
            &f.store,
            &bound,
            &f.schedule,
            &f.writer_key,
            &f.trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .unwrap();
        let writer = WriterId(device_id_of(THEM));
        let g = [0x42; 32];
        bound_fleet
            .retire(&[0x01; 32], &writer, 7, Some(&g), true)
            .await
            .unwrap();
        bound_fleet
            .retire(&[0x02; 32], &writer, 9, None, false)
            .await
            .unwrap();
        // Recorded in the store, where any co-located process reads it.
        let read = f.store.issued_retires().await.unwrap();
        let through = read.last().map(|(ord, _)| *ord).unwrap();
        let issued: Vec<IssuedRetire> = read.into_iter().map(|(_, r)| r).collect();
        assert_eq!(issued.len(), 2);

        // The linked nest lists the first row at its coordinates, the second
        // at a later seq (a newer row there — not the one retired).
        let listing: Listing = [((writer, [0x01; 32]), 7), ((writer, [0x02; 32]), 10)]
            .into_iter()
            .collect();
        // A nest that lists the second cell BELOW the retired coordinate holds
        // a row the retired one superseded at the bound nest (ruling 6): owed
        // there, at the seq that nest lists, once however many entries name
        // the cell.
        let below: Listing = [((writer, [0x02; 32]), 4)].into_iter().collect();
        let twice: Vec<IssuedRetire> = issued
            .iter()
            .cloned()
            .chain([IssuedRetire {
                writer_seq: 4,
                settled: false,
                ..issued[1].clone()
            }])
            .collect();
        assert_eq!(
            mirrored_retires(ACCOUNT_STATE_FLEET_SCOPE, &below, &twice),
            vec![OwedRetire {
                item_key: [0x02; 32],
                writer,
                writer_seq: 4,
                no_rows_sealed_under: None,
                delete_escrow_wraps: false,
                forget_after: true,
            }]
        );
        let owed = mirrored_retires(ACCOUNT_STATE_FLEET_SCOPE, &listing, &issued);
        assert_eq!(
            owed,
            vec![OwedRetire {
                item_key: [0x01; 32],
                writer,
                writer_seq: 7,
                no_rows_sealed_under: Some(g),
                delete_escrow_wraps: true,
                forget_after: true,
            }]
        );
        assert!(
            mirrored_retires(ACCOUNT_STATE_SCOPE, &listing, &issued).is_empty(),
            "a retire follows its row on its own scope only"
        );

        // And the linked plane sends it.
        let linked = Wire::default();
        let linked_fleet = AccountStatePlane::new_linked(
            &f.store,
            &linked,
            &f.schedule,
            &f.writer_key,
            &f.trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .unwrap();
        for row in &owed {
            linked_fleet
                .retire(
                    &row.item_key,
                    &row.writer,
                    row.writer_seq,
                    row.no_rows_sealed_under.as_ref(),
                    row.delete_escrow_wraps,
                )
                .await
                .unwrap();
        }
        let sent = linked.retires();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].writer_id, writer.to_hex());
        assert_eq!(sent[0].writer_seq, 7);
        assert!(sent[0].delete_escrow_wraps);
        f.store.clear_issued_retires_through(through).await.unwrap();
        assert!(
            f.store.issued_retires().await.unwrap().is_empty(),
            "a linked plane's retires are not re-mirrored, and the leg run cleared what it read"
        );
    }

    /// `Shredded` is absorbing: every row a linked nest lists under a shredded
    /// generation is owed a retire whoever wrote it — here a sibling's (THEM),
    /// by this device (US) — and the generation's mint row there goes behind
    /// the dataless belt.
    #[tokio::test]
    async fn a_shredded_generations_rows_at_a_linked_nest_are_retired_by_a_member_that_did_not_write_them()
     {
        let f = fixture().await;
        let (g, gen_key, core) = f.mint_over(&[member_of(US), member_of(THEM)]).await;
        f.put(machinery_row(
            KIND_GENERATION_MINT,
            fauna_core::hex32::encode(&g),
            &GenerationMintRecord::Shredded {
                core,
                shredded_at_ms: 9_000,
                shredded_by: device_id_of(US),
            },
        ))
        .await;
        // THEM's device-endpoints row, sealed under `g`, as the linked walk
        // recorded it.
        let them = device_key(THEM);
        let keys =
            FleetOnlySchedule::derive_for_generation(&gen_key).for_kind(KIND_DEVICE_ENDPOINTS);
        let sealed = seal_entry_v2(
            &keys,
            &EntryCoordinates {
                writer_id: device_id_of(THEM),
                writer_seq: 3,
                scope: ACCOUNT_STATE_FLEET_SCOPE,
            },
            &g,
            &EntryPlaintext {
                kind: KIND_DEVICE_ENDPOINTS.into(),
                key: fauna_core::hex32::encode(&device_id_of(THEM)),
                merge_meta: None,
                value: vec![1, 2, 3].into(),
                tombstone: false,
            },
            &them,
        )
        .unwrap();
        f.store
            .record_relay_row(&RelayRow {
                scope: ACCOUNT_STATE_FLEET_SCOPE.into(),
                item_class: "state-entry".into(),
                writer: WriterId(device_id_of(THEM)),
                writer_seq: 3,
                item_key: sealed.item_key.to_vec(),
                op: "state-put".into(),
                entry: Some(sealed.envelope),
                feed_seq: None,
            })
            .await
            .unwrap();
        let linked = Wire::default();
        let plane: AccountStatePlane<'_, SqliteBackend, Wire> = AccountStatePlane::new_linked(
            &f.store,
            &linked,
            &f.schedule,
            &f.writer_key,
            &f.trust,
            ACCOUNT_STATE_FLEET_SCOPE,
        )
        .unwrap();
        let mint_item = plane
            .gen0_item_key(KIND_GENERATION_MINT, &fauna_core::hex32::encode(&g))
            .unwrap();
        let listing: Listing = [
            ((WriterId(device_id_of(THEM)), sealed.item_key), 3),
            ((WriterId(device_id_of(THEM)), mint_item), 1),
        ]
        .into_iter()
        .collect();
        let shredded = shredded_generations(&f.store).await.unwrap();
        assert_eq!(shredded, [g].into_iter().collect());
        let owed = shred_retires(&f.store, &plane, &listing, &shredded)
            .await
            .unwrap();
        assert_eq!(
            owed,
            vec![
                OwedRetire {
                    item_key: sealed.item_key,
                    writer: WriterId(device_id_of(THEM)),
                    writer_seq: 3,
                    no_rows_sealed_under: None,
                    delete_escrow_wraps: false,
                    forget_after: true,
                },
                OwedRetire {
                    item_key: mint_item,
                    writer: WriterId(device_id_of(THEM)),
                    writer_seq: 1,
                    no_rows_sealed_under: Some(g),
                    delete_escrow_wraps: false,
                    forget_after: false,
                },
            ],
            "the sealed row first, so the mint row's belt finds the generation dataless"
        );
        // A listing that holds the sibling's row at another seq owes nothing.
        let stale: Listing = [((WriterId(device_id_of(THEM)), sealed.item_key), 4)]
            .into_iter()
            .collect();
        assert!(
            shred_retires(&f.store, &plane, &stale, &shredded)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// One evidence row of THEM's, at seq 5, named by a one-byte item key,
    /// which the bound nest has served.
    fn evidence_row(item: u8) -> EvidenceRow {
        EvidenceRow {
            writer: WriterId(device_id_of(THEM)),
            item_key: [item; 32],
            writer_seq: 5,
            feed_seq: Some(9),
        }
    }

    /// A linked nest the run completed, standing on the run's evidence as
    /// `carried` says (`None`: its fleet scope was not read whole).
    fn completed(id: u8, carried: Option<Vec<Carried>>) -> LinkedNestPass {
        LinkedNestPass {
            nest_id: [id; 32],
            outcome: LinkedOutcome::Completed(Box::new(LinkedCompletion {
                evidence: carried,
                ..LinkedCompletion::default()
            })),
        }
    }

    fn conns_to(nests: &[(u8, &Wire)]) -> BTreeMap<[u8; 32], LinkedConnection<Wire>> {
        nests
            .iter()
            .map(|(id, wire)| {
                (
                    [*id; 32],
                    LinkedConnection {
                        rpc: (*wire).clone(),
                        bound_identity: [*id; 32],
                    },
                )
            })
            .collect()
    }

    /// The item keys a nest was asked to retire, in order.
    fn retired_items(wire: &Wire) -> Vec<u8> {
        wire.retires().iter().map(|r| r.item_key[0]).collect()
    }

    /// Ruling 7(c): an evidence row is carried only when every replica lists
    /// it or refused it for good; a carried row is retired at each replica
    /// that lists it and at the bound nest, and the bound nest's retire is not
    /// entered in the retire record. A run that missed a replica — unreachable,
    /// or its fleet scope not read whole — retires nothing anywhere, and a row
    /// the bound nest has not served is retired nowhere either.
    #[tokio::test]
    async fn evidence_is_retired_only_by_a_run_that_found_it_at_every_replica() {
        use Carried::{Listed, Missing, Refused};
        let f = fixture().await;
        // The bound nest is A; B and C are its replicas, each listing A and
        // the other.
        const REPLICA: &[&str] = &[capability::ACCOUNT_REPLICA];
        let replica = |id: u8| pairing(id, REPLICA, Some("https://linked.test"));
        let run_over = async |evidence: [EvidenceRow; 3], nests: Vec<LinkedNestPass>| {
            let (bound, b, c) = (Wire::default(), Wire::default(), Wire::default());
            b.lists(vec![replica(0x0A), replica(0x0C)]);
            c.lists(vec![replica(0x0A), replica(0x0B)]);
            let bound_fleet = AccountStatePlane::new(
                &f.store,
                &bound,
                &f.schedule,
                &f.writer_key,
                &f.trust,
                ACCOUNT_STATE_FLEET_SCOPE,
            )
            .unwrap();
            let ctx = LinkedCtx {
                store: &f.store,
                bound_fleet: &bound_fleet,
                schedule: &f.schedule,
                trust: &f.trust,
                writer_key: &f.writer_key,
                custody: None,
            };
            let pass = retire_carried_evidence(
                &ctx,
                &evidence,
                &nests,
                &conns_to(&[(0x0B, &b), (0x0C, &c)]),
                &[[0x0A; 32]],
            )
            .await;
            (
                pass,
                retired_items(&bound),
                retired_items(&b),
                retired_items(&c),
            )
        };
        let run = async |nests: Vec<LinkedNestPass>| {
            run_over([evidence_row(1), evidence_row(2), evidence_row(3)], nests).await
        };

        // Row 1: B lists it, C refused it — carried. Row 2: C lacks it — not
        // carried. Row 3: both refused it — carried, listed nowhere.
        let (pass, at_bound, at_b, at_c) = run(vec![
            completed(0x0B, Some(vec![Listed, Listed, Refused])),
            completed(0x0C, Some(vec![Refused, Missing, Refused])),
        ])
        .await;
        assert_eq!(
            (pass.retired, pass.deferred, pass.held),
            (2, 0, 0),
            "{pass:?}"
        );
        assert!(pass.errors.is_empty(), "{pass:?}");
        assert_eq!(at_bound, vec![1, 3], "the carried rows, at the bound nest");
        assert_eq!(at_b, vec![1], "and at the replica that lists one");
        assert!(at_c.is_empty(), "a replica that refused it holds nothing");
        assert!(
            f.store.issued_retires().await.unwrap().is_empty(),
            "the arm's retires never enter the retire record"
        );

        // A replica the run could not reach: nothing, anywhere.
        let unreached = LinkedNestPass {
            nest_id: [0x0C; 32],
            outcome: LinkedOutcome::Unreachable,
        };
        let (pass, at_bound, at_b, at_c) = run(vec![
            completed(0x0B, Some(vec![Listed, Listed, Listed])),
            unreached,
        ])
        .await;
        assert_eq!(pass, EvidencePass::default());
        assert!(at_bound.is_empty() && at_b.is_empty() && at_c.is_empty());

        // A replica whose fleet scope was not read whole is one not reached.
        let (pass, at_bound, at_b, _) = run(vec![
            completed(0x0B, Some(vec![Listed, Listed, Listed])),
            completed(0x0C, None),
        ])
        .await;
        assert_eq!(pass, EvidencePass::default());
        assert!(at_bound.is_empty() && at_b.is_empty());

        // No replica listed: every row is carried, and the bound nest alone
        // is asked.
        let (pass, at_bound, ..) = run(Vec::new()).await;
        assert_eq!(pass.retired, 3, "{pass:?}");
        assert_eq!(at_bound, vec![1, 2, 3]);

        // A row this replica knows only from a linked nest's feed — the bound
        // nest has not served it — has not been found at the bound nest, which
        // is a replica of every nest that lists it: retired nowhere, though
        // both replicas list it.
        let unserved = EvidenceRow {
            feed_seq: None,
            ..evidence_row(2)
        };
        let (pass, at_bound, at_b, at_c) = run_over(
            [evidence_row(1), unserved, evidence_row(3)],
            vec![
                completed(0x0B, Some(vec![Listed, Listed, Listed])),
                completed(0x0C, Some(vec![Listed, Listed, Listed])),
            ],
        )
        .await;
        assert_eq!(pass.retired, 2, "{pass:?}");
        assert_eq!((at_bound, at_b, at_c), (vec![1, 3], vec![1, 3], vec![1, 3]));
    }

    /// Ruling 7(c), the pair-list check: a linked nest that itself lists a
    /// replica this run did not reach keeps the row for a run that reaches
    /// both — and so does one whose pairings cannot be read. The bound nest,
    /// every replica of which was reached, retires it all the same.
    #[tokio::test]
    async fn a_linked_nest_listing_a_replica_the_run_did_not_reach_keeps_the_evidence() {
        let f = fixture().await;
        const REPLICA: &[&str] = &[capability::ACCOUNT_REPLICA];
        let replica = |id: u8| pairing(id, REPLICA, Some("https://linked.test"));
        let run = async |b: Wire| {
            let bound = Wire::default();
            let bound_fleet = AccountStatePlane::new(
                &f.store,
                &bound,
                &f.schedule,
                &f.writer_key,
                &f.trust,
                ACCOUNT_STATE_FLEET_SCOPE,
            )
            .unwrap();
            let ctx = LinkedCtx {
                store: &f.store,
                bound_fleet: &bound_fleet,
                schedule: &f.schedule,
                trust: &f.trust,
                writer_key: &f.writer_key,
                custody: None,
            };
            let pass = retire_carried_evidence(
                &ctx,
                &[evidence_row(1)],
                &[completed(0x0B, Some(vec![Carried::Listed]))],
                &conns_to(&[(0x0B, &b)]),
                &[[0x0A; 32]],
            )
            .await;
            (pass, retired_items(&bound), retired_items(&b))
        };

        // B lists the bound nest and D, which the bound nest does not list.
        let b = Wire::default();
        b.lists(vec![replica(0x0A), replica(0x0D)]);
        let (pass, at_bound, at_b) = run(b).await;
        assert_eq!((pass.retired, pass.held), (1, 1), "{pass:?}");
        assert!(at_b.is_empty(), "B keeps the row");
        assert_eq!(at_bound, vec![1]);

        // B's pairings cannot be read: held, and said so.
        let (pass, _, at_b) = run(Wire::default()).await;
        assert_eq!((pass.retired, pass.held), (1, 1), "{pass:?}");
        assert_eq!(pass.errors.len(), 1, "{pass:?}");
        assert!(at_b.is_empty());

        // B lists only the bound nest: it is asked.
        let b = Wire::default();
        b.lists(vec![replica(0x0A)]);
        let (pass, _, at_b) = run(b).await;
        assert_eq!((pass.retired, pass.held), (1, 0), "{pass:?}");
        assert_eq!(at_b, vec![1]);
    }
}
