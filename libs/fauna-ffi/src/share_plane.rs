//! UniFFI host for the **cross-user share plane** (`docs/goal/behavior/p2p.md`
//! § Cross-user shared-set transfer) — the one host macOS, windows, iOS and
//! android share, so none of them re-derives the plane. tui and linux run the
//! same driver natively (`apps/fauna-{tui,linux}/src/share_glue.rs`); this
//! module is that host crossed once for the non-Rust-native apps.
//!
//! The host has **two arms and no phone-only fork**: the plane runs over a
//! **replica access** ([`FfiReplicaAccess`]), whoever hosts the replica — the
//! sync agent's ([`agent_replica_access`], the desktops) or the on-demand
//! hosts' ([`on_demand_replica_access`], a phone's provider;
//! `p2p-shared-set-build.md` § *Phone peers — design*, decision 1). A phone
//! is a **foreground** peer (decision 2): its shell starts the plane where a
//! desktop does and calls [`FfiNestClient::stop_share_plane`] when the app
//! leaves the foreground, which ends the driver and unbinds the seat, so a
//! backgrounded phone holds no socket.
//!
//! Everything app-agnostic stays on the Rust side: the driver
//! (`fauna_sync_engine::share_glue::run`), the five seam answers and the
//! durable advertisement sink (`fauna_client_share_host`), the bind door
//! (`fauna_sync_engine::offline_share::bind_share_plane_seat`, through the SAME
//! per-session seat slot the panel's door binds through —
//! `crate::offline_share::session_seat_for`), and the six readings, handed
//! across as `LocalizedText`. What crosses the boundary is only what genuinely
//! differs per app: the two repaint nudges ([`FfiSharePlaneListener`]), the
//! spool directory, and the replica access (the plane's two verbs ride it).
//!
//! ⚠ The state reading "Limited by {source}" carries an i18n KEY as its
//! argument (`features.tier_*`): a leg resolves it nested, never with a plain
//! lookup, or the raw key reaches the user (the finding both Rust legs pin).

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinHandle;

use fauna_client_share_host::ShareHostSeams;
use fauna_core::feature_gate::EffectivePolicy;
use fauna_core::folder_keys::FolderEngineKeys;
use fauna_core::localized::LocalizedText;
use fauna_sync_engine::offline_share::{CeremonySeat, SessionSeat};
use fauna_sync_engine::share_glue::{
    ReplicaAccess, SharePlane, SharePlaneCell, SharePlaneHost, pump_interval_from_env,
    serve_status_label, transfer_name_label, transfer_progress_label, transfer_state_label,
};

use crate::nest_client::FfiNestClient;
use crate::sync_agent_provisioning::FfiSyncAgentProvisioner;
use crate::{FfiError, general_err, keypair_from_bytes};

/// The app's two repaint nudges. Neither carries data: the app re-reads
/// [`share_plane_view`] — one observable signal and one authoritative read.
#[uniffi::export(with_foreign)]
pub trait FfiSharePlaneListener: Send + Sync {
    /// The seat is serving the plane now. The panel's own seat is the same
    /// seat (one slot per session), so this is only a repaint.
    fn seat_bound(&self);
    /// The plane's state moved — repaint the transfer surface.
    fn state_changed(&self);
}

/// One peer transfer row — the `share-transfer-item` readings.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiShareTransfer {
    /// `share-transfer-name`: the folder and whose device it came from.
    pub name: LocalizedText,
    /// `share-transfer-progress`: files and rows so far.
    pub progress: LocalizedText,
    /// `share-transfer-state`: done, in progress, or the gate's refusal
    /// ("Limited by {source}", `{source}` itself a key — resolve nested).
    pub state: LocalizedText,
}

/// The whole transfer surface for one paint — `share-serve-status` plus the
/// `share-transfer-list`'s rows.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FfiSharePlaneView {
    pub serve_status: LocalizedText,
    pub transfers: Vec<FfiShareTransfer>,
}

/// The plane's two seams over one replica host — what
/// [`FfiNestClient::start_share_plane`] runs on. Built by
/// [`agent_replica_access`] (a desktop: the sync agent hosts the replica) or
/// [`on_demand_replica_access`] (a phone: its provider's on-demand hosts do).
#[derive(uniffi::Object)]
pub struct FfiReplicaAccess(ReplicaAccess);

/// The replica access over the app's sync-agent provisioner — the desktops'
/// arm: serve info and the ingest door both ride the agent's control client.
#[uniffi::export]
pub fn agent_replica_access(provisioner: Arc<FfiSyncAgentProvisioner>) -> Arc<FfiReplicaAccess> {
    Arc::new(FfiReplicaAccess(provisioner.share_access()))
}

/// The replica access over this process's on-demand hosts for `actor_id` —
/// the phones' arm. The hosts are the provider's own
/// (`FfiFileProviderHost::app_dead_owned_tree` registers each one it builds),
/// read afresh at every pass: a set is served while its host lives.
#[cfg(feature = "file-provider-host")]
#[uniffi::export]
pub fn on_demand_replica_access(actor_id: Vec<u8>) -> Result<Arc<FfiReplicaAccess>, FfiError> {
    let actor_id = crate::crypto::bytes32(&actor_id, "actor_id")?;
    Ok(Arc::new(FfiReplicaAccess(
        crate::file_provider_host::on_demand_share_access_for(actor_id),
    )))
}

/// One on-demand replica hosted in ANOTHER process, as the app names it —
/// iOS's File Provider extension, the set's one writer
/// (`p2p-shared-set-build.md` § *Phone peers — design*, decision 3). The
/// plane reads the set's state DB cross-process (derived here from
/// `state_dir` and `folder_id`, the same key the extension's host opens) and
/// serves bodies from the staging root alone: the OS owns the hydrated tree.
#[cfg(feature = "file-provider-host")]
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct FfiForeignReplica {
    /// The nest folder name — the label the transfer surface shows.
    pub folder: String,
    /// The set's `FolderRef` wire string (the domain identity's ref half).
    pub folder_id: String,
    /// The account's engine state dir, as the extension's host was given it.
    pub state_dir: String,
    /// The extension's staging root for the set — iOS's kept root.
    pub staging_root: String,
}

/// One replica host in another process, as the app reaches it. Called from a
/// blocking thread, never the plane's runtime: an implementation may wait on
/// its process channel (iOS: the File Provider service connection).
#[cfg(feature = "file-provider-host")]
#[uniffi::export(with_foreign)]
pub trait FfiForeignReplicaHost: Send + Sync {
    /// The replica this host serves, or `None` while it offers none.
    fn replica(&self) -> Option<FfiForeignReplica>;

    /// The ingest door, answered by the other process's host
    /// ([`crate::file_provider_host::FfiFileProviderHost::share_ingest`]):
    /// one page of accepted peer rows, the pre-fetched bodies at
    /// `spool_dir` — a directory both processes reach.
    fn share_ingest(
        &self,
        proven_actor_hex: String,
        rows: Vec<Vec<u8>>,
        spool_dir: String,
    ) -> Result<crate::file_provider_host::FfiShareIngestOutcome, FfiError>;
}

/// The app's foreign replica hosts for the signed-in account, read afresh at
/// every pass of the plane (a domain comes and goes with its registration).
#[cfg(feature = "file-provider-host")]
#[uniffi::export(with_foreign)]
pub trait FfiForeignReplicaHosts: Send + Sync {
    fn hosts(&self) -> Vec<Arc<dyn FfiForeignReplicaHost>>;
}

/// The replica access over hosts in another process — iOS's arm, where the
/// File Provider extension is each set's one writer and
/// [`on_demand_replica_access`] (this process's registry) sees none of them.
/// Serve info reads each set's state DB and staging root from this process;
/// the ingest door crosses to the extension through the host.
#[cfg(feature = "file-provider-host")]
#[uniffi::export]
pub fn foreign_replica_access(hosts: Arc<dyn FfiForeignReplicaHosts>) -> Arc<FfiReplicaAccess> {
    Arc::new(FfiReplicaAccess(
        fauna_sync_engine::share_glue::on_demand_share_access(Arc::new(ForeignHosts(hosts))),
    ))
}

#[cfg(feature = "file-provider-host")]
struct ForeignHosts(Arc<dyn FfiForeignReplicaHosts>);

#[cfg(feature = "file-provider-host")]
impl fauna_sync_engine::share_glue::OnDemandHosts for ForeignHosts {
    fn hosts(&self) -> Vec<Arc<dyn fauna_sync_engine::share_glue::OnDemandReplicaHost>> {
        self.0
            .hosts()
            .into_iter()
            .map(|host| {
                Arc::new(ForeignHost(host))
                    as Arc<dyn fauna_sync_engine::share_glue::OnDemandReplicaHost>
            })
            .collect()
    }
}

#[cfg(feature = "file-provider-host")]
struct ForeignHost(Arc<dyn FfiForeignReplicaHost>);

/// The plane's replica as a foreign host describes it: the state DB keyed by
/// the set's ref under the given state dir, the staging root as the kept
/// root, no cache root. A replica whose ref does not parse is none.
#[cfg(feature = "file-provider-host")]
fn foreign_replica(r: FfiForeignReplica) -> Option<fauna_sync_engine::share_glue::OnDemandReplica> {
    let folder_ref = match crate::sync_engine_host::parse_folder_id(&r.folder_id) {
        Ok(folder_ref) => folder_ref,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "share plane: a foreign replica names no set; unserved this pass"
            );
            return None;
        }
    };
    // The extension's host creates the state DB; until it has, there is no
    // replica to read, and opening it here would create an empty one.
    let db_path = folder_ref.state_db_path(std::path::Path::new(&r.state_dir));
    if !db_path.is_file() {
        return None;
    }
    Some(fauna_sync_engine::share_glue::OnDemandReplica {
        folder: r.folder,
        folder_id: folder_ref.to_wire(),
        db_path,
        kept_root: PathBuf::from(r.staging_root),
        cache_root: None,
    })
}

#[cfg(feature = "file-provider-host")]
#[async_trait::async_trait]
impl fauna_sync_engine::share_glue::OnDemandReplicaHost for ForeignHost {
    async fn replica(&self) -> Option<fauna_sync_engine::share_glue::OnDemandReplica> {
        let host = Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || host.replica())
            .await
            .ok()
            .flatten()
            .and_then(foreign_replica)
    }

    async fn share_ingest(
        &self,
        proven_actor_hex: &str,
        rows: Vec<Vec<u8>>,
        spool_dir: &std::path::Path,
    ) -> anyhow::Result<fauna_sync_engine::share_pump::ShareIngestSummary> {
        let host = Arc::clone(&self.0);
        let (actor, spool) = (
            proven_actor_hex.to_string(),
            spool_dir.to_string_lossy().into_owned(),
        );
        let outcome = tokio::task::spawn_blocking(move || host.share_ingest(actor, rows, spool))
            .await
            .map_err(|e| anyhow::anyhow!("the foreign ingest door's thread ended: {e}"))?
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(fauna_sync_engine::share_pump::ShareIngestSummary {
            refused: outcome.refused,
            overlaid: outcome.overlaid,
            materialized: outcome.materialized,
            already_current: outcome.already_current,
            cursor: outcome.cursor,
            storage_limited: outcome.storage_limited,
        })
    }
}

/// One running plane: the paint cell its driver writes, the handle that stops
/// that driver, and the session's seat slot the driver binds through.
/// Dropping the plane stops the driver, so a slot cannot lose track of a
/// plane without also stopping it — the one-driver-per-session rule (`p2p.md`
/// § Cross-user shared-set transfer → Implementation status today) holds by
/// construction, not by each caller remembering.
struct RunningPlane {
    cell: SharePlaneCell,
    /// `None` only once [`stop_plane`] has taken it.
    driver: Option<JoinHandle<()>>,
    seat: SessionSeat,
}

impl Drop for RunningPlane {
    fn drop(&mut self) {
        // The driver has no cleanup after its loop (`share_glue::run` returns
        // straight out of it): a stop at any await point drops the same locals
        // its own exit does, and a pass cut mid-way is what an app crash there
        // would leave — the spool is transient by contract and the ingest lands
        // through the state writer's own transaction.
        if let Some(driver) = &self.driver {
            driver.abort();
        }
    }
}

/// This session's plane, once [`FfiNestClient::start_share_plane`] ran.
/// `None` means the plane is not running on this device this session — a
/// different fact from serving no sets, and one the surface renders as
/// nothing at all (the linux leg's finding).
static PLANE: Mutex<Option<RunningPlane>> = Mutex::new(None);

/// Start `driver` as `slot`'s plane, replacing (and so stopping) the plane it
/// already holds. The one place a driver is spawned, so "which driver is
/// running" and "which cell is painted" cannot drift apart. The previous
/// driver is stopped BEFORE the next exists, so the two never pump the same
/// account side by side; the lock spans both steps, so two racing starts
/// (an app that re-enters while the first call is still awaiting its inputs)
/// leave the later one running.
fn install_plane(
    slot: &Mutex<Option<RunningPlane>>,
    cell: SharePlaneCell,
    seat: SessionSeat,
    driver: impl Future<Output = ()> + Send + 'static,
) {
    let mut slot = slot.lock().unwrap_or_else(|e| e.into_inner());
    drop(slot.take());
    *slot = Some(RunningPlane {
        cell,
        driver: Some(tokio::spawn(driver)),
        seat,
    });
}

/// Forget `slot`'s plane and stop its driver.
fn clear_plane(slot: &Mutex<Option<RunningPlane>>) {
    *slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Stop `slot`'s plane for good: the slot is emptied, the driver is ended and
/// awaited, and the session's seat is unbound — so the listener is gone when
/// this returns, not merely no longer painted
/// (`fauna_sync_engine::share_glue::stop_plane`). A no-op with no plane
/// running.
async fn stop_plane(slot: &Mutex<Option<RunningPlane>>) {
    let stopped = slot.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(mut plane) = stopped else {
        return;
    };
    if let Some(driver) = plane.driver.take() {
        fauna_sync_engine::share_glue::stop_plane(driver, &plane.seat).await;
    }
}

/// The transfer surface's readings, or `None` when the plane is not running.
#[uniffi::export]
pub fn share_plane_view() -> Option<FfiSharePlaneView> {
    let cell = PLANE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .map(|plane| Arc::clone(&plane.cell))?;
    let state = cell.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Some(view_of(&state))
}

fn view_of(state: &fauna_sync_engine::share_glue::SharePlaneState) -> FfiSharePlaneView {
    FfiSharePlaneView {
        serve_status: serve_status_label(state.status),
        transfers: state
            .outcomes
            .iter()
            .map(|outcome| FfiShareTransfer {
                name: transfer_name_label(outcome),
                progress: transfer_progress_label(outcome),
                state: transfer_state_label(outcome),
            })
            .collect(),
    }
}

/// Forget this session's plane and stop its driver — the account runtime's
/// teardown calls it, ahead of the store's close (which would end the driver
/// on its own a moment later).
pub(crate) fn forget() {
    clear_plane(&PLANE);
}

/// How long [`FfiNestClient::start_share_plane`] waits for EACH of the two things
/// the plane rests on. Both land on tasks of their own after the calls that start
/// them have returned — `start_account_runtime` returns as soon as its assembly is
/// spawned, and the conversations session is stashed when the app's own build
/// finishes — so an app has no edge to await and would otherwise have to poll. Each
/// app hand-writing that poll is exactly the per-app duplication the shared host
/// exists to remove, so the wait lives here, once.
///
/// Generous on purpose: the window only matters when an input is slow (a cold
/// launch under load), and a plane started late is a plane started, where one that
/// gave up waits for the next launch. It is a ceiling, never a delay — the wait ends
/// the moment the input lands.
const INPUTS_WAIT: Duration = Duration::from_secs(180);
const INPUTS_POLL: Duration = Duration::from_millis(250);

/// Poll `probe` every `poll` until it yields, or give up at `deadline` (`None`).
/// The probe runs once before any sleep, so an input that is already there costs
/// nothing.
async fn wait_for<T>(
    deadline: Duration,
    poll: Duration,
    mut probe: impl FnMut() -> Option<T>,
) -> Option<T> {
    let end = tokio::time::Instant::now() + deadline;
    loop {
        if let Some(found) = probe() {
            return Some(found);
        }
        if tokio::time::Instant::now() >= end {
            return None;
        }
        tokio::time::sleep(poll).await;
    }
}

/// Run `drive` on `first`, then — each time the run ends — on the next LIVE
/// handle `current` yields (`is_live`), until none comes within `deadline`.
///
/// The share plane's driver ends with the runtime it serves, while the slot
/// it came from (`crate::account_runtime::handle`) may already hold the
/// runtime that replaced it. The `poll` pause after every run bounds a driver
/// that returns at once on a handle still reading live, so no run can spin.
async fn follow_runtime<H, Fut>(
    first: H,
    mut current: impl FnMut() -> Option<H>,
    is_live: impl Fn(&H) -> bool,
    deadline: Duration,
    poll: Duration,
    mut drive: impl FnMut(H) -> Fut,
) where
    Fut: Future<Output = ()>,
{
    let mut handle = first;
    loop {
        drive(handle).await;
        tokio::time::sleep(poll).await;
        let Some(next) = wait_for(deadline, poll, || current().filter(|h| is_live(h))).await else {
            tracing::info!(
                "share plane: the account runtime it served ended and none replaced it; \
                 the plane is down until it is started again"
            );
            return;
        };
        tracing::info!(
            "share plane: the account runtime it served ended; following the runtime \
             that replaced it"
        );
        handle = next;
    }
}

#[fauna_uniffi_async::export]
impl FfiNestClient {
    /// Start the share plane for the signed-in account: register the durable
    /// advertisement sink, then run the shared driver until the account store
    /// closes. Call it once [`Self::start_account_runtime`] and
    /// [`Self::conversations_session`] have both been *called* — the plane waits
    /// (bounded, [`INPUTS_WAIT`] each) for their results to land, so an app has no
    /// account-store-ready edge to observe and no ordering to get right, unlike
    /// tui and linux, which start theirs on that edge natively.
    ///
    /// Refuses (an `Err`, never a silent no-op) when the account runtime or the
    /// conversations session never lands within the wait — a failed assembly, or
    /// an app that never built a session — since the plane has no seam to run on
    /// without them. Starting it again replaces the running plane: the previous
    /// driver is stopped before the new one starts, so a session never pumps
    /// its account twice — an app lifecycle that re-fires its store-ready edge
    /// (macOS enters from two launch paths, a phone at every return to the
    /// foreground) is harmless, and the latest call's listener, access and
    /// spool are the ones in use.
    ///
    /// `access` is the replica host's two seams: [`agent_replica_access`] on
    /// a desktop, [`on_demand_replica_access`] on a phone.
    pub async fn start_share_plane(
        &self,
        owner_secret: Vec<u8>,
        access: Arc<FfiReplicaAccess>,
        spool_dir: String,
        listener: Arc<dyn FfiSharePlaneListener>,
    ) -> Result<(), FfiError> {
        let keypair = keypair_from_bytes(&owner_secret)?;
        let account = wait_for(INPUTS_WAIT, INPUTS_POLL, || {
            crate::account_runtime::handle().filter(|h| !h.is_closed())
        })
        .await
        .ok_or_else(|| general_err("the share plane needs the account runtime started"))?;
        let conversations = wait_for(INPUTS_WAIT, INPUTS_POLL, || {
            self.conversations_session_handle()
        })
        .await
        .ok_or_else(|| general_err("the share plane needs a conversations session"))?;
        let nest = self.nest_arc();
        let secret_hex = hex::encode(&owner_secret);

        let cell = SharePlaneCell::default();
        let opening_nudge = Arc::clone(&listener);
        let session_seat = crate::offline_share::session_seat_for(keypair.actor_id());

        let host = Arc::new(FfiShareHost {
            seams: ShareHostSeams {
                nest,
                secret_hex,
                conversations: conversations.clone(),
                folder_keys: crate::account_runtime::folder_key_store(),
            },
            session_seat: session_seat.clone(),
            listener,
        });
        let replica = access.0.clone();
        let own_actor = keypair.actor_id();
        let driver_cell = Arc::clone(&cell);
        let spool_root = PathBuf::from(spool_dir);
        // The driver serves ONE runtime and ends with it (`share_glue::run`
        // selects on `closed()`), but this process's runtime can be replaced
        // under a live plane: an app entering from two sign-in paths
        // assembles twice, and the first runtime exits as the second mounts.
        // A plane that took the outgoing handle would end at its next tick
        // and nothing would start it again — no lift, no fold, and a seat that
        // refuses every member's witness. So the plane follows the runtime:
        // each time the one it served ends, it runs again on whichever live
        // runtime replaced it. A teardown ends it for good (`forget`, which
        // aborts this task).
        let driver = follow_runtime(
            account,
            crate::account_runtime::handle,
            |h: &fauna_sync_engine::account_runtime::AccountStoreHandle| !h.is_closed(),
            INPUTS_WAIT,
            INPUTS_POLL,
            move |account| {
                // The sink writes through the runtime it was given: re-pointed
                // at each runtime the plane serves.
                fauna_client_share_host::install_durable_sink(&conversations, account.clone());
                fauna_sync_engine::share_glue::run(SharePlane {
                    host: Arc::clone(&host),
                    account,
                    replica: replica.clone(),
                    own_actor,
                    cell: Arc::clone(&driver_cell),
                    spool_root: spool_root.clone(),
                    pump_interval: pump_interval_from_env(),
                })
            },
        );
        install_plane(&PLANE, cell, session_seat, driver);
        // The opening reading, painted now rather than a whole pump cadence
        // later (the linux leg's finding): a device with nothing to serve
        // still says so. After the install, so the app's repaint reads the
        // plane it was just told about.
        opening_nudge.state_changed();
        Ok(())
    }

    /// Stop the share plane: end its driver and unbind the session's seat —
    /// what a phone's shell calls when the app leaves the foreground
    /// (`p2p-shared-set-build.md` § *Phone peers — design*, decision 2). The
    /// plane **stops, it does not pause**: when this returns the listener is
    /// gone, so a backgrounded process holds no socket it could not answer,
    /// and [`share_plane_view`] reads `None` — the surface renders nothing.
    /// [`Self::start_share_plane`] starts it again; the pull cursor and the
    /// overlay are at rest in each set's state DB, so a transfer the stop cut
    /// resumes with nothing re-sent. A no-op with no plane running.
    pub async fn stop_share_plane(&self) {
        stop_plane(&PLANE).await;
    }
}

/// The FFI's `SharePlaneHost`: five answers delegated to the shared seams, the
/// bind through the session's one seat slot, and the two nudges across the
/// boundary.
struct FfiShareHost {
    seams: ShareHostSeams,
    session_seat: SessionSeat,
    listener: Arc<dyn FfiSharePlaneListener>,
}

#[async_trait::async_trait]
impl SharePlaneHost for FfiShareHost {
    type Seat = CeremonySeat;

    fn membership(&self) -> Arc<dyn fauna_peer_share::admission::SetMembership + Send + Sync> {
        self.seams.membership()
    }

    async fn bind_seat(
        &self,
        evidence: Option<fauna_sync_engine::peer_leg::PeerLegNestFacts>,
        membership: &Arc<dyn fauna_peer_share::admission::SetMembership + Send + Sync>,
        group_roster: &Arc<dyn fauna_peer_share::admission::GroupRosterState + Send + Sync>,
    ) -> Result<(Arc<CeremonySeat>, Vec<std::net::SocketAddr>), String> {
        fauna_sync_engine::offline_share::bind_share_plane_seat(
            &self.session_seat,
            &self.seams.secret_hex,
            crate::offline_share::FFI_DEVICE_LABEL,
            evidence,
            membership,
            group_roster,
            &fauna_iroh::ceremony_transport(),
        )
        .await
    }

    fn seat_node<'a>(
        &self,
        seat: &'a CeremonySeat,
    ) -> &'a fauna_client_capabilities::group_ceremony_node::CeremonyNode {
        &seat.node
    }

    fn session_seat(&self) -> &SessionSeat {
        &self.session_seat
    }

    fn seat_bound(&self) {
        self.listener.seat_bound();
    }

    fn state_changed(&self) {
        self.listener.state_changed();
    }

    async fn live_nest_facts(
        &self,
    ) -> Result<fauna_sync_engine::peer_leg::PeerLegNestFacts, String> {
        self.seams.live_nest_facts().await
    }

    async fn transfer_policy(&self) -> Option<EffectivePolicy> {
        self.seams.transfer_policy().await
    }

    async fn key_bindings(&self) -> Result<Vec<FolderEngineKeys>, String> {
        self.seams.key_bindings().await
    }

    async fn send_share_endpoints(&self, channel_hex: &str, bytes: Vec<u8>) -> Result<(), String> {
        self.seams.send_share_endpoints(channel_hex, bytes).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_sync_engine::share_glue::{ServeStatus, SharePlaneState};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// A foreign replica (iOS's extension) reaches the plane keyed exactly as
    /// the extension's own host keys it: the state DB under the given state
    /// dir by the set's ref, the staging root as the kept root, no cache
    /// root — and a ref that does not parse is no replica at all.
    #[cfg(feature = "file-provider-host")]
    #[test]
    fn a_foreign_replica_is_keyed_as_the_extensions_host_keys_it() {
        let state = tempfile::tempdir().unwrap();
        let named = |state_dir: &std::path::Path| FfiForeignReplica {
            folder: "Cabin".into(),
            folder_id: "local:1".into(),
            state_dir: state_dir.to_string_lossy().into_owned(),
            staging_root: "/group/FileProvider/roots/aa/local%3A1".into(),
        };
        assert!(
            foreign_replica(named(state.path())).is_none(),
            "no state DB yet — the extension's host has not built the set"
        );
        let folder_ref = crate::sync_engine_host::parse_folder_id("local:1").unwrap();
        std::fs::write(folder_ref.state_db_path(state.path()), b"").unwrap();
        let replica = foreign_replica(named(state.path())).expect("a replica");
        assert_eq!(replica.db_path, folder_ref.state_db_path(state.path()));
        assert_eq!(
            replica.kept_root,
            PathBuf::from("/group/FileProvider/roots/aa/local%3A1")
        );
        assert_eq!(replica.cache_root, None, "the OS owns the hydrated tree");
        assert_eq!(replica.folder_id, folder_ref.to_wire());
        assert!(
            foreign_replica(FfiForeignReplica {
                folder: "Cabin".into(),
                folder_id: "Cabin".into(),
                state_dir: "/s".into(),
                staging_root: "/r".into(),
            })
            .is_none(),
            "a set name is not a ref"
        );
    }

    /// A runtime handle for the follow loop: an id, and a closed flag the
    /// "runtime" flips when it shuts down.
    #[derive(Clone)]
    struct FakeRuntime {
        id: usize,
        closed: Arc<AtomicBool>,
    }

    impl FakeRuntime {
        fn new(id: usize) -> Self {
            Self {
                id,
                closed: Arc::new(AtomicBool::new(false)),
            }
        }
    }

    /// The macOS in-person regression: the plane took the first runtime's
    /// handle a moment before the app's second assembly replaced it. The run
    /// on the outgoing runtime ends, and the plane runs again on the
    /// replacement, never on the closed one the slot may still hold.
    #[tokio::test(start_paused = true)]
    async fn the_plane_follows_the_runtime_that_replaced_the_one_it_served() {
        let first = FakeRuntime::new(1);
        let second = FakeRuntime::new(2);
        let slot = Arc::new(Mutex::new(first.clone()));
        let served = Arc::new(Mutex::new(Vec::new()));

        let follow = {
            let slot = Arc::clone(&slot);
            let reader = Arc::clone(&slot);
            let served = Arc::clone(&served);
            let second = second.clone();
            follow_runtime(
                first.clone(),
                move || Some(reader.lock().unwrap().clone()),
                |h: &FakeRuntime| !h.closed.load(Ordering::SeqCst),
                Duration::from_secs(5),
                Duration::from_millis(250),
                move |h: FakeRuntime| {
                    served.lock().unwrap().push(h.id);
                    let slot = Arc::clone(&slot);
                    let second = second.clone();
                    async move {
                        if h.id == 1 {
                            // The first runtime exits; the slot still names it
                            // for a moment before the second mounts.
                            h.closed.store(true, Ordering::SeqCst);
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            *slot.lock().unwrap() = second;
                        } else {
                            // The replacement serves until it too ends, and
                            // nothing replaces it.
                            h.closed.store(true, Ordering::SeqCst);
                        }
                    }
                },
            )
        };
        follow.await;

        assert_eq!(
            *served.lock().unwrap(),
            vec![1, 2],
            "one run on the outgoing runtime, then one on its replacement"
        );
    }

    /// With no live runtime to follow, the loop gives up at its deadline
    /// rather than waiting for ever or re-running on the closed handle.
    #[tokio::test(start_paused = true)]
    async fn with_no_replacement_the_plane_stops_at_the_deadline() {
        let only = FakeRuntime::new(1);
        let runs = Arc::new(AtomicUsize::new(0));
        let follow = {
            let only = only.clone();
            let runs = Arc::clone(&runs);
            follow_runtime(
                only.clone(),
                move || Some(only.clone()),
                |h: &FakeRuntime| !h.closed.load(Ordering::SeqCst),
                Duration::from_secs(5),
                Duration::from_millis(250),
                move |h: FakeRuntime| {
                    runs.fetch_add(1, Ordering::SeqCst);
                    h.closed.store(true, Ordering::SeqCst);
                    async {}
                },
            )
        };
        follow.await;
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    /// The surface's readings are the shared labels verbatim — the FFI never
    /// makes a second decision about what a state reads as, so the four
    /// native apps paint what tui and linux paint.
    #[test]
    fn the_view_is_the_shared_labels_verbatim() {
        let state = SharePlaneState {
            status: ServeStatus::Serving(2),
            ..SharePlaneState::default()
        };
        let view = view_of(&state);
        assert_eq!(
            view.serve_status,
            serve_status_label(ServeStatus::Serving(2))
        );
        assert!(view.transfers.is_empty());
    }

    /// No plane started this session reads as `None` — "this device is not
    /// running the plane", never the "no shared folders" line, which is a
    /// different statement (the linux leg's finding).
    #[tokio::test]
    async fn no_plane_is_no_view_rather_than_an_empty_one() {
        forget();
        assert!(share_plane_view().is_none());
        install_plane(
            &PLANE,
            SharePlaneCell::default(),
            SessionSeat::default(),
            std::future::pending(),
        );
        assert_eq!(
            share_plane_view().map(|v| v.serve_status),
            Some(serve_status_label(ServeStatus::NoSets))
        );
        forget();
        assert!(share_plane_view().is_none());
    }

    /// The two inputs land on their own tasks after the calls that start them
    /// return, so the start waits for them rather than refusing on a race the
    /// app cannot observe: an input that lands late is picked up, and one that
    /// never lands gives up at the deadline instead of waiting for ever.
    #[tokio::test]
    async fn a_late_input_is_awaited_and_a_missing_one_gives_up() {
        let mut probes = 0;
        let landed = wait_for(Duration::from_secs(5), Duration::from_millis(1), || {
            probes += 1;
            (probes >= 3).then_some(probes)
        })
        .await;
        assert_eq!(landed, Some(3));

        let never: Option<()> =
            wait_for(Duration::from_millis(30), Duration::from_millis(1), || None).await;
        assert_eq!(never, None);
    }

    /// Counts itself while it lives, so a test can say how many drivers run.
    struct LiveDriver(Arc<AtomicUsize>);

    impl LiveDriver {
        fn start(count: &Arc<AtomicUsize>) -> Self {
            count.fetch_add(1, Ordering::SeqCst);
            Self(Arc::clone(count))
        }
    }

    impl Drop for LiveDriver {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }

    async fn counted_driver(count: Arc<AtomicUsize>) {
        let _live = LiveDriver::start(&count);
        std::future::pending::<()>().await;
    }

    /// Let every spawned task run to its next await, and every aborted one be
    /// dropped: the test runtime is current-thread, so nothing moves until the
    /// test yields.
    async fn settle() {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
    }

    /// `start_share_plane` documents that starting again REPLACES the running
    /// plane. Replacing only the paint cell left the first driver pumping the
    /// same account beside the second (two passes re-lending one seat's roster,
    /// two pulls into one spool) until the account store closed — and an app
    /// lifecycle that re-fires its store-ready edge reaches exactly that, macOS
    /// twice on an e2e launch. Two starts leave one driver.
    #[tokio::test]
    async fn starting_again_leaves_exactly_one_driver_running() {
        let slot = Mutex::new(None);
        let live = Arc::new(AtomicUsize::new(0));

        install_plane(
            &slot,
            SharePlaneCell::default(),
            SessionSeat::default(),
            counted_driver(Arc::clone(&live)),
        );
        settle().await;
        assert_eq!(live.load(Ordering::SeqCst), 1);

        install_plane(
            &slot,
            SharePlaneCell::default(),
            SessionSeat::default(),
            counted_driver(Arc::clone(&live)),
        );
        settle().await;
        assert_eq!(
            live.load(Ordering::SeqCst),
            1,
            "the first driver must stop when the plane is replaced"
        );
    }

    /// The account runtime's teardown forgets the plane ahead of the store's
    /// close; forgetting stops the driver with it, so no plane is recorded as
    /// gone while its driver still runs (and no driver runs unrecorded).
    #[tokio::test]
    async fn forgetting_the_plane_stops_its_driver() {
        let slot = Mutex::new(None);
        let live = Arc::new(AtomicUsize::new(0));

        install_plane(
            &slot,
            SharePlaneCell::default(),
            SessionSeat::default(),
            counted_driver(Arc::clone(&live)),
        );
        settle().await;
        assert_eq!(live.load(Ordering::SeqCst), 1);

        clear_plane(&slot);
        settle().await;
        assert_eq!(live.load(Ordering::SeqCst), 0);
        assert!(slot.lock().unwrap().is_none());
    }

    /// `stop_share_plane` is not a forget: when it returns the driver is
    /// already gone (awaited, not merely told to stop) and the slot is empty,
    /// so the view reads "not running". Stopping again, or with no plane, is
    /// a no-op. (That the seat's listener is gone with it is asserted on the
    /// transport in `fauna_sync_engine::offline_share`'s own test of the
    /// shared stop.)
    #[tokio::test]
    async fn stopping_the_plane_ends_its_driver_before_it_returns() {
        let slot = Mutex::new(None);
        let live = Arc::new(AtomicUsize::new(0));
        stop_plane(&slot).await;

        install_plane(
            &slot,
            SharePlaneCell::default(),
            SessionSeat::default(),
            counted_driver(Arc::clone(&live)),
        );
        settle().await;
        assert_eq!(live.load(Ordering::SeqCst), 1);

        stop_plane(&slot).await;
        assert_eq!(
            live.load(Ordering::SeqCst),
            0,
            "the driver must be gone when the stop returns"
        );
        assert!(slot.lock().unwrap().is_none());
        stop_plane(&slot).await;
    }
}
