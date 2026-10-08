use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, broadcast, mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::config::{SyncConfig, SyncPaths};
use crate::engine_driver::RunningEngines;

/// Idle poll cadence shared by [`crate::renewal`] and [`crate::custodian`] — both
/// are "nothing provisioned/assigned yet, look again shortly" loops with no event
/// to wait on. One constant so the two independently-motivated call sites can't
/// silently drift onto different cadences.
pub(crate) const IDLE_RECHECK_SECS: u64 = 60;

/// Shared state for the FaunaSync service.
#[allow(dead_code)]
pub struct SyncServiceState {
    pub config: RwLock<SyncConfig>,
    /// Resolved filesystem layout (config + state DBs + chunk-cache). Carries the
    /// `--data-dir` override so every handler writes/reads the same root the
    /// startup config was loaded from. See [`SyncPaths`].
    pub paths: SyncPaths,
    pub start_time: std::time::Instant,
    pub shutdown_tx: watch::Sender<bool>,
    /// The multi-root sync host: the shared multi-engine driver serving one
    /// bearer-only `SyncEngine` per bound folder — a cfapi sync root for an
    /// on-demand folder, a watcher + upload loop for an always-resident one.
    /// `None` until the first eligible folder is served (a folder bound to a file
    /// set, plus a provisioned capability). See [`crate::engine_driver`].
    pub engines: Mutex<Option<RunningEngines>>,
    pub ipc_event_tx: broadcast::Sender<fauna_ipc::sync::Event>,
    pub http_client: reqwest::Client,
    /// App-provisioned on-demand hydration capability (BackupKey + actor_id + renewable
    /// bearer + renewal device key). The live in-memory copy; since A2
    /// (sync-agent.md § Credential model, ratified 2026-07-19) it is **also
    /// persisted** in the per-user credential store (`crate::credentials`,
    /// namespace `fauna-sync-agent`) and restored at boot, so the agent runs
    /// app-dead — the identity seed still never reaches this process
    /// (`key-material-hierarchy.md` rules #6/#7). Wrapped in `Arc` so the hydration
    /// engine's [`CapabilityBearer`](crate::bearer::CapabilityBearer) shares the same
    /// slot and sees `RefreshBearer` / self-renewal updates without an engine rebuild.
    ///
    /// A provisioned capability is also the "connected to a nest" signal (replaces
    /// the defunct `device_config.is_some()` check). See `pipe_server.rs` status handlers.
    pub capability: Arc<crate::bearer::CapabilitySlot>,
    /// The per-user credential store the capability persists in
    /// (`crate::credentials`; sync-agent.md § Credential model). `None` in unit
    /// tests — persistence is then a no-op, so tests never touch the OS
    /// keychain/Secret Service (testing.md § point 10 applied to the agent's
    /// own tests). `run_agent` injects the production store.
    pub credentials: Option<Arc<fauna_credential_store::CredentialStore>>,
    /// The standing renewal refusal, mirrored in memory from its persisted
    /// record ([`crate::credentials::RefusalRecord`]) so the status reply's
    /// `needs_reenrollment` stays a field read. Written only by
    /// [`crate::renewal`]: set by a refused renewal, cleared by a successful
    /// one and by un-provisioning — never by a pushed bearer
    /// (`sync-agent-credentials.md` § Credential model → *A refused renewal is
    /// terminal*).
    pub renewal_refusal: std::sync::RwLock<Option<crate::credentials::RefusalRecord>>,
    /// Wakes the renewal loop to re-read the machine's evidence now — fired by
    /// the provision and `RefreshBearer` handlers while a refusal stands, so a
    /// re-enrolled machine is asked about within one convergence tick.
    pub renewal_wake: tokio::sync::Notify,
    /// The actor this agent can renew for as the machine's **store principal**
    /// — the cached backing of the `GetServiceStatus` principal-support
    /// advertisement (`sync-agent.md` § Credential model → the RULED
    /// 2026-08-15 block, decision 4; wire field
    /// `ServiceStatusInfo::store_principal_actor`).
    ///
    /// Cached rather than computed per status call because the answer costs a
    /// keyring round trip and the status call rides the app's convergence tick.
    /// [`crate::renewal::refresh_store_principal_presence`] refreshes it where
    /// the answer can actually change — a provision (the actor becomes known or
    /// changes) and each renewal (an app may have enrolled the machine since).
    pub store_principal_actor: RwLock<Option<[u8; 32]>>,
    /// Retained WS-RPC control-plane client for the shell extension's nest-backed
    /// verbs (`ListFileVersions` / `RestoreFileVersion`).
    ///
    /// **Why retained.** These run on Explorer's right-click path. Rebuilding +
    /// reconnecting a WebSocket per menu open would add a TLS + WS handshake to
    /// every right-click on a synced file.
    ///
    /// **Why retaining is safe across a bearer refresh.** The client is built over a
    /// [`CapabilityBearer`](crate::bearer::CapabilityBearer) sharing
    /// [`Self::capability`], which reads the *current* token per request — so a
    /// `RefreshBearer` IPC updates the token this client presents with no rebuild
    /// (the same property the hydration engine relies on). Only `nest_url` /
    /// `actor_id` are baked in at build time, so the cache is keyed on those and
    /// rebuilt if the app ever re-provisions against a different nest or actor.
    ///
    /// **Why a failed request does not rebuild it.** The client's reconnect
    /// supervisor redials on its own after the socket goes away (sleep/resume, a
    /// nest restart), so a request failing on it is never a reason to throw it
    /// away — only a supervisor that has *stopped* is, and [`Self::nest_rpc_client`]
    /// checks for exactly that. Rebuilding per failure restarted the dial curve at
    /// its 1 s floor once a minute on a machine whose credential had died, one
    /// more reason the 2026-09-24 flood dialled far above a single ladder
    /// (`transport-connection.md` § Connection lifecycle).
    ///
    /// Separate from the hydration engine's own client: `SyncEngine::control_plane()`
    /// is `pub(crate)` to `fauna-sync-engine` and unreachable from here, and the
    /// engine only exists once a folder is bound + served.
    pub nest_rpc: Mutex<Option<RetainedNestRpc>>,
    /// Per-folder remote-change nudge senders (`file-sync.md` § Remote-change
    /// nudge). Each running always-resident engine registers its wake sender
    /// here under its folder name; the [`PullFolderNow`](fauna_ipc::sync::RequestMethod::PullFolderNow)
    /// IPC — sent by a client that received a `PushEvent::SyncChanged` — looks it
    /// up and signals an immediate off-cadence pull. Best-effort: a missing entry
    /// (set not resident) or a full/closed channel just drops the nudge, and the
    /// rescan tick is the backstop. Bounded capacity 1, so rapid saves coalesce
    /// to a single pending pull.
    pub wake_senders: Mutex<HashMap<String, mpsc::Sender<()>>>,
    /// The same-account peer data plane's file halves on this host — the
    /// sibling registry every engine asks before the nest, and the door the
    /// peer leg serves a sibling's chunk want through ([`crate::peer_files`]).
    pub peer_files: crate::peer_files::PeerFiles,
    /// Per-folder count of deletes the **mass-delete floor** held on that
    /// set's last reconcile pass (`file-sync.md` § Files Appear Automatically).
    /// Each running engine's progress drain writes its set's latest verdict
    /// here — zero included — and [`ListEngines`](fauna_ipc::sync::RequestMethod::ListEngines)
    /// reads it into [`EngineInfo::deletes_held`](fauna_ipc::sync::EngineInfo::deletes_held).
    ///
    /// **In memory on purpose, and this is the load-bearing part.** The hold
    /// itself is derived — every reconcile re-evaluates it, nothing is stored,
    /// which is what makes a crash mid-hold unable to strand state. Keeping the
    /// *report* in memory gives it exactly that same lifetime: a restarted
    /// agent has no entry and answers `0` (nothing observed yet) until its
    /// first pass, instead of resurrecting a count from disk that no live
    /// engine stands behind — which is precisely the number the app-side
    /// *"apply N deletions"* affordance must never be handed.
    ///
    /// A missing entry is therefore not an error: the set is unbound, not yet
    /// served, or has not completed a pass. Sibling of [`Self::wake_senders`],
    /// same registration lifetime.
    pub deletes_held: Mutex<HashMap<String, u64>>,
    /// Per-folder count of deletes the delete rail **withheld because the path
    /// could not be read** on that set's last reconcile pass
    /// (`delete-propagation.md` § Unreadable is not absent) — the sibling of
    /// [`Self::deletes_held`], written by the same progress drain off
    /// `ProgressEvent::DeletesSkippedUnreadable` and read by `ListEngines` into
    /// [`EngineInfo::deletes_skipped_unreadable`](fauna_ipc::sync::EngineInfo::deletes_skipped_unreadable).
    ///
    /// A different fact from a hold, not a second count of one: a hold looked
    /// and found the folder empty (something to confirm); this could not look at
    /// all (nothing to confirm — the remedy is outside the app). In memory for
    /// the same reason as the hold: the count is derived every pass, so a
    /// restarted agent answers `0` until its first pass rather than resurrecting
    /// one no live engine stands behind.
    pub deletes_skipped_unreadable: Mutex<HashMap<String, u64>>,
    /// Per-folder **public-audience write arm** of the set's running engine
    /// (`ProgressEvent::PublicAudience`), keyed like [`Self::deletes_held`] and
    /// written by the same drain. The `ShareFile` IPC reads it: a file is a
    /// share target only while its set's engine holds the owner-attested public
    /// verdict (`apps/windows.md` § Shell Extension → *The Share hand-off*).
    /// In memory and fail-closed — an absent entry (no engine yet, or stopped)
    /// reads `false`.
    pub public_audience: Mutex<HashMap<String, bool>>,
    /// Per-file-set **command** senders — the invoke-and-reply sibling of
    /// [`Self::wake_senders`], same registration lifetime and `same_channel`
    /// deregistration discipline. Carries
    /// [`EngineCommand`](fauna_sync_engine::always_resident::EngineCommand)
    /// (today: `ApplyHeldDeletes`, the user-confirmed propagation of a
    /// mass-delete-floor hold) to the resident engine that owns the folder; the
    /// [`ApplyHeldDeletes`](fauna_ipc::sync::RequestMethod::ApplyHeldDeletes)
    /// IPC looks it up. A missing entry means the set is not being served by a
    /// resident engine — the handler reports that instead of inventing an
    /// engine.
    pub engine_cmd_senders:
        Mutex<HashMap<String, mpsc::Sender<fauna_sync_engine::always_resident::EngineCommand>>>,
    /// The custodian replica this process is hosting *right now*, published by
    /// [`crate::custodian::host_stint`] for the whole life of a stint and
    /// cleared in its tail — so an empty slot IS "this device is not hosting",
    /// with no second liveness signal that could disagree with it.
    ///
    /// Two callers read it, which is why it is one slot and not two:
    ///
    /// * [`ReclaimCustodianStore`](fauna_ipc::sync::RequestMethod::ReclaimCustodianStore)
    ///   (production) — deleting this device's sealed store must not happen
    ///   under a live writer, so the handler ends the stint through
    ///   [`HostedCustodian::cancel`] and reclaims once the slot is empty.
    /// * [`CustodianRunPassNow`](fauna_ipc::sync::RequestMethod::CustodianRunPassNow)
    ///   (test-only) — runs one pass on the *already-assembled* host rather than
    ///   building a second one. Building its own would be the silent bug: a
    ///   second host means a second store root and two devices' worth of
    ///   check-ins racing over one registry row.
    ///
    /// A second field carrying the cancel token would be a second liveness
    /// signal free to disagree with this one, which is the failure this slot's
    /// clear-on-every-exit-path discipline exists to rule out.
    ///
    /// The `Mutex` is what serializes two **poked** passes. It deliberately does
    /// not exclude the production loop's own periodic/push passes — those are
    /// driven inside `CustodianPull::run_loop`, which holds nothing of ours —
    /// and it does not need to: the periodic arm is `PERIODIC_INTERVAL` away and
    /// the push arm needs a real nest push, so the overlap window is negligible
    /// and a content-addressed pass is re-convergent anyway.
    pub hosted_custodian: Mutex<Option<HostedCustodian>>,
    /// The account whose store [`crate::account_host`] has mounted — or is
    /// assembling — as the app-dead runtime host; `None` once its stint's
    /// teardown has finished. Published by the stint itself, cleared in its
    /// tail, so an empty slot IS "nothing of this process is running out of the
    /// store" (the [`Self::hosted_custodian`] discipline, as a `watch` so a
    /// waiter is woken by the change rather than polling for it).
    ///
    /// Why it exists: this agent takes no account serving lock — it is
    /// *dismissed by command* before an app erases the store
    /// (`account-scoping.md` § Concurrent instances → *An erase refuses while a
    /// sibling serves the account*) — so the un-provision reply has to be the
    /// receipt that the mount is down. `unprovision_now` waits on this slot
    /// before it replies.
    pub hosted_account: watch::Sender<Option<[u8; 32]>>,
    /// Wakes the account host's stint to re-read the capability now, rather than
    /// at its next recheck tick — fired by every change to the capability slot
    /// the host keys its mount on (provision, un-provision).
    pub host_wake: tokio::sync::Notify,
    /// Wakes the custodian stint's watcher to re-check its capability and
    /// assignment now, rather than at its next rediscovery tick — fired by the
    /// same capability-slot changes as [`Self::host_wake`]
    /// (`crate::custodian::recheck_now`).
    pub custodian_wake: tokio::sync::Notify,
    /// The re-seed job's state ([`fauna_ipc::sync::RequestMethod::ReseedCustodianStore`],
    /// `backup-destinations.md` § Re-seed → *Where the ceremony runs*). While it
    /// reads `Running` this process is the store's writer in a second role, so
    /// the custodian loop hosts no stint and a reclaim refuses: one writer at a
    /// time, the same rule [`Self::hosted_custodian`] keeps.
    pub reseed_job: Mutex<fauna_ipc::sync::CustodianReseedState>,
    /// Every set's content keys as this agent last resolved them from its
    /// holder's custody (`crate::content_keys`; `on-demand-files.md` § Shared
    /// sets on a capability host → *One mechanism*). `None` until the first
    /// resolve lands (and again once nothing is provisioned) — no set gets an
    /// engine meanwhile. In memory only: the keys are re-read at every edge and
    /// never written to disk.
    pub content_keys: RwLock<Option<crate::content_keys::ResolvedContentKeys>>,
    /// Wakes [`crate::content_keys::run`] to re-resolve now — fired by every
    /// reconcile and by an engine's refresh edge.
    pub content_keys_wake: tokio::sync::Notify,
    /// The apps attached over IPC right now (`RequestMethod::AttachApp`
    /// leases) — while any is, the push arm leaves banners to the app
    /// ([`crate::push_arm`]).
    pub attached_apps: Arc<crate::push_arm::AttachedApps>,
    /// Where the push arm posts, and what `ServiceStatusInfo::notification_sink`
    /// reports.
    pub notification_sink: Arc<dyn crate::push_arm::NotificationSink>,
    /// Whether this agent can serve an on-demand binding here, answered once at
    /// boot ([`probe_on_demand`]) — what `ServiceStatusInfo::on_demand_available`
    /// reports. Unset until the boot probe ran: the status reply stays a field
    /// read.
    pub on_demand: std::sync::OnceLock<Result<(), &'static str>>,
    /// The on-demand roots running WITHOUT their placeholder surface right
    /// now, by bound path, each with the `ON_DEMAND_MOUNT_*` code of why —
    /// what `LocationInfo::on_demand_mount_error` reports. An entry lives
    /// exactly as long as the root that failed to mount
    /// ([`crate::engine_driver::MountReport`]), so a flip back to always, an
    /// unbind or a restart that mounts clears it. A plain mutex: every use is
    /// a map operation, never held across an await.
    pub on_demand_mount_errors: std::sync::Mutex<HashMap<std::path::PathBuf, &'static str>>,
    /// The account store [`crate::account_host`]'s live stint has mounted —
    /// published once the mount stands, cleared before its teardown. What
    /// [`Self::folder_keys`] reads through.
    pub mounted_store:
        Arc<std::sync::Mutex<Option<fauna_sync_engine::account_runtime::AccountStoreHandle>>>,
    /// The account's folder-key custody (`fauna.state.folder-keys`) read
    /// through the mounted store — every engine build and the content-key
    /// resolution read it (`on-demand-files.md` § Shared sets on a capability
    /// host, decision 1′). With no store mounted a read answers "not running":
    /// custody unreadable, so every bound set builds keyless (fail closed).
    pub folder_keys: Arc<dyn fauna_client_folders::FolderKeyReader>,
}

/// Can this agent serve an on-demand binding on this platform? `Err` carries the
/// `fauna_ipc::sync::ON_DEMAND_REASON_*` code. windows always can (cfapi); linux
/// asks its FUSE binding, which may block on a file open; elsewhere the surface
/// is not the agent's (macOS: the File Provider extension).
pub fn probe_on_demand() -> Result<(), &'static str> {
    #[cfg(windows)]
    {
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        crate::fuse_host::probe_available()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Err(fauna_ipc::sync::ON_DEMAND_REASON_NOT_AGENT_HOSTED)
    }
}

/// The published handle on a running custodian stint — what it is, and how to
/// stop it (see [`SyncServiceState::hosted_custodian`]).
#[derive(Clone)]
pub struct HostedCustodian {
    /// The assembled host the stint is driving.
    pub host: Arc<fauna_sync_engine::custodian_host::CustodianHost>,
    /// Ends the stint. The same token
    /// [`crate::custodian::host_stint`] tears its driver down through — never a
    /// second one — because that function's own teardown rule is that the
    /// driver is *always* stopped through the token and never by dropping it,
    /// which is what stops the push pumps leaking a detached pair per rebuild.
    pub cancel: CancellationToken,
}

/// A built + connected control-plane client, plus the capability identity it was
/// built for (see [`SyncServiceState::nest_rpc`]).
pub struct RetainedNestRpc {
    nest_url: String,
    actor_id: [u8; 32],
    client: Arc<fauna_client::NestClient>,
}

impl SyncServiceState {
    /// Test/no-persistence constructor: `credentials: None`, so capability
    /// persistence is a no-op (unit tests never reach the OS secure store).
    /// Production (`run_agent`) uses [`Self::new_with_credentials`].
    // Every call site is #[cfg(test)] by design (see above) — dead on a --lib
    // (no --tests) build.
    #[allow(dead_code)]
    pub fn new(
        config: SyncConfig,
        shutdown_tx: watch::Sender<bool>,
        ipc_event_tx: broadcast::Sender<fauna_ipc::sync::Event>,
        paths: SyncPaths,
    ) -> Arc<Self> {
        let state = Self::new_with_credentials(config, shutdown_tx, ipc_event_tx, paths, None);
        // The unit tests' stand-in for a nest that lists every bound location as
        // an unbound own set: no test process runs the content-key task, so
        // without this no engine would ever be named. A test about the
        // resolution itself replaces it.
        #[cfg(test)]
        {
            *state
                .content_keys
                .try_write()
                .expect("a fresh state is unshared") =
                Some(crate::content_keys::ResolvedContentKeys::owner_only_for_tests());
        }
        state
    }

    pub fn new_with_credentials(
        config: SyncConfig,
        shutdown_tx: watch::Sender<bool>,
        ipc_event_tx: broadcast::Sender<fauna_ipc::sync::Event>,
        paths: SyncPaths,
        credentials: Option<Arc<fauna_credential_store::CredentialStore>>,
    ) -> Arc<Self> {
        let mounted_store = Arc::new(std::sync::Mutex::new(None));
        let notification_sink = crate::push_arm::platform_sink(&paths.flat_base_dir());
        Arc::new(Self {
            config: RwLock::new(config),
            paths,
            start_time: std::time::Instant::now(),
            shutdown_tx,
            engines: Mutex::new(None),
            ipc_event_tx,
            http_client: reqwest::Client::new(),
            capability: crate::bearer::CapabilitySlot::new(None),
            credentials,
            renewal_refusal: std::sync::RwLock::new(None),
            renewal_wake: tokio::sync::Notify::new(),
            store_principal_actor: RwLock::new(None),
            nest_rpc: Mutex::new(None),
            wake_senders: Mutex::new(HashMap::new()),
            peer_files: crate::peer_files::PeerFiles::new(),
            deletes_held: Mutex::new(HashMap::new()),
            deletes_skipped_unreadable: Mutex::new(HashMap::new()),
            public_audience: Mutex::new(HashMap::new()),
            engine_cmd_senders: Mutex::new(HashMap::new()),
            hosted_custodian: Mutex::new(None),
            hosted_account: watch::channel(None).0,
            host_wake: tokio::sync::Notify::new(),
            custodian_wake: tokio::sync::Notify::new(),
            reseed_job: Mutex::new(fauna_ipc::sync::CustodianReseedState::Idle),
            content_keys: RwLock::new(None),
            content_keys_wake: tokio::sync::Notify::new(),
            attached_apps: Arc::default(),
            notification_sink,
            on_demand: std::sync::OnceLock::new(),
            on_demand_mount_errors: std::sync::Mutex::default(),
            folder_keys: Arc::new(fauna_account_seams::folder_keys::PlaneFolderKeys::new({
                let slot = Arc::clone(&mounted_store);
                move || slot.lock().ok().and_then(|held| held.clone())
            })),
            mounted_store,
        })
    }

    /// A connected control-plane client for the provisioned nest, built once and
    /// retained (see [`Self::nest_rpc`]).
    ///
    /// `Err` when no capability has been provisioned yet — the user has not signed
    /// in to the WinUI app this session, so there is no nest to ask.
    pub async fn nest_rpc_client(&self) -> Result<Arc<fauna_client::NestClient>, String> {
        let (nest_url, actor_id) = {
            let cap = self.capability.read().await;
            let cap = cap
                .as_ref()
                .ok_or_else(|| "nest capability not provisioned".to_string())?;
            let actor_id = cap
                .actor_id_array()
                .ok_or_else(|| "capability carries a malformed actor_id".to_string())?;
            (cap.nest_url.clone(), actor_id)
        };

        let mut slot = self.nest_rpc.lock().await;
        if let Some(retained) = slot.as_ref()
            && retained.nest_url == nest_url
            && retained.actor_id == actor_id
            && retained.client.supervisor_stop().is_none()
        {
            return Ok(Arc::clone(&retained.client));
        }

        // The bearer reads `self.capability` per request, so the rebuilt client keeps
        // following `RefreshBearer` exactly like the hydration engine's.
        let auth = crate::bearer::bearer_only_auth_client(
            Arc::clone(&self.capability),
            nest_url.clone(),
            actor_id,
        );
        let client = fauna_client::NestClient::with_auth(auth);
        client
            .connect()
            .await
            .map_err(|e| format!("connect to nest control plane: {e}"))?;

        *slot = Some(RetainedNestRpc {
            nest_url,
            actor_id,
            client: Arc::clone(&client),
        });
        Ok(client)
    }

    /// Drop the retained client so the next call rebuilds + reconnects — for a
    /// provision that changed what it must speak as. Dropping it stops its
    /// supervisor (`NestClient`'s `Drop`); a failed *request* is no reason to call
    /// this (see [`Self::nest_rpc`]).
    pub async fn invalidate_nest_rpc(&self) {
        *self.nest_rpc.lock().await = None;
    }
}
