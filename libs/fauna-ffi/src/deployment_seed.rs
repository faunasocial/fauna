//! Native-client FFI for the **deployment-seed custody map** — the windows /
//! apple / android leg of box recovery. tui and linux call the shared host
//! seam `fauna_client_account_runtime::deployment_seeds` directly; the UniFFI
//! apps cannot, so this module wraps the **same shared logic** (priority
//! #2/#3) and adds none of its own.
//!
//! The map rests on the account plane — the fleet-only kind
//! `fauna.state.deployment-seeds`, one row per custodied box
//! (`box-recovery.md` § The plane-era recovery floor):
//!
//! - **Reads** ([`deployment_seeds`], [`deployment_seeds_local`],
//!   [`recover_selfhosted_command`]) — this device's own account store joined
//!   with a cold read from the reachable nest when the caller holds a
//!   connection, never either-or (*(b) The reads*). The store is located by
//!   the same container mapping the account runtime opens it under
//!   ([`crate::account_state::store_root_for`]); each getter takes it as a
//!   trailing `store_container_dir` defaulting to the platform location.
//! - **Capture** is only the **custody leg** (*(c) The writes*), run whenever
//!   a session holds both an authenticated connection and the account-store
//!   handle: at the post-auth edge through [`self_heal_deployment_seed_custody`]
//!   and at the store-ready edge inside the account-runtime seat
//!   (`crate::account_runtime::install`'s installed arm) — whichever lands
//!   second finds the other half. The claim reply's seed is not consumed.
//! - **Rotation** ([`rotate_deployment_seed`]) is the plane drive: custody
//!   merged and published before dispatch, no fan-out afterwards.
//!
//! The blob fan-out, the claim-time capture and the device-local
//! replica retired for this kind: no native glue calls a capture or converge
//! entry point any more.
//!
//! Gated behind the default-on `deployment-seed` feature so the Go mail-bridge
//! `--no-default-features` FFI build drops it (the bridge is a server with no
//! onboarding/recovery surface — same dead-code rationale as
//! `backup-destinations` / `pairing`).

use std::path::PathBuf;
use std::sync::Arc;

use fauna_client_config::{
    DeploymentSeedCustody, ResolvingLedgerStore, recoverable_boxes_in,
    run_deployment_seed_custody_leg, selfhosted_recovery_command_in,
};
use fauna_core::data::DeploymentSeedEntry;
use fauna_core::identity::ActorId;
#[cfg(feature = "value-format")]
use fauna_core::localized::LocalizedText;
use fauna_sync_engine::deployment_seed_recovery::{StoreRoot, read_local_deployment_seeds};

use crate::nest_client::FfiNestClient;
use crate::{FfiError, general_err};

/// Outcome of one custody capture, carried by
/// [`FfiDeploymentSeedSelfHeal::Captured`] — the glue matches on it. [`Wrote`](Self::Wrote) = the custody leg merged
/// the box's entry into the plane; [`RefusedMismatch`](Self::RefusedMismatch)
/// = the fetched seed did not derive to the bound nest id (BR-2) — the glue
/// surfaces its "recovery not protected" warning.
#[derive(uniffi::Enum)]
pub enum FfiDeploymentSeedCapture {
    /// The box's entry was merged into the account plane.
    Wrote,
    /// An identical entry is already held — no write.
    AlreadyHeldSame,
    /// Retired: the plane map holds one row per box, so there is no
    /// "different seed already held" refusal any more.
    RefusedDiffering,
    /// The fetched seed does **not** derive to this connection's bound
    /// `nest_actor_id` (BR-2). Refused before any write — the glue surfaces a
    /// loud "recovery not protected" warning.
    RefusedMismatch,
}

/// The `nest_actor_id` this connection is **bound** to, as an [`ActorId`] —
/// the one comparand every custody, rotation and backup drive keys on.
/// `LinkedNestsMachine::bound_nest_id` (the native twin of linux
/// `resolve_this_nest_id`): the login's pin for this origin, else a
/// possession proof over the connection, and a refusal when the nest's own
/// `fauna.nest.info` claim disagrees with it — never that claim on its own,
/// which any box can answer with a sibling's id it learned from the pairing
/// list.
pub(crate) async fn bound_nest_id(nest: &Arc<FfiNestClient>) -> Result<ActorId, FfiError> {
    let id = fauna_client_pair::build_linked_nests_machine(nest.nest_arc())
        .bound_nest_id()
        .await
        .map_err(|e| FfiError::General {
            msg: format!("resolve this nest id: {e}"),
        })?;
    let id: [u8; 32] = id.as_slice().try_into().map_err(|_| FfiError::General {
        msg: "this nest's id was not 32 bytes".into(),
    })?;
    Ok(ActorId(id))
}

/// Outcome of [`rotate_deployment_seed`], collapsed to what the native paint
/// needs: the one-sentence verdict
/// ([`fauna_client_config::seed_rotation_verdict`]) and whether the box's
/// identity actually flipped. There is no fan-out step after it — the plane
/// carries the successor's row.
#[cfg(feature = "value-format")]
#[derive(uniffi::Record)]
pub struct FfiSeedRotationResult {
    /// `true` for [`fauna_client_config::SeedRotation::Rotated`], `false` for
    /// [`fauna_client_config::SeedRotation::RefusedIdentityMismatch`].
    pub rotated: bool,
    /// The one-sentence outcome — render via the client's shared
    /// `LocalizedText` resolver (Swift `renderLocalizedText`, Kotlin/C#'s
    /// twin, `admin.nest_page.rotate_seed_*`).
    pub verdict: LocalizedText,
}

/// Drive the deployment-seed rotation ceremony on the account plane
/// (`fauna_client_config::rotate_deployment_seed_on_plane`) — the native
/// twin of tui/linux's `deployment_seeds::rotate_deployment_seed`
/// (`box-recovery.md` § Deployment-seed rotation; § The plane-era recovery
/// floor, *(c) The writes*). Custody before dispatch, literally: the
/// successor's entry is merged and published to the bound nest before the
/// rotate request goes out; with no account-store handle the drive refuses
/// plainly. There is no post-rotation fan-out.
///
/// Resolves the `nest_actor_id` THIS connection is bound to first, while the
/// box still answers as the predecessor.
///
/// **This call outlives a naive reply budget by design.** The committed
/// rotation drops this very connection (WS 1001) and the drive's own marking
/// step reconnects — bound it by the ceremony's own internal retry
/// discipline, never an agent-command timeout (mirrors tui's
/// `PageOp::outlives_click`; convention 11's corollary).
///
/// `owner_secret` is validated as a 32-byte secret only: the connection
/// already signs as the owner and the plane door holds the custody.
///
/// # Errors
///
/// - `FfiError::General` if `owner_secret` is not 32 bytes, if this nest's
///   identity can't be resolved, or resolves to a non-32-byte id.
/// - `FfiError::General` carrying the drive's refusal (no account store, the
///   custody merge refused, the successor row not published, the dispatch
///   failed) — nothing rotated. A best-effort failure marking the predecessor
///   folds into [`FfiSeedRotationResult::verdict`] as a success-with-caveat.
#[cfg(feature = "value-format")]
#[fauna_uniffi_async::export]
pub async fn rotate_deployment_seed(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
) -> Result<FfiSeedRotationResult, FfiError> {
    crate::keypair_from_bytes(&owner_secret)?;
    let current = bound_nest_id(&nest).await?;
    let handle = crate::account_runtime::handle();
    let outcome = fauna_client_config::rotate_deployment_seed_on_plane(
        &*nest.nest_arc(),
        current,
        handle.as_ref(),
    )
    .await
    .map_err(general_err)?;
    let rotated = matches!(outcome, fauna_client_config::SeedRotation::Rotated { .. });
    let verdict = fauna_client_config::seed_rotation_verdict(&outcome);
    Ok(FfiSeedRotationResult { rotated, verdict })
}

/// Outcome of [`self_heal_deployment_seed_custody`] — one custody-leg run,
/// mapped from [`fauna_client_config::DeploymentSeedCustody`]. The glue
/// surfaces its existing "recovery custody not yet saved / not protected"
/// warning for the unconfirmed arms — [`HandoffUnavailable`](Self::HandoffUnavailable),
/// [`NestHoldsNoSeed`](Self::NestHoldsNoSeed) and
/// `Captured(RefusedMismatch)` — and logs the rest.
#[derive(uniffi::Enum)]
pub enum FfiDeploymentSeedSelfHeal {
    /// The plane already holds a live entry for this nest — the steady state
    /// on every connect after the first; no round trip.
    AlreadyCustodied,
    /// Not a current roster admin on this nest (or the admin check itself
    /// failed) — nothing is owed.
    NotAdmin,
    /// Custody is owed but did not land this run — the fetch failed after its
    /// bounded retry, the account store refused the merge, or the store is
    /// not up yet (the seat's store-ready edge runs the leg then). Retried at
    /// the next edge.
    HandoffUnavailable,
    /// Admin, and the fetch answered, but this nest holds no deployment key to
    /// hand over — not expected post-boot.
    NestHoldsNoSeed,
    /// The leg fetched the seed: [`FfiDeploymentSeedCapture::Wrote`] when it
    /// merged the entry, [`FfiDeploymentSeedCapture::RefusedMismatch`] when
    /// the seed did not derive to the bound id.
    Captured(FfiDeploymentSeedCapture),
}

impl From<DeploymentSeedCustody> for FfiDeploymentSeedSelfHeal {
    fn from(o: DeploymentSeedCustody) -> Self {
        match o {
            DeploymentSeedCustody::AlreadyCustodied => Self::AlreadyCustodied,
            DeploymentSeedCustody::NotAdmin => Self::NotAdmin,
            DeploymentSeedCustody::HandoffUnavailable | DeploymentSeedCustody::StoreRefused(_) => {
                Self::HandoffUnavailable
            }
            DeploymentSeedCustody::NestHoldsNoSeed => Self::NestHoldsNoSeed,
            DeploymentSeedCustody::Captured => Self::Captured(FfiDeploymentSeedCapture::Wrote),
            DeploymentSeedCustody::RefusedMismatch => {
                Self::Captured(FfiDeploymentSeedCapture::RefusedMismatch)
            }
        }
    }
}

/// **The custody leg's post-auth entry** — call at the app's universal
/// post-auth hook, on **every** connect (`box-recovery.md` § The plane-era
/// recovery floor, *(c) The writes*). It resolves the nest this connection is
/// bound to and runs the shared `run_deployment_seed_custody_leg` over this
/// process's account-store handle: a live entry for the box → done with no
/// round trip; otherwise the admin check (fail-closed), the seed fetch, the
/// seed→id refusal, the merge, and the supersession-mark reconcile.
///
/// The store is this process's account-store handle resolved per call, and
/// **waited for** while its assembly is still in flight
/// ([`fauna_client_config::ResolvingLedgerStore`], bounded by
/// [`fauna_client_config::LEDGER_READY_WAIT`]) — the app's post-auth hook
/// fires right beside the connect that spawns the assembly, so an unwaited
/// read would miss the store on nearly every fresh launch. A store still
/// absent after the wait answers
/// [`FfiDeploymentSeedSelfHeal::HandoffUnavailable`] (custody unconfirmed,
/// retried next connect); the account-runtime seat also runs the same leg at
/// its store-ready edge, so no second app call is owed.
///
/// `owner_secret` is validated as a 32-byte secret only; `app_data_dir` is
/// unused (the device-local replica retired for this kind) and
/// kept so no caller's signature changes.
///
/// # Errors
///
/// - `FfiError::General` if `owner_secret` is not 32 bytes, or if this nest's
///   identity can't be resolved. Non-fatal to launch; the glue warns.
#[fauna_uniffi_async::export]
pub async fn self_heal_deployment_seed_custody(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    app_data_dir: String,
) -> Result<FfiDeploymentSeedSelfHeal, FfiError> {
    let _ = app_data_dir;
    crate::keypair_from_bytes(&owner_secret)?;
    let bound = bound_nest_id(&nest).await?;
    let store = ResolvingLedgerStore::new(crate::account_runtime::handle);
    let report = run_deployment_seed_custody_leg(&*nest.nest_arc(), bound, &store).await;
    if matches!(report.custody, DeploymentSeedCustody::StoreRefused(_)) {
        tracing::info!(
            "deployment-seed custody leg: the account store refused or is not up yet — the store-ready edge and the next connect retry"
        );
    }
    if report.custody.unconfirmed() {
        tracing::warn!(outcome = ?report.custody, "deployment-seed custody leg: custody unconfirmed");
    }
    Ok(report.custody.into())
}

/// One custodied box for the total-box-loss recovery box list — the native twin
/// of the wasm `WasmDeploymentSeedEntry` (`libs/fauna-wasm`). Carries **only**
/// the box's `nest_actor_id` (hex) and domain: the raw deployment seed is the
/// nest's signing identity and never crosses the FFI boundary — the recovery
/// re-provision drive resolves the seed in Rust by `nest_actor_id`.
#[derive(uniffi::Record)]
pub struct FfiDeploymentSeedEntry {
    /// 64-char hex of the box's `nest_actor_id` (= `ed25519(seed).public`).
    pub nest_actor_id: String,
    /// The box's own handle domain (server_name / DNS zone), for the
    /// `recover-box-item` label + the cloud re-provision zone. `None` for a
    /// domainless box. Non-secret — the recovery UI shows it.
    pub domain: Option<String>,
}

/// The account-store location for `store_container_dir` — the one mapping the
/// account runtime opens the store under ([`crate::account_state::store_root_for`]:
/// `Some(dir) => StoreRoot::at(dir)`, `None => StoreRoot::platform()`).
fn store_root(store_container_dir: Option<String>) -> StoreRoot {
    StoreRoot::at(crate::account_state::store_root_for(
        store_container_dir.map(PathBuf::from),
    ))
}

/// The recoverable rows (superseded boxes excluded) as FFI records.
fn project_boxes(seeds: &[DeploymentSeedEntry]) -> Vec<FfiDeploymentSeedEntry> {
    recoverable_boxes_in(seeds)
        .into_iter()
        .map(|e| FfiDeploymentSeedEntry {
            nest_actor_id: hex::encode(e.nest_actor_id),
            domain: e.domain,
        })
        .collect()
}

/// This device's local read joined with a cold read over `nest` — the
/// pre-login resolver (`resolve_deployment_seeds_over`) over the connection
/// the caller already holds; an error only when every source failed.
///
/// A `Send` future (the shared seam drives the resolver on the blocking pool),
/// as the UniFFI async export requires.
async fn resolve_over(
    nest: &Arc<FfiNestClient>,
    secret: &[u8; 32],
    store_container_dir: Option<String>,
) -> Result<Vec<DeploymentSeedEntry>, FfiError> {
    let resolved = fauna_client_account_runtime::deployment_seeds::resolve_over_connection(
        nest.nest_arc(),
        *secret,
        store_root(store_container_dir),
    )
    .await;
    resolved
        .into_result()
        .map_err(|e| general_err(format!("{e:#}")))
}

/// The custodied box list for the total-box-loss recovery UI
/// (`box-recovery.md` § Restore, step 4) — the native twin of the wasm
/// `deploymentSeeds()` getter. Reads this device's own account store joined
/// with a cold read from the nest behind `nest` (*(b) The reads* — never
/// either-or), projected through the shared `recoverable_boxes_in` so a
/// rotated-away box is not offered. The seed stays Rust-internal.
///
/// `owner_secret` is the admin actor's 32-byte identity seed (the cold read
/// opens the escrow wraps with it); `store_container_dir` locates the account
/// store as the account runtime does (`None` = the platform location).
///
/// # Errors
///
/// - `FfiError::General` if `owner_secret` is not 32 bytes.
/// - `FfiError::General` when **both** the local read and the cold read
///   failed — one that answered, even empty, is an answer.
#[fauna_uniffi_async::export(default(store_container_dir = None))]
pub async fn deployment_seeds(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    store_container_dir: Option<String>,
) -> Result<Vec<FfiDeploymentSeedEntry>, FfiError> {
    let keypair = crate::keypair_from_bytes(&owner_secret)?;
    let seeds = resolve_over(&nest, keypair.secret_bytes(), store_container_dir).await?;
    Ok(project_boxes(&seeds))
}

/// The custodied box list read with **no nest at all** — this device's own
/// account store only (*(b) The reads*, the local read), for the
/// single-last-box total-loss case where no reachable connection exists.
/// Synchronous: the native store read completes on the calling thread and
/// never touches a transport. Empty for a store that does not exist (it
/// never creates one).
///
/// `owner_secret` is the admin actor's 32-byte identity seed (only its actor
/// id is used, to locate the store); `app_data_dir` is **unused** — it rooted
/// the retired device-local replica and stays in the signature so
/// the Swift and Kotlin call sites keep binding; `store_container_dir` locates
/// the account store as the account runtime does (`None` = the platform
/// location).
///
/// # Errors
///
/// - `FfiError::General` if `owner_secret` is not 32 bytes.
/// - `FfiError::General` if the store fails to open or a stored row does not
///   decode strictly — surfaced loudly rather than read as "no boxes".
#[uniffi::export(default(store_container_dir = None))]
pub fn deployment_seeds_local(
    owner_secret: Vec<u8>,
    app_data_dir: String,
    store_container_dir: Option<String>,
) -> Result<Vec<FfiDeploymentSeedEntry>, FfiError> {
    let _ = app_data_dir;
    let keypair = crate::keypair_from_bytes(&owner_secret)?;
    let root = store_root(store_container_dir);
    let seeds = poll_once(read_local_deployment_seeds(&root, &keypair.actor_id_hex()))?
        .map_err(|e| general_err(format!("{e:#}")))?;
    Ok(project_boxes(&seeds))
}

/// Drive a future that completes on its first poll — the native local read,
/// whose SQLite read is synchronous under its async signature
/// (`read_local_deployment_seeds`' own contract). Polled with a no-op waker on
/// the calling thread, so it is safe from any thread, a tokio worker
/// included (no nested runtime, no blocking); a future that ever suspended
/// answers an error rather than hanging.
fn poll_once<F: std::future::Future>(fut: F) -> Result<F::Output, FfiError> {
    use std::task::{Context, Poll, Waker};
    let fut = std::pin::pin!(fut);
    match fut.poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(out) => Ok(out),
        Poll::Pending => Err(FfiError::General {
            msg: "the account store read did not complete synchronously".into(),
        }),
    }
}

/// The `recover-selfhosted-command` for the box-recovery step-4 self-hosted
/// install page (`box-recovery.md` § Recovery UI (step 4)) — the native twin
/// of the wasm `recoverSelfhostedCommand()` getter. Returns the `.env` line
/// `FAUNA_DEPLOYMENT_SEED=<64-hex>` the admin runs their installer with on a
/// fresh box, so the rebuilt box re-presents the **same** `nest_actor_id`.
///
/// Resolves the selected box's seed **in Rust** over the same resolver as
/// [`deployment_seeds`] (local read ⊔ cold read over `nest`), rendered by the
/// shared `selfhosted_recovery_command_in` so every app emits a
/// byte-identical line. The seed **is** surfaced here by design — it is the
/// installer input the admin pastes (`box-recovery.md` § Trust & audience).
///
/// # Errors
///
/// - `FfiError::General` if `owner_secret` is not 32 bytes.
/// - `FfiError::General` when both sources failed.
/// - `FfiError::General` if no deployment seed is custodied for
///   `nest_actor_id` (or it is not 64-char hex).
#[fauna_uniffi_async::export(default(store_container_dir = None))]
pub async fn recover_selfhosted_command(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    nest_actor_id: String,
    store_container_dir: Option<String>,
) -> Result<String, FfiError> {
    let keypair = crate::keypair_from_bytes(&owner_secret)?;
    let seeds = resolve_over(&nest, keypair.secret_bytes(), store_container_dir).await?;
    selfhosted_recovery_command_in(&seeds, nest_actor_id.trim()).ok_or_else(|| FfiError::General {
        msg: "no custodied deployment seed for that box".into(),
    })
}
