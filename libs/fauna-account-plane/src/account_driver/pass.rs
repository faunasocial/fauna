//! One pump pass and its report: departures → the bind leg's replica check →
//! enrollment → publish → reconcile → escrow recovery + re-escrow → the
//! host's legs → device-endpoints → the rest of the generation machinery →
//! removals → severance → content walks → seen-set
//! (`account-client-lifecycle.md` § The client-side lifecycle, the pump bullet).
//!
//! Beside it, the **seed pass** ([`seed_pass`]): the steps only a signed-in
//! app can run — escrow recovery, then the secondary leg with its custody arm
//! — alone, in pass order, for the runtime that holds the seed-leg role while
//! another process holds the engine role (`account-runtime.md` § Multi-instance
//! concurrency → *The seed-leg role*). A runtime that holds both roles runs
//! those steps inside [`pump`]; one that holds the engine role alone skips
//! them.

use std::time::Duration;

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use fauna_account_store::backend::StoreBackend;
use fauna_account_store::store::AccountStore;
use fauna_client_config::{
    CustodySeedFold, DeploymentSeedCustody, LinkedCustodyLeg, SupersessionMarks,
    run_linked_deployment_seed_custody_leg,
};
use fauna_core::crypto::AccountStateKeySchedule;
use fauna_core::data::DeploymentSeedEntry;
use fauna_core::identity::ActorId;
use fauna_protocol::account_state::{ACCOUNT_STATE_FLEET_SCOPE, ACCOUNT_STATE_SCOPE};
use fauna_protocol::scope::ContentScope;
use fauna_protocol::{KeyedRpcRequester, RpcErrorClass, RpcRequester};

use crate::account_state_plane::{AccountStatePlane, WalkReport};
use crate::content_scope_plane::{ContentScopePlane, ContentWalkReport};
use crate::departure::{DepartureReport, drop_departed_scopes};
use crate::device_endpoints_writer::{EndpointFacts, EndpointsPass};
use crate::generation_topup::TopupPass;
use crate::generation_unkeyable::UnkeyablePass;
use crate::host_legs::{CustodyDialPass, CustodyNestPass, CustodyServePass, DialPass, PeerLegPass};
use crate::principal_custody::PrincipalCustody;
use crate::seen_set_producer::{SeenSetPassReport, auto_in_set_pass};

use super::enrollment::{EnrollmentPass, ensure_enrollment_registered};
use super::{HostLegs, LegsCtx, now_ms};

/// Wall-clock elapsed since `start_ms` (`now_ms`), saturating.
fn since(start_ms: u64) -> Duration {
    Duration::from_millis(now_ms().saturating_sub(start_ms))
}

/// What one pump pass did. Every step is always attempted; a step that failed
/// leaves its slot `None` and pushes why onto [`Self::errors`] (offline is
/// the expected cause — the next tick retries).
#[derive(Debug, Default)]
pub struct PumpReport {
    /// The scope-departure pass (`departure`) — T2 transition 3. First step of
    /// the pass, so a scope dropped here is already absent from everything
    /// below it.
    pub departures: Option<DepartureReport>,
    /// The bind leg's replica check (`crate::bind_leg`), ahead of every leg
    /// that talks to the nest: `None` when the bound replica could not be
    /// asked (offline — the next pass asks again).
    pub bind: Option<crate::bind_leg::BindPass>,
    /// The enrollment ceremony's nest legs: register the machine's
    /// `sync_devices` row + its `RenewBearer` grant, retried per pass until
    /// they land, then latched content-addressed in the slot.
    pub enrollment: Option<EnrollmentPass>,
    /// True when [`Self::enrollment`] is `RemovedFromAccount` **on this
    /// machine's own `Removed` device-set row** rather than on the nest's
    /// revoked answer — principal succession's third trigger
    /// (`account-replica-posture.md` § The store device principal →
    /// *Principal succession after a device delete*, decision 1). The two
    /// differ in what the next assembly may do: the nest's answer is asked
    /// for again by the ceremony probe, while this finding is the evidence
    /// itself and a seed-holding worker carries it across the reassembly
    /// ([`AccountDriver::own_row_removed`]).
    ///
    /// [`AccountDriver::own_row_removed`]: super::AccountDriver::own_row_removed
    pub own_row_removed: bool,
    /// Un-published local rows replayed to the nest.
    pub published: Option<usize>,
    /// The same, for the fleet-only plane (`state-fleet` — the A5 partition's
    /// other half: the generation machinery kinds and every fleet-only data kind).
    pub fleet_published: Option<usize>,
    /// How many rows the delegable plane holds parked after the pass — own
    /// rows the nest refused for room, which the publish retries every pass
    /// (`account-replica-posture.md` § The store device principal,
    /// refinement 11 → *A row refused for room is parked*). `None` when the
    /// count could not be read.
    pub parked: Option<usize>,
    /// The succession tail re-author (`principal_succession::
    /// tail_reauthor_pass`) — `None` on every pass without a pending
    /// rotation marker; a report exactly once per completed rotation, on the
    /// pass that re-journaled the predecessor's un-pushed tail under the
    /// successor (before the publish steps, so the same pass ships them).
    pub reauthor: Option<crate::succession_tail::ReauthorPass>,
    /// The outbox drain (`outbox::drain_outbox`) — the other
    /// "push our writes out" leg, right after the publish.
    pub outbox: Option<crate::outbox::DrainReport>,
    /// The account-state scope walk.
    pub walk: Option<WalkReport>,
    /// The fleet-only scope walk — how this replica learns the device set,
    /// mints, wraps and escrow receipts its siblings wrote.
    pub fleet_walk: Option<WalkReport>,
    /// The `ext:<kind>` walks, one per kind the account's verified manifests
    /// admitted (`third-party-kinds.md` § The `ext` sub-scope), in the
    /// overlay's order: the scope beside its walk.
    pub ext_walks: Vec<(String, WalkReport)>,
    /// The publish diff behind the account-state reconcile
    /// (`crate::publish_diff`; `account-sync-plane.md` § The bind leg, ruling
    /// 1): the relay rows the bound nest's listing lacked, pushed verbatim.
    pub publish_diff: Option<crate::publish_diff::DiffPush>,
    /// The same, behind the fleet-only reconcile.
    pub fleet_publish_diff: Option<crate::publish_diff::DiffPush>,
    /// The delegable scope's retire behind the succession carry
    /// (`crate::delegable_reclaim`; `succession-aftermath.md` § Re-key scope):
    /// the predecessor rows retired once a listed member row carries their
    /// item. `None` on a plane handed no predecessor schedule, or with no
    /// completed reconcile.
    pub carry_retire: Option<crate::delegable_reclaim::CarryRetire>,
    /// The delegable scope's cover step (`crate::delegable_reclaim::
    /// reclaim_below_cover`; `delegable-scope-reclamation.md` § Delegable-scope
    /// reclamation, parts (3) and (4)): the items handed over, and the rows
    /// below a cover retired, withheld or deferred. `None` with no completed
    /// reconcile of the scope.
    pub cover_reclaim: Option<crate::delegable_reclaim::CoverReclaim>,
    /// The peer-leg ensure step (`peer_leg`'s assembly seam), right
    /// before the device-endpoints step so a first bind's facts publish on
    /// the same pass.
    pub peer_leg: Option<PeerLegPass>,
    /// The peer-leg dial pass (the pull half): `None` while the leg
    /// is not bound (unbound = un-elected, unenrolled, braked, or no
    /// factory), a per-pass sibling summary while it is.
    pub peer_dial: Option<DialPass>,
    /// The custody serve refresh (`custody_leg::CustodyLegState::
    /// serve_refresh`): the served-custodies registry, re-derived per pass.
    /// `None` when the step errored.
    pub custody_serve: Option<CustodyServePass>,
    /// The custodian NEST pass (its pump half): this machine pulling the
    /// accounts it holds custody FOR from their OWNERS' NESTS — the leg that
    /// needs no owner device awake. `None` when the step errored.
    pub custody_nest: Option<CustodyNestPass>,
    /// The custodian dial pass: this machine pulling the accounts it
    /// holds custody FOR from their owner fleets — and, since the nest leg
    /// landed, the pass that applies T15's budget to every held custody
    /// whether or not a peer transport is bound. `None` when the step errored.
    pub custody_dial: Option<CustodyDialPass>,
    /// A7 receipts the check-in mint recorded this pass (
    /// `custody_leg::mint_due_receipts`); 0 on most passes by design (the
    /// cadence), and always 0 while the custody leg is unbound.
    pub custody_receipts_minted: usize,
    /// The device-endpoints ensure step (`device_endpoints_writer` — the T5
    /// discovery feed's production writer), right after the fleet walk so a
    /// sibling's mint merged this pass is already visible to it.
    pub device_endpoints: Option<EndpointsPass>,
    /// The generation top-up self-heal pass (`generation_topup`) — the wraps
    /// this device published so siblings a mint left out can key it. Runs
    /// after the fleet walk for the same reason the step above does: a
    /// sibling's enrollment merged this pass is a target this pass can serve.
    pub generation_topup: Option<TopupPass>,
    /// The escrow-recovery pass (`generation_escrow_recover`) — generation
    /// keys this seed-holding device recovered from the holder's escrow wraps
    /// because nothing on the plane keys them here. `None` on a runtime that
    /// does not hold the seed-leg role (the seedless host; a seed holder
    /// beside the one that took it). Runs BEFORE the top-up pass, so a key
    /// recovered this pass heals siblings this pass.
    pub generation_escrow_recovery: Option<crate::generation_escrow_recover::EscrowRecoveryPass>,
    /// The fleet scope's re-presentation behind an escrow recovery that keyed
    /// a generation: the rows `fleet_walk` left unopened for want of that key,
    /// opened and merged **this** pass. `None` on every pass that recovered
    /// nothing — which is every pass but a fresh sign-in's first.
    pub fleet_rewalk: Option<WalkReport>,
    /// The re-escrow pass (`generation_reescrow`) — the succession rider:
    /// generations this device keys that no trusted holder had receipted for
    /// THIS identity, deposited under this identity's target. It never mints.
    /// Runs after escrow recovery (a key recovered this pass is re-escrowed
    /// this pass) and before the top-up pass.
    pub generation_reescrow: Option<crate::generation_reescrow::ReescrowPass>,
    /// The target-authored "cannot key" signal pass (`generation_unkeyable`,
    /// row 41) — assertions this device published where apparent coverage
    /// opens nothing, and retractions where it keys again. After the top-up
    /// pass: a wrap merged or published this pass is tried before testifying.
    pub generation_unkeyable: Option<UnkeyablePass>,
    /// The fleet-scope reclamation pass (`generation_reclaim` — the reach
    /// writer and the retire caller). After the two passes above: a wrap
    /// published or merged this pass is in this device's reach, and a
    /// `Satisfied` published this pass is retired this pass.
    pub generation_reclaim: Option<crate::generation_reclaim::ReclaimPass>,
    /// The secondary leg (`crate::linked_leg`; `account-sync-plane.md` § The
    /// bind leg, ruling 4), right behind reclamation so its retires follow
    /// this pass's rows to every linked nest. `None` on a runtime that does
    /// not hold the seed-leg role, and on a host that wired no linked-nest
    /// connector (the seedless host, tests) — no leg ran.
    pub linked: Option<crate::linked_leg::LinkedPass>,
    /// The deployment-seed custody leg over each linked nest's connection
    /// (`nest/box-recovery.md` § The plane-era recovery floor, *(c)*, the
    /// linked-connection paragraph) — one entry per linked nest the secondary
    /// leg reached this pass, ahead of that nest's completion so a row
    /// captured here reaches it on the same pass. Empty when no leg ran.
    pub linked_custody: Vec<LinkedCustodyPass>,
    /// The road (`crate::owed_delivery`): the succession statements this
    /// account is owed at, delivered — one entry for the bound nest's owed
    /// list, just ahead of the secondary leg, then one for each linked nest's
    /// that the leg reached, in the order reached. Empty when the runtime holds no
    /// deliverer (the seedless host, a holder beside the role's, tests).
    pub owed_nests: Vec<crate::owed_delivery::OwedNestsPass>,
    /// The staged device removals' reconcile
    /// (`fleet_removal::complete_pending`) — `None` when nothing is staged,
    /// which is every pass but the rare one after an interrupted removal.
    pub fleet_removals: Option<crate::fleet_removal::FleetRemovalPass>,
    /// The removed members' grants revoked at the bound nest
    /// (`crate::removed_grants` — `account-data-taxonomy.md` § Fleet-scope
    /// reclamation, clause (4) → *The nest half follows merged state*), in
    /// every full pass, whichever process pumps. All-default — nothing asked of
    /// the nest — until merged state reads another device removed; `None` when
    /// no full pass ran.
    pub removed_grants: Option<crate::removed_grants::RemovedGrantsPass>,
    /// The group plane's authority-device severance
    /// (`group_authority_revocation::ensure_revoked`) — the revocation
    /// publisher, its re-admissions and the severance mint. All-default on an
    /// account that holds no group scope, and `None` on a machine that has run
    /// no enrollment ceremony (no carriage, so nothing it authored would
    /// verify).
    pub group_authority_revocation: Option<crate::group_authority_revocation::RevocationPass>,
    /// Registered content-scope walks, in registration order.
    pub content_walks: Vec<ContentWalkReport>,
    /// The auto-in-set seen-set pass (`seen_set_producer`).
    pub seen_set: Option<SeenSetPassReport>,
    /// Step failures, in step order. Empty means a fully clean pass.
    pub errors: Vec<String>,
    /// True when no pass ran because this runtime is not the engine-singleton
    /// holder (charter § Multi-instance concurrency): another
    /// co-located process pumps this store; this one serves reads and plain
    /// writes. A caller that needs a pass (rather than a role answer) is
    /// holding the wrong process's handle — convergence arrives through the
    /// shared store.
    ///
    /// Every other field is then at its default — **except on the seed-leg
    /// role's holder**, whose report is its seed pass's ([`seed_pass`]): the
    /// escrow-recovery, secondary-leg and custody slots, the publish counts of
    /// its own publish step, their timings and errors.
    pub skipped_non_holder: bool,
    /// True when a step's failure chain carried the store's typed
    /// [`fauna_account_store::store::StaleWriter`] refusal: the writer was
    /// rotated away under this process (principal succession, charter § The
    /// store device principal → succession decision 4). The worker reacts by
    /// reassembling — it re-resolves the successor from the shared slot and
    /// continues — so a caller seeing this flag on a returned report should
    /// simply retry its call. Detection rides the typed step funnels; a
    /// refusal inside a string-only sub-report (seen-set producer) surfaces
    /// on the next pass's typed step instead — same heal, one pass later.
    pub stale_writer: bool,
    /// True when a sign-out cut this pass short ([`SIGN_OUT_PASS_GRACE`]):
    /// every step it had not finished is at its default, and nothing about
    /// that failed — the account is being signed out, and its store erased.
    pub cut_by_sign_out: bool,
    /// How long the pass's walks and generation-machinery steps took
    /// ([`PassTimings`]) — what lets a sweep's app log name the dominant step
    /// of a long prologue without a hand count.
    pub timings: PassTimings,
}

/// The custody leg's outcome at one linked nest
/// ([`PumpReport::linked_custody`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedCustodyPass {
    /// The pairing row's nest id.
    pub nest_id: [u8; 32],
    pub leg: LinkedCustodyLeg,
}

/// Wall-clock time per pass step, for the steps whose cost scales with an
/// account's history rather than its live size (`account-data-plane.md`
/// § The client-side lifecycle → *Bounding the pass itself is not this
/// rule's job*): the two scope walks, the device-endpoints step and the
/// generation machinery's four steps, beside the whole pass. Zero
/// for a step that did not run (a non-holder, a seedless host's escrow
/// recovery, a pass cut by a sign-out before it). `log_pump` names the
/// slowest of them on every pass that took a second or longer.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PassTimings {
    /// The whole pass, first step to last.
    pub total: Duration,
    /// The account-state scope walk (`walk`).
    pub walk: Duration,
    /// The fleet-only scope walk (`fleet_walk`).
    pub fleet_walk: Duration,
    /// The device-endpoints ensure step.
    pub device_endpoints: Duration,
    /// The escrow-recovery step.
    pub escrow_recovery: Duration,
    /// The re-escrow step (the succession rider).
    pub reescrow: Duration,
    /// The top-up step.
    pub topup: Duration,
    /// The unkeyable-signal step.
    pub unkeyable: Duration,
    /// The reclamation step.
    pub reclaim: Duration,
    /// The secondary leg, every linked nest together.
    pub linked: Duration,
}

impl PassTimings {
    /// The step that took longest, by name, with its duration — `None` when
    /// no step ran.
    #[must_use]
    pub fn slowest(&self) -> Option<(&'static str, Duration)> {
        [
            ("walk", self.walk),
            ("fleet_walk", self.fleet_walk),
            ("device_endpoints", self.device_endpoints),
            ("escrow_recovery", self.escrow_recovery),
            ("reescrow", self.reescrow),
            ("topup", self.topup),
            ("unkeyable", self.unkeyable),
            ("reclaim", self.reclaim),
            ("linked", self.linked),
        ]
        .into_iter()
        .filter(|(_, d)| !d.is_zero())
        .max_by_key(|(_, d)| *d)
    }
}

/// Push a step failure onto the report — the one funnel every step and every
/// host leg uses, because it also flags the typed stale-writer refusal
/// wherever it appears in the chain (the one error the loop must not treat as
/// retry-next-tick — see [`PumpReport::stale_writer`]).
pub fn push_step_error(report: &mut PumpReport, msg: String, e: &anyhow::Error) {
    if fauna_account_store::store::is_stale_writer(e) {
        report.stale_writer = true;
    }
    report.errors.push(msg);
}

/// Whether a pump report demands a reassembly rather than an ordinary
/// next-tick retry — and why, for the log line. Two causes:
/// - a typed stale-writer refusal (a sibling rotated this machine; every
///   process heals by re-resolving the successor from the slot) —
///   unconditional;
/// - a `RemovedFromAccount` enrollment answer on a SEED-HOLDING runtime —
///   reassembly *is* a ceremony here, so the ruled sign-in remedy runs
///   without waiting for the user to quit and relaunch (
///   the seed holder's presence is the authority; a seedless host stays loud
///   instead). The answer is the nest's revoked reply or this machine's own
///   `Removed` device-set row ([`PumpReport::own_row_removed`] — decision 1's
///   third trigger, which the caller carries to the assembly); a
///   `SignedOut` answer is neither and never reassembles. Gated by
///   `allow_removed_heal` — the caller's
///   one-rotation-per-chain cap — or a nest that keeps revoking fresh keys
///   would draw an unbounded reassemble→mint loop out of one session; and
///   the caller's pacing beside it (`removed_heal_taken`), or a reassembly
///   whose probe rotated nothing would repeat on every prologue.
pub(crate) fn reassembly_reason(
    report: &PumpReport,
    allow_removed_heal: bool,
    allow_burnt_heal: bool,
) -> Option<&'static str> {
    if report.stale_writer {
        return Some("the writer was rotated under this process — adopting the successor");
    }
    if allow_removed_heal && report.enrollment == Some(EnrollmentPass::RemovedFromAccount) {
        return Some(
            "this device was deleted from the account — re-running the ceremony \
             to mint a successor principal",
        );
    }
    if allow_burnt_heal && walk_found_burnt(report) {
        return Some(
            "the walk found this store's writer burnt (a feed holds rows under it this \
             journal never authored) — reassembling to rotate onto a fresh writer",
        );
    }
    None
}

/// Whether either class-2 walk this pass refused an own-current row as the
/// burnt-journal signature ([`WalkReport::own_burnt`]) — the third
/// reassembly cause. Capped at one reassembly per runtime worker by the
/// caller (`burnt_heal_taken`): the heal arm mints and fences under the
/// migration section, and if it could not (a degraded lock, the re-mint
/// cap), reassembling on every pass would be a livelock at the pump cadence.
pub(crate) fn walk_found_burnt(report: &PumpReport) -> bool {
    report.walk.as_ref().is_some_and(|w| w.own_burnt > 0)
        || report.fleet_walk.as_ref().is_some_and(|w| w.own_burnt > 0)
}

/// Whether a pump pass proved the enrollment HEALTHY — what licenses the
/// one-rotation-per-chain cap to re-arm (a later genuine delete may rotate
/// once again), and the removed-heal's pacing with it. Deliberately not "a serve phase was reached": under a nest
/// that keeps answering revoked, every reassembly reaches a serve phase, and
/// a reach-based reset re-arms the loop the cap exists to stop.
pub(crate) fn enrollment_healthy(report: &PumpReport) -> bool {
    matches!(
        report.enrollment,
        Some(EnrollmentPass::Registered | EnrollmentPass::Current)
    )
}

/// Whether a pass's enrollment verdict puts this machine's grant on the nest
/// — what opens the store principal's connect gate
/// ([`AccountStoreHandle::subscribe_grant_registered`]). Coincides with
/// [`enrollment_healthy`] today, and is spelled out on its own because the two
/// answer different questions: "may the rotation cap re-arm?" versus "will a
/// `fauna.auth.device_handshake` be answered?".
///
/// [`AccountStoreHandle::subscribe_grant_registered`]: super::AccountStoreHandle::subscribe_grant_registered
pub(crate) fn grant_on_nest(report: &PumpReport) -> bool {
    match report.enrollment {
        // The latch matched: an earlier pass (or process) registered it.
        Some(EnrollmentPass::Current) => true,
        // Both nest legs landed this pass.
        Some(EnrollmentPass::Registered) => true,
        // No grant in the slot, so nothing a handshake could present.
        Some(EnrollmentPass::Unenrolled) => false,
        // This principal is ended: the nest refuses its key for good, or its
        // own device-set row reads `Removed` — and there the grant may well
        // still stand (a guardian-marked row, a removal by key, a rebuilt
        // box). The pass learned of no grant that should be dialed with; a
        // gate the latch already opened at assembly stays open, so a removed
        // machine that was reading goes on reading.
        Some(EnrollmentPass::RemovedFromAccount) => false,
        // The sign-out's retirement cleared, or is about to clear, the grant.
        Some(EnrollmentPass::SignedOut) => false,
        // The register was refused and wrote nothing.
        Some(EnrollmentPass::DeviceLimitExceeded) => false,
        // The step itself failed (offline, a leg error): nothing learned.
        None => false,
    }
}

/// Fold custodians' carried candidates (T13 step 4) into this account's
/// `fauna.state.custodian-endpoints` rows.
///
/// A row is rewritten only when the observation's key is the node that row
/// **already names** — the grant's accept-bound device principal. So a
/// re-exchange refreshes *where* a known custodian is reachable and can never
/// change *who* the custodian is; that stays a ceremony act. Rows nobody
/// dialed in for are untouched (a silent pass must never erase an address).
async fn refresh_custodian_endpoints<B, R>(
    store: &AccountStore<B>,
    fleet: &crate::account_state_plane::AccountStatePlane<'_, B, R>,
    writer_key: &SigningKey,
    observed: &std::collections::HashMap<[u8; 32], fauna_core::device_endpoints::DeviceEndpoints>,
) -> anyhow::Result<()>
where
    B: fauna_account_store::backend::StoreBackend,
    R: fauna_protocol::RpcRequester + Clone,
{
    let device_id = writer_key.verifying_key().to_bytes();
    let rows = store
        .states_of_kind(fauna_protocol::merge_policy::KIND_CUSTODIAN_ENDPOINTS)
        .await?
        .into_iter()
        .filter_map(
            |entry| match fauna_core::encoding::canonical_decode(&entry.value) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!("custodian-endpoints row unreadable ({e}) — skipped");
                    None
                }
            },
        );
    for row in crate::custody_rows::custodian_rows_to_refresh(rows, observed) {
        crate::custody_rows::put_custodian_endpoints(fleet, device_id, &row).await?;
    }
    Ok(())
}

/// The custodian-side twin: fold observed candidates into this machine's
/// `fauna.state.custodies-held` rows' `owner_devices`.
///
/// The dial pass already folds in what an owner device answered when WE
/// dialed it. This covers the sessions that pass the other way — and with
/// them the case a custodian cannot get out of on its own: every owner
/// address it holds is stale, so its own dials all fail, and only an inbound
/// session can teach it where the fleet moved.
async fn refresh_held_owner_devices<B, R>(
    store: &AccountStore<B>,
    fleet: &crate::account_state_plane::AccountStatePlane<'_, B, R>,
    writer_key: &SigningKey,
    observed: &std::collections::HashMap<[u8; 32], fauna_core::device_endpoints::DeviceEndpoints>,
) -> anyhow::Result<()>
where
    B: fauna_account_store::backend::StoreBackend,
    R: fauna_protocol::RpcRequester + Clone,
{
    let device_id = writer_key.verifying_key().to_bytes();
    let rows = store
        .states_of_kind(fauna_protocol::merge_policy::KIND_CUSTODIES_HELD)
        .await?
        .into_iter()
        .filter_map(
            |entry| match fauna_core::encoding::canonical_decode(&entry.value) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!("custodies-held row unreadable ({e}) — skipped");
                    None
                }
            },
        );
    for row in crate::custody_rows::held_rows_to_refresh(rows, observed) {
        crate::custody_rows::put_custodies_held(fleet, device_id, &row).await?;
    }
    Ok(())
}

/// The account's **two** class-2 planes — the A5 partition as the pump sees it.
///
/// A struct rather than two same-typed parameters for the reason
/// [`PassInputs`] gives about its own third field: adjacent arguments of one
/// type can be passed swapped, and swapping these two would seal fleet-only
/// machinery into the very scope a delegable grantee subscribes to — the one
/// thing the partition exists to prevent.
pub(crate) struct Planes<'a, B: StoreBackend, R: RpcRequester> {
    /// `state` — every delegable kind: the preference surfaces, the seen set.
    pub(crate) delegable: &'a AccountStatePlane<'a, B, R>,
    /// `state-fleet` — the generation machinery kinds and every fleet-only data kind.
    pub(crate) fleet: &'a AccountStatePlane<'a, B, R>,
}

// Hand-written rather than derived: `derive(Clone, Copy)` would demand
// `B: Clone, R: Clone`, which no backend or requester owes — this struct is two
// shared references and is `Copy` regardless of what they point at.
impl<B: StoreBackend, R: RpcRequester> Clone for Planes<'_, B, R> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<B: StoreBackend, R: RpcRequester> Copy for Planes<'_, B, R> {}

/// What one pass is run against: the sets it walks, and whether the membership
/// answer behind them was affirmative.
///
/// A struct rather than three parameters because the third only means anything
/// beside the first — `membership_answered` is a claim *about* `content_scopes`
/// ("this set reflects a source that just spoke"), and a call site that could
/// pass them separately could pass them mismatched, which is the one mistake
/// here that deletes data.
#[derive(Clone, Copy)]
pub(crate) struct PassInputs<'a> {
    pub(crate) content_scopes: &'a [ContentScope],
    pub(crate) own_scopes: &'a [ContentScope],
    pub(crate) membership_answered: bool,
    /// The app-fed transport facts for the device-endpoints step — `None`
    /// until `set_endpoint_facts`, which publishes the node-id-only floor.
    pub(crate) endpoint_facts: Option<&'a EndpointFacts>,
}

/// The fleet-plane writer identity + trust + principal bundle, bundled for
/// the pump steps that act as *this device* rather than on a plane: the
/// device-endpoints step (resolves tips directly off the store, so it needs
/// trust + key by name) and the enrollment-registration step (the
/// slot carries the grant to register; the key names the machine's
/// `sync_devices` row).
pub(crate) struct FleetWriter<'a, R> {
    pub(crate) trust: &'a crate::generation_tip::GenerationTrust,
    pub(crate) key: &'a SigningKey,
    pub(crate) slot: &'a dyn PrincipalCustody,
    /// The app-session requester the enrollment ceremony's registration legs
    /// ride — the ONE consumer; every data leg rides the pump's own
    /// requester, which is the principal's session when the caller wired one.
    pub(crate) session_rpc: &'a R,
    /// The key schedule the planes seal under — the dial pass
    /// constructs fresh pull-only planes per admitted sibling and needs it by
    /// name, exactly as it needs `key` and `trust`.
    pub(crate) schedule: &'a AccountStateKeySchedule,
    /// The `sync_devices` row the enrollment registers on — the machine's
    /// **named** row, the app's own derived id
    /// ([`AccountRuntimeParams::enrollment_target_device_id`]).
    pub(crate) enrollment_target: &'a str,
    /// The identity seed, the attested predecessors' keypairs (the kept
    /// wrap's recovery — `super::SeedHolder`) and this worker's answered-set,
    /// for the escrow-recovery step — filled only for the **seed-leg role's holder**
    /// (`account-runtime.md` § Multi-instance concurrency → *The seed-leg
    /// role*). `None` on the seedless host, where recovery is one more
    /// seed-only leg: skipped, and healed through the shared slot's retained
    /// bundle by the seed pass of a signed-in app on the same machine; `None`
    /// too on a seed holder beside the one that took the role.
    pub(crate) escrow_recovery: Option<(
        &'a [u8; 32],
        &'a [fauna_core::identity::ActorKeypair],
        &'a crate::generation_escrow_recover::EscrowRecoveryMemo,
    )>,
    /// The store principal's connect gate, opened by the enrollment step the
    /// moment its verdict puts the grant on the nest ([`grant_on_nest`]) —
    /// mid-pass, so the rest of that very pass can already ride the
    /// principal's connection.
    pub(crate) grant_registered: &'a tokio::sync::watch::Sender<bool>,
    /// Where [`Self::trust`]'s holder set is re-read from at the start of
    /// every pass — the pin, never the set frozen at assembly.
    pub(crate) holder_source: &'a super::TrustedHolderSource,
    /// What this assembly has done of the bind leg (`crate::bind_leg`).
    pub(crate) bind: &'a crate::bind_leg::BindMemo,
    /// The host's linked-nest connector for the secondary leg
    /// (`crate::linked_leg`) — filled only for the seed-leg role's holder,
    /// like [`Self::escrow_recovery`]. `None` on the seedless host, whatever
    /// it was handed: it runs no secondary leg (`account-sync-plane.md` § The
    /// bind leg, ruling 4, *the stated bounds*; ruling 5 — the leg runs in
    /// the seed holder that holds the role).
    pub(crate) linked_nests: Option<&'a super::LinkedNestConnector<R>>,
    /// The host's succession deliverer for the road ([`deliver_owed_at`]) —
    /// filled for the seed-leg role's holder alone, like
    /// [`Self::linked_nests`]: the seedless host signs in as no retired
    /// identity, and one delivery per machine per pass is enough.
    pub(crate) owed_nests: Option<&'a super::OwedNestDeliverer<R>>,
}

// Manual, not derived: the derives would demand `R: Copy`/`R: Clone`, but the
// struct holds only `&R`, which is `Copy` for any `R`.
impl<R> Clone for FleetWriter<'_, R> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<R> Copy for FleetWriter<'_, R> {}

/// This device's row statement as the pump publishes it
/// (`device_endpoints_writer::ensure_published`'s `enrolled_row`): the
/// registration latch's row half — or, on an e2e build only, whatever
/// `FAUNA_E2E_STATED_ROW` names. The override exists for ONE journey: a
/// sibling seat playing the stolen device that mis-states its own row
/// (`account-data-taxonomy.md` § The generation machinery → *Fleet-scope
/// reclamation*, clause (4), *A disagreement is the user's to settle* —
/// `test_device_member_removal.py`), a statement no honest code path produces
/// and the harness cannot forge from outside (the entry is sealed under the
/// generation key only a member holds). Compile-gated outer, env inner
/// (`always_resident::debounce_delay`'s shape; convention 15): a release
/// build never reads the var, and an unset or empty value is the honest
/// statement, so a missing override is production-safe.
#[cfg(any(debug_assertions, feature = "e2e-agent"))]
fn stated_row(latched: Option<String>) -> Option<String> {
    match std::env::var("FAUNA_E2E_STATED_ROW") {
        Ok(row) if !row.is_empty() => Some(row),
        _ => latched,
    }
}

#[cfg(not(any(debug_assertions, feature = "e2e-agent")))]
fn stated_row(latched: Option<String>) -> Option<String> {
    latched
}

/// One nudge-triggered walk: the account-state scope and the fleet scope walk
/// their class-2 plane; a registered content scope walks its class-1 feed;
/// anything else is a scope this replica does not track.
pub(crate) async fn walk_one_scope<B, R>(
    store: &AccountStore<B>,
    planes: Planes<'_, B, R>,
    rpc: &R,
    content_scopes: &[ContentScope],
    scope: &str,
) -> Result<()>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    if scope == ACCOUNT_STATE_SCOPE {
        planes
            .delegable
            .walk()
            .await
            .context("nudged account-state walk")?;
        // This unit keys nothing after its walk, so the walk's end is the
        // unit's: a listing held back only by unopened rows is recorded now.
        planes.delegable.pass_ended().await?;
        return Ok(());
    }
    if scope == ACCOUNT_STATE_FLEET_SCOPE {
        planes
            .fleet
            .walk()
            .await
            .context("nudged fleet-state walk")?;
        planes.fleet.pass_ended().await?;
        return Ok(());
    }
    if let Some(kind) = fauna_protocol::scope::ext_scope_kind(scope) {
        // An `ext:<kind>` nudge walks that kind's plane only while a
        // verified manifest admits the kind here; otherwise the scope is
        // one this replica does not track yet.
        let overlay = crate::kind_manifest_rows::read_admitted_kinds(store).await?;
        if overlay.kinds.kinds().any(|k| *k == kind) {
            let plane = planes.fleet.ext_plane(&kind, &overlay.kinds);
            plane
                .walk()
                .await
                .with_context(|| format!("nudged {scope} walk"))?;
            plane.pass_ended().await?;
            return Ok(());
        }
    }
    if let Some(cs) = content_scopes.iter().find(|cs| cs.to_string() == scope) {
        ContentScopePlane::new(store, rpc, cs.clone())
            .walk()
            .await
            .with_context(|| format!("nudged content walk ({scope})"))?;
        return Ok(());
    }
    tracing::debug!("account pump: nudge for untracked scope {scope} — ignored");
    Ok(())
}

/// The local-write wake's work (the pump bullet's wake source (4)): the
/// ordered own publish of every unsent row — the delegable plane's, then the
/// fleet plane's, each plane its own ordered log, as a full pass runs them:
/// the network leg a local write used to run inline. Reported like a pass
/// so the loop's stale-writer heal reads it the same way.
pub(crate) async fn publish_step<B, R>(planes: Planes<'_, B, R>) -> PumpReport
where
    B: StoreBackend,
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let mut report = PumpReport::default();
    match planes.delegable.publish_pending().await {
        Ok(n) => report.published = Some(n),
        Err(e) => push_step_error(&mut report, format!("publish_pending: {e:#}"), &e),
    }
    match planes.fleet.publish_pending().await {
        Ok(n) => report.fleet_published = Some(n),
        Err(e) => push_step_error(&mut report, format!("publish_pending (fleet): {e:#}"), &e),
    }
    note_parked(planes, &mut report).await;
    report
}

/// [`PumpReport::parked`], read once the pass's last step has run.
async fn note_parked<B, R>(planes: Planes<'_, B, R>, report: &mut PumpReport)
where
    B: StoreBackend,
    R: RpcRequester,
{
    match planes.delegable.parked_count().await {
        Ok(n) => report.parked = Some(n),
        Err(e) => push_step_error(report, format!("reading the parked rows: {e:#}"), &e),
    }
}

/// One full pump pass: departures → publish → reconcile → content walks →
/// seen-set. Every step always attempted (see the module header);
/// failures collected. The seen-set step follows the content walks (it raises
/// to the frontier they advance) and the reconcile (sibling raises are merged
/// in first, so a covered raise is the no-op echo-stop).
///
/// Departures run **first**, so no later step in the same pass touches a scope
/// this replica has just left: the walk loop reads `content_scopes`, which
/// already excludes it, and the seen-set producer's domain is own-actor scopes,
/// which never depart.
pub(crate) async fn pump<B, R, L>(
    store: &AccountStore<B>,
    planes: Planes<'_, B, R>,
    rpc: &R,
    fleet_writer: FleetWriter<'_, R>,
    legs: &mut L,
    pass: &PassInputs<'_>,
) -> PumpReport
where
    B: StoreBackend,
    R: KeyedRpcRequester + Clone,
    R::Error: RpcErrorClass,
    L: HostLegs<B, R>,
{
    let PassInputs {
        content_scopes,
        own_scopes,
        membership_answered,
        endpoint_facts,
    } = *pass;
    let pass_started = now_ms();
    let mut report = PumpReport::default();
    let departures = drop_departed_scopes(store, content_scopes, membership_answered).await;
    report.errors.extend(departures.errors.iter().cloned());
    report.departures = Some(departures);
    // Trust follows the pin, every pass (`account-data-taxonomy.md` § The
    // generation machinery → *A holder change re-receipts and never mints*,
    // (1)): a rotation the app accepted since the last pass moves the holder
    // set every plane of this assembly reads, with no reassembly.
    let pinned = (fleet_writer.holder_source)();
    if fleet_writer.trust.trusted_holders.replace(pinned.clone()) {
        tracing::info!("escrow trust: the pinned holder moved — this pass trusts the new pin");
    }
    // The bind leg's replica check, ahead of every leg that talks to the
    // nest (`account-sync-plane.md` § The bind leg, ruling 2).
    let bind = bind_check(store, fleet_writer, &pinned, &mut report).await;
    // The enrollment ceremony's nest legs — early so a first pass on
    // a fresh machine registers the principal before anything else needs it;
    // after departures only because that step's first-place contract is
    // documented above (this step touches no scopes). On a bind verification
    // the latch is void: it was learned from another replica.
    let latch_void = bind.as_ref().is_some_and(|b| b.verify);
    enrollment_step(store, fleet_writer, latch_void, &mut report).await;
    // A cap-refused machine ends its pass here. The refusal means the
    // principal's grant is registered nowhere, so the data path it
    // authenticates (`fauna.auth.device_handshake`) cannot come up, and every
    // leg below would spend its kind's whole deadline waiting for a connection
    // that cannot come. The pass-bound commands wait for this pass to end, and
    // among them is the fleet-removal resolve behind the Devices page's
    // remove button, which is the remedy the refusal itself names. Ending here
    // serves that remedy at once. The next pass retries the register first,
    // and once a slot frees it runs in full again.
    if report.enrollment == Some(EnrollmentPass::DeviceLimitExceeded) {
        report.timings.total = since(pass_started);
        if let Some(b) = &bind {
            report.bind = Some(b.pass(false));
        }
        return report;
    }
    // The succession tail re-author, before the publish
    // steps so a just-rotated machine's re-journaled tail ships on this same
    // pass. Marker-gated: a plain `meta_get` on every ordinary pass.
    match crate::succession_tail::tail_reauthor_pass(store).await {
        Ok(p) => report.reauthor = p,
        Err(e) => push_step_error(&mut report, format!("tail re-author: {e:#}"), &e),
    }
    match planes.delegable.publish_pending().await {
        Ok(n) => report.published = Some(n),
        Err(e) => push_step_error(&mut report, format!("publish_pending: {e:#}"), &e),
    }
    match planes.fleet.publish_pending().await {
        Ok(n) => report.fleet_published = Some(n),
        Err(e) => push_step_error(&mut report, format!("publish_pending (fleet): {e:#}"), &e),
    }
    let outbox = crate::outbox::drain_outbox(store, rpc).await;
    report.errors.extend(outbox.errors.iter().cloned());
    report.outbox = Some(outbox);
    let step = now_ms();
    match planes.delegable.reconcile().await {
        Ok(w) => report.walk = Some(w),
        Err(e) => push_step_error(&mut report, format!("reconcile: {e:#}"), &e),
    }
    // The bind leg's row half, right behind the reconcile whose listing it
    // diffs against — and before reclamation, whose "published" evidence is
    // that listing plus what this pass acked.
    match crate::publish_diff::publish_diff(
        store,
        planes.delegable,
        fleet_writer.trust,
        fleet_writer.key,
    )
    .await
    {
        Ok(d) => report.publish_diff = Some(d),
        Err(e) => push_step_error(&mut report, format!("publish diff: {e:#}"), &e),
    }
    report.timings.walk = since(step);
    // The fleet walk is what makes the machinery *shared* rather than
    // per-device: a sibling's enrollment, a sibling's mint and its escrow
    // receipt only become this replica's merged state — and so only become
    // resolvable as a tip — by walking this scope.
    let step = now_ms();
    match planes.fleet.reconcile().await {
        Ok(w) => report.fleet_walk = Some(w),
        Err(e) => push_step_error(&mut report, format!("reconcile (fleet): {e:#}"), &e),
    }
    match crate::publish_diff::publish_diff(
        store,
        planes.fleet,
        fleet_writer.trust,
        fleet_writer.key,
    )
    .await
    {
        Ok(d) => report.fleet_publish_diff = Some(d),
        Err(e) => push_step_error(&mut report, format!("publish diff (fleet): {e:#}"), &e),
    }
    // The retire behind the succession carry, behind both reconciles: the
    // delegable walk's candidates and listing, judged against the member view
    // the fleet walk just merged.
    match crate::delegable_reclaim::retire_behind_carry(
        store,
        planes.delegable,
        fleet_writer.trust,
        fleet_writer.key,
    )
    .await
    {
        Ok(r) => report.carry_retire = r,
        Err(e) => push_step_error(&mut report, format!("carry retire: {e:#}"), &e),
    }
    // One live row per item, on the same evidence: the cover step's
    // hand-overs and the retires below each cover, and a departed scope's
    // retires. A member scope's item is handed over only on this pass's
    // affirmative answer (`delegable-scope-reclamation.md` part (7)).
    // Boxed: inline, the step's future would sit in every pass's frame on the
    // store thread's stack (`native-async-execution.md` § The rule).
    let answered: Option<std::collections::BTreeSet<String>> =
        membership_answered.then(|| content_scopes.iter().map(ToString::to_string).collect());
    match Box::pin(crate::delegable_reclaim::reclaim_below_cover(
        store,
        planes.delegable,
        fleet_writer.trust,
        fleet_writer.key,
        answered.as_ref(),
    ))
    .await
    {
        Ok(r) => report.cover_reclaim = r,
        Err(e) => push_step_error(&mut report, format!("cover step: {e:#}"), &e),
    }
    report.timings.fleet_walk = since(step);
    // The third-party kinds, behind the fleet walk that merged the manifest
    // rows admitting them.
    ext_kinds_step(store, planes.fleet, &mut report).await;
    // Escrow recovery (charter § The generation machinery → *Escrow
    // recovery*), right after the fleet walk — every wrap the plane carries
    // has been tried first — and ahead of every writer this pass (the host's
    // legs, the device-endpoints writer): a generation this device keys only
    // from escrow (the single-device sign-out → sign-in) is one each of them
    // seals under, and the top-up pass heals siblings with, this same pass.
    if fleet_writer.escrow_recovery.is_some() {
        let step = now_ms();
        recover_from_escrow(store, planes.fleet, fleet_writer, &mut report).await;
        // A key recovered this pass opens rows the fleet walk above had to
        // leave unopened — on a single-device sign-out → sign-in, every
        // account-level row the signed-out device sealed (the senior ATProto
        // rotation keys, the reception keypair, a tier's period keys).
        // Re-present them now (`cold_replica`'s walk does the same): the pass
        // that keys a generation is the pass that reads it, so the readiness
        // edge (`AccountStoreHandle::settled`) never answers a consumer "no
        // key held" about a row this device can already open, and the writers
        // and the hand-over below work on the merged entries this same pass.
        // An error that still recovered some keys logs them and reports
        // nothing here; the next pass's standing reconcile covers that case.
        if matches!(
            report.generation_escrow_recovery,
            Some(crate::generation_escrow_recover::EscrowRecoveryPass::Recovered(_))
        ) {
            match planes.fleet.reconcile().await {
                Ok(w) => report.fleet_rewalk = Some(w),
                Err(e) => push_step_error(
                    &mut report,
                    format!("reconcile (fleet, after escrow recovery): {e:#}"),
                    &e,
                ),
            }
        }
        report.timings.escrow_recovery = since(step);
    }
    // The re-escrow pass — the succession rider (`generation_reescrow`
    // module docs): every live generation this device keys is escrowed under
    // THIS identity's target at the trusted holder. Deposits only, never a
    // mint. After the fleet walk, which carries a predecessor's mint record
    // for each generation the slot keys, so a succession's first full pass
    // deposits them; after escrow recovery,
    // so a key recovered this pass is deposited this pass; before every
    // writer, so a pin that moved since the last pass (a second nest, a
    // rotated one) is re-receipted before anything seals — no tip resolves
    // under the new pin until it is, and a writer meeting no tip would
    // first-need mint on a mere holder change. Needs no seed — it
    // seals to the PUBLISHED target — so every key-holding runtime runs it.
    let step = now_ms();
    // The holdings check, in the first pass after every assembly and every
    // bind verification: the holder's own answer, never the receipts' say-so
    // (`account-data-taxonomy.md` § The generation machinery → *A holder
    // change re-receipts and never mints*, (3)). Unanswered → an ordinary
    // pass, and the check stays due.
    // With no trusted holder nothing is acked, so there is nothing to check.
    let holdings =
        if fleet_writer.bind.holdings_due() && !fleet_writer.trust.trusted_holders.is_empty() {
            match crate::bind_leg::holder_holdings(planes.fleet.requester()).await {
                Ok(h) => Some(h),
                Err(e) => {
                    push_step_error(&mut report, format!("holdings check: {e:#}"), &e);
                    None
                }
            }
        } else {
            None
        };
    match crate::generation_reescrow::ensure_reescrowed(
        store,
        planes.fleet,
        fleet_writer.trust,
        fleet_writer.key,
        holdings.as_ref(),
    )
    .await
    {
        Ok(p) => {
            if holdings.is_some() {
                fleet_writer.bind.holdings_done();
            }
            report.generation_reescrow = Some(p);
        }
        Err(e) => push_step_error(&mut report, format!("generation re-escrow: {e:#}"), &e),
    }
    report.timings.reescrow = since(step);
    // The host's legs — natively the peer leg and the custody leg, on web
    // none — run here, between the fleet walk and the device-endpoints step:
    // after the walk so a sibling's removal row merged THIS pass already
    // severs by the dial, before the endpoints step so a first bind's facts
    // publish on the same pass (`HostLegs` owns the order inside, and each
    // leg reports into its own `PumpReport` slot). What the steps below need
    // from them comes back as [`LegsOutput`]: the bind's self-observed
    // transport facts, and every peer endpoint this pass observed.
    let legs_out = legs
        .run(
            LegsCtx {
                store,
                fleet: planes.fleet,
                trust: fleet_writer.trust,
                writer_key: fleet_writer.key,
                slot: fleet_writer.slot,
                schedule: fleet_writer.schedule,
                rpc,
                content_scopes,
                endpoint_facts,
            },
            &mut report,
        )
        .await;
    // App-fed facts (the `set_endpoint_facts` Cmd — the explicit override)
    // win over the bind's self-observed ones.
    let effective_facts = endpoint_facts.or(legs_out.bind_facts.as_ref());
    // T13 step 4, the write-back. Everything this pass observed about a peer
    // — carried IN by a dialer (the listener's drain) or answered OUT to our
    // own dials (the dial passes' replies) — lands in whichever registry row
    // already names that node: `custodian-endpoints` when we are the owner,
    // `custodies-held` when we are the custodian. Every observation is
    // already bound to the channel-proven key, and both folds move only
    // WHERE a known peer is, never WHO it is. Running both sides
    // unconditionally is what saves the machine whose every stored address
    // is stale: its own dials fail, so an inbound session is the only
    // teacher it has left.
    if !legs_out.observed_endpoints.is_empty() {
        if let Err(e) = refresh_custodian_endpoints(
            store,
            planes.fleet,
            fleet_writer.key,
            &legs_out.observed_endpoints,
        )
        .await
        {
            tracing::debug!("custody endpoint refresh (custodian row): {e:#}");
        }
        if let Err(e) = refresh_held_owner_devices(
            store,
            planes.fleet,
            fleet_writer.key,
            &legs_out.observed_endpoints,
        )
        .await
        {
            tracing::debug!("custody endpoint refresh (held row): {e:#}");
        }
    }
    // The T5 discovery feed's writer, after the fleet walk so a sibling's
    // mint merged this pass is already resolvable, and after the re-escrow so
    // the tip it seals under is the one this pass's pin trusts. The account's
    // first publish here is what trips the mint protocol's trigger (a) at the
    // door.
    let step = now_ms();
    match crate::device_endpoints_writer::ensure_published(
        store,
        planes.fleet,
        fleet_writer.trust,
        fleet_writer.key,
        effective_facts,
        // This device's own row statement — what a sibling's devices page
        // resolves a removal's target from (`fauna_core::fleet_removal`).
        stated_row(fleet_writer.slot.grant_registration_row()),
    )
    .await
    {
        Ok(p) => report.device_endpoints = Some(p),
        Err(e) => push_step_error(&mut report, format!("device-endpoints: {e:#}"), &e),
    }
    report.timings.device_endpoints = since(step);
    // The top-up self-heal pass, after the same walk and for the same reason:
    // a sibling's enrollment row merged this pass is exactly the device that
    // needs a wrap for every generation minted before it existed. Best-effort
    // like every other step — a failure here partitions nobody further than
    // they already are, and the next pass retries.
    let step = now_ms();
    match crate::generation_topup::ensure_topped_up(
        store,
        planes.fleet,
        fleet_writer.trust,
        fleet_writer.key,
    )
    .await
    {
        Ok(p) => report.generation_topup = Some(p),
        Err(e) => push_step_error(&mut report, format!("generation top-up: {e:#}"), &e),
    }
    report.timings.topup = since(step);
    // The target's own testimony, after the top-up pass so any wrap
    // merged or published this pass is tried before this device asserts it
    // cannot key — and so a heal that just landed is retracted this pass, not
    // next.
    let step = now_ms();
    match crate::generation_unkeyable::ensure_signalled(
        store,
        planes.fleet,
        fleet_writer.trust,
        fleet_writer.key,
    )
    .await
    {
        Ok(p) => report.generation_unkeyable = Some(p),
        Err(e) => push_step_error(&mut report, format!("generation unkeyable: {e:#}"), &e),
    }
    report.timings.unkeyable = since(step);
    // The reclamation pass (charter § The generation machinery → *Fleet-scope
    // reclamation*), after the top-up and unkeyable passes so this pass's own
    // heals and retractions are what its reach and its retires reflect.
    let step = now_ms();
    match crate::generation_reclaim::ensure_reclaimed(
        store,
        planes.fleet,
        fleet_writer.trust,
        fleet_writer.key,
    )
    .await
    {
        Ok(p) => report.generation_reclaim = Some(p),
        Err(e) => push_step_error(&mut report, format!("generation reclaim: {e:#}"), &e),
    }
    report.timings.reclaim = since(step);
    // The secondary leg (`account-sync-plane.md` § The bind leg, ruling 4),
    // right behind reclamation: what it retired at the bound nest this pass
    // is asked of every linked nest still listing the row. Only the seed-leg
    // role's holder carries the connector; without the role the retires this
    // pass sent stay in the store's record for the runtime that holds it.
    // The road's bound-nest step goes first: the owed list the successor is
    // served where it is bound.
    deliver_owed_at(fleet_writer, fleet_writer.session_rpc, None, &mut report).await;
    if let Some(connector) = fleet_writer.linked_nests {
        let step = now_ms();
        report.linked = Some(
            secondary_leg(
                store,
                planes.fleet,
                fleet_writer,
                connector,
                &pinned,
                &mut report,
            )
            .await,
        );
        report.timings.linked = since(step);
    }
    // The staged device removals' reconcile (clause (4), *The completion
    // rule*) — the boot-reconcile half of the two-leg removal. Costs nothing
    // on the ordinary pass: the roster is read only while an intent is staged.
    // The roster it reads is shared with the removed-grants step below.
    let mut staged_roster = None;
    if !fleet_writer.slot.pending_fleet_removals().is_empty() {
        let read = crate::removed_grants::read_roster(fleet_writer.session_rpc).await;
        let roster: Option<Vec<String>> = match &read {
            Ok(devices) => Some(devices.iter().map(|d| d.device_id.clone()).collect()),
            Err(e) => {
                tracing::debug!("fleet removal: the roster could not be read ({e:#}) — waiting");
                None
            }
        };
        staged_roster = Some(read);
        let removals = crate::fleet_removal::complete_pending(
            store,
            planes.fleet,
            fleet_writer.trust,
            fleet_writer.key,
            fleet_writer.slot,
            roster.as_deref(),
        )
        .await;
        // The `Removed` rows it journaled are local writes (the quartet is
        // served inside passes — `fleet_removal` module docs): this pass,
        // already past its own fleet publish, ships them itself.
        if removals.completed > 0 {
            match planes.fleet.publish_pending().await {
                Ok(n) => *report.fleet_published.get_or_insert(0) += n,
                Err(e) => push_step_error(
                    &mut report,
                    format!("publish_pending (fleet, removals): {e:#}"),
                    &e,
                ),
            }
        }
        report.fleet_removals = Some(removals);
    }
    // The removal's nest half at the bound nest (clause (4), *The nest half
    // follows merged state*): the grant of every roster row whose principal
    // merged state reads removed is revoked by key. After the reconcile above,
    // so a removal it finished this pass is read removed here; on the account's
    // session, so a seedless agent's pass runs it too. Nothing is asked of the
    // nest before the account's first removal.
    let me = fleet_writer.key.verifying_key().to_bytes();
    let roster = match &staged_roster {
        Some(Ok(roster)) => crate::removed_grants::Roster::Read(roster),
        Some(Err(e)) => crate::removed_grants::Roster::Unreadable(format!("{e:#}")),
        None => crate::removed_grants::Roster::Unread,
    };
    let removed_grants = crate::removed_grants::revoke_removed_grants(
        store,
        fleet_writer.trust,
        &me,
        fleet_writer.session_rpc,
        roster,
    )
    .await;
    report.errors.extend(
        removed_grants
            .errors
            .iter()
            .map(|e| format!("removed grants: {e}")),
    );
    report.removed_grants = Some(removed_grants);
    // The group plane's leg: the authority-device severance. Placed after the
    // fleet steps for symmetry, though nothing here reads their output: the
    // group plane is pull-only with no feed wired, so its rows arrive by
    // ceremony adoption, not by a walk.
    // The severance's carriage comes from the SAME assembly as its key, for
    // `Cmd::GroupCeremonyAuthority`'s reason: a caller pairing a freshly
    // rotated writer key with a stale carriage would author group-plane rows
    // nothing can verify. `None` — this machine ran no enrollment ceremony —
    // is the quiet, self-healing absence, and the pass writes nothing.
    match crate::group_authority_revocation::ensure_revoked(
        store,
        fleet_writer.trust,
        fleet_writer.key,
        fleet_writer.slot.device_authorization_carriage().as_deref(),
    )
    .await
    {
        Ok(p) => report.group_authority_revocation = Some(p),
        Err(e) => push_step_error(&mut report, format!("group authority severance: {e:#}"), &e),
    }
    for cs in content_scopes {
        match ContentScopePlane::new(store, rpc, cs.clone()).walk().await {
            Ok(w) => report.content_walks.push(w),
            Err(e) => push_step_error(&mut report, format!("content walk ({cs}): {e:#}"), &e),
        }
    }
    let seen = auto_in_set_pass(store, planes.delegable, own_scopes).await;
    report.errors.extend(seen.errors.iter().cloned());
    report.seen_set = Some(seen);
    // The pass ends: a scope whose listing ran to its end with rows still
    // unopened is listed now (`account-client-lifecycle.md` § The client-side
    // lifecycle → *The first listing*, clause (1)) — the escrow recovery above
    // has re-presented what it keyed, and a row this device still cannot open
    // must not hold the fact back. A pass that was cut never reaches here.
    for plane in [planes.delegable, planes.fleet] {
        if let Err(e) = plane.pass_ended().await {
            push_step_error(
                &mut report,
                format!("recording the listing ({}): {e:#}", plane.scope()),
                &e,
            );
        }
    }
    // The pass completed: a bind verification it carried through is settled
    // now, never earlier, so one cut short runs again; a first bind is adopted.
    if let Some(b) = &bind {
        let owed = b.first || (b.verify && b.verified && grant_on_nest(&report));
        let settled = owed
            && match crate::bind_leg::record_settled_replica(store, &b.bound).await {
                Ok(()) => true,
                Err(e) => {
                    push_step_error(&mut report, format!("settling the replica: {e:#}"), &e);
                    false
                }
            };
        if settled {
            fleet_writer.bind.settle(&b.bound);
            tracing::info!("bind leg: the bound replica is settled");
        }
        report.bind = Some(b.pass(settled));
    }
    note_parked(planes, &mut report).await;
    report.timings.total = since(pass_started);
    report
}

/// The `ext.*` kinds' step (`third-party-kinds.md` § The `ext` sub-scope):
/// read the overlay off the merged `fauna.state.kind-manifest` rows —
/// re-verified on every read, so a row that no longer verifies admits
/// nothing — and run one `ext:<kind>` plane per admitted kind, derived from
/// the bound fleet plane: publish its unsent rows, then reconcile. Read at
/// every pass, so a kind admitted on a sibling device is walked here on the
/// pass after the fleet walk merged its row. Each kind's failure is its own
/// error; the rest still run.
async fn ext_kinds_step<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    report: &mut PumpReport,
) where
    B: StoreBackend,
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let overlay = match crate::kind_manifest_rows::read_admitted_kinds(store).await {
        Ok(overlay) => overlay,
        Err(e) => {
            push_step_error(report, format!("reading the admitted kinds: {e:#}"), &e);
            return;
        }
    };
    for (client_id, why) in &overlay.refused {
        tracing::warn!("account pump: the kind manifest of {client_id} admits nothing: {why}");
    }
    for kind in overlay.kinds.kinds() {
        let plane = fleet.ext_plane(kind, &overlay.kinds);
        let scope = plane.scope().to_owned();
        if let Err(e) = plane.publish_pending().await {
            push_step_error(report, format!("publish_pending ({scope}): {e:#}"), &e);
        }
        match plane.reconcile().await {
            Ok(w) => report.ext_walks.push((scope.clone(), w)),
            Err(e) => push_step_error(report, format!("reconcile ({scope}): {e:#}"), &e),
        }
        if let Err(e) = plane.pass_ended().await {
            push_step_error(
                report,
                format!("recording the listing ({scope}): {e:#}"),
                &e,
            );
        }
    }
}

/// The escrow-recovery step (charter § The generation machinery → *Escrow
/// recovery*), for the seed-leg role's holder — a step of [`pump`] and of
/// [`seed_pass`]. Recovery, and only recovery, admits a verified ancestor's
/// receipt; the ancestry is the assembly's ([`crate::bind_leg::BindMemo`]).
async fn recover_from_escrow<B, R>(
    store: &AccountStore<B>,
    fleet: &AccountStatePlane<'_, B, R>,
    fleet_writer: FleetWriter<'_, R>,
    report: &mut PumpReport,
) where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    let Some((identity_seed, predecessors, memo)) = fleet_writer.escrow_recovery else {
        return;
    };
    let ancestors = fleet_writer.bind.ancestors().unwrap_or_default();
    match crate::generation_escrow_recover::ensure_recovered(
        store,
        fleet,
        fleet_writer.trust,
        fleet_writer.key,
        identity_seed,
        predecessors,
        memo,
        &ancestors,
    )
    .await
    {
        Ok(p) => report.generation_escrow_recovery = Some(p),
        Err(e) => push_step_error(report, format!("generation escrow recovery: {e:#}"), &e),
    }
}

/// One run of the secondary leg (`account-sync-plane.md` § The bind leg,
/// rulings 4 and 5) — a step of [`pump`] and of [`seed_pass`], for the
/// seed-leg role's holder. The store's retire record is read, re-issued at
/// each linked nest the run reaches ([`linked_pass`]), and cleared through
/// what was read when the run ends, whether or not every linked nest was
/// reached: one leg run deep. An entry another process records meanwhile
/// stays for the next run. The run ends with the removed-device arm (ruling
/// 7), which needs no record.
async fn secondary_leg<B, R>(
    store: &AccountStore<B>,
    bound_fleet: &AccountStatePlane<'_, B, R>,
    fleet_writer: FleetWriter<'_, R>,
    connector: &super::LinkedNestConnector<R>,
    pinned: &[[u8; 32]],
    report: &mut PumpReport,
) -> crate::linked_leg::LinkedPass
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let record = match store.issued_retires().await {
        Ok(r) => r,
        Err(e) => {
            push_step_error(
                report,
                format!("secondary leg: the retire record: {e:#}"),
                &e,
            );
            Vec::new()
        }
    };
    let through = record.last().map(|(ord, _)| *ord);
    let issued: Vec<_> = record.into_iter().map(|(_, retire)| retire).collect();
    let pass = linked_pass(
        store,
        bound_fleet,
        fleet_writer,
        connector,
        pinned,
        &issued,
        report,
    )
    .await;
    if let Some(through) = through
        && let Err(e) = store.clear_issued_retires_through(through).await
    {
        push_step_error(
            report,
            format!("secondary leg: clearing the retire record: {e:#}"),
            &e,
        );
    }
    pass
}

/// The enrollment ceremony's nest legs as one pass step — a full pass's and a
/// seed pass's alike: the verdict into the report's slot, the store
/// principal's connect gate opened the moment the grant is on the nest.
async fn enrollment_step<B, R>(
    store: &AccountStore<B>,
    fleet_writer: FleetWriter<'_, R>,
    latch_void: bool,
    report: &mut PumpReport,
) where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    match ensure_enrollment_registered(store, fleet_writer, latch_void).await {
        Ok(step) => {
            tracing::debug!(verdict = ?step.pass, "enrollment: pass verdict");
            report.enrollment = Some(step.pass);
            report.own_row_removed = step.own_row_removed;
            if grant_on_nest(report) {
                super::open_grant_gate(fleet_writer.grant_registered);
            }
        }
        Err(e) => {
            // The error otherwise reaches only `report.errors`, which no log
            // prints — so a grant leg failing every pass is invisible at every
            // level, and the *consequence* (a device-principal client refused
            // `not_registered` forever, hence a data path that never connects,
            // hence no generation can be minted) surfaces minutes later as
            // something else's problem. Measured on the share journey's linux
            // seats 2026-08-26: the 403 was
            // right there and its cause had no line anywhere. Debug, not warn:
            // offline is the expected cause and the next tick retries.
            tracing::debug!("enrollment: grant legs failed this pass: {e:#}");
            push_step_error(report, format!("enrollment: {e:#}"), &e);
        }
    }
}

/// The seed pass's enrollment step (`account-runtime.md` § Multi-instance
/// concurrency → *The seed-leg role*, part 4, *Enrollment*): beside an engine
/// holder that cannot register — the seedless agent, whose own connection a
/// rebuilt or second bound box refuses `not_registered` — the signed-in app
/// puts the machine's grant on the bound nest.
///
/// The **read-only half** of the bind check ([`bound_and_settled`]) decides
/// the latch: void when the bound nest is a replica other than the settled
/// one, since the latch was learned from another. It clears no watermark,
/// re-arms no holdings check and settles nothing — those are the engine
/// holder's, whose own register probe finds the grant on the nest at its next
/// pass. So the replica stays unsettled across seed passes, and the latch is
/// void **once per target** ([`BindMemo::seed_registered_at`]): after that the
/// slot's latch is this replica's again. A probe that could not be asked voids
/// nothing; a latch the device handshake voided reads as no latch and
/// registers either way.
///
/// [`BindMemo::seed_registered_at`]: crate::bind_leg::BindMemo::seed_registered_at
async fn seed_enrollment<B, R>(
    store: &AccountStore<B>,
    fleet_writer: FleetWriter<'_, R>,
    pinned: &[[u8; 32]],
    report: &mut PumpReport,
) where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let moved = bound_and_settled(store, fleet_writer, pinned, report)
        .await
        .filter(|(bound, settled)| {
            crate::bind_leg::needs_verification(settled.as_ref(), bound, fleet_writer.bind)
        })
        .map(|(bound, _)| bound);
    let latch_void = moved
        .as_ref()
        .is_some_and(|bound| !fleet_writer.bind.seed_registered_at(bound));
    if latch_void {
        tracing::info!(
            "seed pass: bound to a replica other than the settled one — registering this \
             machine there (the latch is void); the verification is the engine holder's"
        );
    }
    enrollment_step(store, fleet_writer, latch_void, report).await;
    if let Some(bound) = &moved
        && grant_on_nest(report)
    {
        fleet_writer.bind.note_seed_registered(bound);
    }
}

/// The **seed pass** (`account-runtime.md` § Multi-instance concurrency → *The
/// seed-leg role*, parts 2–5): the seed-only steps alone, in pass order, run
/// by the seed-leg role's holder while another process — ordinarily the
/// seedless sync agent — holds the engine role. At every wake on which an
/// engine holder runs a full pass (the first after assembly, the backstop
/// tick, a reconnect, `reconcile_now`), never per nudge.
///
/// **It writes nothing the engine role makes exclusive** (part 3): it walks
/// and reconciles no bound plane, sends and banks no watermark, drains no
/// outbox, clears nothing and settles no replica. What it writes, a non-holder
/// already may: rows through the bound planes' writer door (a linked holder's
/// receipt, a custody row), published by the pass's own publish step at its
/// end, and whatever the linked planes write — which by construction leave
/// every memory about the bound nest alone.
///
/// The steps:
/// 1. **The pin re-read**, as every pass opens.
/// 2. **The enrollment registration** ([`seed_enrollment`]): the machine's
///    grant on the bound nest, registered over the owner session when the
///    nest is a replica other than the settled one or the device handshake
///    voided the latch. A cap-refused machine ends its seed pass here, as it
///    ends a full pass.
/// 3. **Escrow recovery.** The pass reads for itself the ancestry recovery
///    admits — the bind check that reads it in a full pass is the engine
///    holder's — once per assembly and again whenever the pin moved. A
///    recovered key lands in the shared slot's retained bundle, where the
///    holder reads it; the rows it opens are re-presented by the holder's
///    next full reconcile, never by a walk of a bound plane here.
/// 4. **The road, then the secondary leg**: the bound nest's owed list
///    delivered ([`deliver_owed_at`]), then the leg and the custody arm riding
///    it, over the store's retire record ([`secondary_leg`]) — which delivers
///    each linked nest's owed list as it reaches it.
/// 5. **The publish step** for what 3–4 wrote through the bound planes.
///
/// The report says `skipped_non_holder` — the engine role's answer is
/// unchanged — with this pass's slots filled.
pub(crate) async fn seed_pass<B, R>(
    store: &AccountStore<B>,
    planes: Planes<'_, B, R>,
    fleet_writer: FleetWriter<'_, R>,
) -> PumpReport
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    let pass_started = now_ms();
    let mut report = PumpReport {
        skipped_non_holder: true,
        ..PumpReport::default()
    };
    let pinned = (fleet_writer.holder_source)();
    let pin_moved = fleet_writer.trust.trusted_holders.replace(pinned.clone());
    if pin_moved {
        tracing::info!("escrow trust: the pinned holder moved — this seed pass trusts the new pin");
        // The ancestry was proven to the old pin.
        fleet_writer.bind.set_ancestors(None);
    }
    seed_enrollment(store, fleet_writer, &pinned, &mut report).await;
    // As in a full pass: the refusal means the grant is registered nowhere,
    // so the legs below would wait out their deadlines on a connection that
    // cannot come.
    if report.enrollment == Some(EnrollmentPass::DeviceLimitExceeded) {
        report.timings.total = since(pass_started);
        return report;
    }
    if fleet_writer.escrow_recovery.is_some() {
        let step = now_ms();
        // Best-effort, as in the bind check: the ancestry only widens what
        // recovery may ask for, and a nest that serves no chain reads as
        // "never rotated".
        if fleet_writer.bind.ancestors().is_none() {
            match crate::bind_leg::fetch_verified_ancestors(fleet_writer.session_rpc, &pinned).await
            {
                Ok(a) => fleet_writer.bind.set_ancestors(Some(a)),
                Err(e) => tracing::debug!(
                    "seed pass: the rotation chain could not be read (retried): {e:#}"
                ),
            }
        }
        recover_from_escrow(store, planes.fleet, fleet_writer, &mut report).await;
        report.timings.escrow_recovery = since(step);
    }
    deliver_owed_at(fleet_writer, fleet_writer.session_rpc, None, &mut report).await;
    if let Some(connector) = fleet_writer.linked_nests {
        let step = now_ms();
        report.linked = Some(
            secondary_leg(
                store,
                planes.fleet,
                fleet_writer,
                connector,
                &pinned,
                &mut report,
            )
            .await,
        );
        report.timings.linked = since(step);
    }
    // The pass's own publish step: no full pass of this runtime follows to
    // ship what the steps above journaled.
    match planes.delegable.publish_pending().await {
        Ok(n) => report.published = Some(n),
        Err(e) => push_step_error(&mut report, format!("publish_pending: {e:#}"), &e),
    }
    match planes.fleet.publish_pending().await {
        Ok(n) => *report.fleet_published.get_or_insert(0) += n,
        Err(e) => push_step_error(&mut report, format!("publish_pending (fleet): {e:#}"), &e),
    }
    note_parked(planes, &mut report).await;
    report.timings.total = since(pass_started);
    report
}

/// The secondary leg over every linked replica the account lists
/// (`crate::linked_leg`): the pairings are read on the owner session, each
/// target is connected through the host's connector and completed in turn —
/// the deployment-seed custody leg first ([`linked_custody`]), over the same
/// connection. A nest that cannot be reached is reported and asked again next
/// pass. A run that reached every one of them ends by retiring the removal
/// evidence it carried (`crate::linked_leg::retire_carried_evidence`).
async fn linked_pass<B, R>(
    store: &AccountStore<B>,
    bound_fleet: &AccountStatePlane<'_, B, R>,
    fleet_writer: FleetWriter<'_, R>,
    connector: &super::LinkedNestConnector<R>,
    pinned: &[[u8; 32]],
    issued: &[crate::account_state_plane::IssuedRetire],
    report: &mut PumpReport,
) -> crate::linked_leg::LinkedPass
where
    B: StoreBackend,
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
{
    use crate::linked_leg::{
        LinkedChainOutcome, LinkedChainPass, LinkedCtx, LinkedNestPass, LinkedOutcome, LinkedPass,
        complete_linked_nest, list_linked_targets, reconcile_linked_chain, retire_carried_evidence,
    };
    let targets = match list_linked_targets(fleet_writer.session_rpc, pinned).await {
        Ok(t) => t,
        Err(e) => {
            tracing::debug!("secondary leg: the pairings could not be listed: {e:#}");
            push_step_error(report, format!("secondary leg: {e:#}"), &e);
            return LinkedPass {
                unlisted: true,
                ..LinkedPass::default()
            };
        }
    };
    let ctx = LinkedCtx {
        store,
        bound_fleet,
        schedule: fleet_writer.schedule,
        trust: fleet_writer.trust,
        writer_key: fleet_writer.key,
        custody: bound_fleet.generation_custody(),
    };
    // The removal evidence the relay plane holds NOW, before the first linked
    // nest is touched: a push a nest refuses for good retires the local copy
    // mid-run (`crate::linked_leg`, *The removed-device arm*).
    let evidence =
        match crate::generation_reclaim::removal_evidence(store, bound_fleet, fleet_writer.trust)
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                push_step_error(
                    report,
                    format!("secondary leg: the removal evidence: {e:#}"),
                    &e,
                );
                Vec::new()
            }
        };
    // The account every connection of the run is authenticated as — whose
    // registration chain the run carries.
    let account = fauna_core::hex32::decode(store.actor_id_hex()).ok();
    let mut conns = std::collections::BTreeMap::new();
    let mut pass = LinkedPass::default();
    // Each linked nest's pending seed-alone window, for the standing alert.
    let mut windows = Vec::new();
    for target in targets {
        crate::pass_breath::pass_breath().await;
        let conn = match connector(target.clone()).await {
            Ok(conn) => Some(conn),
            Err(e) => {
                tracing::debug!(
                    linked = %fauna_core::hex32::encode(&target.nest_id),
                    "secondary leg: a linked nest could not be reached: {e:#}"
                );
                None
            }
        };
        // The chain follows the link (`identity-succession.md` § Enforcement
        // on the home nest → *Every nest the identity is linked to*, clause
        // (b)): every addressed pairing, whatever its capabilities, ahead of
        // the completion — a nest that cannot verify a succession is the gap
        // a seed thief uses, and closing it waits on nothing else here.
        if let Some(actor_id) = account {
            let outcome = match &conn {
                Some(conn) => {
                    reconcile_linked_chain(fleet_writer.session_rpc, actor_id, &target, conn).await
                }
                None => LinkedChainOutcome::Unreachable,
            };
            log_linked_chain(&target, &outcome);
            pass.chains.push(LinkedChainPass {
                nest_id: target.nest_id,
                outcome,
            });
            windows.push((
                target.nest_id,
                read_linked_window(&target, conn.as_ref()).await,
            ));
        }
        // The road closes over nests: a linked nest that applied a statement
        // burned pairings of its own and keeps its own owed nests, served to
        // this account there — behind the same channel binding.
        if let Some(conn) = conn.as_ref().filter(|c| c.bound_identity == target.nest_id) {
            deliver_owed_at(fleet_writer, &conn.rpc, Some(target.nest_id), report).await;
        }
        if !target.replica {
            continue;
        }
        let outcome = match conn {
            Some(conn) => {
                // The custody leg first: a row it captures is shipped to the
                // bound nest here and to this nest by the completion below.
                let custody = linked_custody(store, bound_fleet, &target, &conn).await;
                if custody_wrote(&custody.leg) {
                    match bound_fleet.publish_pending().await {
                        Ok(n) => *report.fleet_published.get_or_insert(0) += n,
                        Err(e) => push_step_error(
                            report,
                            format!("publish_pending (fleet, linked custody): {e:#}"),
                            &e,
                        ),
                    }
                }
                report.linked_custody.push(custody);
                let outcome = complete_linked_nest(&ctx, &target, &conn, issued, &evidence).await;
                conns.insert(target.nest_id, conn);
                outcome
            }
            None => LinkedOutcome::Unreachable,
        };
        pass.nests.push(LinkedNestPass {
            nest_id: target.nest_id,
            outcome,
        });
    }
    pass.evidence = retire_carried_evidence(&ctx, &evidence, &pass.nests, &conns, pinned).await;
    if let Some(actor_id) = account {
        fauna_client_core::recovery_pending::record_linked_readings(
            &fauna_core::identity::ActorId(actor_id),
            &windows,
        );
    }
    pass
}

/// Read one linked nest's pending seed-alone window over the leg's
/// connection, behind the same channel binding as every step of the leg
/// (`identity-succession.md` § Enforcement on the home nest → *Every nest the
/// identity is linked to*, clause (c): the reconcile reads each linked nest's
/// pending replacement and feeds the standing alert). Anything short of an
/// answer is [`LinkedReading::Unread`], which keeps that nest's last reading.
///
/// [`LinkedReading::Unread`]: fauna_client_core::recovery_pending::LinkedReading::Unread
async fn read_linked_window<R: RpcRequester>(
    target: &crate::linked_leg::LinkedNestTarget,
    conn: Option<&crate::linked_leg::LinkedConnection<R>>,
) -> fauna_client_core::recovery_pending::LinkedReading {
    use fauna_client_core::recovery_pending::LinkedReading;
    let Some(conn) = conn.filter(|c| c.bound_identity == target.nest_id) else {
        return LinkedReading::Unread;
    };
    match fauna_client_core::recovery_chain::fetch_replacement_status(&conn.rpc).await {
        Ok(window) => {
            if window.is_some() {
                tracing::warn!(
                    linked = %fauna_core::hex32::encode(&target.nest_id),
                    "secondary leg: a seed-alone recovery-key replacement is pending at a \
                     linked nest — the standing alert carries it"
                );
            }
            LinkedReading::Read(window.map(Into::into))
        }
        Err(e) => {
            tracing::debug!(
                linked = %fauna_core::hex32::encode(&target.nest_id),
                "secondary leg: the pending replacement could not be read (retried): {e}"
            );
            LinkedReading::Unread
        }
    }
}

/// The road at one keeping nest (`crate::owed_delivery`;
/// `identity-succession.md` § Enforcement on the home nest → *Every nest the
/// identity is linked to*, **The road**): the host's deliverer reads the owed
/// list `keeper` serves this account and delivers each entry, and the pass
/// records what came of it. `keeper_id` is `None` for the bound nest. A
/// runtime holding no deliverer does nothing; an unread list or an entry that
/// could not land is reported and asked again next pass, never a step error —
/// the delivery waits on nests this device does not control.
async fn deliver_owed_at<R>(
    fleet_writer: FleetWriter<'_, R>,
    keeper: &R,
    keeper_id: Option<[u8; 32]>,
    report: &mut PumpReport,
) where
    R: Clone,
{
    let Some(deliver) = fleet_writer.owed_nests else {
        return;
    };
    let pass = crate::owed_delivery::OwedNestsPass {
        keeper: keeper_id,
        outcome: deliver(keeper.clone()).await,
    };
    crate::owed_delivery::log_owed_nests(&pass);
    report.owed_nests.push(pass);
}

/// Log one linked nest's chain reconcile on the event: a carried chain at
/// info, a fork or a failure at warn — both leave a linked nest unable to
/// verify what the bound nest can — and the quiet answers at debug.
fn log_linked_chain(
    target: &crate::linked_leg::LinkedNestTarget,
    outcome: &crate::linked_leg::LinkedChainOutcome,
) {
    use crate::linked_leg::LinkedChainOutcome;
    use fauna_client_core::recovery_chain::ChainReconcile;
    let linked = fauna_core::hex32::encode(&target.nest_id);
    match outcome {
        LinkedChainOutcome::Reconciled(ChainReconcile::Extended { side, records }) => {
            tracing::info!(
                %linked, records,
                "secondary leg: the recovery-key chain was carried to {side}"
            );
        }
        LinkedChainOutcome::Reconciled(ChainReconcile::Forked) => tracing::warn!(
            %linked,
            "secondary leg: this nest and the bound nest hold different recovery keys for the \
             account — neither chain is submitted over the other"
        ),
        LinkedChainOutcome::Failed(e) => tracing::warn!(
            %linked,
            "secondary leg: the recovery-key chain could not be reconciled (retried next pass): {e}"
        ),
        LinkedChainOutcome::IdentityMismatch { .. } => tracing::warn!(
            %linked,
            "secondary leg: the connection is bound to another identity — no chain read or sent"
        ),
        other => tracing::debug!(%linked, outcome = ?other, "secondary leg: recovery-key chain"),
    }
}

/// The custody fold over the open store — the [`CustodySeedFold`] the pass
/// hands the linked custody leg. The pass runs on the store thread, so it
/// reads and joins in place (`crate::deployment_seed_rows`) rather than
/// through the handle's command channel; the join is the same writer door,
/// tip refusal included.
struct PassCustodyFold<'a, 'p, B: StoreBackend, R: RpcRequester> {
    store: &'a AccountStore<B>,
    fleet: &'a AccountStatePlane<'p, B, R>,
}

impl<B: StoreBackend, R: RpcRequester> CustodySeedFold for PassCustodyFold<'_, '_, B, R> {
    async fn custody_fold(&self) -> Result<Vec<DeploymentSeedEntry>, String> {
        crate::deployment_seed_rows::read_deployment_seeds(self.store)
            .await
            .map_err(|e| format!("{e:#}"))
    }

    async fn custody_merge(&self, replica: Vec<DeploymentSeedEntry>) -> Result<(), String> {
        crate::deployment_seed_rows::merge_deployment_seeds(self.store, self.fleet, &replica)
            .await
            .map(drop)
            .map_err(|e| format!("{e:#}"))
    }
}

/// The deployment-seed custody leg over one linked nest's connection
/// (`nest/box-recovery.md` § The plane-era recovery floor, *(c)*): the shared
/// leg against the pairing row's id, behind the channel-binding check. An
/// admin of that nest custodies it here without ever binding a device to it;
/// anyone else is answered `NotAdmin` there and owes nothing.
async fn linked_custody<B, R>(
    store: &AccountStore<B>,
    bound_fleet: &AccountStatePlane<'_, B, R>,
    target: &crate::linked_leg::LinkedNestTarget,
    conn: &crate::linked_leg::LinkedConnection<R>,
) -> LinkedCustodyPass
where
    B: StoreBackend,
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let fold = PassCustodyFold {
        store,
        fleet: bound_fleet,
    };
    let leg = run_linked_deployment_seed_custody_leg(
        &conn.rpc,
        ActorId(conn.bound_identity),
        ActorId(target.nest_id),
        &fold,
    )
    .await;
    let linked = fauna_core::hex32::encode(&target.nest_id);
    match &leg {
        LinkedCustodyLeg::IdentityMismatch { .. } => tracing::debug!(
            %linked,
            "linked custody leg: the connection is bound to another identity — nothing asked"
        ),
        LinkedCustodyLeg::Ran(run) => {
            match &run.custody {
                DeploymentSeedCustody::Captured => tracing::info!(
                    %linked,
                    "linked nest's deployment seed custodied off-box on the account plane \
                     (linked custody leg: captured)"
                ),
                DeploymentSeedCustody::AlreadyCustodied => tracing::debug!(
                    %linked,
                    "linked nest's deployment seed already custodied off-box on the account \
                     plane (linked custody leg: held)"
                ),
                DeploymentSeedCustody::NotAdmin => {
                    tracing::debug!(%linked, "linked custody leg: not an admin of this nest")
                }
                other if other.retryable() => tracing::debug!(
                    %linked,
                    outcome = ?other,
                    "linked custody leg: custody unconfirmed (retried next pass)"
                ),
                other => tracing::warn!(
                    %linked,
                    outcome = ?other,
                    "linked custody leg: custody unconfirmed"
                ),
            }
            if let SupersessionMarks::Checked(marked) = &run.marks
                && !marked.is_empty()
            {
                tracing::info!(
                    %linked,
                    marked = marked.len(),
                    "linked custody leg: marked rotated-away boxes superseded"
                );
            }
        }
    }
    LinkedCustodyPass {
        nest_id: target.nest_id,
        leg,
    }
}

/// Whether a linked custody run put a row through the writer door — a
/// capture, or a supersession mark.
fn custody_wrote(leg: &LinkedCustodyLeg) -> bool {
    match leg {
        LinkedCustodyLeg::IdentityMismatch { .. } => false,
        LinkedCustodyLeg::Ran(run) => {
            run.custody == DeploymentSeedCustody::Captured
                || matches!(&run.marks, SupersessionMarks::Checked(m) if !m.is_empty())
        }
    }
}

/// This pass's bind check (`pump`'s first nest leg).
struct BindCheck {
    bound: crate::bind_leg::BoundReplica,
    /// Nothing was settled yet: the completed pass adopts `bound`.
    first: bool,
    /// The bound replica is not the settled one: this pass owes the
    /// verification.
    verify: bool,
    /// The verification's own leg (the watermark clear) landed — the
    /// enrollment half is read off the report at the end.
    verified: bool,
}

impl BindCheck {
    fn pass(&self, settled: bool) -> crate::bind_leg::BindPass {
        if self.first {
            crate::bind_leg::BindPass::FirstBind { adopted: settled }
        } else if self.verify {
            crate::bind_leg::BindPass::Verified { settled }
        } else {
            crate::bind_leg::BindPass::Settled
        }
    }
}

/// The bind check's **read-only half**: the replica the bound nest says it is
/// (asked over the owner session) beside the one this store settled. `None`
/// when either could not be read — reported, and asked again next pass.
async fn bound_and_settled<B, R>(
    store: &AccountStore<B>,
    fleet_writer: FleetWriter<'_, R>,
    pinned: &[[u8; 32]],
    report: &mut PumpReport,
) -> Option<(
    crate::bind_leg::BoundReplica,
    Option<crate::bind_leg::BoundReplica>,
)>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    use crate::bind_leg::{BoundReplica, probe_bound_replica, settled_replica};
    let replica = match probe_bound_replica(fleet_writer.session_rpc).await {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("bind leg: the bound replica could not be asked: {e:#}");
            push_step_error(report, format!("bind leg: {e:#}"), &e);
            return None;
        }
    };
    let bound = BoundReplica::new(pinned, replica);
    match settled_replica(store).await {
        Ok(settled) => Some((bound, settled)),
        Err(e) => {
            push_step_error(report, format!("bind leg: settled replica: {e:#}"), &e);
            None
        }
    }
}

/// Ask the bound nest which replica it is and, when it is not the settled one,
/// run the verification's own legs: the watermarks cleared, the holdings check
/// re-armed. The enrollment half is the enrollment step's, handed
/// `latch_void`. Rides the owner session. The rotation chain is read whenever
/// this assembly holds no ancestry — at its first pass, and after a
/// verification forgot it — since recovery needs it whether or not the
/// replica moved.
async fn bind_check<B, R>(
    store: &AccountStore<B>,
    fleet_writer: FleetWriter<'_, R>,
    pinned: &[[u8; 32]],
    report: &mut PumpReport,
) -> Option<BindCheck>
where
    B: StoreBackend,
    R: RpcRequester + Clone,
{
    use crate::bind_leg::{clear_watermarks, fetch_verified_ancestors, needs_verification};
    let (bound, settled) = bound_and_settled(store, fleet_writer, pinned, report).await?;
    let verify = needs_verification(settled.as_ref(), &bound, fleet_writer.bind);
    let mut verified = true;
    // The verification's one-shot legs, once per target: a verification that
    // does not settle (a seedless host waiting for a signed-in app to register)
    // retries only its register probe, never a watermark clear per pass.
    if verify && !fleet_writer.bind.verifying(&bound) {
        tracing::info!(
            "bind leg: bound to a replica other than the settled one — verifying ahead of \
             the other legs (the latch is void, the watermarks are cleared, the holdings are \
             asked again)"
        );
        match clear_watermarks(store).await {
            Ok(()) => fleet_writer.bind.begin(&bound),
            Err(e) => {
                push_step_error(report, format!("bind leg: {e:#}"), &e);
                verified = false;
            }
        }
    }
    // Best-effort, and never what a verification waits on: the ancestry only
    // widens what recovery may ask for, and a nest that cannot serve the chain
    // right now just retries; an account never rotated has an empty chain.
    if fleet_writer.bind.ancestors().is_none() {
        match fetch_verified_ancestors(fleet_writer.session_rpc, pinned).await {
            Ok(a) => fleet_writer.bind.set_ancestors(Some(a)),
            Err(e) => {
                tracing::debug!("bind leg: the rotation chain could not be read (retried): {e:#}");
            }
        }
    }
    Some(BindCheck {
        first: settled.is_none(),
        bound,
        verify,
        verified,
    })
}

pub(crate) fn log_pump(label: &str, report: &PumpReport) {
    if report.cut_by_sign_out {
        // Neither clean nor failed; `contained_pump` already said so.
        return;
    }
    if report.errors.is_empty() {
        tracing::debug!(?report, "account pump ({label}) clean");
    } else {
        tracing::warn!(?report, "account pump ({label}) had failures");
    }
    // The step timings, at info once a pass takes a second — so a sweep's
    // app log names the dominant step of a long prologue without a hand
    // count (`PassTimings`); at debug otherwise, beside the report dump.
    let t = &report.timings;
    let (slowest, slowest_ms) = t
        .slowest()
        .map_or(("none", 0), |(name, d)| (name, d.as_millis()));
    if t.total >= std::time::Duration::from_secs(1) {
        tracing::info!(
            total_ms = t.total.as_millis(),
            walk_ms = t.walk.as_millis(),
            fleet_walk_ms = t.fleet_walk.as_millis(),
            device_endpoints_ms = t.device_endpoints.as_millis(),
            escrow_recovery_ms = t.escrow_recovery.as_millis(),
            reescrow_ms = t.reescrow.as_millis(),
            topup_ms = t.topup.as_millis(),
            unkeyable_ms = t.unkeyable.as_millis(),
            reclaim_ms = t.reclaim.as_millis(),
            slowest,
            slowest_ms,
            "account pump ({label}): step timings"
        );
    } else {
        tracing::debug!(
            total_ms = t.total.as_millis(),
            slowest,
            slowest_ms,
            "account pump ({label}): step timings"
        );
    }
}
