//! The **plane custody leg** for deployment seeds, and the plane form of the
//! rotation drive — both over the [`DeploymentSeedStore`] seam.
//!
//! Authority: `docs/goal/architecture/nest/box-recovery.md` § The plane-era
//! recovery floor → *(c) The writes*, and § Deployment-seed rotation → *The
//! ceremony* and *Custody after rotation*.
//!
//! **Every owed write is a reconcile.** Nothing here is parked and nothing
//! consumes the claim reply's seed: the box serves the same ground truth to
//! its admin for as long as it lives, so capture is re-derived at every edge
//! the host runs the leg on, and so is the supersession mark (from the box's
//! own verified rotation chain). A leg that fails leaves nothing but a retry
//! at the next edge.
//!
//! **The connection and its bound id are parameters.** The leg runs over the
//! bound nest's connection, and — box-recovery.md § (c), the linked-nest
//! paragraph — over each linked nest's connection too, against *that*
//! connection's bound id. So neither function resolves an id itself: the host
//! passes the identity the connection is bound to
//! (`LinkedNestsMachine::bound_nest_id` for the bound nest), never the box's
//! own `fauna.nest.info` claim. [`run_linked_deployment_seed_custody_leg`] is
//! the linked arm's entry: the same leg behind the channel-binding check a
//! linked connection owes (its bound identity must be the pairing row's).
//!
//! **The store is the [`CustodySeedFold`] seam** — the fold and its join, with
//! no `Send` bound on their futures. A host hands a [`DeploymentSeedStore`]
//! (the account-store handle, whose futures are `Send`), which the leg reads
//! through [`StoreFold`]; the account runtime's own pass, which runs the
//! linked arm on the store thread over the connection it already holds,
//! implements the seam over the open store directly.
//!
//! What is deliberately **not** here: a host mapping. Each host maps the
//! custody outcome onto its own warning surface; the outcomes
//! below say whether a run ended with custody unconfirmed
//! ([`DeploymentSeedCustody::unconfirmed`]) and whether the next edge will
//! retry it ([`DeploymentSeedCustody::retryable`]), and each host maps that
//! onto the surface it has.

use core::time::Duration;

use fauna_core::data::DeploymentSeedEntry;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::secret::SecretString;
use fauna_protocol::admin::{AdminDeploymentSeedRotateReply, AdminDeploymentSeedRotateRequest};
use fauna_protocol::nest_rotation::{
    ROTATION_CHAIN_KIND, RotationChainReply, RotationChainRequest, SignedNestRotation, verify_chain,
};
use fauna_protocol::{RpcErrorClass, RpcRequester};

use crate::rotate::{MAX_ROTATION_ATTEMPTS, SeedRotation};
use crate::store_seam::{DeploymentSeedStore, StoreError};

/// Max total attempts the custody leg makes when a **transient** transport
/// fault interrupts one step of the deployment-seed hand-off: a transient WS failure at the custody moment must
/// not silently lose off-box recovery custody. Each attempt's WS-RPC call
/// already blocks for reconnect up to the kind's per-request deadline (~30s,
/// `NestClient::request`), so a *bounded immediate* retry re-arms that wait
/// without an in-loop sleep — which keeps the path wasm-clean (no cross-platform
/// timer dep). Three attempts span ~90s of reconnect tolerance.
pub(crate) const MAX_CAPTURE_ATTEMPTS: usize = 3;

/// Best-effort resolve of the connected box's own **handle domain** via
/// `fauna.nest.info`, for the deployment-seed custody label + the cloud
/// re-provision DNS zone ([`fauna_core::data::DeploymentSeedEntry::domain`]).
/// The box's domain need not equal the admin's handle domain in the multi-nest
/// case, so it is read from the box itself rather than derived. Returns `None`
/// on any transport error or the `"unknown"`/empty unset sentinel
/// (`NestInfoReply::domain` doc) — a domainless box. The seed custody must never
/// fail over a missing label (the seed is irreplaceable; the domain a hint), so
/// every error path collapses to `None`.
pub(crate) async fn resolve_box_domain<R: fauna_protocol::RpcRequester>(
    nest: &R,
) -> Option<String> {
    let reply: fauna_protocol::discovery::NestInfoReply = nest
        .request(
            "fauna.nest.info",
            fauna_protocol::discovery::NestInfoRequest::default(),
        )
        .await
        .ok()?;
    match reply.domain.trim() {
        "" | "unknown" => None,
        d => Some(d.to_string()),
    }
}

/// How long [`rotate_deployment_seed_on_plane`] waits for the successor's
/// custody row to reach the bound nest before it gives up **without
/// dispatching**. The account runtime ships a local write at its next publish
/// step, which is normally well under a second; a box that has not received
/// the row after this long is one the drive must not flip.
pub const SEED_PUBLISH_WAIT: Duration = Duration::from_secs(30);

/// The poll step of [`SEED_PUBLISH_WAIT`].
const SEED_PUBLISH_POLL: Duration = Duration::from_millis(100);

/// How the capture half of [`run_deployment_seed_custody_leg`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeploymentSeedCustody {
    /// The fold already held a live entry for the bound nest — no round trip
    /// at all. The steady state on every edge after the first.
    AlreadyCustodied,
    /// Not a roster admin here, or the admin check itself failed — fail
    /// closed (`admin.md` § Don't do these). No custody is owed.
    NotAdmin,
    /// An admin, but the seed fetch failed after its bounded transient retry.
    /// Retried at the next edge.
    HandoffUnavailable,
    /// An admin, and the fetch answered, but the box holds no deployment key
    /// to hand over — not expected post-boot.
    NestHoldsNoSeed,
    /// The fetched seed does not derive to the bound id (BR-2) — refused
    /// before any write. A nest integrity fault or wire corruption.
    RefusedMismatch,
    /// The box's entry was merged into the plane.
    Captured,
    /// The account store refused the read or the merge — most often the
    /// door's transient refusal while no generation tip resolves yet, or a
    /// store not up. Retried at the next edge.
    StoreRefused(String),
}

impl DeploymentSeedCustody {
    /// Whether the run ended with custody **unconfirmed for an admin** of this
    /// connection's nest — the case box-recovery.md § (c) requires a
    /// user-visible warning for. `false` for the two outcomes where nothing
    /// is owed (custody held; not an admin).
    #[must_use]
    pub fn unconfirmed(&self) -> bool {
        !matches!(
            self,
            Self::AlreadyCustodied | Self::Captured | Self::NotAdmin
        )
    }

    /// Whether the next run of the leg can land what this one did not — a
    /// transient refusal, as against a verdict the box will repeat.
    #[must_use]
    pub fn retryable(&self) -> bool {
        matches!(self, Self::HandoffUnavailable | Self::StoreRefused(_))
    }
}

/// The user-visible warning a custody-leg run owes the admin, or `None` when
/// nothing is owed (custody held, captured, or not an admin here) —
/// `box-recovery.md` § The plane-era recovery floor, *(c)*: a run that ends
/// with custody unconfirmed for an admin of the bound nest warns. Every host
/// renders this one text onto its own existing warning surface, so the wording
/// cannot drift per app.
#[must_use]
pub fn custody_leg_warning(custody: &DeploymentSeedCustody) -> Option<String> {
    match custody {
        DeploymentSeedCustody::AlreadyCustodied
        | DeploymentSeedCustody::Captured
        | DeploymentSeedCustody::NotAdmin => None,
        DeploymentSeedCustody::RefusedMismatch => Some(
            "the nest handed off an inconsistent recovery key — total-box-loss recovery is \
             not protected"
                .to_string(),
        ),
        DeploymentSeedCustody::NestHoldsNoSeed => Some(
            "this nest holds no recovery key to save — total-box-loss recovery is not \
             protected"
                .to_string(),
        ),
        DeploymentSeedCustody::HandoffUnavailable => Some(
            "total-box-loss recovery custody not yet saved: the nest could not be asked for \
             its recovery key; retrying on the next connect"
                .to_string(),
        ),
        DeploymentSeedCustody::StoreRefused(detail) => Some(format!(
            "total-box-loss recovery custody not yet saved: {detail}; retrying on the next \
             connect"
        )),
    }
}

/// How the mark half of [`run_deployment_seed_custody_leg`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupersessionMarks {
    /// The fold holds no live entry for any box but the bound one — nothing
    /// could be an ancestor, so the chain was not fetched.
    NothingToCheck,
    /// The bound nest's rotation chain could not be fetched. Retried at the
    /// next edge.
    ChainUnavailable,
    /// The chain was checked; these entries (possibly none) were marked
    /// superseded, each by its own successor on the verified chain.
    Checked(Vec<[u8; 32]>),
    /// The account store refused the read or the merge. Retried at the next
    /// edge.
    StoreRefused(String),
}

/// What one run of the leg did — the capture, then the mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustodyLegReport {
    pub custody: DeploymentSeedCustody,
    pub marks: SupersessionMarks,
}

/// The two store operations the custody leg runs: the custody fold, and the
/// join of entries into it. [`DeploymentSeedStore`] without the `Send` bound
/// its futures carry natively, so the leg also runs where the store is driven
/// in place (the account runtime's pass, on the store thread). A refusal is
/// its text — the leg only ever reports it
/// ([`DeploymentSeedCustody::StoreRefused`]).
pub trait CustodySeedFold {
    /// The account's custody map as it now reads, superseded entries included.
    fn custody_fold(
        &self,
    ) -> impl core::future::Future<Output = Result<Vec<DeploymentSeedEntry>, String>>;
    /// Join `replica` into the stored rows.
    fn custody_merge(
        &self,
        replica: Vec<DeploymentSeedEntry>,
    ) -> impl core::future::Future<Output = Result<(), String>>;
}

/// A [`DeploymentSeedStore`] as the leg's [`CustodySeedFold`].
pub struct StoreFold<'a, S: ?Sized>(pub &'a S);

impl<S: DeploymentSeedStore + ?Sized> CustodySeedFold for StoreFold<'_, S> {
    async fn custody_fold(&self) -> Result<Vec<DeploymentSeedEntry>, String> {
        self.0.seeds().await.map_err(|e| e.to_string())
    }

    async fn custody_merge(&self, replica: Vec<DeploymentSeedEntry>) -> Result<(), String> {
        self.0
            .merge_seeds(replica)
            .await
            .map(drop)
            .map_err(|e| e.to_string())
    }
}

/// How the custody leg ended at one **linked** nest
/// ([`run_linked_deployment_seed_custody_leg`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkedCustodyLeg {
    /// The connection is bound to another identity than the pairing row
    /// names: nothing was asked of it.
    IdentityMismatch { presented: [u8; 32] },
    /// The leg ran against the linked nest's own id.
    Ran(CustodyLegReport),
}

/// **The custody leg over a linked nest's connection** (`box-recovery.md`
/// § The plane-era recovery floor, *(c)*, the linked-connection paragraph):
/// [`run_deployment_seed_custody_leg_over`] against `paired_nest_id` — the admin
/// check there, the fetch there, the seed→id refusal against that id, the
/// mark reconcile likewise — so an admin who links their boxes custodies each
/// of them without binding a device to it.
///
/// `connection_bound_id` is the identity `linked` is bound to, as the host's
/// connector read it off the connection; it must be the pairing row's
/// `paired_nest_id`, and a connection presenting another is asked nothing.
pub async fn run_linked_deployment_seed_custody_leg<R, S>(
    linked: &R,
    connection_bound_id: ActorId,
    paired_nest_id: ActorId,
    store: &S,
) -> LinkedCustodyLeg
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
    S: CustodySeedFold + ?Sized,
{
    if connection_bound_id != paired_nest_id {
        return LinkedCustodyLeg::IdentityMismatch {
            presented: connection_bound_id.0,
        };
    }
    LinkedCustodyLeg::Ran(run_deployment_seed_custody_leg_over(linked, paired_nest_id, store).await)
}

/// **The custody leg** — capture the bound box's seed if the plane lacks it,
/// then re-derive every supersession mark the box's rotation chain proves.
///
/// Run over `nest`, a connection authenticated as the account's identity,
/// whose bound identity is `bound_nest_id` (see the module docs: a parameter,
/// never resolved here). Safe to run at every edge: the steady state makes
/// no round trip for the capture, and fetches the chain only when the fold
/// holds a live entry for some other box.
///
/// **Capture.** A live entry for `bound_nest_id` in the fold → done.
/// Otherwise `fauna.account.am_i_admin` (fail closed) →
/// `fauna.admin.deployment_seed.get` (a transport fault retried up to
/// [`MAX_CAPTURE_ATTEMPTS`], no in-loop sleep) → refuse a seed that does not
/// derive to `bound_nest_id` → resolve the box's domain → merge the entry.
///
/// **Mark.** For every live entry other than the bound one: when the bound
/// nest's chain (`fauna.auth.rotation_chain`) verifies from that entry's id
/// to `bound_nest_id` ([`verify_chain`]), the entry is an ancestor of this
/// box and is merged back marked `superseded_by` its successor on that
/// chain. An entry the chain does not reach is another box and is left
/// alone; a chain that does not verify marks nothing.
pub async fn run_deployment_seed_custody_leg<R, S>(
    nest: &R,
    bound_nest_id: ActorId,
    store: &S,
) -> CustodyLegReport
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
    S: DeploymentSeedStore + ?Sized,
{
    run_deployment_seed_custody_leg_over(nest, bound_nest_id, &StoreFold(store)).await
}

/// [`run_deployment_seed_custody_leg`] over the [`CustodySeedFold`] seam.
pub async fn run_deployment_seed_custody_leg_over<R, S>(
    nest: &R,
    bound_nest_id: ActorId,
    store: &S,
) -> CustodyLegReport
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
    S: CustodySeedFold + ?Sized,
{
    let custody = capture(nest, bound_nest_id, store).await;
    let marks = mark_ancestors(nest, bound_nest_id, store).await;
    CustodyLegReport { custody, marks }
}

fn is_live_for(entry: &DeploymentSeedEntry, id: &[u8; 32]) -> bool {
    &entry.nest_actor_id == id && entry.superseded_by.is_none()
}

async fn capture<R, S>(nest: &R, bound_nest_id: ActorId, store: &S) -> DeploymentSeedCustody
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
    S: CustodySeedFold + ?Sized,
{
    // The fold first: the steady state (custody held) makes no round trip. A
    // fold the store refused is owed only to an admin, so the admin check
    // decides whether that refusal is reported at all — a non-admin is never
    // warned about custody it does not owe.
    let refused = match store.custody_fold().await {
        Ok(fold) if fold.iter().any(|e| is_live_for(e, &bound_nest_id.0)) => {
            return DeploymentSeedCustody::AlreadyCustodied;
        }
        Ok(_) => None,
        Err(e) => Some(e),
    };
    let is_admin = nest
        .request::<_, fauna_protocol::account::AmIAdminReply>(
            "fauna.account.am_i_admin",
            fauna_protocol::account::AmIAdminRequest {
                extra: Default::default(),
            },
        )
        .await
        .is_ok_and(|r| r.admin);
    if !is_admin {
        return DeploymentSeedCustody::NotAdmin;
    }
    if let Some(why) = refused {
        return DeploymentSeedCustody::StoreRefused(why);
    }
    let fetched = retry_transport(MAX_CAPTURE_ATTEMPTS, || {
        nest.request::<_, fauna_protocol::admin::AdminDeploymentSeedGetReply>(
            "fauna.admin.deployment_seed.get",
            fauna_protocol::admin::AdminDeploymentSeedGetRequest {
                extra: Default::default(),
            },
        )
    })
    .await;
    let Ok(reply) = fetched else {
        return DeploymentSeedCustody::HandoffUnavailable;
    };
    let Some(seed_hex) = reply.deployment_seed else {
        return DeploymentSeedCustody::NestHoldsNoSeed;
    };
    // BR-2: the seed must be the preimage of the identity this connection is
    // bound to. A malformed seed derives to nothing, so it is the same refusal.
    let Ok(seed) = fauna_core::hex32::decode(seed_hex.as_str()) else {
        return DeploymentSeedCustody::RefusedMismatch;
    };
    if ActorKeypair::from_secret(seed).actor_id() != bound_nest_id {
        return DeploymentSeedCustody::RefusedMismatch;
    }
    let entry = DeploymentSeedEntry {
        nest_actor_id: bound_nest_id.0,
        seed: seed.into(),
        domain: resolve_box_domain(nest).await,
        ..Default::default()
    };
    match store.custody_merge(vec![entry]).await {
        Ok(()) => DeploymentSeedCustody::Captured,
        Err(e) => DeploymentSeedCustody::StoreRefused(e),
    }
}

async fn mark_ancestors<R, S>(nest: &R, bound_nest_id: ActorId, store: &S) -> SupersessionMarks
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
    S: CustodySeedFold + ?Sized,
{
    let fold = match store.custody_fold().await {
        Ok(fold) => fold,
        Err(e) => return SupersessionMarks::StoreRefused(e),
    };
    let candidates: Vec<&DeploymentSeedEntry> = fold
        .iter()
        .filter(|e| e.superseded_by.is_none() && e.nest_actor_id != bound_nest_id.0)
        .collect();
    if candidates.is_empty() {
        return SupersessionMarks::NothingToCheck;
    }
    let Ok(reply) = retry_transport(MAX_CAPTURE_ATTEMPTS, || {
        nest.request::<_, RotationChainReply>(ROTATION_CHAIN_KIND, RotationChainRequest::default())
    })
    .await
    else {
        return SupersessionMarks::ChainUnavailable;
    };
    let marked: Vec<DeploymentSeedEntry> = candidates
        .into_iter()
        .filter_map(|e| {
            let successor =
                successor_on_verified_chain(&reply.chain, &e.nest_actor_id, &bound_nest_id.0)?;
            Some(DeploymentSeedEntry {
                superseded_by: Some(successor),
                ..e.clone()
            })
        })
        .collect();
    if marked.is_empty() {
        return SupersessionMarks::Checked(Vec::new());
    }
    let ids = marked.iter().map(|e| e.nest_actor_id).collect();
    match store.custody_merge(marked).await {
        Ok(()) => SupersessionMarks::Checked(ids),
        Err(e) => SupersessionMarks::StoreRefused(e),
    }
}

/// `ancestor`'s immediate successor, when `chain` verifies from `ancestor` to
/// `head`; `None` otherwise (another box, or a chain that does not verify).
fn successor_on_verified_chain(
    chain: &[SignedNestRotation],
    ancestor: &[u8; 32],
    head: &[u8; 32],
) -> Option<[u8; 32]> {
    verify_chain(chain, ancestor, head).ok()?;
    chain
        .iter()
        .find(|hop| &hop.statement.old_nest_actor_id == ancestor)
        .map(|hop| hop.statement.new_nest_actor_id)
}

/// Why [`rotate_deployment_seed_on_plane`] refused **before** dispatching —
/// in every arm the box was not asked to rotate.
#[derive(Debug)]
pub enum PlaneSeedRotationError<E> {
    /// No account-store handle: the plane custody the ceremony must precede
    /// does not exist in this session.
    NoAccountStore,
    /// The account store refused the successor's custody merge (or the read
    /// of it) — most often the door's refusal while no generation tip
    /// resolves.
    CustodyRefused(StoreError),
    /// The successor's row did not reach the bound nest within
    /// [`SEED_PUBLISH_WAIT`].
    NotPublished,
    /// The dispatch failed terminally, or kept failing transiently past its
    /// bound. The successor stays custodied — an orphan entry, harmless.
    Transport(E),
}

impl<E: core::fmt::Display> core::fmt::Display for PlaneSeedRotationError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoAccountStore => {
                write!(
                    f,
                    "seed rotation needs the account store, which is not ready"
                )
            }
            Self::CustodyRefused(e) => write!(f, "seed rotation: successor custody refused: {e}"),
            Self::NotPublished => write!(
                f,
                "seed rotation: the successor's custody did not reach the box in time"
            ),
            Self::Transport(e) => write!(f, "seed rotation: dispatch failed: {e}"),
        }
    }
}

impl<E: core::fmt::Display + core::fmt::Debug> std::error::Error for PlaneSeedRotationError<E> {}

/// **The plane rotation drive** — the deployment-seed rotation ceremony over
/// the [`DeploymentSeedStore`] seam (the account plane's
/// `fauna.state.deployment-seeds`), the only rotation drive there is.
///
/// `bound_nest_id` is the identity `nest` is bound to (a parameter, as for
/// the leg); it is the entry this rotation supersedes. Five steps, in the
/// order box-recovery.md § The ceremony fixes:
///
/// 1. **Mint** the successor seed client-side.
/// 2. **Merge** its custody entry, carrying the predecessor's domain label
///    forward (or asking the box while it still answers on the old identity).
/// 3. **Wait until that row is published to the bound nest** — polled through
///    [`DeploymentSeedStore::seed_published`] for up to [`SEED_PUBLISH_WAIT`].
///    Custody the box itself holds is what survives the admin's device; a row
///    still local-only is custody nobody else has.
/// 4. **Dispatch** `fauna.admin.deployment_seed.rotate`, the same minted seed
///    on every transient retry (a landed-but-unacked rotation acks
///    idempotently).
/// 5. **Mark** the predecessor `superseded_by` the successor.
///
/// No handle, a refused merge, or a row not published in time aborts BEFORE
/// step 4. A mark that fails after the dispatch returns
/// `predecessor_marked: false` — and is no longer final: the custody leg's
/// mark reconcile re-derives it from the verified chain at the next edge.
pub async fn rotate_deployment_seed_on_plane<R, S>(
    nest: &R,
    bound_nest_id: ActorId,
    store: Option<&S>,
) -> Result<SeedRotation, PlaneSeedRotationError<R::Error>>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
    S: DeploymentSeedStore + ?Sized,
{
    let Some(store) = store else {
        return Err(PlaneSeedRotationError::NoAccountStore);
    };

    // Step 1 — mint.
    let successor_kp = ActorKeypair::generate();
    let successor_seed = *successor_kp.secret_bytes();
    let successor_id = successor_kp.actor_id().0;

    // Step 2 — custody, merged.
    let fold = store
        .seeds()
        .await
        .map_err(PlaneSeedRotationError::CustodyRefused)?;
    let predecessor = fold
        .iter()
        .find(|e| e.nest_actor_id == bound_nest_id.0)
        .cloned();
    let domain = match predecessor.as_ref().and_then(|e| e.domain.clone()) {
        Some(d) => Some(d),
        None => resolve_box_domain(nest).await,
    };
    store
        .merge_seeds(vec![DeploymentSeedEntry {
            nest_actor_id: successor_id,
            seed: successor_seed.into(),
            domain,
            ..Default::default()
        }])
        .await
        .map_err(PlaneSeedRotationError::CustodyRefused)?;

    // Step 3 — the row must be on the box before the box is asked to flip.
    wait_published(store, successor_id).await?;

    // Step 4 — dispatch.
    let request = AdminDeploymentSeedRotateRequest {
        new_seed: SecretString::new(fauna_core::hex32::encode(&successor_seed)),
        ..Default::default()
    };
    let reply: AdminDeploymentSeedRotateReply = retry_transport(MAX_ROTATION_ATTEMPTS, || {
        nest.request("fauna.admin.deployment_seed.rotate", request.clone())
    })
    .await
    .map_err(PlaneSeedRotationError::Transport)?;
    let presented: [u8; 32] = match reply.nest_actor_id.as_ref().try_into() {
        Ok(id) => id,
        Err(_) => return Ok(SeedRotation::RefusedIdentityMismatch { presented: [0; 32] }),
    };
    if presented != successor_id {
        return Ok(SeedRotation::RefusedIdentityMismatch { presented });
    }

    // Step 5 — mark. Nothing custodied for the predecessor leaves nothing
    // stale in the recovery list, so nothing is owed.
    let predecessor_marked = match predecessor {
        None => true,
        Some(entry) => store
            .merge_seeds(vec![DeploymentSeedEntry {
                superseded_by: Some(successor_id),
                ..entry
            }])
            .await
            .is_ok(),
    };

    Ok(SeedRotation::Rotated {
        nest_actor_id: successor_id,
        seq: reply.seq,
        already_rotated: reply.already_rotated,
        predecessor_marked,
    })
}

/// Poll until `id`'s row is published, for up to [`SEED_PUBLISH_WAIT`].
async fn wait_published<S, E>(store: &S, id: [u8; 32]) -> Result<(), PlaneSeedRotationError<E>>
where
    S: DeploymentSeedStore + ?Sized,
{
    let polls = SEED_PUBLISH_WAIT.as_millis() / SEED_PUBLISH_POLL.as_millis();
    for poll in 0..=polls {
        if store
            .seed_published(id)
            .await
            .map_err(PlaneSeedRotationError::CustodyRefused)?
        {
            return Ok(());
        }
        if poll < polls {
            fauna_sleep::sleep(SEED_PUBLISH_POLL).await;
        }
    }
    Err(PlaneSeedRotationError::NotPublished)
}

/// Run `call` until it answers or fails other than by a transport fault,
/// re-attempting a transport fault up to `attempts` times. No in-loop sleep:
/// each attempt already blocks for reconnect up to the kind's deadline.
async fn retry_transport<T, E, F, Fut>(attempts: usize, mut call: F) -> Result<T, E>
where
    E: RpcErrorClass,
    F: FnMut() -> Fut,
    Fut: core::future::Future<Output = Result<T, E>>,
{
    let mut last = None;
    for _ in 0..attempts.max(1) {
        match call().await {
            Ok(v) => return Ok(v),
            Err(e) if !e.is_rejection() => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.expect("at least one attempt ran"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::{FakeDeploymentSeedStore, SeedPublish};
    use crate::test_nest::{FakeConfigNest, block_on};
    use fauna_protocol::nest_rotation::NestRotation;

    const ROTATE_KIND: &str = "fauna.admin.deployment_seed.rotate";

    fn kp(b: u8) -> ActorKeypair {
        ActorKeypair::from_secret([b; 32])
    }

    fn entry(k: &ActorKeypair) -> DeploymentSeedEntry {
        DeploymentSeedEntry {
            nest_actor_id: k.actor_id().0,
            seed: (*k.secret_bytes()).into(),
            ..Default::default()
        }
    }

    fn hop(old: &ActorKeypair, new: &ActorKeypair, seq: u64) -> SignedNestRotation {
        NestRotation {
            old_nest_actor_id: old.actor_id().0,
            new_nest_actor_id: new.actor_id().0,
            seq,
            rotated_at: 1_800_000_000,
        }
        .sign(old.signing_key(), new.signing_key())
        .expect("sign hop")
    }

    fn held(store: &FakeDeploymentSeedStore, k: &ActorKeypair) -> Option<DeploymentSeedEntry> {
        store
            .current()
            .into_iter()
            .find(|e| e.nest_actor_id == k.actor_id().0)
    }

    /// An admin nest handing off `seed_of`'s seed.
    fn admin_nest(seed_of: &ActorKeypair) -> FakeConfigNest {
        let nest = FakeConfigNest::default();
        *nest.is_admin.lock().unwrap() = true;
        *nest.handoff_seed.lock().unwrap() =
            Some(fauna_core::hex32::encode(seed_of.secret_bytes()));
        nest
    }

    // ── The warning ────────────────────────────────────────────────────────

    /// Silent exactly where nothing is owed; every unconfirmed outcome warns,
    /// in the one shared wording — `unconfirmed()` and the warning agree.
    #[test]
    fn the_warning_is_owed_exactly_when_custody_is_unconfirmed() {
        let every = [
            DeploymentSeedCustody::AlreadyCustodied,
            DeploymentSeedCustody::NotAdmin,
            DeploymentSeedCustody::HandoffUnavailable,
            DeploymentSeedCustody::NestHoldsNoSeed,
            DeploymentSeedCustody::RefusedMismatch,
            DeploymentSeedCustody::Captured,
            DeploymentSeedCustody::StoreRefused("no tip".into()),
        ];
        for outcome in every {
            assert_eq!(
                custody_leg_warning(&outcome).is_some(),
                outcome.unconfirmed(),
                "{outcome:?}"
            );
        }
        let refused = custody_leg_warning(&DeploymentSeedCustody::StoreRefused("no tip".into()))
            .expect("warns");
        assert!(refused.contains("no tip"), "carries the detail: {refused}");
    }

    /// A host that builds its machines before the store is up holds the
    /// per-call resolver; once the handle resolves, the leg runs through it.
    #[test]
    fn the_leg_runs_through_the_per_call_resolver() {
        let bound = kp(9);
        let nest = admin_nest(&bound);
        let store = FakeDeploymentSeedStore::empty();
        let resolved = store.clone();
        let resolver = crate::ResolvingLedgerStore::new(move || Some(resolved.clone()));

        let report = block_on(run_deployment_seed_custody_leg(
            &nest,
            bound.actor_id(),
            &resolver,
        ));

        assert_eq!(report.custody, DeploymentSeedCustody::Captured);
        assert!(held(&store, &bound).is_some());
    }

    // ── The capture ────────────────────────────────────────────────────────

    /// A store that cannot be read owes nothing to a non-admin: the admin
    /// check decides whether the refusal is reported, so a non-admin is never
    /// warned (box-recovery.md § The plane-era recovery floor, (c) — the
    /// warning is for an admin of the bound nest). An admin gets the refusal.
    #[test]
    fn an_unreadable_store_warns_an_admin_and_never_a_non_admin() {
        let bound = kp(4);
        let store = FakeDeploymentSeedStore::empty();
        store.refuse_reads();

        let non_admin = admin_nest(&bound);
        *non_admin.is_admin.lock().unwrap() = false;
        let report = block_on(run_deployment_seed_custody_leg(
            &non_admin,
            bound.actor_id(),
            &store,
        ));
        assert_eq!(report.custody, DeploymentSeedCustody::NotAdmin);
        assert!(custody_leg_warning(&report.custody).is_none());

        let admin = admin_nest(&bound);
        let report = block_on(run_deployment_seed_custody_leg(
            &admin,
            bound.actor_id(),
            &store,
        ));
        assert!(matches!(
            report.custody,
            DeploymentSeedCustody::StoreRefused(_)
        ));
        assert!(
            !admin
                .calls
                .lock()
                .unwrap()
                .contains(&"fauna.admin.deployment_seed.get"),
            "nothing is fetched while the store cannot take the merge"
        );
    }

    /// The steady state: custody held for the bound box → not one RPC, not
    /// even the chain fetch (no other box to check).
    #[test]
    fn a_held_custody_makes_no_round_trip_at_all() {
        let bound = kp(1);
        let nest = admin_nest(&bound);
        let store = FakeDeploymentSeedStore::with(vec![entry(&bound)]);

        let report = block_on(run_deployment_seed_custody_leg(
            &nest,
            bound.actor_id(),
            &store,
        ));

        assert_eq!(report.custody, DeploymentSeedCustody::AlreadyCustodied);
        assert_eq!(report.marks, SupersessionMarks::NothingToCheck);
        assert!(nest.calls.lock().unwrap().is_empty(), "no RPC when held");
        assert_eq!(store.merges(), 0);
    }

    #[test]
    fn an_admin_with_no_custody_captures_the_bound_box() {
        let bound = kp(2);
        let nest = admin_nest(&bound);
        let store = FakeDeploymentSeedStore::empty();

        let report = block_on(run_deployment_seed_custody_leg(
            &nest,
            bound.actor_id(),
            &store,
        ));

        assert_eq!(report.custody, DeploymentSeedCustody::Captured);
        assert!(!report.custody.unconfirmed());
        let got = held(&store, &bound).expect("the box's row landed");
        assert_eq!(got.seed.to_array(), *bound.secret_bytes());
        assert_eq!(got.domain.as_deref(), Some("box.example"));
        assert_eq!(got.superseded_by, None);
        let calls = nest.calls.lock().unwrap().clone();
        assert_eq!(
            calls,
            vec![
                "fauna.account.am_i_admin",
                "fauna.admin.deployment_seed.get",
                "fauna.nest.info",
            ],
            "admin check, fetch, domain — and no chain fetch with no other box held"
        );
    }

    #[test]
    fn a_non_admin_captures_nothing_and_never_fetches() {
        let bound = kp(3);
        let nest = admin_nest(&bound);
        *nest.is_admin.lock().unwrap() = false;
        let store = FakeDeploymentSeedStore::empty();

        let report = block_on(run_deployment_seed_custody_leg(
            &nest,
            bound.actor_id(),
            &store,
        ));

        assert_eq!(report.custody, DeploymentSeedCustody::NotAdmin);
        assert!(!report.custody.unconfirmed(), "nothing is owed a non-admin");
        assert!(store.current().is_empty());
        assert!(
            !nest
                .calls
                .lock()
                .unwrap()
                .contains(&"fauna.admin.deployment_seed.get")
        );
    }

    /// BR-2: a seed that derives to another identity is refused before any
    /// write.
    #[test]
    fn a_seed_that_does_not_derive_to_the_bound_id_writes_nothing() {
        let bound = kp(4);
        let other = kp(5);
        let nest = admin_nest(&other);
        let store = FakeDeploymentSeedStore::empty();

        let report = block_on(run_deployment_seed_custody_leg(
            &nest,
            bound.actor_id(),
            &store,
        ));

        assert_eq!(report.custody, DeploymentSeedCustody::RefusedMismatch);
        assert!(report.custody.unconfirmed());
        assert!(!report.custody.retryable());
        assert_eq!(store.merges(), 0);
        assert!(store.current().is_empty());
    }

    /// The door refuses while no generation tip resolves: the run reports a
    /// retryable, unconfirmed custody, and the next run lands it.
    #[test]
    fn a_transient_door_refusal_is_retryable_and_the_next_run_lands() {
        let bound = kp(6);
        let nest = admin_nest(&bound);
        let store = FakeDeploymentSeedStore::empty();
        store.refuse_next_merges(1);

        let first = block_on(run_deployment_seed_custody_leg(
            &nest,
            bound.actor_id(),
            &store,
        ));
        assert!(
            matches!(first.custody, DeploymentSeedCustody::StoreRefused(_)),
            "{first:?}"
        );
        assert!(first.custody.retryable());
        assert!(first.custody.unconfirmed());
        assert!(store.current().is_empty());

        let second = block_on(run_deployment_seed_custody_leg(
            &nest,
            bound.actor_id(),
            &store,
        ));
        assert_eq!(second.custody, DeploymentSeedCustody::Captured);
        assert!(held(&store, &bound).is_some());
    }

    // ── The linked arm ─────────────────────────────────────────────────────

    /// The device is bound to box A and custodies it; the same leg over the
    /// connection to linked box B — where this identity is an admin too —
    /// captures B beside A, against B's own id, with B's own domain. Neither
    /// box is an ancestor of the other, so nothing is marked.
    #[test]
    fn a_linked_nest_where_the_caller_is_admin_is_captured_beside_the_bound_box() {
        let (a, b) = (kp(40), kp(41));
        let store = FakeDeploymentSeedStore::with(vec![entry(&a)]);
        let linked = admin_nest(&b);

        let leg = block_on(run_linked_deployment_seed_custody_leg(
            &linked,
            b.actor_id(),
            b.actor_id(),
            &StoreFold(&store),
        ));

        let LinkedCustodyLeg::Ran(report) = leg else {
            panic!("the leg ran: {leg:?}");
        };
        assert_eq!(report.custody, DeploymentSeedCustody::Captured);
        assert_eq!(report.marks, SupersessionMarks::Checked(Vec::new()));
        let got = held(&store, &b).expect("the linked box's row landed");
        assert_eq!(got.seed.to_array(), *b.secret_bytes());
        assert_eq!(got.domain.as_deref(), Some("box.example"));
        assert_eq!(got.superseded_by, None);
        assert_eq!(
            held(&store, &a).expect("the bound box stays").superseded_by,
            None,
            "another box is never marked by a linked nest's chain"
        );
    }

    /// The steady state at a linked nest: its row held → no capture round
    /// trip; the one request is the chain fetch the other box's entry owes.
    #[test]
    fn a_linked_nest_already_custodied_is_not_fetched_again() {
        let (a, b) = (kp(42), kp(43));
        let store = FakeDeploymentSeedStore::with(vec![entry(&a), entry(&b)]);
        let linked = admin_nest(&b);

        let leg = block_on(run_linked_deployment_seed_custody_leg(
            &linked,
            b.actor_id(),
            b.actor_id(),
            &StoreFold(&store),
        ));

        let LinkedCustodyLeg::Ran(report) = leg else {
            panic!("the leg ran: {leg:?}");
        };
        assert_eq!(report.custody, DeploymentSeedCustody::AlreadyCustodied);
        assert_eq!(
            linked.calls.lock().unwrap().clone(),
            vec![ROTATION_CHAIN_KIND]
        );
        assert_eq!(store.merges(), 0);
    }

    /// A user's linked nest is usually one they do not administer: the admin
    /// check there fails closed, nothing is fetched and nothing is written.
    #[test]
    fn a_linked_nest_where_the_caller_is_not_admin_captures_nothing() {
        let (a, b) = (kp(44), kp(45));
        let store = FakeDeploymentSeedStore::with(vec![entry(&a)]);
        let linked = admin_nest(&b);
        *linked.is_admin.lock().unwrap() = false;

        let leg = block_on(run_linked_deployment_seed_custody_leg(
            &linked,
            b.actor_id(),
            b.actor_id(),
            &StoreFold(&store),
        ));

        let LinkedCustodyLeg::Ran(report) = leg else {
            panic!("the leg ran: {leg:?}");
        };
        assert_eq!(report.custody, DeploymentSeedCustody::NotAdmin);
        assert!(held(&store, &b).is_none());
        assert_eq!(store.merges(), 0);
        assert!(
            !linked
                .calls
                .lock()
                .unwrap()
                .contains(&"fauna.admin.deployment_seed.get")
        );
    }

    /// BR-2 at a linked nest: the seed→id refusal is against **that
    /// connection's** id. A linked nest handing off a seed that derives to
    /// another identity — the bound box's included — writes nothing.
    #[test]
    fn a_linked_nest_whose_seed_does_not_derive_to_its_connections_id_writes_nothing() {
        let (a, b) = (kp(46), kp(47));
        let store = FakeDeploymentSeedStore::with(vec![entry(&a)]);
        let linked = admin_nest(&a); // hands off the bound box's seed

        let leg = block_on(run_linked_deployment_seed_custody_leg(
            &linked,
            b.actor_id(),
            b.actor_id(),
            &StoreFold(&store),
        ));

        let LinkedCustodyLeg::Ran(report) = leg else {
            panic!("the leg ran: {leg:?}");
        };
        assert_eq!(report.custody, DeploymentSeedCustody::RefusedMismatch);
        assert_eq!(store.merges(), 0);
        assert!(held(&store, &b).is_none());
    }

    /// A connection bound to another identity than the pairing row's is asked
    /// nothing at all — not even the admin check.
    #[test]
    fn a_linked_connection_bound_to_another_identity_is_asked_nothing() {
        let (b, other) = (kp(48), kp(49));
        let store = FakeDeploymentSeedStore::empty();
        let linked = admin_nest(&other);

        let leg = block_on(run_linked_deployment_seed_custody_leg(
            &linked,
            other.actor_id(),
            b.actor_id(),
            &StoreFold(&store),
        ));

        assert_eq!(
            leg,
            LinkedCustodyLeg::IdentityMismatch {
                presented: other.actor_id().0
            }
        );
        assert!(linked.calls.lock().unwrap().is_empty());
        assert!(store.current().is_empty());
    }

    // ── The mark ───────────────────────────────────────────────────────────

    /// A → B → C, bound to C: A is an ancestor and is marked by its own
    /// successor B; an unrelated box U is another box and is left alone.
    #[test]
    fn the_mark_reconcile_marks_verified_ancestors_and_nothing_else() {
        let (a, b, c, u) = (kp(10), kp(11), kp(12), kp(13));
        let nest = FakeConfigNest::default();
        *nest.rotation_chain.lock().unwrap() = vec![hop(&a, &b, 1), hop(&b, &c, 2)];
        let store = FakeDeploymentSeedStore::with(vec![entry(&a), entry(&c), entry(&u)]);

        let report = block_on(run_deployment_seed_custody_leg(&nest, c.actor_id(), &store));

        assert_eq!(report.custody, DeploymentSeedCustody::AlreadyCustodied);
        assert_eq!(
            report.marks,
            SupersessionMarks::Checked(vec![a.actor_id().0])
        );
        assert_eq!(
            held(&store, &a).unwrap().superseded_by,
            Some(b.actor_id().0),
            "the ancestor is marked by its immediate successor"
        );
        assert_eq!(held(&store, &u).unwrap().superseded_by, None);
        assert_eq!(held(&store, &c).unwrap().superseded_by, None);
    }

    /// A chain whose hop does not verify proves nothing, so nothing is marked.
    #[test]
    fn a_chain_that_does_not_verify_marks_nothing() {
        let (a, c) = (kp(20), kp(21));
        let mut forged = hop(&a, &c, 1);
        forged.statement.seq = 7; // signed as seq 1
        let nest = FakeConfigNest::default();
        *nest.rotation_chain.lock().unwrap() = vec![forged];
        let store = FakeDeploymentSeedStore::with(vec![entry(&a), entry(&c)]);

        let report = block_on(run_deployment_seed_custody_leg(&nest, c.actor_id(), &store));

        assert_eq!(report.marks, SupersessionMarks::Checked(Vec::new()));
        assert_eq!(held(&store, &a).unwrap().superseded_by, None);
        assert_eq!(store.merges(), 0);
    }

    // ── The plane rotation drive ───────────────────────────────────────────

    /// Arrange a box bound as `pred`, its custody held on the plane, and the
    /// box able to see the plane.
    fn rotating(pred: &ActorKeypair) -> (FakeConfigNest, FakeDeploymentSeedStore) {
        let store = FakeDeploymentSeedStore::with(vec![DeploymentSeedEntry {
            domain: Some("rotating.example".into()),
            ..entry(pred)
        }]);
        let nest = FakeConfigNest::default();
        *nest.rotate_plane.lock().unwrap() = Some(store.clone());
        (nest, store)
    }

    fn rotate_dispatched(nest: &FakeConfigNest) -> bool {
        nest.calls.lock().unwrap().contains(&ROTATE_KIND)
    }

    /// Custody precedes dispatch, pinned against what the BOX could see: the
    /// pump ships the successor's row only a few polls after it lands, and at
    /// the instant the rotate arrives that row is already among the published.
    #[tokio::test(start_paused = true)]
    async fn the_successor_row_is_published_before_the_ceremony_is_dispatched() {
        let pred = kp(30);
        let (nest, store) = rotating(&pred);
        store.set_publish(SeedPublish::AfterPolls(3));

        let outcome = rotate_deployment_seed_on_plane(&nest, pred.actor_id(), Some(&store))
            .await
            .expect("rotate");
        let SeedRotation::Rotated {
            nest_actor_id: successor,
            predecessor_marked,
            ..
        } = outcome
        else {
            panic!("expected a committed rotation, got {outcome:?}");
        };

        let seen = nest
            .plane_at_rotate
            .lock()
            .unwrap()
            .clone()
            .expect("the ceremony was dispatched");
        let row = seen
            .iter()
            .find(|e| e.nest_actor_id == successor)
            .expect("the successor's row was on the box when the rotate arrived");
        assert_eq!(
            row.domain.as_deref(),
            Some("rotating.example"),
            "label carried"
        );
        assert!(predecessor_marked);
        assert_eq!(held(&store, &pred).unwrap().superseded_by, Some(successor));
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_custody_merge_means_no_dispatch() {
        let pred = kp(31);
        let (nest, store) = rotating(&pred);
        store.refuse_next_merges(1);

        let err = rotate_deployment_seed_on_plane(&nest, pred.actor_id(), Some(&store))
            .await
            .expect_err("refused before dispatch");

        assert!(
            matches!(err, PlaneSeedRotationError::CustodyRefused(_)),
            "{err:?}"
        );
        assert!(!rotate_dispatched(&nest));
    }

    #[tokio::test(start_paused = true)]
    async fn a_row_never_published_means_no_dispatch() {
        let pred = kp(32);
        let (nest, store) = rotating(&pred);
        store.set_publish(SeedPublish::Never);

        let err = rotate_deployment_seed_on_plane(&nest, pred.actor_id(), Some(&store))
            .await
            .expect_err("gave up before dispatch");

        assert!(
            matches!(err, PlaneSeedRotationError::NotPublished),
            "{err:?}"
        );
        assert!(!rotate_dispatched(&nest));
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_handle_the_drive_refuses_plainly() {
        let pred = kp(33);
        let nest = FakeConfigNest::default();

        let err = rotate_deployment_seed_on_plane::<_, FakeDeploymentSeedStore>(
            &nest,
            pred.actor_id(),
            None,
        )
        .await
        .expect_err("no store");

        assert!(
            matches!(err, PlaneSeedRotationError::NoAccountStore),
            "{err:?}"
        );
        assert!(nest.calls.lock().unwrap().is_empty());
    }

    /// The mark after the dispatch fails; the rotation stands, and the next
    /// leg run — bound to the successor now — re-derives the mark from the
    /// box's verified chain.
    #[tokio::test(start_paused = true)]
    async fn a_mark_that_fails_after_dispatch_is_healed_by_the_next_leg_run() {
        let pred = kp(34);
        let (nest, store) = rotating(&pred);
        store.refuse_after(1); // the successor's merge lands; the mark does not

        let outcome = rotate_deployment_seed_on_plane(&nest, pred.actor_id(), Some(&store))
            .await
            .expect("the rotation itself committed");
        let SeedRotation::Rotated {
            nest_actor_id: successor,
            predecessor_marked,
            ..
        } = outcome
        else {
            panic!("expected a committed rotation, got {outcome:?}");
        };
        assert!(!predecessor_marked);
        assert_eq!(held(&store, &pred).unwrap().superseded_by, None);

        // The box now serves its chain; the next edge runs the leg bound to
        // the successor.
        store.stop_refusing();
        let succ_seed = store
            .current()
            .into_iter()
            .find(|e| e.nest_actor_id == successor)
            .expect("successor custodied")
            .seed
            .to_array();
        let succ = ActorKeypair::from_secret(succ_seed);
        *nest.rotation_chain.lock().unwrap() = vec![hop(&pred, &succ, 1)];

        let report = run_deployment_seed_custody_leg(&nest, succ.actor_id(), &store).await;

        assert_eq!(report.custody, DeploymentSeedCustody::AlreadyCustodied);
        assert_eq!(
            report.marks,
            SupersessionMarks::Checked(vec![pred.actor_id().0])
        );
        assert_eq!(held(&store, &pred).unwrap().superseded_by, Some(successor));
    }
}
