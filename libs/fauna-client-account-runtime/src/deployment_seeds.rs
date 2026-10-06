//! The deployment-seed custody's **native host seam** — what tui, linux and
//! fauna-ffi (for windows, apple and android) call so the box-recovery reads
//! and writes run through the account plane with one composition per edge
//! (`nest/box-recovery.md` § The plane-era recovery floor).
//!
//! - [`run_custody_leg`] — the custody leg over the bound nest, run at the
//!   second of the post-auth and store-ready edges and at every later
//!   post-auth edge (*(c) The writes*). Answers the warning the host owes the
//!   admin, in the one shared wording.
//! - [`rotate_deployment_seed`] — the plane rotation drive behind the admin
//!   surface's "Rotate deployment identity" action: custody before dispatch,
//!   one [`LocalizedText`] verdict.
//! - [`recoverable_box_ids`] / [`selfhosted_command`] — the pre-login reads
//!   (*(b) The reads*): this device's own store joined with a cold read from
//!   the nest at `nest_url` when one is given, never either-or.
//!
//! The bound id every write keys on is the identity the connection is bound
//! to ([`fauna_client_pair::resolve_this_nest_id`] —
//! `LinkedNestsMachine::bound_nest_id`), never the box's own `nest.info` claim.

use std::sync::Arc;

use fauna_account_plane::deployment_seed_recovery::{
    ResolvedDeploymentSeeds, StoreRoot, resolve_deployment_seeds_over,
};
use fauna_client::NestClient;
use fauna_client_config::{
    DeploymentSeedCustody, DeploymentSeedStore, SupersessionMarks, custody_leg_warning,
    rotate_deployment_seed_on_plane, run_deployment_seed_custody_leg, seed_rotation_verdict,
};
use fauna_core::data::DeploymentSeedEntry;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::localized::LocalizedText;

/// The identity `nest` is bound to, as an [`ActorId`].
///
/// # Errors
///
/// The bound id could not be resolved (no reachable connection, a nest whose
/// own claim disagrees with what the connection proved) or is not 32 bytes.
pub async fn bound_nest_id(nest: &Arc<NestClient>) -> Result<ActorId, String> {
    let bytes = fauna_client_pair::resolve_this_nest_id(nest).await?;
    <[u8; 32]>::try_from(bytes.as_slice())
        .map(ActorId)
        .map_err(|_| "the nest reported an identity that is not 32 bytes".to_string())
}

/// The warning [`run_custody_leg`] answers when the bound id cannot be
/// resolved — custody cannot even be attempted.
const UNRESOLVED_WARNING: &str =
    "could not confirm this nest's identity — total-box-loss recovery is not yet protected";

/// **The custody leg** over the bound nest (`fauna_client_config::
/// run_deployment_seed_custody_leg`), logged, answering the warning the host
/// surfaces on its existing warning surface — `None` when nothing is owed.
///
/// The host calls it at whichever of its post-auth edge and its store-ready
/// edge lands second, and at every later post-auth edge; the steady state
/// makes no round trip. `store` is the account-store handle, or the host's
/// per-call resolver over it (`fauna_client_config::ResolvingLedgerStore`).
pub async fn run_custody_leg<S>(nest: &Arc<NestClient>, store: &S) -> Option<String>
where
    S: DeploymentSeedStore + ?Sized,
{
    let bound = match bound_nest_id(nest).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!("deployment-seed custody leg: bound nest id unresolved: {e}");
            return Some(UNRESOLVED_WARNING.to_string());
        }
    };
    let report = run_deployment_seed_custody_leg(&**nest, bound, store).await;
    match &report.custody {
        DeploymentSeedCustody::Captured => tracing::info!(
            "deployment seed custodied off-box on the account plane (custody leg: captured)"
        ),
        DeploymentSeedCustody::AlreadyCustodied => tracing::info!(
            "deployment seed already custodied off-box on the account plane (custody leg: held)"
        ),
        DeploymentSeedCustody::NotAdmin => {
            tracing::debug!("deployment-seed custody leg: not an admin of this nest")
        }
        other => tracing::warn!(
            outcome = ?other,
            retryable = other.retryable(),
            "deployment-seed custody leg: custody unconfirmed"
        ),
    }
    match &report.marks {
        SupersessionMarks::Checked(marked) if !marked.is_empty() => tracing::info!(
            marked = marked.len(),
            "deployment-seed custody leg: marked rotated-away boxes superseded"
        ),
        SupersessionMarks::ChainUnavailable | SupersessionMarks::StoreRefused(_) => {
            tracing::debug!(marks = ?report.marks, "deployment-seed mark reconcile deferred")
        }
        _ => {}
    }
    custody_leg_warning(&report.custody)
}

/// **The plane rotation drive** behind the admin surface's rotate action.
/// Resolves the bound id first, while the box still answers as the
/// predecessor, then runs `rotate_deployment_seed_on_plane` (custody merged
/// and published before dispatch). Every outcome collapses to ONE
/// [`LocalizedText`] — [`seed_rotation_verdict`] on a dispatch, the
/// `rotate_seed_failed` key otherwise — so every host renders it through the
/// same resolver. There is no fan-out step: the plane carries the successor's
/// row.
pub async fn rotate_deployment_seed<S>(nest: &Arc<NestClient>, store: Option<&S>) -> LocalizedText
where
    S: DeploymentSeedStore + ?Sized,
{
    let failed = |cause: String| {
        LocalizedText::key_arg("admin.nest_page.rotate_seed_failed", "cause", cause)
    };
    let bound = match bound_nest_id(nest).await {
        Ok(id) => id,
        Err(e) => return failed(e),
    };
    match rotate_deployment_seed_on_plane(&**nest, bound, store).await {
        Ok(outcome) => seed_rotation_verdict(&outcome),
        Err(e) => failed(e.to_string()),
    }
}

/// Drive a resolver future to completion on a blocking-pool thread of the
/// ambient tokio runtime, so the future the caller awaits is `Send`.
///
/// **Why.** The cold read awaits over the `RpcRequester` AFIT trait with a
/// borrowed `&NestClient`, and rustc cannot prove that future `Send` for every
/// lifetime ("implementation of `Send` is not general enough") — which every
/// host's spawner (`tokio::spawn`, linux's `run_on_tokio`, the UniFFI async
/// export) requires. `block_on` asks no `Send` of the future it drives, only
/// of the closure, which owns its inputs; the connection's I/O and timers stay
/// on the same runtime. One home for the hop, so no host carries a copy.
/// Must be called from within a tokio runtime.
async fn on_blocking_pool<F, Fut, T>(build: F) -> Result<T, String>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = T>,
    T: Send + 'static,
{
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || runtime.block_on(build()))
        .await
        .map_err(|e| format!("the custody read task failed: {e}"))
}

/// The custody map as a pre-login surface reads it over a connection the
/// caller already holds (authenticated as the identity behind `secret`): the
/// local read under `root` joined with a cold read over `nest`. Every failure
/// is kept ([`ResolvedDeploymentSeeds::into_result`] errs only when both
/// sources failed). A `Send` future.
pub async fn resolve_over_connection(
    nest: Arc<NestClient>,
    secret: [u8; 32],
    root: StoreRoot,
) -> ResolvedDeploymentSeeds {
    on_blocking_pool(move || async move {
        resolve_deployment_seeds_over(&root, &secret, Some(&*nest)).await
    })
    .await
    .unwrap_or_else(|e| ResolvedDeploymentSeeds {
        local_failure: Some(anyhow::anyhow!("{e}")),
        cold_asked: true,
        cold_failure: Some(anyhow::anyhow!("{e}")),
        ..Default::default()
    })
}

/// The custody map a pre-login surface reads for the identity behind
/// `secret`: the local read under `root`, joined with a cold read from the
/// nest at `nest_url` when one is given and answers (this connects and
/// authenticates itself). Best-effort — a source that fails is logged and
/// leaves the other's answer. A `Send` future.
pub async fn resolve_pre_login(
    nest_url: Option<String>,
    secret: [u8; 32],
    root: StoreRoot,
) -> Vec<DeploymentSeedEntry> {
    on_blocking_pool(move || async move {
        let nest = match nest_url {
            None => None,
            Some(url) => {
                let nest = NestClient::new(url.clone(), ActorKeypair::from_secret(secret));
                match nest.connect().await {
                    Ok(()) => Some(nest),
                    Err(e) => {
                        // The saved nest may well be the dead box — the case
                        // recovery exists for. Best-effort, so info, not error.
                        tracing::info!("recovery: connect to {url} failed (best-effort): {e}");
                        None
                    }
                }
            }
        };
        resolve_deployment_seeds_over(&root, &secret, nest.as_deref())
            .await
            .seeds
    })
    .await
    .unwrap_or_else(|e| {
        tracing::warn!("recovery: {e}");
        Vec::new()
    })
}

/// The custodied boxes the `nest_recovery` hub lists (superseded boxes
/// excluded) — [`resolve_pre_login`] projected by the shared
/// `fauna_client_config::recoverable_box_ids_in`. Only public `nest_actor_id`s
/// cross out; the seed stays in Rust.
pub async fn recoverable_box_ids(
    nest_url: Option<String>,
    secret: [u8; 32],
    root: StoreRoot,
) -> Vec<String> {
    fauna_client_config::recoverable_box_ids_in(&resolve_pre_login(nest_url, secret, root).await)
}

/// The `recover-selfhosted-command` for the box `nest_actor_id_hex` —
/// [`resolve_pre_login`] rendered by the shared
/// `fauna_client_config::selfhosted_recovery_command_in`, so every app emits a
/// byte-identical line. `None` when no source custodies that box.
pub async fn selfhosted_command(
    nest_url: Option<String>,
    secret: [u8; 32],
    root: StoreRoot,
    nest_actor_id_hex: &str,
) -> Option<String> {
    fauna_client_config::selfhosted_recovery_command_in(
        &resolve_pre_login(nest_url, secret, root).await,
        nest_actor_id_hex,
    )
}
