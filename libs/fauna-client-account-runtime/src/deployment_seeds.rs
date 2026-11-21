//! The deployment-seed custody's **native host seam** — what tui, linux and
//! fauna-ffi (for windows, apple and android) call so the box-recovery reads
//! and writes run through the account plane with one composition per edge
//! (`nest/box-recovery.md` § The plane-era recovery floor).
//!
//! - [`run_custody_leg_signalled`] — the custody leg over the bound nest, run
//!   at the second of the post-auth and store-ready edges and at every later
//!   post-auth edge (*(c) The writes*), waiting out a store still settling
//!   and, after a warned run, retrying at every later connection. Hands the
//!   host every run's [`CustodyLegSignal`] — the warning it owes the admin as
//!   one shared [`LocalizedText`], or the cleared mark that takes it down.
//!   [`run_custody_leg`] is its first-run-only form.
//! - [`rotate_deployment_seed`] — the plane rotation drive behind the admin
//!   surface's "Rotate deployment identity" action: custody before dispatch
//!   over the host's per-call store resolver, one [`LocalizedText`] verdict.
//! - [`recoverable_box_ids`] / [`selfhosted_command`] — the pre-login reads
//!   (*(b) The reads*): this device's own store joined with a cold read from
//!   the nest at `nest_url` when one is given, never either-or.
//!
//! The bound id every write keys on is the identity the connection is bound
//! to, never the box's own `nest.info` claim: both drives run through
//! [`fauna_client_pair::deployment_seed`] — the one composition web shares —
//! which resolves that id from the identity machine over the connection
//! (the same native seam [`fauna_client_pair::resolve_this_nest_id`] builds)
//! and is where the lying-box pins live.

use std::sync::Arc;

use fauna_account_plane::deployment_seed_recovery::{
    ResolvedDeploymentSeeds, StoreRoot, resolve_deployment_seeds_over,
};
use fauna_client::NestClient;
use fauna_client_config::{
    CustodyLegSignal, DeploymentSeedCustody, DeploymentSeedStore, ResolvingLedgerStore,
    SeedRotation, SupersessionMarks, custody_leg_unattempted_warning, seed_rotation_verdict,
};
use fauna_client_pair::{LinkedNestsMachine, deployment_seed};
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
    deployment_seed::bound_actor_id(&identity_of(nest)).await
}

/// The identity machine over `nest`'s connection — the native seam every
/// drive here resolves the bound id through.
fn identity_of(nest: &Arc<NestClient>) -> LinkedNestsMachine {
    fauna_client_pair::build_linked_nests_machine(Arc::clone(nest))
}

/// **The custody leg** over the bound nest (`fauna_client_config::
/// run_deployment_seed_custody_leg_settling`), logged, answering the warning
/// the host renders through its own `LocalizedText` resolver onto its
/// existing warning surface — `None` when nothing is owed. A bound id that
/// does not resolve answers [`custody_leg_unattempted_warning`].
///
/// The first run's answer only: a later retry ([`retry_on_next_connections`])
/// that lands custody is logged, and nothing tells this caller to take the
/// warning down. A host with a warning surface it can clear calls
/// [`run_custody_leg_signalled`] instead; this form stays for the glue that
/// has no channel back yet (fauna-ffi's one-shot export).
pub async fn run_custody_leg<S>(nest: &Arc<NestClient>, store: S) -> Option<LocalizedText>
where
    S: DeploymentSeedStore + Send + Sync + 'static,
{
    let mut first = None;
    drive_custody_leg(nest, store, |signal| first = Some(signal), Arc::new(|_| {})).await;
    match first {
        Some(CustodyLegSignal::Warning(warning)) => Some(warning),
        Some(CustodyLegSignal::Cleared) | None => None,
    }
}

/// **The custody leg**, every run's [`CustodyLegSignal`] handed to `sink` —
/// this run's, then each retry's at a later connection of `nest` — so the
/// host paints a warning while custody is unconfirmed and takes it down when
/// a later run confirms it (`box-recovery.md` § The plane-era recovery
/// floor, *(c) The writes*). The host maps [`CustodyLegSignal::Cleared`] onto
/// clearing its warning surface only while that surface still shows a
/// custody warning (`fauna_client_config::is_custody_leg_warning`).
///
/// The host calls it at whichever of its post-auth edge and its store-ready
/// edge lands second, and at every later post-auth edge; the steady state
/// makes no round trip. `store` is the account-store handle, or the host's
/// per-call resolver over it (`fauna_client_config::ResolvingLedgerStore`).
///
/// **The warning's promise is kept here.** A store that refuses the write
/// while it settles is waited out inside the run (bounded by
/// `fauna_client_config::CUSTODY_STORE_SETTLE_WAIT`), so the first
/// connection after provisioning confirms custody without warning. A run
/// that still ends unconfirmed and retryable warns, and leaves a retry on
/// every later connection of `nest` ([`retry_on_next_connections`]) — the
/// warning's "the app tries again on the next connection", true on every
/// host whether or not its own glue re-runs the leg at a reconnect. `sink`
/// is called from whichever task ran the run: a host hops to its UI thread
/// itself (a channel send).
pub async fn run_custody_leg_signalled<S, F>(nest: &Arc<NestClient>, store: S, sink: F)
where
    S: DeploymentSeedStore + Send + Sync + 'static,
    F: Fn(CustodyLegSignal) + Send + Sync + 'static,
{
    let sink: SignalSink = Arc::new(sink);
    let later = Arc::clone(&sink);
    drive_custody_leg(nest, store, move |signal| sink(signal), later).await;
}

/// Where the runs after the first send their signal.
type SignalSink = Arc<dyn Fn(CustodyLegSignal) + Send + Sync>;

/// One run, its signal to `first` — before any retry exists, so a retry's
/// signal can never overtake it — then, after a warned retryable run, the
/// retry at every later connection, its signals to `later`.
async fn drive_custody_leg<S>(
    nest: &Arc<NestClient>,
    store: S,
    first: impl FnOnce(CustodyLegSignal),
    later: SignalSink,
) where
    S: DeploymentSeedStore + Send + Sync + 'static,
{
    let store = Arc::new(store);
    let (signal, retryable) = run_custody_leg_once(nest, &*store).await;
    let retry = retryable && matches!(signal, CustodyLegSignal::Warning(_));
    first(signal);
    if retry {
        tokio::spawn(retry_on_next_connections(nest, store, later));
    }
}

/// One logged run of the leg: the signal it owes the host, and whether a
/// later run can land what this one did not.
async fn run_custody_leg_once<S>(nest: &Arc<NestClient>, store: &S) -> (CustodyLegSignal, bool)
where
    S: DeploymentSeedStore + ?Sized,
{
    let report =
        match deployment_seed::run_custody_leg_bound(&**nest, &identity_of(nest), store).await {
            Ok(report) => report,
            Err(e) => {
                tracing::warn!("deployment-seed custody leg: bound nest id unresolved: {e}");
                return (
                    CustodyLegSignal::Warning(custody_leg_unattempted_warning()),
                    true,
                );
            }
        };
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
    (
        CustodyLegSignal::of(&report.custody),
        report.custody.retryable(),
    )
}

/// After a warned, retryable run: re-run the leg at every reconnect of
/// `nest` until a run owes no warning or is not retryable, or the client is
/// dropped (signed out). Holds the client only weakly between connections.
/// Each run's signal goes to `sink`: the run that lands custody sends
/// [`CustodyLegSignal::Cleared`], which takes down the warning the first run
/// painted.
fn retry_on_next_connections<S>(
    nest: &Arc<NestClient>,
    store: Arc<S>,
    sink: SignalSink,
) -> impl std::future::Future<Output = ()> + Send + 'static
where
    S: DeploymentSeedStore + Send + Sync + 'static,
{
    let mut reconnects = nest.subscribe_reconnects();
    let nest = Arc::downgrade(nest);
    async move {
        loop {
            if reconnects.changed().await.is_err() {
                return; // The client is gone: signed out.
            }
            let Some(client) = nest.upgrade() else {
                return;
            };
            tracing::info!("deployment-seed custody leg: retrying on this connection");
            let (signal, retryable) = run_custody_leg_once(&client, &*store).await;
            let done = !retryable || signal == CustodyLegSignal::Cleared;
            sink(signal);
            if done {
                return;
            }
        }
    }
}

/// **The plane rotation drive** behind the admin surface's rotate action.
/// Resolves the bound id first, while the box still answers as the
/// predecessor, then runs `rotate_deployment_seed_on_plane` (custody merged
/// and published before dispatch). Every outcome collapses to ONE
/// [`LocalizedText`] — [`seed_rotation_verdict`] on a dispatch, the
/// `rotate_seed_failed` key otherwise — so every host renders it through the
/// same resolver. There is no fan-out step: the plane carries the successor's
/// row.
///
/// `resolve` is the host's own account-store accessor (`account_runtime::handle`
/// on linux and fauna-ffi, tui's settings slot), read **per call** through
/// [`ResolvingLedgerStore`]: a confirm clicked while the store is still
/// assembling waits out `fauna_client_config::LEDGER_READY_WAIT`, and only a
/// store that never comes is refused — plainly, before any dispatch. One
/// shape on every host, web's `rotateDeploymentSeed` included.
pub async fn rotate_deployment_seed<F, H>(nest: &Arc<NestClient>, resolve: F) -> LocalizedText
where
    F: Fn() -> Option<H> + Send + Sync,
    H: DeploymentSeedStore,
{
    match rotate_deployment_seed_outcome(nest, resolve).await {
        Ok(outcome) => seed_rotation_verdict(&outcome),
        Err(cause) => LocalizedText::key_arg("admin.nest_page.rotate_seed_failed", "cause", cause),
    }
}

/// [`rotate_deployment_seed`]'s outcome itself, for a host that paints more
/// than the verdict — fauna-ffi's `FfiSeedRotationResult` also carries whether
/// the box's identity flipped. `Err` is the cause of a refusal (nothing
/// rotated), as [`deployment_seed::rotate_deployment_seed_bound`] words it.
///
/// # Errors
///
/// The bound id did not resolve, or the drive refused before or at dispatch.
pub async fn rotate_deployment_seed_outcome<F, H>(
    nest: &Arc<NestClient>,
    resolve: F,
) -> Result<SeedRotation, String>
where
    F: Fn() -> Option<H> + Send + Sync,
    H: DeploymentSeedStore,
{
    let store = ResolvingLedgerStore::new(resolve);
    deployment_seed::rotate_deployment_seed_bound(&**nest, &identity_of(nest), &store).await
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
                // The mint first: `connect()` alone is `Ok` over a dead box,
                // its supervisor retrying, and every cold read would then wait
                // out its request deadline instead of being skipped.
                match async {
                    nest.authenticate().await?;
                    nest.connect().await
                }
                .await
                {
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
