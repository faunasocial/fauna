//! The multi-root sync driver: selects which folders to serve and multiplexes one
//! `SyncEngine` per bound folder over the shared
//! [`fauna_sync_engine::engine_host::EngineHost`].
//!
//! It serves **both** halves of the sync contract, one engine per bound folder,
//! keyed by folder:
//!
//! - **On-demand** (`LocationMode::OnDemand`) — a cfapi provider root + the
//!   placeholder/hydration loop, **plus the same watch→upload half the always-resident
//!   root runs**. On-demand is a *storage* choice, never a *direction* choice: a change
//!   to a tracked file **must** be uploaded (USER-ratified 2026-07-14, `file-sync.md`
//!   § On-Demand Files → *Sync direction*, which supersedes the old "download-only by
//!   design" clause — that was an implementation fact written down as intent).
//! - **Always-resident** (`LocationMode::Always`) — a file watcher + the upload
//!   loop, over the **shared** [`fauna_sync_engine::always_resident`] loop that
//!   Linux drives too (priority #2/#4: one loop, not a per-app copy).
//!
//! **The two modes now differ only in how bytes reach the disk.** Both watch, both
//! upload, both honour `.faunaignore`, and both run the same
//! `LocalWriteHost::converge` backstop — the on-demand root additionally serves cfapi's
//! read callbacks and folds `changes.list` into placeholders instead of downloading
//! bytes eagerly. There is exactly one uploader in the tree, and `HydrationHost`
//! *requires* it as a supertrait, so a download-only on-demand root does not compile.
//!
//! Two bugs of the same shape are buried here, worth remembering because they were both
//! "the code says what it does, so what it does must be intended":
//!
//! - Until 2026-07-13 this module planned *only* on-demand folders, so a folder marked
//!   always-resident persisted its config and then **silently never synced** — no watcher
//!   existed anywhere in the service. That was the root cause of "no file is being synced"
//!   on Windows.
//! - Until 2026-07-14 the on-demand root ran no watcher either, so a user's edit to a
//!   tracked placeholder was **silently orphaned** (live-measured: two rescans logged
//!   `placeholders=0`, the row stayed `placeholder`, the badge went on claiming
//!   `CloudOnly`). It had been written up as "download-only by design"; it was a missing
//!   capability all along.
//!
//! [`crate::bridge`] remains the "what one engine serves" layer (byte/placeholder
//! serving + engine construction); this is the "multiplex N engines" layer. See
//! `docs/goal/behavior/file-sync.md` § On-Demand Files (*Hosting multiple
//! on-demand folders*) and `docs/goal/architecture/apps/windows.md`
//! § On-demand hydration host.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::Result;
use tokio::sync::{broadcast, mpsc};

use fauna_core::folder_keys::{FolderEngineKeys, FolderRef};
use fauna_ipc::sync::{Event, SyncProgressInfo};
use fauna_sync_engine::always_resident;
use fauna_sync_engine::engine_host::{
    CancellationToken, EngineCommand, EngineFuture, EngineHost, EngineSpec,
};

use crate::bridge::{HydrationCommand, HydrationHost};
use crate::config::{LocationMode, SyncConfig, SyncPaths};
use crate::state::SyncServiceState;
use fauna_client::NestClient;
use fauna_sync_engine::engine_lifecycle::{BuiltEngine, EngineParams, build_engine};
use fauna_sync_engine::relay_seat::{RelaySeat, ServeInbox};

/// One folder bound to a nest folder: the unit the driver serves. `mode` decides
/// which loop it gets (on-demand placeholders vs. always-resident watch+upload).
/// `Send` so it crosses the [`EngineHost`] command channel; the engine key is the
/// folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineMapping {
    /// The local directory this engine serves — the binding's location on this
    /// device. Named `path` to match [`LocationConfig::path`], and because the
    /// re-model's vocabulary reserves `folder` for the nest-side unit below
    /// (`folders.md` § Target re-model).
    pub path: PathBuf,
    /// Nest folder this engine is scoped to (built `folder = Some(name)`, so it
    /// folds only this set's `changes.list`). A label: nothing resolves by it.
    pub folder: String,
    /// This set's identity — the **unambiguous** key for the engine registry,
    /// key material and the per-set state DB (names are unique only per owner:
    /// account-data-plane.md § The ratified decisions). Required: [`plan_engines`] serves only a location whose
    /// [`binding`](crate::config::LocationConfig::binding) carries one.
    pub folder_ref: FolderRef,
    /// Which loop to run for this folder.
    pub mode: LocationMode,
}

impl EngineMapping {
    /// The key identifying this engine everywhere one engine must be told apart
    /// from another — the [`EngineSpec::key`] registry key, the `started` stamp
    /// map, and the per-set state DB.
    ///
    /// The [`folder_ref`](Self::folder_ref) in wire form, always — the name
    /// fallback a pre-identity binding took was retired 2026-09-24 (the
    /// compat-remnant sweep).
    ///
    /// Using the name here was the registry half of the same-named-sets hazard: two sets *can*
    /// share a name, and a shared key means the second `Start` cancels and
    /// replaces the first — one of the user's two bound folders silently stops
    /// syncing, on top of the wrong-key material.
    #[must_use]
    pub fn engine_key(&self) -> String {
        self.folder_ref.to_wire()
    }
}

/// Select the folders to serve: one [`EngineMapping`] per folder — in **either**
/// mode — that carries a **binding** ([`LocationConfig::binding`](crate::config::LocationConfig::binding):
/// a label and a parseable ref). A folder with no binding is **unbound** —
/// logged and skipped, never served under a manufactured name (`file-sync.md`
/// § On-Demand Files: *"the host never manufactures a folder name"*), nor
/// under a name alone.
///
/// Both modes are planned here: an always-resident folder gets a watch+upload
/// engine, an on-demand one gets a cfapi placeholder root. Selecting only
/// on-demand folders (the pre-2026-07-13 behaviour) is what made always-resident
/// folders silently dead.
pub fn plan_engines(config: &SyncConfig) -> Vec<EngineMapping> {
    config
        .locations
        .iter()
        // A binding the owning nest has refused is **parked** (D4): it is dropped
        // from the running plan, which is what stops the engine and keeps it
        // stopped across restarts. The config row deliberately survives — the
        // client renders the revoked state from it — and so do the local files.
        .filter(|f| {
            if f.access_revoked {
                tracing::info!(
                    path = %f.path,
                    folder = ?f.folder,
                    "sync folder is parked: write access was revoked by the nest that owns \
                     the set; not serving it (local files untouched; re-bind to retry)"
                );
            }
            !f.access_revoked
        })
        // A mode a newer agent wrote that this build cannot name runs NO engine
        // (`LocationMode::Other`): neither loop is the one that build chose.
        // The row survives, carried, and so do the local files.
        .filter(|f| {
            let LocationMode::Other(mode) = &f.mode else {
                return true;
            };
            tracing::warn!(
                path = %f.path,
                mode = %mode,
                "sync folder is in a mode this build cannot name; not serving it \
                 (local files untouched; pick a mode, or run a build that knows it)"
            );
            false
        })
        .filter_map(|f| match f.binding() {
            Some(binding) => Some(EngineMapping {
                path: PathBuf::from(&f.path),
                folder: binding.folder.to_string(),
                folder_ref: binding.folder_ref,
                mode: f.mode.clone(),
            }),
            None => {
                tracing::info!(
                    folder = %f.path,
                    mode = ?f.mode,
                    "sync folder has no folder binding; skipping (not served until bound)"
                );
                None
            }
        })
        .collect()
}

/// The engine **stamp** for `folder`: everything that materially determines the
/// engine's identity — the `(mls_group_id, current generation version)` pair that
/// decides its seal/open key, plus the [`LocationMode`] that decides which loop
/// it runs. [`reconcile_engines`] compares it against the live engine's last-built
/// stamp to detect a change that requires an `EngineHost` restart: a fresh binding
/// (`None` → `Some(gid)`), a keyless→keyed transition, a rotate-on-removal
/// (`Some(N)` → `Some(N+1)`), a re-bind to a different group (the `gid` bytes
/// change) — **or a mode flip**, which swaps the whole loop (placeholder host ⇄
/// watch+upload) and so must rebuild the engine, not leave the old loop running.
/// The `gid` (public routing material, not a secret) is included so a re-bind that
/// keeps the same version still restarts. The **folder** is included so re-binding
/// a folder to a *different* folder restarts too — without it the "live +
/// unchanged" branch kept serving the old folder — and so the stop/restart paths
/// can tell [`full_teardown_location`] which on-disk root a retiring on-demand
/// engine was registered on. The **actor scope** (`file-sync.md` § Multi-account ×
/// File Provider, consequence 3) is included so a per-actor re-scope restarts the
/// engine under the incoming actor's state dir: windows switches accounts on the
/// *provision* path (it sends no Unprovision to stop engines first, unlike macOS's
/// unprovision-first teardown), so an owner-only set sharing a name + folder across
/// two accounts would otherwise match on the scope-blind stamp and keep serving the
/// outgoing actor's already-open `fs-<set>.db` handle — a cross-account DB leak.
/// The **cross-nest routing pair** is included because it decides
/// which nest the control plane relays to and which the byte plane POSTs at — a
/// set that becomes foreign (or is re-accepted from a different home nest) must
/// rebuild, not keep a live engine talking to the wrong nest.
/// The **retired-generation version** (`webdav-server.md` § Key model,
/// Revocation) is included so a re-resolve that ONLY adds or rotates
/// `retired_content_keys` — a set the owner just unflagged over WebDAV, or a
/// second serve/unserve cycle that rotates a retired generation again — still
/// restarts a live engine: without it, a set already running owner-only
/// (`mls_group_id`/`content_keys` both `None`, hence unchanged) would keep the
/// engine that predates the change, and `reseal_predecessor_sealed` would never
/// see the candidate that lets it re-seal the served-era back-catalogue until
/// some UNRELATED restart happened to rebuild the engine.
/// The **set nonce** is included because an engine is built from the custody
/// its build reads, and the first reconcile after a provision runs before the
/// account host has mounted the store that custody is read through: an
/// owner-only set (named by its nest row alone) gets an engine then, built
/// with no nonce — it signs no record and can judge no served row. When the
/// resolution first reads the nonce, nothing else in the stamp moves, so
/// without it that engine would stay nonce-less until an unrelated restart.
/// The **serve window** (whether custody calls the set WebDAV-served,
/// `writer-signed-change-records.md` ruling (7)(b)(ii) rule (2)) is included
/// because a SHARED set's serve flip moves only custody's stamps — its group
/// and generations stay put — and the engine's reader exempts the owner's
/// pseudo-device rows on the window it was built with.
type EngineStamp = (
    Option<Vec<u8>>,
    Option<u64>,
    LocationMode,
    PathBuf,
    Option<String>,
    Option<(String, String)>,
    Option<u64>,
    Option<[u8; 32]>,
    bool,
);

///
/// `None` when the agent's resolution does not name the set
/// ([`crate::content_keys::ResolvedContentKeys::keys_for`]): such a set has no
/// stamp because it must not have an engine yet.
fn engine_stamp(
    keys: Option<&FolderEngineKeys>,
    mode: LocationMode,
    path: &std::path::Path,
    actor_scope: Option<&str>,
) -> Option<EngineStamp> {
    let keys = keys?;
    Some((
        keys.mls_group_id.clone(),
        keys.content_keys.as_ref().map(|k| k.current_version()),
        mode,
        path.to_path_buf(),
        actor_scope.map(str::to_owned),
        keys.foreign_routing(),
        keys.retired_content_keys
            .as_ref()
            .map(|k| k.current_version()),
        keys.set_nonce,
        keys.webdav_served(),
    ))
}

/// The registration-lifecycle decision (`file-sync.md` § Per-file sync-status
/// display; mechanics `crate::cfapi_host`): a shell/filter sync-root registration
/// belongs to the **binding**, not the serve-session, so a retiring engine's
/// registration comes down only when the binding itself ended. Given the retiring
/// engine's last stamp and what (if anything) replaces it, return the **local
/// path** to [`crate::cfapi_host::mark_root_for_full_teardown`] — or `None` to
/// keep the registration (service stop, restart-in-place for key rotation).
/// (Production call sites are `#[cfg(windows)]` — only cfapi registers a root —
/// but the decision itself is platform-neutral and unit-tested everywhere.)
///
/// ⚠ Both sides of the comparison below are **paths**. The stamp's location
/// became the sync-root path, and comparing it against
/// [`EngineMapping::folder`] — the nest-side *name* — is never equal, so every
/// restart-in-place tore the registration down. The re-model's vocabulary is the
/// guard: `path` = local directory, `folder` = the nest unit
/// (`folders.md` § Target re-model).
#[cfg_attr(not(windows), allow(dead_code))]
fn full_teardown_location(prev: &EngineStamp, next: Option<&EngineMapping>) -> Option<PathBuf> {
    let (_, _, prev_mode, prev_location, ..) = prev;
    if *prev_mode != LocationMode::OnDemand {
        return None; // no cfapi root was registered for a watcher engine
    }
    match next {
        // Unbound / location removed: the binding ended.
        None => Some(prev_location.clone()),
        // Re-bound to a different local path, or flipped to always-resident: the
        // old path must stop being a cfapi root. A key rotation (same path,
        // still on-demand) keeps the registration and just reconnects.
        Some(m) if m.path != *prev_location || m.mode != LocationMode::OnDemand => {
            Some(prev_location.clone())
        }
        Some(_) => None,
    }
}

// ---------------------------------------------------------------------------
// The per-engine spec: how to build + run one folder's engine
// ---------------------------------------------------------------------------

/// The shared engine inputs that are the same for every root and fixed for the
/// host's lifetime: derived once from the device config + capability when the
/// host is first built. (The bearer token *inside* the capability still rotates
/// live via the shared `capability` slot — only `actor_id`/`backup_key`/url are
/// captured here.)
/// The per-**account** engine inputs resolved from the provisioned capability.
///
/// `PartialEq` is what lets [`reconcile_engines`] notice that the running host was
/// built for a different account and rebuild it. That comparison is between two
/// copies of *our own* capability, never against caller-supplied bytes, so the
/// derived (non-constant-time) equality is not a secret-comparison oracle.
#[derive(Clone, PartialEq, Eq)]
struct SpecInputs {
    nest_base_url: String,
    device_id: [u8; 32],
    actor_id: [u8; 32],
    backup_key: [u8; 32],
    /// The account's retired owner keys (identity succession) — part of the
    /// per-account inputs on purpose, so `PartialEq` makes a change here rebuild
    /// the host exactly as an `actor_id`/`backup_key` change does (R2). A
    /// succession *is* an account-identity change, and the eventual drop of these
    /// keys once the corpus is re-sealed must take effect the same way. The
    /// paired keys and the attested ids ride with them.
    predecessors: crate::bridge::AgentPredecessors,
}

/// The Windows [`EngineSpec`]: turns one [`EngineMapping`] into a cancellable
/// per-engine future running the loop its mode calls for. The shared deps
/// (`capability` slot, device/actor/key/url, event channel) are captured once and
/// cloned into each engine future.
///
/// Because `device_id` / `actor_id` / `backup_key` / `nest_base_url` are captured
/// here rather than re-read per engine, this spec is **account-scoped**: a host
/// holding it may only ever serve the account whose capability built it.
/// [`reconcile_engines`] enforces that by rebuilding the host when the provisioned
/// [`SpecInputs`] change.
struct LocationEngineSpec {
    /// Shared, app-provisioned capability slot (the bearer rotates live; the
    /// engine reads it per request). `Arc`-shared so every engine sees refreshes.
    capability: Arc<crate::bearer::CapabilitySlot>,
    /// The service state, held **weakly** so this spec — which the running host
    /// owns, which the state owns — does not close a reference cycle. Used only
    /// by the D4 park watcher: on an `access-revoked` transition it persists the
    /// parked binding and re-reconciles (dropping the set from the plan).
    state: Weak<SyncServiceState>,
    /// IPC event channel for per-chunk hydration progress.
    event_tx: broadcast::Sender<Event>,
    /// Resolved data-root, so each per-folder engine opens its state DB under the
    /// same root the service loaded from (honors `--data-dir`).
    paths: SyncPaths,
    nest_base_url: String,
    device_id: [u8; 32],
    actor_id: [u8; 32],
    backup_key: [u8; 32],
    /// Frozen for the host's lifetime beside `backup_key` — see
    /// [`SpecInputs::predecessors`].
    predecessors: crate::bridge::AgentPredecessors,
    /// This host's relay-serving seat (`file-sync.md` § Relay serving): every
    /// engine registers its folder here for as long as it runs, and the
    /// host's background task announces the set and routes the nest's asks.
    /// One per host, so it is account-scoped exactly as the spec is.
    relay_seat: Arc<RelaySeat>,
}

impl LocationEngineSpec {
    fn new(
        inputs: SpecInputs,
        capability: Arc<crate::bearer::CapabilitySlot>,
        event_tx: broadcast::Sender<Event>,
        paths: SyncPaths,
        state: Weak<SyncServiceState>,
    ) -> Self {
        Self {
            capability,
            state,
            event_tx,
            paths,
            nest_base_url: inputs.nest_base_url,
            device_id: inputs.device_id,
            actor_id: inputs.actor_id,
            backup_key: inputs.backup_key,
            predecessors: inputs.predecessors,
            relay_seat: RelaySeat::new(),
        }
    }
}

impl EngineSpec for LocationEngineSpec {
    type Desc = EngineMapping;

    fn key(desc: &EngineMapping) -> String {
        desc.engine_key()
    }

    fn run(&self, desc: EngineMapping, cancel: CancellationToken) -> EngineFuture {
        let capability = self.capability.clone();
        let event_tx = self.event_tx.clone();
        let paths = self.paths.clone();
        let state = self.state.clone();
        // A second weak handle for the remote-change nudge registry (the first is
        // moved into the D4 park watcher below).
        let state_for_wake = self.state.clone();
        // A third, for the progress drain's mass-delete-floor reporting.
        let state_for_progress = self.state.clone();
        let nest_base_url = self.nest_base_url.clone();
        let device_id = self.device_id;
        let actor_id = self.actor_id;
        let backup_key = self.backup_key;
        let predecessors = self.predecessors.clone();
        let path = desc.path.clone();
        let folder_ref = desc.folder_ref;
        let folder = desc.folder.clone();
        let mode = desc.mode;
        let relay_seat = Arc::clone(&self.relay_seat);

        Box::pin(async move {
            // Per-folder state DBs live under the resolved data-root
            // (`SyncPaths::sync_db_path_for_ref` is `state_db_path` under it), so
            // this engine folds only its own `changes.list`.
            let state_dir = paths.base_dir();
            // The agent's own custody resolution (`crate::content_keys`) decides
            // WHICH sets get an engine — and its stamp, when one restarts (a
            // rotation above all). The build below re-reads what it builds from
            // (the set's row, or a cross-nest set's custody record) through the
            // shared builder, so the engine is keyed from the same custody.
            let (names_set, folder_keys) = match state_for_wake.upgrade() {
                Some(st) => (
                    st.content_keys
                        .read()
                        .await
                        .as_ref()
                        .filter(|r| r.is_for(actor_id))
                        .is_some_and(|r| r.keys_for(folder_ref).is_some()),
                    Arc::clone(&st.folder_keys),
                ),
                None => return, // the service is shutting down
            };
            // `reconcile_engines` never starts a set the resolution does not name,
            // but a re-resolve between its filter and this read can withdraw one;
            // the answer is the same either way — no engine until an edge's
            // resolution names the set.
            if !names_set {
                tracing::warn!(
                    folder = %folder,
                    "the agent's content-key resolution does not name this set; not building \
                     its engine (fail closed) — the next edge that names it rebuilds it"
                );
                return;
            }
            // Per-file completed-sync notification (sync-agent.md § Implementation
            // status): the engine's transfer pool emits `FileDone` on this channel;
            // `drive_progress_notifications` re-broadcasts it as a subscribable
            // `FileStatusChanged{Synced}` event. Ends when `engine` drops.
            //
            // The same drain carries the mass-delete floor's per-pass verdict
            // into `SyncServiceState::deletes_held` for `ListEngines` to read —
            // it is already the one task per engine that outlives no engine, so
            // the entry's lifetime is the engine's by construction.
            let (progress_tx, progress_rx) = mpsc::unbounded_channel();
            tokio::spawn(drive_progress_notifications(
                progress_rx,
                event_tx.clone(),
                path.clone(),
                folder.clone(),
                folder_ref,
                state_for_progress,
            ));

            // The terminal `access-revoked` park flag (D4). Created here — before
            // the engine — because a cross-nest set's byte-plane bearer, built
            // inside the shared builder before the engine, shares it, and because
            // this task keeps a handle to watch for the transition below.
            let access_gate = fauna_sync_engine::access_gate::AccessGate::new();
            // Persist + re-plan the moment the owning nest refuses this set's
            // write grant. Spawned rather than `select!`ed into the engine loop:
            // the engine parks *itself* on the same gate, so this task's only job
            // is the durable half, and it must survive the engine future ending.
            tokio::spawn(watch_for_access_revocation(
                Arc::clone(&access_gate),
                state,
                folder_ref,
            ));

            // Build through the shared one builder (`on-demand-files.md` § Shared
            // sets on a capability host → *One mechanism*, question 2): it reads
            // the set's row — a cross-nest set's custody record — over the control
            // plane, keys the engine from custody under the capability's
            // `BackupKey`, and for a cross-nest set graduates the home nest's pin
            // and dials its byte plane there. The control plane is connected first
            // because the build reads over it; its supervisor re-dials on its own
            // from then on, so the roots below do not connect again.
            let nest_rpc = crate::bridge::agent_control_plane(capability, nest_base_url, actor_id);
            // The machine's change-record signer: the principal writer key the
            // app's enrollment ceremony certified with `SyncWrite`. Load-only —
            // absent (not enrolled yet, or a `[RenewBearer]`-only grant) the
            // engine records unsigned and says so once.
            let change_signer = fauna_sync_engine::principal_bundle::load_change_signer(
                &fauna_sync_engine::account_runtime::production_credential_store(),
                &actor_id,
            )
            .map(Arc::new);
            // A linux on-demand root is built over its REACH, opened before the
            // build because the engine's `watch_dir` is fixed there.
            let mount_report = MountReport::new(state_for_wake.clone(), &path);
            let reach = open_root_reach(mode.clone(), &path, &folder, &mount_report);
            let engine_dir = root_dir(&reach, &path);
            let Some(built) = build_until_served(
                &nest_rpc,
                || {
                    crate::bridge::agent_engine_params(
                        Arc::clone(&nest_rpc),
                        state_dir.clone(),
                        engine_dir.clone(),
                        folder_ref,
                        device_id,
                        backup_key,
                        &predecessors,
                        Some(progress_tx.clone()),
                        Arc::clone(&access_gate),
                        change_signer.clone(),
                        Arc::clone(&folder_keys),
                    )
                },
                &folder,
                &cancel,
            )
            .await
            else {
                return; // cancelled before a build succeeded
            };
            // The refresh edge: a tick whose row read no longer answers the basis
            // the build resolved from (binding, access or floor moved, or the
            // floor still ahead of the generation held) wakes the content-key
            // task to re-resolve.
            let edge = {
                let state = state_for_wake.clone();
                fauna_sync_engine::binding_edge::BindingEdge {
                    folder_ref,
                    basis: built.basis,
                    on_rebuild: Arc::new(move || {
                        if let Some(st) = state.upgrade() {
                            st.content_keys_wake.notify_one();
                        }
                    }),
                }
            };
            let engine = built.engine.with_binding_edge(edge);

            // Both modes register the SAME two per-folder channels — the
            // remote-change nudge and the invoke-and-reply command — so a
            // folder's mode never decides whether its engine hears
            // `PullFolderNow` or answers the mass-delete floor's confirm
            // (`delete-propagation.md` § *The floor on an on-demand root*,
            // point 4: until 2026-09-29 only the `Always` arm registered, and
            // an on-demand hold could not be confirmed).
            let (inbox, wake_rx, cmd_rx) =
                EngineInbox::register(&state_for_wake, folder_ref.to_wire()).await;
            // The folder joins this host's announce from here and leaves it
            // when the root below returns and drops the inbox — an engine that
            // is not running is not announced (`file-sync.md` § Relay serving).
            let serve = relay_seat.register(&folder_ref);
            match mode {
                LocationMode::OnDemand => {
                    run_on_demand_root(
                        engine,
                        path,
                        reach,
                        mount_report,
                        folder,
                        folder_ref.state_db_path(&state_dir),
                        event_tx,
                        cancel,
                        Some(wake_rx),
                        Some(cmd_rx),
                        serve,
                    )
                    .await;
                }
                LocationMode::Always => {
                    run_always_resident_root(
                        engine,
                        path,
                        folder.clone(),
                        cancel,
                        Some(wake_rx),
                        Some(cmd_rx),
                        serve,
                    )
                    .await;
                }
                // Never planned ([`plan_engines`] drops it): no loop runs.
                LocationMode::Other(_) => {}
            }
            inbox.deregister().await;
        })
    }

    /// The relay-serving seat's connection half, for the host's whole life:
    /// announce the running engines' folders and route the nest's asks to
    /// them, app open or closed (`sync-agent.md` § Scope per platform).
    fn background(&self) -> Option<EngineFuture> {
        // The seat's own WS-RPC connection, on this host's runtime and gone
        // with it — never the retained client the pipe verbs share
        // (`SyncServiceState::nest_rpc`), whose supervisor must outlive a host
        // rebuild.
        let client = crate::bridge::agent_control_plane(
            self.capability.clone(),
            self.nest_base_url.clone(),
            self.actor_id,
        );
        Some(Box::pin(run_relay_seat(
            Arc::clone(&self.relay_seat),
            client,
            hex::encode(self.device_id),
        )))
    }
}

/// How long the relay seat waits before it brings its connection up again
/// after a failed connect or a supervisor that stopped for good — and how
/// often it looks for that stop. A running supervisor re-dials on its own; this
/// covers only the stops no retry of its own clears (a bearer the app has not
/// refreshed yet, a nest that changed version).
const RELAY_SEAT_REDIAL: Duration = Duration::from_secs(30);

/// Keep the host's relay seat announced: connect, run the shared seat loop,
/// and start over when the connection's supervisor has stopped for good.
/// Never returns; the host drops it.
async fn run_relay_seat(seat: Arc<RelaySeat>, client: Arc<NestClient>, device_id_hex: String) {
    loop {
        match client.ensure_connected().await {
            Ok(()) => {
                tracing::debug!("relay seat: control plane up; announcing");
                let stopped = async {
                    while client.supervisor_stop().is_none() {
                        tokio::time::sleep(RELAY_SEAT_REDIAL).await;
                    }
                };
                tokio::select! {
                    () = seat.run(&client, &device_id_hex) => {}
                    () = stopped => {}
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "relay seat: control-plane connect failed; retrying");
            }
        }
        tokio::time::sleep(RELAY_SEAT_REDIAL).await;
    }
}

/// One running engine's two per-folder inbound channels, registered in the
/// service state's registries for the pipe server to route on: the
/// remote-change nudge (`wake_senders`, `PullFolderNow`) and the
/// invoke-and-reply [`always_resident::EngineCommand`] (`engine_cmd_senders` —
/// the mass-delete floor's confirm, a share-ingest page). Every engine
/// registers through this one type, whatever its mode.
///
/// Registry key = the `EngineMapping::engine_key` semantic (the ref): the NAME
/// alone is ambiguous across two same-named sets, and a name-keyed map silently
/// replaced the first engine's entry with the second's — which is how a nudge,
/// a held-deletes apply, or a share ingest could reach the WRONG same-named
/// engine. Name-only IPC verbs resolve the ref through
/// `pipe_server::resolve_engine_key`.
struct EngineInbox {
    state: Weak<SyncServiceState>,
    key: String,
    wake_tx: mpsc::Sender<()>,
    cmd_tx: mpsc::Sender<always_resident::EngineCommand>,
}

impl EngineInbox {
    /// Mint both channels and register their senders under `key`, replacing a
    /// previous engine's entries (an engine restart — key rotation, mode flip —
    /// registers before the old run deregisters). Returns the receivers the
    /// engine's loop drains.
    async fn register(
        state: &Weak<SyncServiceState>,
        key: String,
    ) -> (
        Self,
        mpsc::Receiver<()>,
        mpsc::Receiver<always_resident::EngineCommand>,
    ) {
        // Bounded(1): rapid collaborator saves coalesce to one pending pull,
        // which pulls everything.
        let (wake_tx, wake_rx) = mpsc::channel::<()>(1);
        // Capacity 2: a rare, user-initiated verb; a queued command waits for
        // the loop's current arm — busy is not full.
        let (cmd_tx, cmd_rx) = mpsc::channel::<always_resident::EngineCommand>(2);
        if let Some(st) = state.upgrade() {
            st.wake_senders
                .lock()
                .await
                .insert(key.clone(), wake_tx.clone());
            st.engine_cmd_senders
                .lock()
                .await
                .insert(key.clone(), cmd_tx.clone());
        }
        let inbox = Self {
            state: state.clone(),
            key,
            wake_tx,
            cmd_tx,
        };
        (inbox, wake_rx, cmd_rx)
    }

    /// Deregister on exit — but only entries that are still this engine's. An
    /// engine restart spawns a fresh `run` that overwrites the entries with its
    /// own channels; `same_channel` keeps us from removing the newer engine's.
    async fn deregister(self) {
        let Some(st) = self.state.upgrade() else {
            return;
        };
        let mut wakes = st.wake_senders.lock().await;
        if wakes
            .get(&self.key)
            .is_some_and(|cur| cur.same_channel(&self.wake_tx))
        {
            wakes.remove(&self.key);
        }
        drop(wakes);
        let mut cmds = st.engine_cmd_senders.lock().await;
        if cmds
            .get(&self.key)
            .is_some_and(|cur| cur.same_channel(&self.cmd_tx))
        {
            cmds.remove(&self.key);
        }
    }
}

/// The longest an agent engine waits between two build attempts.
const BUILD_RETRY_MAX: Duration = Duration::from_secs(600);

/// Build one agent engine through the shared one builder, retrying a refusal
/// until it serves or the engine is cancelled (`None`). The control plane is
/// connected before the first attempt — the build reads over it — and again
/// only until a connect succeeds (its supervisor re-dials on its own after
/// that).
///
/// A refusal is not final here: the agent restarts an engine only when its
/// stamp moves, so an engine refused once — a nest briefly down at start, a
/// home-nest pin that would not graduate, custody not yet readable — would
/// otherwise stay inert until the next rotation. The backoff doubles from the
/// agent's idle cadence to [`BUILD_RETRY_MAX`]; the build's own log line names
/// each refusal's cause.
async fn build_until_served(
    nest_rpc: &Arc<NestClient>,
    params: impl Fn() -> EngineParams,
    folder: &str,
    cancel: &CancellationToken,
) -> Option<BuiltEngine> {
    let mut connected = false;
    let mut backoff = Duration::from_secs(crate::state::IDLE_RECHECK_SECS);
    loop {
        if !connected {
            match nest_rpc.ensure_connected().await {
                Ok(()) => connected = true,
                Err(e) => tracing::warn!(
                    folder,
                    error = %e,
                    retry_in = ?backoff,
                    "control-plane connect failed; retrying"
                ),
            }
        }
        if connected {
            if let Some(built) = build_engine(params()).await {
                return Some(built);
            }
            tracing::warn!(folder, retry_in = ?backoff, "engine build refused; retrying");
        }
        tokio::select! {
            () = cancel.cancelled() => return None,
            () = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(BUILD_RETRY_MAX);
    }
}

/// Wait for `gate` to park, then make the park **durable and visible** (D4,
/// `file-sync.md` § Multi-writer shared sets).
///
/// The engine stops itself the instant the gate flips — that half is in shared
/// Rust and every host gets it. This is the agent-side half: mark every folder
/// bound to `folder` as `access_revoked` in the persisted config and
/// re-reconcile, which drops the set from [`plan_engines`] and so stops it
/// staying stopped only by accident.
///
/// Persisting matters because without it an agent restart would happily rebuild
/// the engine for a binding the owning nest has already refused, and the client
/// would show a folder as syncing until the user's next edit re-discovered the
/// refusal. Recovery is the explicit re-bind, which clears the flag and re-runs
/// the eager bind-time mint verify (D3) — so a re-granted writer is one gesture
/// away, and a still-revoked one is refused loudly at that gesture.
///
/// Resolves (ending the task) either on the park or when the service is gone.
async fn watch_for_access_revocation(
    gate: Arc<fauna_sync_engine::access_gate::AccessGate>,
    state: Weak<SyncServiceState>,
    folder_ref: FolderRef,
) {
    gate.wait_revoked().await;
    let Some(state) = state.upgrade() else {
        return; // service shutting down; nothing to persist into
    };
    park_revoked_set(&state, folder_ref).await;
}

/// Mark every location bound to the set `folder_ref` parked, persist, and
/// re-plan. By the ref, not the name: a same-named set the nest did NOT refuse
/// must keep syncing.
///
/// Idempotent: a set already parked writes nothing and re-plans nothing, so a
/// second refusal (another folder on the same set, a racing plane) is free.
/// **Never touches the local folder or its contents** — only the binding's
/// `access_revoked` flag — which is the whole point of D4's "local files and
/// pending local edits are never touched".
pub(crate) async fn park_revoked_set(state: &Arc<SyncServiceState>, folder_ref: FolderRef) {
    let folder = folder_ref.to_wire();
    let folder = folder.as_str();
    let persisted = {
        let mut config = state.config.write().await;
        let mut changed = false;
        for location in config.locations.iter_mut().filter(|f| {
            f.binding().is_some_and(|b| b.folder_ref == folder_ref) && !f.access_revoked
        }) {
            tracing::warn!(
                path = %location.path,
                folder,
                "write access revoked: parking this binding. The folder and its contents \
                 are untouched; it is no longer synced until re-bound."
            );
            location.access_revoked = true;
            changed = true;
        }
        if changed {
            // Best-effort: an unwritable config still leaves the engine parked in
            // memory (the gate is terminal), so the failure costs durability
            // across a restart, not fail-closedness now.
            if let Err(e) = state.paths.save_config(&config) {
                tracing::error!(
                    folder,
                    error = %e,
                    "failed to persist the parked binding; it will be re-discovered \
                     on the next refused write after a restart"
                );
            }
        }
        changed
    };

    if persisted && let Err(e) = reconcile_engines(state).await {
        tracing::error!(folder, error = %e, "reconcile after parking the set failed");
    }
}

/// How often the drain flushes a moved fold as an [`EventKind::SyncProgress`].
///
/// The coalescing cadence, and the whole reason the push is not per-chunk: a
/// 4 MB file at the default chunk size fires ~64 `ChunkDone`s, a 4 GB one
/// ~65,000, and the engine uploads chunks *concurrently* — so the raw rate is
/// bounded by the network, not by anything a UI can paint. A flush every
/// 250 ms is ~4 repaints/second, which reads as smooth and costs a bounded
/// number of events per second no matter how fast the transfer runs.
const PROGRESS_FLUSH_INTERVAL: Duration = Duration::from_millis(250);

/// How long the in-flight window stays open with nothing transferring before
/// it is declared drained and retracted.
///
/// Not zero, and that is the point: `converge` uploads files **sequentially**,
/// so the open set is empty for a moment between every pair of files. Closing
/// on "nothing open right now" would retract and re-open the window once per
/// file — a hundred-file pass would flicker `1 item` a hundred times instead of
/// counting up to `100 items` once. This grace spans the inter-file gap (a DB
/// write plus a hash) while still clearing a genuinely finished pass promptly.
const PROGRESS_WINDOW_GRACE: Duration = Duration::from_secs(2);

/// One file the engine has begun transferring and not yet finished.
struct OpenFile {
    /// `FileStarted.size` — the whole-file size this file contributes to
    /// `bytes_total`.
    size: u64,
    /// Bytes credited so far, clamped to `size`.
    done: u64,
}

/// The live in-flight transfer window for one engine — the *push* half of the
/// two-channel sync status (`sync-agent.md` § Local agent health → the
/// sync-status projection).
///
/// The durable half (`files_pending`/`bytes_pending` over `GetSyncStatus` /
/// `ListEngines`) answers "what is queued", survives a restart, and is polled.
/// This answers "what is moving right now", lives only in memory, and is
/// pushed. They are deliberately different questions: whole-file queue sizes
/// there, per-file byte progress here.
///
/// **In-memory is correct here** — the same reasoning `ProgressEvent::DeletesHeld`
/// is built on. The window has exactly the lifetime of the transfers it
/// describes: a restarted agent reports nothing in flight (true — nothing is)
/// rather than resurrecting a stale byte count no live engine stands behind.
#[derive(Default)]
struct ProgressFold {
    /// Files started and not yet done. Empty does **not** mean the window is
    /// closed — see [`PROGRESS_WINDOW_GRACE`].
    open: HashMap<String, OpenFile>,
    files_total: u64,
    files_done: u64,
    bytes_total: u64,
    bytes_done: u64,
    /// Set by every state-changing `observe`, cleared by [`Self::take_pending`]
    /// — so a flush tick that saw no movement sends nothing.
    moved: bool,
}

impl ProgressFold {
    /// Fold one engine progress event into the window.
    fn observe(&mut self, event: &fauna_sync_engine::progress::ProgressEvent) {
        use fauna_sync_engine::progress::ProgressEvent as P;
        match event {
            P::FileStarted { path, size, .. } => {
                // A re-`FileStarted` for a path already open (a resumed
                // transfer) replaces its entry rather than double-counting the
                // file; its credited bytes start over with it.
                if let Some(prev) = self.open.remove(path) {
                    self.files_total -= 1;
                    self.bytes_total -= prev.size;
                    self.bytes_done -= prev.done;
                }
                self.open.insert(
                    path.clone(),
                    OpenFile {
                        size: *size,
                        done: 0,
                    },
                );
                self.files_total += 1;
                self.bytes_total += *size;
                self.moved = true;
            }
            P::ChunkDone { path, bytes } => {
                // A chunk for a file no window opened is dropped, never
                // guessed: its size was never counted into `bytes_total`, so
                // crediting its bytes would push `bytes_done` past the total
                // and render as a progress bar beyond 100%. Every real
                // transfer is bracketed `FileStarted` … `FileDone` (all three
                // emit sites in `fauna-sync-engine`), so this is the
                // defensive arm, not the common one.
                if let Some(f) = self.open.get_mut(path) {
                    let credit = (*bytes).min(f.size - f.done);
                    f.done += credit;
                    self.bytes_done += credit;
                    self.moved = true;
                }
            }
            P::FileDone { path } => {
                // Settle the file to its full size. `ChunkDone` fires only for
                // chunks that actually *moved*, and the upload pipeline skips
                // every chunk the nest already holds — so a fully-deduped file
                // transfers no chunks at all, and a byte counter fed by
                // `ChunkDone` alone would sit at 0% and then jump.
                if let Some(f) = self.open.remove(path) {
                    self.bytes_done += f.size - f.done;
                    self.files_done += 1;
                    self.moved = true;
                }
            }
            // Carried by the same channel but not part of the transfer window:
            // `DeletesHeld` is the mass-delete floor's per-pass verdict (its
            // own `SyncServiceState` field), `DeletesSkippedUnreadable` is the
            // per-pass count of rows a scan could not READ and therefore
            // withheld from delete detection — a fault report, not a
            // transfer — and `CycleDone` has no production emit site at all: it
            // exists only in a `bins/fauna-sync` test fixture, which is why it
            // cannot serve as the window terminator.
            P::DeletesHeld { .. }
            | P::DeletesSkippedUnreadable { .. }
            | P::PublicAudience { .. }
            | P::CycleDone { .. } => {}
        }
    }

    /// Nothing is transferring right now. The window may still be open — the
    /// gap between two sequential files looks exactly like this.
    fn is_idle(&self) -> bool {
        self.open.is_empty()
    }

    /// Has this window ever seen a transfer? A closed window snapshots as
    /// zeros, which is the retraction, so there is nothing to retract twice.
    fn is_closed(&self) -> bool {
        self.files_total == 0
    }

    /// The snapshot to push, if the fold moved since the last one. Clears the
    /// moved flag, so a flush tick with no event behind it sends nothing.
    fn take_pending(&mut self, folder: &str) -> Option<SyncProgressInfo> {
        self.moved.then(|| {
            self.moved = false;
            self.snapshot(folder)
        })
    }

    /// The current window as it goes on the wire.
    fn snapshot(&self, folder: &str) -> SyncProgressInfo {
        SyncProgressInfo {
            folder: folder.to_string(),
            files_done: self.files_done,
            files_total: self.files_total,
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
        }
    }

    /// Retract the window: the pass finished, and the next transfer opens a
    /// fresh one.
    ///
    /// The caller pushes the resulting all-zero snapshot, and that zero is the
    /// *only* thing that ever takes a painted "Uploading 3 items" off a
    /// surface — the same rule `DeletesHeld` is pinned on. A final frame left
    /// at 100% would stay painted until the next transfer, possibly for days.
    fn close_window(&mut self) {
        *self = Self::default();
    }
}

/// One flush decision for [`drive_progress_notifications`]: retract a drained
/// window, or push the fold if it moved since the last push.
///
/// Split out because both select arms make it — the timer arm on cadence, and
/// the event arm when a fast chunk stream would otherwise starve the timer.
fn flush_progress(
    fold: &mut ProgressFold,
    folder: &str,
    event_tx: &broadcast::Sender<Event>,
    idle_since: &mut Option<tokio::time::Instant>,
    now: tokio::time::Instant,
) {
    let send = |info| {
        // Best-effort: no subscriber (or a lagged one) is not this drain's
        // problem — the durable projection is what a late-connecting app reads.
        let _ = event_tx.send(Event {
            event: fauna_ipc::sync::EventKind::SyncProgress(info),
        });
    };

    if fold.is_closed() {
        *idle_since = None;
        return;
    }
    if fold.is_idle() {
        let since = *idle_since.get_or_insert(now);
        if now.saturating_duration_since(since) >= PROGRESS_WINDOW_GRACE {
            fold.close_window();
            *idle_since = None;
            send(fold.snapshot(folder));
            return;
        }
    } else {
        *idle_since = None;
    }
    if let Some(info) = fold.take_pending(folder) {
        send(info);
    }
}

/// Re-surface an engine's per-file upload completions as desktop-notification
/// events: [`fauna_sync_engine::progress::ProgressEvent::FileDone`] becomes a
/// broadcast [`EventKind::FileStatusChanged`] (`status: Synced`) — the pushed-event
/// half of the socket a client subscribes to (`sync-agent.md` § Implementation
/// status: the notification the in-app `SyncDriver` used to emit directly before
/// the A3 cutover retired it). Runs for both modes: both roots watch+upload, so
/// both complete files.
///
/// **Also folds the live transfer window** ([`ProgressFold`]) and pushes it as a
/// coalesced [`EventKind::SyncProgress`]. Before this, every byte delta the
/// engine reported was dropped here and `SyncProgress` fired for Windows
/// hydration only, so no app could show an upload advancing between the 10 s
/// status polls (`sync-agent.md` § Implementation status today, the captured
/// "upload-side progress push" follow-on).
///
/// Ends when every `progress_tx` clone drops — i.e. when the engine itself drops
/// (its `TransferPool` is the sole owner of the sender) — so no cancellation
/// token is needed here; it winds down exactly when its engine does.
///
/// Builds the absolute path by pushing `rel`'s components onto `folder` rather
/// than reusing [`crate::path_map::overlay_abs_path`]: that helper picks its
/// separator from the root's shape and maps a linux on-demand root's descriptor
/// reach back to its mount point — the on-demand roots' concerns. This drain runs
/// for every platform's always-resident roots, whose `folder` is already the
/// bound path, so a plain component join is the whole job.
async fn drive_progress_notifications(
    mut progress_rx: mpsc::UnboundedReceiver<fauna_sync_engine::progress::ProgressEvent>,
    event_tx: broadcast::Sender<Event>,
    path: PathBuf,
    folder: String,
    folder_ref: FolderRef,
    state: std::sync::Weak<crate::state::SyncServiceState>,
) {
    // `deletes_held` is keyed like the engine registry — by the ref, never the
    // label two sets can share.
    let held_key = folder_ref.to_wire();
    let mut fold = ProgressFold::default();
    // When the window last had nothing transferring — the clock the grace
    // period runs on. `None` means "not idle (or nothing open)".
    let mut idle_since: Option<tokio::time::Instant> = None;
    let mut next_flush = tokio::time::Instant::now() + PROGRESS_FLUSH_INTERVAL;

    loop {
        tokio::select! {
            // `biased` so a queued event is always folded before a flush is
            // considered: an unbiased select picks a ready branch at random,
            // which pushes a frame built from a partly-drained queue — the
            // pushed number then lags the engine by an arbitrary amount.
            // Starvation under a fast chunk stream (where `recv` is always
            // ready) is handled by the recv arm checking the flush deadline
            // itself, not by leaving it to the timer arm.
            biased;
            received = progress_rx.recv() => {
                let Some(event) = received else { break };
                fold.observe(&event);
                match event {
                    fauna_sync_engine::progress::ProgressEvent::FileDone { path: rel } => {
                        let mut abs = path.clone();
                        for part in rel.split('/') {
                            if !part.is_empty() {
                                abs.push(part);
                            }
                        }
                        let _ = event_tx.send(Event {
                            event: fauna_ipc::sync::EventKind::FileStatusChanged {
                                path: abs.to_string_lossy().into_owned(),
                                status: fauna_ipc::sync::FileStatus::Synced,
                            },
                        });
                    }
                    // The mass-delete floor's verdict for the pass that just ran
                    // (`file-sync.md` § Files Appear Automatically). Written on EVERY
                    // pass including zero — a zero is what clears a surface currently
                    // showing a hold, and since the hold is derived, nothing else
                    // would ever retract it.
                    fauna_sync_engine::progress::ProgressEvent::DeletesHeld { held } => {
                        if let Some(st) = state.upgrade() {
                            st.deletes_held.lock().await.insert(held_key.clone(), held);
                        }
                    }
                    // The delete rail's unreadable-path verdict for the same pass
                    // (`delete-propagation.md` § Unreadable is not absent) — the
                    // hold's sibling, same every-pass-including-zero rule: only a
                    // zero retracts a surface that is showing it.
                    fauna_sync_engine::progress::ProgressEvent::DeletesSkippedUnreadable {
                        skipped,
                    } => {
                        if let Some(st) = state.upgrade() {
                            st.deletes_skipped_unreadable
                                .lock()
                                .await
                                .insert(held_key.clone(), skipped);
                        }
                    }
                    // The set's live public-audience arm — what `ShareFile`
                    // answers the Explorer Share leaf from (`apps/windows.md`
                    // § Shell Extension → *The Share hand-off*, step 1).
                    fauna_sync_engine::progress::ProgressEvent::PublicAudience { armed } => {
                        if let Some(st) = state.upgrade() {
                            st.public_audience.lock().await.insert(held_key.clone(), armed);
                        }
                    }
                    _ => {}
                }
                let now = tokio::time::Instant::now();
                if now >= next_flush {
                    flush_progress(&mut fold, &folder, &event_tx, &mut idle_since, now);
                    next_flush = now + PROGRESS_FLUSH_INTERVAL;
                }
            }
            _ = tokio::time::sleep_until(next_flush) => {
                let now = tokio::time::Instant::now();
                flush_progress(&mut fold, &folder, &event_tx, &mut idle_since, now);
                next_flush = now + PROGRESS_FLUSH_INTERVAL;
            }
        }
    }
    // The channel closed, so this engine is gone. Drop its entry rather than
    // leaving the last count behind: a stopped engine reports nothing, and a
    // held count outliving the engine that observed it is exactly the stale
    // number the `ListEngines` field's contract rules out. A set that is still
    // bound simply reports 0 until its next engine completes a pass.
    if let Some(st) = state.upgrade() {
        st.deletes_held.lock().await.remove(&held_key);
        st.deletes_skipped_unreadable.lock().await.remove(&held_key);
        // A stopped engine arms nothing: its set is no share target until a
        // running engine reports again (fail-closed).
        st.public_audience.lock().await.remove(&held_key);
    }
    // Same rule for the live window: an engine that stopped mid-transfer must
    // not leave a half-finished byte count painted on a surface forever.
    if !fold.is_closed() {
        fold.close_window();
        let _ = event_tx.send(Event {
            event: fauna_ipc::sync::EventKind::SyncProgress(fold.snapshot(&folder)),
        });
    }
}

/// What an on-demand root must hold open from BEFORE its engine is built until
/// after its loop ends. linux: the descriptor reach on the bound directory, which
/// the FUSE mount will cover (`crate::fuse_host` — `None` for an always-resident
/// root, and for an on-demand one whose directory could not be opened, which then
/// runs unmounted over the directory itself). Elsewhere nothing: cfapi and the
/// always-resident loop work on the bound path directly.
#[cfg(target_os = "linux")]
type RootReach = Option<crate::fuse_host::Reach>;
#[cfg(not(target_os = "linux"))]
struct RootReach;

/// One root's line in [`SyncServiceState::on_demand_mount_errors`]: an
/// on-demand root whose placeholder surface could not be mounted says why here,
/// and `ListLocations` carries the code to the app's binding row
/// (`on-demand-files.md` § Linux FUSE binding, the lifecycle rule — the
/// per-location refusal the boot probe cannot see). The entry is this root's and
/// ends with it: dropping the report — the root's task ending, however it ends —
/// removes it, so no row is ever handed a failure no running root stands behind.
pub(crate) struct MountReport {
    state: Weak<SyncServiceState>,
    path: PathBuf,
}

impl MountReport {
    fn new(state: Weak<SyncServiceState>, path: &Path) -> Self {
        let report = Self {
            state,
            path: path.to_path_buf(),
        };
        // A root that starts has not failed yet, whatever its predecessor at
        // this path left behind in the moment before its own drop.
        report.clear();
        report
    }

    /// The mount failed with `error`: record the code a row renders.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn failed(&self, error: &anyhow::Error) {
        let code = mount_error_code(error);
        if let Some(state) = self.state.upgrade()
            && let Ok(mut errors) = state.on_demand_mount_errors.lock()
        {
            errors.insert(self.path.clone(), code);
        }
    }

    fn clear(&self) {
        if let Some(state) = self.state.upgrade()
            && let Ok(mut errors) = state.on_demand_mount_errors.lock()
        {
            errors.remove(&self.path);
        }
    }
}

impl Drop for MountReport {
    fn drop(&mut self) {
        self.clear();
    }
}

/// The `ON_DEMAND_MOUNT_*` code for a failed mount. A permission refusal is the
/// location's — a confined `fusermount3` admits a mount point only under the
/// user's home, `/mnt`, `/media`, `/run/user/<uid>` or `/tmp`, and the remedy
/// (a folder somewhere it may mount) is the user's to take; anything else is the
/// generic failure.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn mount_error_code(error: &anyhow::Error) -> &'static str {
    let refused = error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied)
            || cause
                .to_string()
                .to_ascii_lowercase()
                .contains("permission denied")
    });
    if refused {
        fauna_ipc::sync::ON_DEMAND_MOUNT_REFUSED
    } else {
        fauna_ipc::sync::ON_DEMAND_MOUNT_FAILED
    }
}

/// Open `path`'s [`RootReach`] for a root of `mode`.
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
fn open_root_reach(
    mode: LocationMode,
    path: &Path,
    folder: &str,
    mount_report: &MountReport,
) -> RootReach {
    #[cfg(target_os = "linux")]
    {
        if mode != LocationMode::OnDemand {
            return None;
        }
        match crate::fuse_host::Reach::open(path) {
            Ok(reach) => Some(reach),
            Err(e) => {
                tracing::error!(
                    folder,
                    path = %path.display(),
                    error = %e,
                    "opening the on-demand root's directory failed; the root runs unmounted \
                     and lists no placeholders"
                );
                mount_report.failed(&e);
                None
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        RootReach
    }
}

/// The directory a root's engine, watcher and scans work on: the reach when the
/// root has one — the directory UNDER the mount — else the bound path itself.
#[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
fn root_dir(reach: &RootReach, path: &Path) -> PathBuf {
    #[cfg(target_os = "linux")]
    if let Some(reach) = reach {
        return reach.path().to_path_buf();
    }
    path.to_path_buf()
}

/// The on-demand (placeholder) engine: bind the platform's provider root — a
/// cfapi sync root on windows, a FUSE mount over the directory on linux — route
/// its requests here, and serve placeholders until cancelled.
///
/// **Two-way since 2026-07-14** (`file-sync.md` § On-Demand Files → *Sync direction*).
/// `serve_hydration_root` runs the shared `always_resident` watcher/debouncer/uploader
/// alongside the read callbacks, so a local edit to a tracked file is uploaded exactly as
/// it would be from an always-resident folder — no second uploader, no new user-facing
/// setting. Before that, a local edit here was silently orphaned.
#[allow(clippy::too_many_arguments)]
async fn run_on_demand_root(
    engine: fauna_sync_engine::engine::SyncEngine,
    path: PathBuf,
    // Dropped after the loop's future ends: the engine and the mount both stand
    // on the reach's descriptor.
    reach: RootReach,
    // Where a failed mount is said, for the binding's row; dropped with the
    // root, which retracts it.
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))] mount_report: MountReport,
    folder: String,
    // The engine's own state DB — the full-teardown hook reopens it to clear the
    // seen marks (decision (e)) after the engine that held it has gone.
    #[cfg_attr(not(windows), allow(unused_variables))] db_path: PathBuf,
    event_tx: broadcast::Sender<Event>,
    cancel: CancellationToken,
    // The engine's per-folder nudge + command receivers ([`EngineInbox`]) —
    // the same two the always-resident root takes, served by the hydration loop.
    wake_rx: Option<mpsc::Receiver<()>>,
    engine_cmd_rx: Option<mpsc::Receiver<always_resident::EngineCommand>>,
    // This folder's relay asks — an on-demand root serves its hydrated bodies
    // (a placeholder answers none).
    serve: ServeInbox,
) {
    // This root's command channel: the cfapi callbacks (routed by the connection
    // key) push Fetch/Populate here; the loop serves them.
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<HydrationCommand>();

    // Decision (e)'s input, read BEFORE anything registers: did this root's
    // registration survive the downtime? If not, the OS removed its placeholders
    // at the unregister and the loop clears every seen mark before its boot sweep
    // (`delete-propagation.md` § *An offline placeholder delete propagates*).
    //
    // A linux FUSE root ALWAYS boots fresh: its placeholders are never on the
    // disk, so a seen mark is never true of it, and one that arrived with an
    // inherited state DB must not meet the sweep (`on-demand-files.md` § Linux
    // FUSE binding, the dehydrate rule, obligation 3).
    #[cfg(windows)]
    let fresh_registration = !crate::cfapi_host::registration_survived(&path);
    #[cfg(target_os = "linux")]
    let fresh_registration = true;
    #[cfg(not(any(windows, target_os = "linux")))]
    let fresh_registration = false;

    // The root's connection, handed to the loop to make AFTER its boot sweep
    // (decision (c) — evidence before re-population): register the cfapi sync root
    // for this folder — SHELL-registered (`register_and_connect_shell`), so a real
    // user's root renders Explorer's cloud verbs / status column (a filter-only
    // root is registered over in place) — and route its callbacks to this
    // engine's channel. The returned guard disconnects when the loop's future ends
    // and KEEPS the registration unless `reconcile_engines` marked this root's
    // binding as ended — see `crate::cfapi_host` (registration belongs to the
    // binding, not the serve-session); on that full teardown it first clears the
    // seen marks, since the unregister removes every cloud-only placeholder and
    // that removal is never a user's delete (decision (e)). If registration fails
    // the engine still runs but receives no callbacks (serves nothing) — logged.
    #[cfg(windows)]
    let connect = {
        let (path, folder, cmd_tx) = (path.clone(), folder.clone(), cmd_tx.clone());
        move || match crate::cfapi_host::register_and_connect_product_root(
            &path, &folder, cmd_tx, db_path,
        ) {
            Ok(conn) => Some(conn),
            Err(e) => {
                tracing::error!(
                    folder,
                    path = %path.display(),
                    error = %e,
                    "cfapi sync-root registration failed; root serves nothing"
                );
                None
            }
        }
    };
    // linux: the mount IS the connection — made after the boot sweep like the
    // cfapi registration, its guard unmounting when the loop's future ends. A
    // failed mount (no `fusermount3`, a sandbox without `/dev/fuse`) leaves the
    // root running unmounted: the hydrated files keep syncing two-way, and no
    // placeholder is listed.
    #[cfg(target_os = "linux")]
    let connect = {
        let (reach, folder, cmd_tx) = (reach.as_ref(), folder.clone(), cmd_tx.clone());
        let mount_report = &mount_report;
        move || {
            let reach = reach?;
            match crate::fuse_host::mount_over(reach, &folder, cmd_tx) {
                Ok(mounted) => Some(mounted),
                Err(e) => {
                    mount_report.failed(&e);
                    tracing::error!(
                        folder,
                        path = %reach.mount_point().display(),
                        error = %e,
                        "mounting the on-demand root failed; root lists no placeholders"
                    );
                    None
                }
            }
        }
    };
    #[cfg(not(any(windows, target_os = "linux")))]
    let connect = || ();

    // Keep the send half alive for the whole serve loop so `cmd_rx` stays open. The
    // platform binding holds a clone too (the cfapi callback context, the FUSE
    // session); where there is none the host serves nothing and only `cancel` ends
    // the loop.
    let _cmd_tx = cmd_tx;

    // The loop works on the directory UNDER a linux mount, never on the view.
    let sync_root = root_dir(&reach, &path);
    // The OS binding's half of a dehydrate, a pin and an in-sync flip: cfapi on
    // windows; on linux the FUSE root's, whose placeholders are rows only.
    #[cfg(target_os = "linux")]
    let invalidator = crate::fuse_host::FuseInvalidator::new(&sync_root, _cmd_tx.clone());
    #[cfg(not(target_os = "linux"))]
    let invalidator = crate::bridge::CfapiInvalidator;
    crate::bridge::serve_hydration_root(
        engine,
        invalidator,
        crate::bridge::RootBoot {
            fresh_registration,
            connect,
        },
        cmd_rx,
        Some(event_tx),
        sync_root,
        folder,
        cancel,
        wake_rx,
        engine_cmd_rx,
        Some(serve),
    )
    .await;
    // On return (cancel), the loop's connection guard drops → disconnect; the
    // registration stays unless this root's binding ended
    // (`mark_root_for_full_teardown`).
}

/// The always-resident engine: converge, re-seal, then watch the folder and upload
/// local edits — the **shared** [`always_resident`] loop, driven by this agent on
/// every platform that runs one.
///
/// **The pre-bind (M2) re-seal is ungated and runs on every engine start — this
/// call is the ONE mechanism** (`mls-group-key-material.md` § M2 *Pre-bind re-seal
/// migration*; the sentinel that once gated it was retired 2026-09-25 with no reader
/// left). `reseal_pending_under_current` needs only the
/// pushed content keys + the local `SyncDb` — never the identity keypair this
/// bearer-only agent does not hold — is idempotent (a successful record stamps the
/// local row's `content_key_version`, and the pass skips rows already at the target
/// generation, so it terminates on its own) and returns `0` for an unbound set, so
/// re-driving it on every start is safe; a failed pass leaves its rows unstamped
/// and the next start retries. Skipping it would be the unsafe option — an owner
/// who shares a set they already populated would leave the pre-bind back-catalogue
/// sealed under their `BackupKey`, i.e. **undecryptable for every joiner**.
async fn run_always_resident_root(
    engine: fauna_sync_engine::engine::SyncEngine,
    path: PathBuf,
    folder: String,
    cancel: CancellationToken,
    // Remote-change nudge receiver (`file-sync.md` § Remote-change nudge); `Some`
    // for the agent's resident engines (wired to `SyncServiceState::wake_senders`),
    // threaded straight into the shared watch loop.
    wake_rx: Option<mpsc::Receiver<()>>,
    // Invoke-and-reply command receiver (`EngineCommand` — apply-held-deletes);
    // `Some` for the agent's resident engines (wired to
    // `SyncServiceState::engine_cmd_senders`), threaded into the same loop.
    cmd_rx: Option<mpsc::Receiver<fauna_sync_engine::always_resident::EngineCommand>>,
    // This folder's relay asks, answered beside everything below — the
    // start-up passes included, so a seat catching up on a large folder still
    // serves (and still declines at once what it does not hold).
    mut serve: ServeInbox,
) {
    // `SyncEngine` is `!Sync` (its `SyncDb` wraps a raw `rusqlite::Connection`,
    // itself `RefCell`-based); the `Arc` below is a single-task refcount for the
    // caller-handle contract `run_watch_loop` documents, never a cross-thread
    // share — this future is `!Send` end to end (`engine_host.rs`'s
    // `EngineFuture` is a bare `Pin<Box<dyn Future<Output = ()>>>` precisely so
    // it is never `tokio::spawn`ed onto a multi-thread runtime).
    #[allow(clippy::arc_with_non_send_sync)]
    let engine = std::sync::Arc::new(engine);
    let serving_engine = std::sync::Arc::clone(&engine);
    tokio::select! {
        () = serve.serve(&serving_engine) => {}
        () = drive_always_resident_root(engine, path, folder, cancel, wake_rx, cmd_rx) => {}
    }
}

/// [`run_always_resident_root`]'s own work: the start-up passes, then the
/// shared watch loop until cancelled.
async fn drive_always_resident_root(
    engine: std::sync::Arc<fauna_sync_engine::engine::SyncEngine>,
    path: PathBuf,
    folder: String,
    cancel: CancellationToken,
    wake_rx: Option<mpsc::Receiver<()>>,
    cmd_rx: Option<mpsc::Receiver<fauna_sync_engine::always_resident::EngineCommand>>,
) {
    // The control plane must be open before `changes.record` (upload) or
    // `pull_remote_changes` can run. The driver connected it before the build
    // (`build_until_served`), which reads over it, so this is a no-op there and
    // its supervisor re-dials on its own — an unreachable nest just means the
    // first rescan tick retries.
    //
    // Deliberately NOT `HydrationHost::prepare`: that also folds `changes.list` into
    // cfapi *placeholders*, which is meaningless here — an always-resident folder
    // holds real files, not placeholders.
    if let Err(e) = engine.connect_control_plane().await {
        tracing::warn!(
            folder,
            error = %e,
            "control-plane connect failed; retrying on the rescan tick"
        );
    }

    // Always-resident: no placeholder surface, so converge's recorded rels are ignored.
    let _ = always_resident::LocalWriteHost::converge(&*engine, &folder).await;

    // The once-per-start corpus passes — the pre-bind (M2) re-seal (ungated; see
    // this fn's doc comment), phase 4's audience convergence (an audience flip
    // made real for the back-catalogue), and the post-succession re-seal (the
    // agent is the process that holds the retired owner keys on desktop,
    // `sync-agent.md` § Credential model, so it is the one positioned to move
    // the bytes off them). The SAME sequence the on-demand root runs through
    // `HydrationHost::converge_corpus_at_start` — one home, so the two modes
    // cannot drift on what a start owes the corpus.
    always_resident::converge_corpus_at_start(&engine, &folder).await;

    // This set's **nest-authoritative** cadence, read off the already-open control
    // plane via the shared resolver — never a client-invented constant
    // (`file-sync.md`: *"A client must read this cadence, never invent one"*). The
    // reader lives on the engine's `HydrationHost` impl; the trait name is
    // historical, the cadence read is not hydration-specific (every failure mode
    // degrades to the shared `DEFAULT_RESCAN_INTERVAL`).
    let rescan_interval = HydrationHost::rescan_interval(&*engine).await;

    // The seat's sync mode is no longer installed here: `run_watch_loop`
    // resolves it itself at entry — above its eager first pull — and re-resolves
    // on every rescan tick (`SyncEngine::refresh_sync_mode`; `file-sync.md`
    // § 4). The once-per-process install this spot used to hold was
    // leg 1: a role the user changed never reached the running engine.

    // Cancellation is raced here, exactly as Linux does it in its `EngineSpec::run`:
    // dropping the loop future drops its `FsWatcher` and releases the OS watch handle.
    tokio::select! {
        _ = cancel.cancelled() => {}
        _ = always_resident::run_watch_loop(
            engine,
            path,
            folder,
            rescan_interval,
            wake_rx,
            cmd_rx,
        ) => {}
    }
}

// ---------------------------------------------------------------------------
// The running multi-root host + reconciliation against config/capability
// ---------------------------------------------------------------------------

/// The running multi-root sync host: the shared [`EngineHost`] multiplexer plus the
/// folders currently started on it, each mapped to the [`EngineStamp`] its engine
/// was last (re)built under. Tracking `started` lets [`reconcile_engines`] start
/// only newly-eligible folders, stop only no-longer-eligible ones, and **restart** a
/// live engine whose content-key material or mode changed, never needlessly
/// restarting an unchanged one. Dropping it cancels every engine (each tears its
/// cfapi root or watcher down).
pub struct RunningEngines {
    host: EngineHost<EngineMapping>,
    started: HashMap<String, EngineStamp>,
    /// The account inputs the host's [`LocationEngineSpec`] was built with. They are
    /// frozen for the host's lifetime — every engine `Start` reuses them — so this
    /// is the host's *account identity*, and [`reconcile_engines`] rebuilds the
    /// whole host when the provisioned capability no longer matches it.
    inputs: SpecInputs,
}

impl RunningEngines {
    /// True while at least one folder is being served — the "syncing" status the
    /// IPC handlers report. An idle host (zero started) reports `false`.
    pub fn is_serving(&self) -> bool {
        !self.started.is_empty()
    }

    /// Whether `folder` is currently started (being served). [`ListEngines`]
    /// (`pipe_server`) uses this to mark each *planned* engine as actively serving
    /// vs. bound-but-not-started (e.g. no capability yet, or the device is paused).
    pub fn is_serving_set(&self, engine_key: &str) -> bool {
        self.started.contains_key(engine_key)
    }
}

/// Bring the running sync host in line with config + capability: serve every folder
/// bound to a folder (on-demand *and* always-resident), and stop any that is no
/// longer bound / present. Idempotent and best-effort — a re-trigger starts only
/// newly-eligible folders and stops only no-longer-eligible ones, restarting a live
/// engine only when its key material or its mode changed. Called from the IPC
/// handlers that can change the desired set (capability provision, folder mode /
/// folder change, folder removal).
///
/// Without a well-formed capability no engine can be built, so any running engines
/// are stopped and none are started until it arrives. `nest_url` is taken from the
/// provisioned capability (app-provisioned at login), not `device.toml`.
///
/// Note: the shared engine inputs (`actor_id`/`backup_key`/url/device id) are
/// captured once when the host is first built, so they are the host's **account
/// identity**. A capability re-provisioned with *different* inputs — an account
/// switch, or a sign-out/sign-in as someone else on the same long-lived agent —
/// therefore rebuilds the whole host rather than starting the incoming account's
/// engines under the outgoing account's identity. Within one account (a bearer
/// rotation, a content-key re-resolve) the inputs are unchanged and the host is
/// reused, with only the per-engine [`EngineStamp`] deciding what restarts.
pub async fn reconcile_engines(state: &Arc<SyncServiceState>) -> Result<()> {
    // Every reconcile is a content-key edge (`crate::content_keys`): wake the task
    // to re-read custody. It re-enters [`reconcile_resolved`] when the resolution
    // changed; this reconcile runs now on what the agent holds.
    state.content_keys_wake.notify_one();
    reconcile_resolved(state).await
}

/// [`reconcile_engines`] without the content-key wake — the content-key task's
/// own re-entry after a re-resolve changed what it holds (waking itself again
/// would re-resolve for nothing).
pub(crate) async fn reconcile_resolved(state: &Arc<SyncServiceState>) -> Result<()> {
    // Resolve the desired engines + shared inputs, releasing the config / capability
    // locks BEFORE taking the engines lock (the IPC handlers take config then
    // engines; the reverse order here would risk a deadlock).
    let (desired, inputs) = {
        let config = state.config.read().await;
        // Device-global pause (sync-agent.md § Control plane split — Pause/Resume):
        // run nothing while paused. The bound sets stay in config, so `ListEngines`
        // still lists them (serving=false); only the *running* desired set is emptied
        // here, so a reconcile stops every live engine and starts none until Resume.
        let desired = if config.paused {
            Vec::new()
        } else {
            plan_engines(&config)
        };
        let cap_guard = state.capability.read().await;
        match cap_guard.as_ref() {
            Some(cap) => {
                let inputs = match (cap.actor_id_array(), cap.backup_key_array()) {
                    (Some(actor_id), Some(backup_key)) => Some(SpecInputs {
                        // nest_url is sourced from the provisioned capability, not device.toml.
                        nest_base_url: cap.nest_url.clone(),
                        // device_id is carried by the capability as a string — hex-decode
                        // with zero fallback.
                        device_id: hex::decode(&cap.device_id)
                            .ok()
                            .and_then(|v| <[u8; 32]>::try_from(v).ok())
                            .unwrap_or([0u8; 32]),
                        actor_id,
                        backup_key,
                        predecessors: crate::bridge::AgentPredecessors::from_capability(cap),
                    }),
                    _ => None, // malformed capability
                };
                (desired, inputs)
            }
            None => (desired, None), // no capability yet
        }
    };
    // Every set's keys as the agent last resolved them from custody, resolved per
    // set here so the stamps are computed after the lock is released. `None` = not
    // resolved yet: no set is named, so none starts.
    let keys: HashMap<String, FolderEngineKeys> = {
        let resolved = state.content_keys.read().await;
        let resolved = resolved
            .as_ref()
            .filter(|r| inputs.as_ref().is_some_and(|i| r.is_for(i.actor_id)));
        desired
            .iter()
            .filter_map(|m| {
                resolved
                    .and_then(|r| r.keys_for(m.folder_ref))
                    .map(|k| (m.engine_key(), k))
            })
            .collect()
    };

    // Which actor's scoped state dir the engines serve — part of each engine's
    // identity (`file-sync.md` § Multi-account × File Provider, consequence 3), so a
    // per-actor re-scope forces a rebuild under the incoming actor's DBs. `None`
    // until a capability is provisioned; set on every layout (production and
    // `--data-dir` alike) by `apply_actor_scope`. Read once from the shared `Arc` slot.
    let actor_scope = state.paths.actor_scope();

    let mut guard = state.engines.lock().await;

    // No well-formed capability: stop any running engines, start none.
    let Some(inputs) = inputs else {
        if let Some(running) = guard.as_mut() {
            let tx = running.host.command_sender();
            for (key, _stamp) in running.started.drain() {
                let _ = tx.send(EngineCommand::Stop { key });
            }
        }
        return Ok(());
    };

    // Fail closed on the agent's own resolution: a set it does not name is not
    // owner-only, it is not keyed *yet* — bound or served after the last
    // re-resolve, custody not yet readable for it, or not resolved at all — and an
    // engine built now would seal it on the owner path. Withhold it; the next edge
    // whose resolution names it re-enters here, and it starts keyed. (A live
    // engine whose set drops out of the resolution is stopped by the stale loop
    // below, the same as a set whose binding ended.)
    let desired: Vec<EngineMapping> = desired
        .into_iter()
        .filter(|m| {
            let keyed = keys.contains_key(&m.engine_key());
            if !keyed {
                tracing::info!(
                    folder = %m.folder,
                    "not in the agent's content-key resolution yet; withholding its engine \
                     until an edge names it"
                );
            }
            keyed
        })
        .collect();
    let desired_keys: HashSet<String> = desired.iter().map(EngineMapping::engine_key).collect();

    // The host's account identity (`actor_id` / `BackupKey` / device id / nest url)
    // is FROZEN into its `LocationEngineSpec` when the host is first built, and every
    // later `Start` reuses it — so a host outliving the account it was built for
    // would serve the *incoming* account's sets under the *outgoing* account's
    // identity: the engine dials `AuthClient::bearer_only` on the stale `actor_id`,
    // i.e. the previous actor's WS path carrying the new actor's bearer, and seals
    // under the previous actor's `BackupKey`. Nothing else rebuilds it — an unbind
    // to zero engines keeps the host, and so does an `UnprovisionCapability`
    // (the no-capability arm above stops engines but leaves the host in place) —
    // so this is the one place the swap can be caught. Rebuild from scratch:
    // dropping the host cancels every engine of the outgoing account, each tearing
    // its own root down, and the `None` arm below then builds a fresh spec.
    if guard
        .as_ref()
        .is_some_and(|running| running.inputs != inputs)
    {
        tracing::info!(
            "provisioned account changed; rebuilding the sync host under the new identity"
        );
        if let Some(running) = guard.as_ref() {
            // The outgoing account's bindings all end here, so their sync-root
            // registrations are fully unregistered (mark before the drop that
            // cancels them, same ordering as the stale-engine loop below).
            #[cfg(windows)]
            for stamp in running.started.values() {
                if let Some(folder) = full_teardown_location(stamp, None) {
                    crate::cfapi_host::mark_root_for_full_teardown(&folder);
                }
            }
            #[cfg(not(windows))]
            let _ = running;
        }
        drop(guard.take());
    }

    match guard.as_mut() {
        None => {
            if desired.is_empty() {
                return Ok(()); // nothing to serve yet — don't spin up an idle host
            }
            // First eligible engines: build the host with them as initial engines,
            // recording each one's stamp so a later rotation/mode-flip restarts it.
            let started: HashMap<String, EngineStamp> = desired
                .iter()
                .filter_map(|m| {
                    // `desired` is already filtered to the sets the resolution names,
                    // so every stamp resolves; `filter_map` keeps that a fact
                    // of the data rather than an `expect`.
                    engine_stamp(
                        keys.get(&m.engine_key()),
                        m.mode.clone(),
                        &m.path,
                        actor_scope.as_deref(),
                    )
                    .map(|stamp| (m.engine_key(), stamp))
                })
                .collect();
            let spec = LocationEngineSpec::new(
                inputs.clone(),
                state.capability.clone(),
                state.ipc_event_tx.clone(),
                state.paths.clone(),
                // Weak: the state owns the host, the host owns this spec — a
                // strong handle here would close the cycle and leak the service.
                Arc::downgrade(state),
            );
            let host = EngineHost::start(spec, desired);
            tracing::info!(engines = started.len(), "multi-root sync host started");
            *guard = Some(RunningEngines {
                host,
                started,
                inputs,
            });
        }
        Some(running) => {
            let tx = running.host.command_sender();
            // Start newly-eligible engines, and RESTART a live one whose stamp changed
            // — a re-resolve found a rotate-on-removal / bind / re-bind, or the user
            // flipped the folder's mode (which swaps the whole loop). An
            // `EngineHost` `Start` on an existing key cancels the old engine and
            // rebuilds it, and the rebuild re-reads the agent's resolution
            // (`LocationEngineSpec::run`), so the restarted engine seals/opens under the
            // new content-key generation and runs the new mode's loop.
            for mapping in desired {
                let Some(stamp) = engine_stamp(
                    keys.get(&mapping.engine_key()),
                    mapping.mode.clone(),
                    &mapping.path,
                    actor_scope.as_deref(),
                ) else {
                    continue; // filtered above; unreachable by construction
                };
                let engine_key = mapping.engine_key();
                match running.started.get(&engine_key) {
                    None => {
                        tracing::info!(
                            folder = %mapping.folder,
                            mode = ?mapping.mode,
                            "serving folder"
                        );
                        running.started.insert(engine_key.clone(), stamp);
                        let _ = tx.send(EngineCommand::Start(mapping));
                    }
                    Some(prev) if *prev != stamp => {
                        tracing::info!(
                            folder = %mapping.folder,
                            mode = ?mapping.mode,
                            "engine key material, folder or mode changed; restarting engine"
                        );
                        // If the change ends the old *binding* (folder swap /
                        // mode flip) rather than just rotating keys, the old
                        // root's registration goes down with the old engine —
                        // marked BEFORE the Start that cancels it, so the drop
                        // guard sees the mark whenever the cancel lands.
                        #[cfg(windows)]
                        if let Some(folder) = full_teardown_location(prev, Some(&mapping)) {
                            crate::cfapi_host::mark_root_for_full_teardown(&folder);
                        }
                        running.started.insert(engine_key.clone(), stamp);
                        let _ = tx.send(EngineCommand::Start(mapping));
                    }
                    Some(_) => { /* live + unchanged — leave it serving */ }
                }
            }
            // Stop engines no longer eligible (unbound, folder removed) — the
            // binding ended, so the sync-root registration is fully unregistered
            // by the retiring engine's drop guard (mark first, then Stop).
            let stale: Vec<String> = running
                .started
                .keys()
                .filter(|k| !desired_keys.contains(*k))
                .cloned()
                .collect();
            for key in stale {
                let prev = running.started.remove(&key);
                tracing::info!(folder = %key, "stopping folder");
                #[cfg(windows)]
                if let Some(folder) = prev.as_ref().and_then(|p| full_teardown_location(p, None)) {
                    crate::cfapi_host::mark_root_for_full_teardown(&folder);
                }
                #[cfg(not(windows))]
                let _ = prev;
                let _ = tx.send(EngineCommand::Stop { key });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocationConfig;
    use fauna_core::folder_keys::{FolderContentKeys, FolderEngineKeys, FolderRef};

    /// The fixtures' stable per-name identity: one set per name unless a test
    /// builds a same-named twin by hand.
    fn test_ref(folder: &str) -> FolderRef {
        FolderRef::Local(folder.bytes().fold(0i64, |acc, b| {
            acc.wrapping_mul(31).wrapping_add(i64::from(b))
        }))
    }

    /// A location bound to `folder` under its [`test_ref`], or unbound.
    fn location(path: &str, mode: LocationMode, folder: Option<&str>) -> LocationConfig {
        LocationConfig {
            path: path.to_string(),
            mode,
            folder: folder.map(str::to_string),
            folder_id: folder.map(|f| test_ref(f).to_wire()),
            ..Default::default()
        }
    }

    /// Service state with a `--data-dir` override so config saves + per-set state
    /// DBs land under the test's temp root.
    fn service_state(paths: crate::config::SyncPaths) -> Arc<SyncServiceState> {
        let (shutdown_tx, _shutdown_rx) = tokio::sync::watch::channel(false);
        let (event_tx, _event_rx) = tokio::sync::broadcast::channel(16);
        SyncServiceState::new(SyncConfig::default(), shutdown_tx, event_tx, paths)
    }

    /// A root whose mount failed says why for exactly as long as it runs: a
    /// permission refusal is the location's code (the confined mount helper),
    /// anything else the generic one, and the root ending — a flip back to
    /// always, an unbind, a restart — retracts the line, so `ListLocations`
    /// never reports a failure no running root stands behind.
    #[tokio::test]
    async fn a_failed_mount_is_reported_for_its_root_and_retracted_with_it() {
        let tmp = tempfile::tempdir().unwrap();
        let state = service_state(SyncPaths::new(Some(tmp.path().to_path_buf())));
        let path = Path::new("/srv/outside-the-admitted-set");
        let code = |state: &Arc<SyncServiceState>| {
            state
                .on_demand_mount_errors
                .lock()
                .unwrap()
                .get(path)
                .copied()
        };

        let report = MountReport::new(Arc::downgrade(&state), path);
        assert_eq!(code(&state), None, "a root that starts has not failed");

        let refused =
            anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
                .context("mounting the on-demand root");
        report.failed(&refused);
        assert_eq!(code(&state), Some(fauna_ipc::sync::ON_DEMAND_MOUNT_REFUSED));

        report.failed(&anyhow::anyhow!(
            "fusermount3: mount failed: Permission denied"
        ));
        assert_eq!(
            code(&state),
            Some(fauna_ipc::sync::ON_DEMAND_MOUNT_REFUSED),
            "the helper's own words count when no io error is in the chain"
        );

        report.failed(&anyhow::anyhow!("the mount helper went away"));
        assert_eq!(code(&state), Some(fauna_ipc::sync::ON_DEMAND_MOUNT_FAILED));

        drop(report);
        assert_eq!(code(&state), None, "the line ends with its root");
    }

    /// Every engine's two per-folder channels register through [`EngineInbox`]
    /// BEFORE `run` branches on the mode, so an on-demand engine is routable
    /// exactly like an always-resident one (until 2026-09-29 only the `Always`
    /// arm registered, and the mass-delete floor's confirm on an on-demand root
    /// answered "not being served by a resident engine"). Registration puts both
    /// senders under the ref, a routed command reaches the receiver the engine's
    /// loop drains, and a deregister removes only entries still its own — a
    /// restarted engine that registered in between keeps its channels.
    #[tokio::test]
    async fn an_engine_inbox_registers_both_senders_and_deregisters_only_its_own() {
        let tmp = tempfile::tempdir().unwrap();
        let state = service_state(SyncPaths::new(Some(tmp.path().to_path_buf())));
        let weak = Arc::downgrade(&state);
        let key = test_ref("docs").to_wire();

        let (first, _wake_rx, mut cmd_rx) = EngineInbox::register(&weak, key.clone()).await;
        assert!(state.wake_senders.lock().await.contains_key(&key));
        let (reply, _reply_rx) = tokio::sync::oneshot::channel();
        let routed = state
            .engine_cmd_senders
            .lock()
            .await
            .get(&key)
            .expect("the command sender is registered under the ref")
            .try_send(always_resident::EngineCommand::ApplyHeldDeletes { reply });
        assert!(routed.is_ok());
        assert!(
            matches!(
                cmd_rx.try_recv(),
                Ok(always_resident::EngineCommand::ApplyHeldDeletes { .. })
            ),
            "a routed command reaches the receiver the engine's loop drains"
        );

        // An engine restart registers before the old run deregisters.
        let (second, _wake_rx2, _cmd_rx2) = EngineInbox::register(&weak, key.clone()).await;
        first.deregister().await;
        assert!(
            state
                .wake_senders
                .lock()
                .await
                .get(&key)
                .is_some_and(|cur| cur.same_channel(&second.wake_tx)),
            "the old run must not remove the restarted engine's nudge sender"
        );
        assert!(
            state
                .engine_cmd_senders
                .lock()
                .await
                .get(&key)
                .is_some_and(|cur| cur.same_channel(&second.cmd_tx)),
            "the old run must not remove the restarted engine's command sender"
        );

        second.deregister().await;
        assert!(state.wake_senders.lock().await.is_empty());
        assert!(state.engine_cmd_senders.lock().await.is_empty());
    }

    /// A well-formed capability for `actor_id`, differing from its sibling in
    /// exactly the identity fields (`actor_id` / `backup_key` / `device_id`).
    fn capability_for(actor: u8) -> fauna_ipc::sync::SyncCapability {
        fauna_ipc::sync::SyncCapability::new(
            vec![actor.wrapping_add(0x40); 32], // backup_key
            vec![actor; 32],                    // actor_id
            "https://nest.example".into(),
            hex::encode([actor; 32]),
            fauna_ipc::sync::BearerToken::new(format!("tok-{actor}"), 9_999),
        )
    }

    /// Bind one folder to `folder` in the agent's config (the persisted shape
    /// `plan_engines` reads), replacing whatever was bound before.
    async fn bind_only(state: &Arc<SyncServiceState>, path: &str, folder: &str) {
        let mut config = state.config.write().await;
        config.locations = vec![location(path, LocationMode::Always, Some(folder))];
    }

    /// The bind→unbind→bind agent-lifecycle break, at its real seam.
    ///
    /// `LocationEngineSpec` freezes `actor_id` / `backup_key` / `device_id` /
    /// `nest_base_url` when the host is **first** built, and nothing ever rebuilt
    /// the host afterwards: not an unbind to zero engines, not an
    /// `UnprovisionCapability`, not a re-provision under a *different actor*. So
    /// every engine started on an already-built host ran under the identity of
    /// whichever actor happened to bind first — the engine dials
    /// `AuthClient::bearer_only` on the stale `actor_id`, i.e. the previous
    /// actor's WS path carrying the new actor's bearer, and the new actor's
    /// uploads never land (`fauna.media.list` stays empty).
    ///
    /// This is what blocked `@pytest.mark.tui` on
    /// `test_media.py::test_media_delete_removes_the_file_from_disk`: run
    /// `test_sync_live_apply.py` first (it binds and unbinds as the shared actor,
    /// leaving a built host behind) and the media fixture's *dedicated* actor then
    /// reaches `running: True` with its folder listed while the engine uploads
    /// nothing. Not tui-specific — windows drives the same agent.
    #[tokio::test]
    async fn a_host_built_for_one_actor_is_rebuilt_when_another_actor_provisions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = crate::config::SyncPaths::new(Some(tmp.path().to_path_buf()));
        let state = service_state(paths);

        // Actor A provisions and binds a folder → the host is built under A.
        *state.capability.write().await = Some(capability_for(0xAA));
        bind_only(&state, r"/tmp/folder-a", "set-a").await;
        reconcile_engines(&state)
            .await
            .expect("reconcile for actor A");
        assert_eq!(
            state
                .engines
                .lock()
                .await
                .as_ref()
                .expect("actor A's bind must build the host")
                .inputs
                .actor_id,
            [0xAAu8; 32],
            "precondition: the host serves actor A"
        );

        // Actor A unbinds — zero engines, but the host object survives.
        state.config.write().await.locations.clear();
        reconcile_engines(&state)
            .await
            .expect("reconcile after unbind");
        assert!(
            !state
                .engines
                .lock()
                .await
                .as_ref()
                .expect("the host survives an unbind to zero engines")
                .is_serving(),
            "precondition: nothing is served after the unbind"
        );

        // Actor B signs in on the same long-lived agent and binds its own folder.
        *state.capability.write().await = Some(capability_for(0xBB));
        bind_only(&state, r"/tmp/folder-b", "set-b").await;
        reconcile_engines(&state)
            .await
            .expect("reconcile for actor B");

        let guard = state.engines.lock().await;
        let running = guard.as_ref().expect("actor B's bind must leave a host");
        assert_eq!(
            running.inputs.actor_id, [0xBBu8; 32],
            "THE BUG: the engine serving actor B was started on the host built for \
             actor A, so it runs under actor A's actor_id — its WS path resolves to \
             the previous actor while carrying actor B's bearer, and actor B's \
             uploads never land"
        );
        assert_eq!(
            running.inputs.backup_key,
            [0xBBu8.wrapping_add(0x40); 32],
            "the stale host also seals actor B's bytes under actor A's BackupKey"
        );
        assert!(
            running.is_serving_set(
                &EngineMapping {
                    path: PathBuf::from(r"/tmp/folder-b"),
                    folder: "set-b".to_string(),
                    folder_ref: test_ref("set-b"),
                    mode: LocationMode::Always,
                }
                .engine_key()
            ),
            "the rebuild must still serve actor B's freshly-bound set"
        );
        drop(guard);

        // Cancel the engines before the temp root goes away.
        drop(state.engines.lock().await.take());
    }

    /// The symmetric guard on the rebuild above: **within one account** the host
    /// must be reused, not rebuilt.
    ///
    /// The capability is re-provisioned constantly for reasons that are not an
    /// account change — the bearer rotates on the renewal cadence, and every app
    /// sign-in re-provisions the same account. Rebuilding on those would cancel every live engine (and, on
    /// windows, unregister its sync root) on a timer, killing in-flight transfers
    /// for no reason. Only the identity fields may force a rebuild, which is why
    /// [`SpecInputs`] holds those four and nothing else.
    #[tokio::test]
    async fn a_bearer_rotation_reuses_the_running_host() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = crate::config::SyncPaths::new(Some(tmp.path().to_path_buf()));
        let state = service_state(paths);

        *state.capability.write().await = Some(capability_for(0xAA));
        bind_only(&state, r"/tmp/folder-a", "set-a").await;
        reconcile_engines(&state).await.expect("initial reconcile");

        let engine_key = EngineMapping {
            path: PathBuf::from(r"/tmp/folder-a"),
            folder: "set-a".to_string(),
            folder_ref: test_ref("set-a"),
            mode: LocationMode::Always,
        }
        .engine_key();
        // The host's OWN identity, not its stamps: a rebuild recomputes an *equal*
        // stamp for an unchanged binding, so stamp equality cannot tell "reused"
        // from "rebuilt" and an assertion on it would pass either way. The command
        // channel is per-host, so it discriminates.
        let first_host = {
            let guard = state.engines.lock().await;
            let running = guard.as_ref().expect("the bind builds the host");
            assert!(
                running.started.contains_key(&engine_key),
                "precondition: the bound set is served"
            );
            running.host.command_sender()
        };

        // Same account, new bearer.
        *state.capability.write().await = Some(fauna_ipc::sync::SyncCapability::new(
            vec![0xAAu8.wrapping_add(0x40); 32],
            vec![0xAAu8; 32],
            "https://nest.example".into(),
            hex::encode([0xAAu8; 32]),
            fauna_ipc::sync::BearerToken::new("tok-rotated".into(), 19_999),
        ));
        reconcile_engines(&state)
            .await
            .expect("reconcile after rotation");

        let guard = state.engines.lock().await;
        let running = guard.as_ref().expect("the host must survive a rotation");
        assert_eq!(
            running.inputs.actor_id, [0xAAu8; 32],
            "the account did not change, so the host's identity must not either"
        );
        assert!(
            running.host.command_sender().same_channel(&first_host),
            "a bearer rotation must REUSE the running host — rebuilding here would \
             cancel every live engine (and unregister its sync root on windows) on \
             the renewal cadence, killing in-flight transfers"
        );
        assert!(
            running.started.contains_key(&engine_key),
            "the bound set must still be served after the rotation"
        );
        drop(guard);

        drop(state.engines.lock().await.take());
    }

    /// The regression that motivated this module's rewrite: an always-resident
    /// folder used to be filtered out here, so no engine — and therefore no watcher
    /// and no upload path — ever existed for it. A file dropped into it could never
    /// sync, silently and permanently.
    #[test]
    fn plans_always_resident_folders_not_just_on_demand() {
        let config = SyncConfig {
            locations: vec![
                location(r"C:\always", LocationMode::Always, Some("docs")),
                location(r"C:\od", LocationMode::OnDemand, Some("photos")),
            ],
            ..SyncConfig::default()
        };

        let plans = plan_engines(&config);

        assert_eq!(
            plans,
            vec![
                EngineMapping {
                    path: PathBuf::from(r"C:\always"),
                    folder: "docs".to_string(),
                    folder_ref: test_ref("docs"),
                    mode: LocationMode::Always,
                },
                EngineMapping {
                    path: PathBuf::from(r"C:\od"),
                    folder: "photos".to_string(),
                    folder_ref: test_ref("photos"),
                    mode: LocationMode::OnDemand,
                },
            ],
            "BOTH modes get an engine; an always-resident folder must not be dropped"
        );
    }

    #[test]
    fn skips_unbound_folders_in_either_mode() {
        let config = SyncConfig {
            locations: vec![
                location(r"C:\always-unbound", LocationMode::Always, None),
                location(r"C:\od-unbound", LocationMode::OnDemand, None),
                location(r"C:\bound", LocationMode::OnDemand, Some("docs")),
            ],
            ..SyncConfig::default()
        };

        let plans = plan_engines(&config);

        assert_eq!(
            plans,
            vec![EngineMapping {
                path: PathBuf::from(r"C:\bound"),
                folder: "docs".to_string(),
                folder_ref: test_ref("docs"),
                mode: LocationMode::OnDemand,
            }],
            "an unbound folder is never served under a manufactured folder name, in either mode"
        );
    }

    /// D4: a binding the owning nest has refused is dropped from the plan, so no
    /// engine is built for it — including after a restart, which is the whole
    /// reason the flag is persisted rather than kept in memory.
    #[test]
    fn skips_a_parked_binding_whose_access_was_revoked() {
        let mut revoked = location(r"C:\shared", LocationMode::Always, Some("photos"));
        revoked.access_revoked = true;
        let config = SyncConfig {
            locations: vec![
                revoked,
                location(r"C:\mine", LocationMode::Always, Some("docs")),
            ],
            ..SyncConfig::default()
        };

        assert_eq!(
            plan_engines(&config),
            vec![EngineMapping {
                path: PathBuf::from(r"C:\mine"),
                folder: "docs".to_string(),
                folder_ref: test_ref("docs"),
                mode: LocationMode::Always,
            }],
            "the parked set is not served; the caller's other bindings are untouched"
        );
    }

    /// The persist half of the park, and the two properties that keep it from
    /// doing damage: it marks **every** folder bound to the revoked set (a set may
    /// carry more than one), and it touches **no other set's** bindings — not
    /// even a same-NAMED one, which is a different set the nest did not refuse.
    #[tokio::test]
    async fn park_revoked_set_marks_every_folder_of_that_set_and_no_others() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = SyncPaths::new(Some(tmp.path().to_path_buf()));
        let mut someone_elses_photos =
            location(r"C:\their-photos", LocationMode::Always, Some("photos"));
        someone_elses_photos.folder_id = Some(FolderRef::Foreign([0xcd; 32]).to_wire());
        let config = SyncConfig {
            locations: vec![
                location(r"C:\photos-a", LocationMode::Always, Some("photos")),
                location(r"C:\photos-b", LocationMode::Always, Some("photos")),
                location(r"C:\docs", LocationMode::Always, Some("docs")),
                someone_elses_photos,
            ],
            ..SyncConfig::default()
        };
        let (shutdown_tx, _) = tokio::sync::watch::channel(false);
        let (event_tx, _) = broadcast::channel(16);
        let state = SyncServiceState::new(config, shutdown_tx, event_tx, paths);

        park_revoked_set(&state, test_ref("photos")).await;

        let config = state.config.read().await;
        let revoked: Vec<(&str, bool)> = config
            .locations
            .iter()
            .map(|f| (f.path.as_str(), f.access_revoked))
            .collect();
        assert_eq!(
            revoked,
            vec![
                (r"C:\photos-a", true),
                (r"C:\photos-b", true),
                (r"C:\docs", false),
                (r"C:\their-photos", false),
            ],
            "every folder bound to the revoked set parks; another set's binding — \
             same-named included — is untouched"
        );

        // Durable: an agent restart re-reads this and still refuses to serve it,
        // rather than silently resuming a binding the nest already refused.
        let reloaded = state.paths.load_config().expect("config round-trips");
        assert!(
            reloaded
                .locations
                .iter()
                .filter(|f| f.folder_id == Some(test_ref("photos").to_wire()))
                .all(|f| f.access_revoked),
            "the park survives a restart"
        );
    }

    #[test]
    fn a_mode_flip_changes_the_stamp_so_the_engine_restarts() {
        // The two modes run entirely different loops (placeholder host vs.
        // watch+upload), so flipping the mode MUST rebuild the engine — leaving the
        // old loop running would keep an always-resident folder download-only.
        let f = std::path::Path::new(r"C:\flip");
        let on_demand = engine_stamp(
            Some(&FolderEngineKeys::default()),
            LocationMode::OnDemand,
            f,
            None,
        );
        let always = engine_stamp(
            Some(&FolderEngineKeys::default()),
            LocationMode::Always,
            f,
            None,
        );

        assert_ne!(on_demand, always, "a mode flip must restart the engine");
    }

    #[tokio::test]
    async fn file_done_progress_becomes_file_status_changed_synced() {
        let (event_tx, mut event_rx) = broadcast::channel::<Event>(8);
        let (progress_tx, progress_rx) =
            mpsc::unbounded_channel::<fauna_sync_engine::progress::ProgressEvent>();
        let folder = PathBuf::from("/home/user/Fauna");

        tokio::spawn(drive_progress_notifications(
            progress_rx,
            event_tx,
            folder.clone(),
            "documents".to_string(),
            test_ref("documents"),
            std::sync::Weak::new(),
        ));

        progress_tx
            .send(fauna_sync_engine::progress::ProgressEvent::FileDone {
                path: "sub/report.bin".to_string(),
            })
            .unwrap();
        drop(progress_tx); // let the drain end once it has processed the event

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), event_rx.recv())
            .await
            .expect("drain must broadcast the FileStatusChanged before ending")
            .expect("broadcast recv must not error");

        match event.event {
            fauna_ipc::sync::EventKind::FileStatusChanged { path, status } => {
                assert_eq!(status, fauna_ipc::sync::FileStatus::Synced);
                assert_eq!(
                    path,
                    folder.join("sub").join("report.bin").to_string_lossy()
                );
            }
            other => panic!("expected FileStatusChanged, got {other:?}"),
        }
    }

    /// The link between the engine's report and the `ListEngines` projection:
    /// the drain WRITES what the mass-delete floor held into the shared state
    /// the IPC handler reads (`file-sync.md` § Files Appear Automatically).
    ///
    /// Pinned separately from the two ends because a projection test seeds the
    /// map by hand and an engine test only proves the emission — neither would
    /// notice this middle link going missing, which is the flow break that
    /// leaves a real held folder silent.
    #[tokio::test]
    async fn deletes_held_progress_reaches_the_shared_state_and_clears_on_zero() {
        let (event_tx, _event_rx) = broadcast::channel::<Event>(8);
        let (progress_tx, progress_rx) =
            mpsc::unbounded_channel::<fauna_sync_engine::progress::ProgressEvent>();
        let tmp = tempfile::tempdir().unwrap();
        let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
        let (ev, _) = broadcast::channel(16);
        let state = crate::state::SyncServiceState::new(
            crate::config::SyncConfig::default(),
            shutdown_tx,
            ev,
            crate::config::SyncPaths::new(Some(tmp.path().to_path_buf())),
        );

        let drain = tokio::spawn(drive_progress_notifications(
            progress_rx,
            event_tx,
            PathBuf::from("/home/user/Fauna"),
            "documents".to_string(),
            test_ref("documents"),
            Arc::downgrade(&state),
        ));

        // A hold is reported and lands under this engine's set.
        progress_tx
            .send(fauna_sync_engine::progress::ProgressEvent::DeletesHeld { held: 4 })
            .unwrap();
        await_held(&state, "documents", Some(4)).await;

        // The drive comes back: the next pass reports zero, and the entry
        // becomes zero rather than merely stopping being updated.
        progress_tx
            .send(fauna_sync_engine::progress::ProgressEvent::DeletesHeld { held: 0 })
            .unwrap();
        await_held(&state, "documents", Some(0)).await;

        // The engine stops: its entry goes away entirely, so a stale count can
        // never outlive the engine that observed it.
        drop(progress_tx);
        drain
            .await
            .expect("drain task must end when its channel closes");
        assert_eq!(
            state
                .deletes_held
                .lock()
                .await
                .get(&test_ref("documents").to_wire()),
            None,
            "a stopped engine must leave no count behind"
        );
    }

    /// The unreadable-path twin of the hold test above: the drain writes the
    /// delete rail's `DeletesSkippedUnreadable` verdict into its own map (never
    /// the hold's), a zero report clears it, and a stopped engine leaves no
    /// count behind (`delete-propagation.md` § Unreadable is not absent).
    #[tokio::test]
    async fn deletes_skipped_unreadable_progress_reaches_the_shared_state_and_clears_on_zero() {
        let (event_tx, _event_rx) = broadcast::channel::<Event>(8);
        let (progress_tx, progress_rx) =
            mpsc::unbounded_channel::<fauna_sync_engine::progress::ProgressEvent>();
        let tmp = tempfile::tempdir().unwrap();
        let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
        let (ev, _) = broadcast::channel(16);
        let state = crate::state::SyncServiceState::new(
            crate::config::SyncConfig::default(),
            shutdown_tx,
            ev,
            crate::config::SyncPaths::new(Some(tmp.path().to_path_buf())),
        );

        let drain = tokio::spawn(drive_progress_notifications(
            progress_rx,
            event_tx,
            PathBuf::from("/home/user/Fauna"),
            "photos".to_string(),
            test_ref("photos"),
            Arc::downgrade(&state),
        ));

        let key = test_ref("photos").to_wire();
        let unreadable = |state: Arc<crate::state::SyncServiceState>, key: String| async move {
            state
                .deletes_skipped_unreadable
                .lock()
                .await
                .get(&key)
                .copied()
        };
        let await_unreadable = |want: Option<u64>| {
            let state = Arc::clone(&state);
            let key = key.clone();
            async move {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    let got = unreadable(Arc::clone(&state), key.clone()).await;
                    if got == want {
                        return;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "unreadable count never became {want:?} (last saw {got:?})"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }
        };

        progress_tx
            .send(
                fauna_sync_engine::progress::ProgressEvent::DeletesSkippedUnreadable { skipped: 6 },
            )
            .unwrap();
        await_unreadable(Some(6)).await;
        assert_eq!(
            state.deletes_held.lock().await.get(&key),
            None,
            "an unreadable path is not a hold — it must never reach the hold's map"
        );

        progress_tx
            .send(
                fauna_sync_engine::progress::ProgressEvent::DeletesSkippedUnreadable { skipped: 0 },
            )
            .unwrap();
        await_unreadable(Some(0)).await;

        drop(progress_tx);
        drain
            .await
            .expect("drain task must end when its channel closes");
        assert_eq!(
            unreadable(Arc::clone(&state), key.clone()).await,
            None,
            "a stopped engine must leave no count behind"
        );
    }

    /// Poll until `folder`'s held count equals `want`, on a generous budget.
    ///
    /// A deadline poll rather than a sleep (testing.md convention 14): the drain
    /// is a separate task, so the write is *ordered after* the send but not
    /// synchronous with it. A green run pays only the first poll.
    async fn await_held(
        state: &Arc<crate::state::SyncServiceState>,
        folder: &str,
        want: Option<u64>,
    ) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let got = state
                .deletes_held
                .lock()
                .await
                .get(&test_ref(folder).to_wire())
                .copied();
            if got == want {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "held count for {folder} never became {want:?} (last saw {got:?})"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// A `ChunkDone` must never be mistaken for a *file* completing.
    ///
    /// ⚠ Since the live-progress fold landed, chunk deltas are no longer
    /// dropped outright — they move the window and surface as
    /// `EventKind::SyncProgress`. What stays forbidden is the confusion this
    /// test was written for: a chunk surfacing as `FileStatusChanged{Synced}`,
    /// which would mark a half-transferred file synced. The event used here
    /// names a path no `FileStarted` opened, so the fold declines it too (see
    /// `a_chunk_for_an_unopened_file_is_ignored`) and nothing is broadcast at
    /// all — the assertion below still holds literally.
    #[tokio::test]
    async fn a_chunk_never_surfaces_as_a_completed_file() {
        let (event_tx, mut event_rx) = broadcast::channel::<Event>(8);
        let (progress_tx, progress_rx) =
            mpsc::unbounded_channel::<fauna_sync_engine::progress::ProgressEvent>();
        let folder = PathBuf::from("/home/user/Fauna");

        tokio::spawn(drive_progress_notifications(
            progress_rx,
            event_tx,
            folder,
            "documents".to_string(),
            test_ref("documents"),
            std::sync::Weak::new(),
        ));

        progress_tx
            .send(fauna_sync_engine::progress::ProgressEvent::ChunkDone {
                path: "sub/report.bin".to_string(),
                bytes: 10,
            })
            .unwrap();
        drop(progress_tx);

        // Either outcome is correct here: a timeout (still nothing sent), or the
        // broadcast channel closing with no message ever delivered (the drain
        // task's `event_tx` clone drops once `progress_rx` closes). Only an
        // actually-received event would mean a ChunkDone leaked through.
        match tokio::time::timeout(std::time::Duration::from_millis(200), event_rx.recv()).await {
            Err(_) => {}     // timed out waiting — nothing was sent
            Ok(Err(_)) => {} // channel closed with no message ever sent
            Ok(Ok(event)) => {
                panic!("a ChunkDone must not surface as a FileStatusChanged: {event:?}")
            }
        }
    }

    // ── The in-flight transfer window (`ProgressFold`) ──
    //
    // tier_1: the fold is a pure state machine over `ProgressEvent`, with no
    // clock and no channel, so every rule below is asserted by construction
    // rather than by watching a live transfer (testing.md convention 14).

    use fauna_sync_engine::progress::ProgressEvent as PE;

    fn started(path: &str, size: u64) -> PE {
        PE::FileStarted {
            path: path.to_string(),
            size,
            chunk_count: 1,
        }
    }

    #[test]
    fn fold_accumulates_files_and_bytes_across_a_multi_file_window() {
        let mut fold = ProgressFold::default();
        fold.observe(&started("a.bin", 100));
        fold.observe(&started("b.bin", 300));
        fold.observe(&PE::ChunkDone {
            path: "a.bin".to_string(),
            bytes: 40,
        });

        let snap = fold.snapshot("documents");
        assert_eq!(snap.folder, "documents");
        assert_eq!(snap.files_total, 2);
        assert_eq!(snap.files_done, 0);
        assert_eq!(snap.bytes_total, 400);
        assert_eq!(snap.bytes_done, 40);
    }

    /// The dedup trap: `ChunkDone` fires only for chunks that were actually
    /// *uploaded*, and `upload_chunked_bytes` skips every chunk the nest
    /// already holds. So a fully-deduped 1 GB file transfers zero chunks, and
    /// a byte counter fed by `ChunkDone` alone would sit at 0% and then jump.
    /// `FileDone` therefore settles the file to its full size.
    #[test]
    fn file_done_settles_a_deduped_file_to_its_full_size() {
        let mut fold = ProgressFold::default();
        fold.observe(&started("dedup.bin", 1000));
        fold.observe(&PE::ChunkDone {
            path: "dedup.bin".to_string(),
            bytes: 10, // only one chunk was missing nest-side
        });
        fold.observe(&PE::FileDone {
            path: "dedup.bin".to_string(),
        });

        let snap = fold.snapshot("documents");
        assert_eq!(snap.files_done, 1);
        assert_eq!(
            snap.bytes_done, 1000,
            "a completed file counts its whole size, not just the bytes that moved"
        );
        assert_eq!(snap.bytes_done, snap.bytes_total);
    }

    /// `bytes_done > bytes_total` renders as a progress bar past 100%. The
    /// chunk arithmetic is clamped per file so that is unrepresentable, even
    /// if an engine over-reports (a resumed transfer replaying chunk events).
    #[test]
    fn bytes_done_never_exceeds_bytes_total() {
        let mut fold = ProgressFold::default();
        fold.observe(&started("a.bin", 50));
        for _ in 0..10 {
            fold.observe(&PE::ChunkDone {
                path: "a.bin".to_string(),
                bytes: 30,
            });
        }
        let snap = fold.snapshot("documents");
        assert_eq!(snap.bytes_done, 50);
        assert!(snap.bytes_done <= snap.bytes_total);
    }

    /// Every real transfer is bracketed `FileStarted` … `FileDone` (all three
    /// emit sites in `fauna-sync-engine`'s `engine.rs` do this), so a
    /// `ChunkDone` naming a path no window ever opened cannot be placed: its
    /// bytes were never counted into `bytes_total`, and adding them to
    /// `bytes_done` is exactly the >100% reading above. Dropped, not guessed.
    #[test]
    fn a_chunk_for_an_unopened_file_is_ignored() {
        let mut fold = ProgressFold::default();
        fold.observe(&PE::ChunkDone {
            path: "ghost.bin".to_string(),
            bytes: 999,
        });
        assert!(fold.is_idle());
        let snap = fold.snapshot("documents");
        assert_eq!(snap.files_total, 0);
        assert_eq!(snap.bytes_done, 0);
        assert_eq!(snap.bytes_total, 0);
    }

    /// A window spans a whole pass, not one file. `converge` uploads files
    /// sequentially, so the open set is empty for a moment between every pair
    /// of files — closing on "nothing open" would emit `1 item` a hundred
    /// times for a hundred-file pass instead of `100 items` once.
    #[test]
    fn a_window_survives_the_gap_between_two_sequential_files() {
        let mut fold = ProgressFold::default();
        fold.observe(&started("a.bin", 100));
        fold.observe(&PE::FileDone {
            path: "a.bin".to_string(),
        });
        assert!(fold.is_idle(), "nothing is open between the two files");

        fold.observe(&started("b.bin", 100));
        let snap = fold.snapshot("documents");
        assert_eq!(snap.files_total, 2, "the second file joins the same window");
        assert_eq!(snap.files_done, 1);
        assert_eq!(snap.bytes_total, 200);
    }

    /// The retraction rule, inherited verbatim from `DeletesHeld`: this state
    /// is derived and in-memory, so the only thing that ever takes a painted
    /// "Uploading 3 items" off a surface is a zero. A last frame left at 100%
    /// would stay painted until the next transfer — possibly for days.
    #[test]
    fn a_closed_window_snapshots_as_zeros() {
        let mut fold = ProgressFold::default();
        fold.observe(&started("a.bin", 100));
        fold.observe(&PE::FileDone {
            path: "a.bin".to_string(),
        });
        fold.close_window();

        let snap = fold.snapshot("documents");
        assert_eq!(snap.files_total, 0);
        assert_eq!(snap.files_done, 0);
        assert_eq!(snap.bytes_total, 0);
        assert_eq!(snap.bytes_done, 0);
    }

    /// Coalescing, the half that has to hold for a UI: a tick emits only when
    /// the fold actually moved. A 4 MB file at a 64 KB chunk size fires ~64
    /// `ChunkDone`s; without this the push is a per-chunk event storm.
    #[test]
    fn a_tick_that_saw_no_movement_emits_nothing() {
        let mut fold = ProgressFold::default();
        fold.observe(&started("a.bin", 100));
        assert!(
            fold.take_pending("documents").is_some(),
            "a new window is movement"
        );
        assert!(
            fold.take_pending("documents").is_none(),
            "a second tick with no event between must emit nothing"
        );

        fold.observe(&PE::ChunkDone {
            path: "a.bin".to_string(),
            bytes: 10,
        });
        assert!(
            fold.take_pending("documents").is_some(),
            "a chunk moved the fold"
        );
        assert!(fold.take_pending("documents").is_none());
    }

    /// The grace period itself, driven directly rather than through the loop's
    /// timer — `flush_progress` takes `now` as a parameter precisely so this is
    /// expressible with hand-made instants and no scheduler at all.
    ///
    /// ⚠ This test exists because mutation grading caught its absence: neuter
    /// the grace comparison to `if true` and every other test in this module
    /// stayed **green**. `a_window_survives_the_gap_between_two_sequential_files`
    /// looks like it covers this and does not — it asserts the pure fold's
    /// `is_idle()`, while the decision that actually retracts a window lives
    /// here. The rule was richly documented and completely unpinned.
    #[tokio::test(start_paused = true)]
    async fn the_grace_period_spans_an_inter_file_gap_and_only_then_retracts() {
        let (event_tx, mut event_rx) = broadcast::channel::<Event>(64);
        let mut fold = ProgressFold::default();
        let mut idle_since = None;
        let t0 = tokio::time::Instant::now();

        // File one transfers and completes. Flushes land while nothing is in
        // flight — but well inside the grace, which is the whole inter-file gap.
        fold.observe(&started("a.bin", 100));
        fold.observe(&PE::FileDone {
            path: "a.bin".to_string(),
        });
        flush_progress(&mut fold, "documents", &event_tx, &mut idle_since, t0);
        flush_progress(
            &mut fold,
            "documents",
            &event_tx,
            &mut idle_since,
            t0 + PROGRESS_WINDOW_GRACE / 2,
        );

        // File two starts inside that gap: it must join the SAME window.
        fold.observe(&started("b.bin", 100));
        flush_progress(
            &mut fold,
            "documents",
            &event_tx,
            &mut idle_since,
            t0 + PROGRESS_WINDOW_GRACE / 2 + Duration::from_millis(1),
        );

        let latest = drain_sync_progress(&mut event_rx)
            .pop()
            .expect("the window must have pushed at least one frame");
        assert_eq!(
            latest.files_total, 2,
            "a window retracted across the inter-file gap would restart at 1 — \
             the hundred-file pass would flicker `1 item` a hundred times"
        );
        assert_eq!(latest.files_done, 1);

        // Now let the grace genuinely elapse with nothing transferring.
        fold.observe(&PE::FileDone {
            path: "b.bin".to_string(),
        });
        flush_progress(
            &mut fold,
            "documents",
            &event_tx,
            &mut idle_since,
            t0 + PROGRESS_WINDOW_GRACE,
        );
        let _ = drain_sync_progress(&mut event_rx);
        flush_progress(
            &mut fold,
            "documents",
            &event_tx,
            &mut idle_since,
            t0 + PROGRESS_WINDOW_GRACE * 3,
        );

        let retraction = drain_sync_progress(&mut event_rx)
            .pop()
            .expect("a drained window must retract");
        assert_eq!(retraction.files_total, 0);
        assert_eq!(retraction.bytes_total, 0);
        assert_eq!(retraction.bytes_done, 0);
    }

    /// Every `SyncProgress` frame currently queued on `rx`, oldest first.
    fn drain_sync_progress(
        rx: &mut broadcast::Receiver<Event>,
    ) -> Vec<fauna_ipc::sync::SyncProgressInfo> {
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let fauna_ipc::sync::EventKind::SyncProgress(info) = event.event {
                out.push(info);
            }
        }
        out
    }

    /// The flow assertion the pure fold tests above cannot make: an engine's
    /// byte deltas actually leave the drain as a broadcast
    /// `EventKind::SyncProgress`, and the window is retracted afterwards.
    ///
    /// Before this track the drain forwarded `FileDone` only and dropped every
    /// byte delta on the floor, so each pure-fold rule could pass while nothing
    /// reached a subscriber at all — a flow break squarely between two named
    /// symbols.
    ///
    /// `start_paused` makes it latency-independent (convention 14): the flush
    /// cadence and the grace window are *fake*-clock durations the runtime
    /// auto-advances when the task is idle, so a loaded machine cannot change
    /// the outcome and a green run costs no wall-clock time.
    #[tokio::test(start_paused = true)]
    async fn byte_deltas_reach_subscribers_as_coalesced_sync_progress() {
        let (event_tx, mut event_rx) = broadcast::channel::<Event>(64);
        let (progress_tx, progress_rx) =
            mpsc::unbounded_channel::<fauna_sync_engine::progress::ProgressEvent>();

        tokio::spawn(drive_progress_notifications(
            progress_rx,
            event_tx,
            PathBuf::from("/home/user/Fauna"),
            "documents".to_string(),
            test_ref("documents"),
            std::sync::Weak::new(),
        ));

        progress_tx.send(started("big.bin", 1_000)).unwrap();
        for _ in 0..20 {
            progress_tx
                .send(fauna_sync_engine::progress::ProgressEvent::ChunkDone {
                    path: "big.bin".to_string(),
                    bytes: 25,
                })
                .unwrap();
        }

        // The in-flight frame: 20 chunks collapsed into ONE push, carrying the
        // fold, not the last chunk.
        let progress = next_sync_progress(&mut event_rx).await;
        assert_eq!(progress.folder, "documents");
        assert_eq!(progress.files_total, 1);
        assert_eq!(progress.files_done, 0);
        assert_eq!(progress.bytes_total, 1_000);
        assert_eq!(
            progress.bytes_done, 500,
            "the frame carries the folded total, not one chunk's bytes"
        );

        progress_tx
            .send(fauna_sync_engine::progress::ProgressEvent::FileDone {
                path: "big.bin".to_string(),
            })
            .unwrap();

        // …and once the pass drains, the window is retracted with a zero. A
        // surface that latched "1 item, 50%" has something to clear it with.
        let mut seen_retraction = false;
        for _ in 0..16 {
            let frame = next_sync_progress(&mut event_rx).await;
            if frame.files_total == 0 && frame.bytes_total == 0 && frame.bytes_done == 0 {
                seen_retraction = true;
                break;
            }
        }
        assert!(
            seen_retraction,
            "a drained window must retract itself with a zeroed frame"
        );
    }

    /// Pull broadcast frames until a `SyncProgress` arrives, skipping the
    /// `FileStatusChanged` events the same drain interleaves. Bounded by a
    /// generous budget rather than a settle-sleep (convention 14).
    async fn next_sync_progress(
        rx: &mut broadcast::Receiver<Event>,
    ) -> fauna_ipc::sync::SyncProgressInfo {
        for _ in 0..64 {
            let event = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv())
                .await
                .expect("the drain must push a SyncProgress")
                .expect("broadcast recv must not error");
            if let fauna_ipc::sync::EventKind::SyncProgress(info) = event.event {
                return info;
            }
        }
        panic!("no SyncProgress among the first 64 broadcast frames");
    }

    /// A set the resolution does not name has no stamp, so reconcile never
    /// starts it: absent is "not keyed yet", never owner-only.
    #[test]
    fn a_set_the_resolution_does_not_name_gets_no_stamp() {
        assert!(
            engine_stamp(None, LocationMode::Always, std::path::Path::new("/x"), None,).is_none(),
            "no keys → no stamp → reconcile never starts it"
        );
    }

    #[test]
    fn engine_stamp_distinguishes_every_transition() {
        let gen1 = FolderContentKeys::genesis([1u8; 32], 1_000);
        let mut gen2 = gen1.clone();
        gen2.rotate([2u8; 32], 2_000);

        let unbound = FolderEngineKeys {
            folder: "s".into(),
            folder_id: test_ref("s").to_wire(),
            ..Default::default()
        };
        let keyless = FolderEngineKeys {
            folder: "s".into(),
            folder_id: test_ref("s").to_wire(),
            mls_group_id: Some(b"g".to_vec()),
            ..Default::default()
        };
        let bound1 = FolderEngineKeys {
            folder: "s".into(),
            folder_id: test_ref("s").to_wire(),
            mls_group_id: Some(b"g".to_vec()),
            content_keys: Some(gen1.clone()),
            ..Default::default()
        };
        let bound2 = FolderEngineKeys {
            folder: "s".into(),
            folder_id: test_ref("s").to_wire(),
            mls_group_id: Some(b"g".to_vec()),
            content_keys: Some(gen2),
            ..Default::default()
        };
        // Same set, same keys — but homed on ANOTHER nest.
        let foreign = FolderEngineKeys {
            folder: "s".into(),
            folder_id: test_ref("s").to_wire(),
            mls_group_id: Some(b"g".to_vec()),
            content_keys: Some(gen1),
            home_nest_url: Some("https://home.example".into()),
            channel_id_hex: Some("ab".repeat(32)),
            home_nest_actor_id: Some("cd".repeat(32)),
            ..Default::default()
        };

        let m = LocationMode::OnDemand;
        let f = std::path::Path::new(r"C:\od");
        let stamp = |keys: &FolderEngineKeys, path: &std::path::Path| {
            engine_stamp(Some(keys), m.clone(), path, None).expect("the resolution names `s`")
        };
        let s_unbound = stamp(&unbound, f);
        let s_keyless = stamp(&keyless, f);
        let s_bound1 = stamp(&bound1, f);
        let s_bound2 = stamp(&bound2, f);

        // Every transition yields a DISTINCT stamp → reconcile restarts the engine.
        assert_ne!(s_unbound, s_keyless, "bind (unbound→keyless) must restart");
        assert_ne!(s_keyless, s_bound1, "keyless→keyed must restart");
        assert_ne!(s_bound1, s_bound2, "rotation gen1→gen2 must restart");
        // A re-resolve with the SAME generation is a no-op (no needless restart).
        assert_eq!(stamp(&bound1, f), s_bound1);
        // A re-bind of the same set to a DIFFERENT folder must restart — without
        // the folder in the stamp, the "live + unchanged" branch kept serving the
        // old folder while config pointed at the new one.
        assert_ne!(
            stamp(&bound1, std::path::Path::new(r"C:\elsewhere")),
            s_bound1,
            "folder re-bind must restart"
        );
        // A set that becomes cross-nest (or is re-accepted from a different home
        // nest) must restart: the routing decides which nest the control plane
        // relays to and which the byte plane POSTs at, so a live engine would
        // otherwise keep talking to the wrong one.
        assert_ne!(
            stamp(&foreign, f),
            s_bound1,
            "same-nest → cross-nest must restart"
        );
    }

    /// Row 744: a re-resolve that touches ONLY `retired_content_keys` — the WebDAV
    /// serve-toggle's read candidate — must restart a live engine too, even
    /// though the set's LIVE binding (`mls_group_id`/`content_keys`) is
    /// unchanged owner-only in every case. Without the retired generation in
    /// the stamp, a device already running that set's owner-only engine would
    /// keep it, and `reseal_predecessor_sealed` would never see the candidate
    /// that lets it converge the served-era back-catalogue until some
    /// unrelated restart happened to rebuild the engine.
    #[test]
    fn engine_stamp_distinguishes_a_retired_generation_only_change() {
        let gen1 = FolderContentKeys::genesis([1u8; 32], 1_000);
        let mut gen2 = gen1.clone();
        gen2.rotate([2u8; 32], 2_000);

        let never_served = FolderEngineKeys {
            folder: "s".into(),
            folder_id: test_ref("s").to_wire(),
            ..Default::default()
        };
        let retired_gen1 = FolderEngineKeys {
            folder: "s".into(),
            folder_id: test_ref("s").to_wire(),
            retired_content_keys: Some(gen1),
            ..Default::default()
        };
        let retired_gen2 = FolderEngineKeys {
            folder: "s".into(),
            folder_id: test_ref("s").to_wire(),
            retired_content_keys: Some(gen2),
            ..Default::default()
        };

        let m = LocationMode::Always;
        let f = std::path::Path::new("/watch");
        let stamp = |keys: &FolderEngineKeys| {
            engine_stamp(Some(keys), m.clone(), f, None).expect("the resolution names `s`")
        };

        let s_never_served = stamp(&never_served);
        let s_retired_gen1 = stamp(&retired_gen1);
        let s_retired_gen2 = stamp(&retired_gen2);

        assert_ne!(
            s_never_served, s_retired_gen1,
            "a re-resolve that ONLY adds a retired generation must restart the engine \
             (both live bindings are owner-only — content_keys/mls_group_id agree)"
        );
        assert_ne!(
            s_retired_gen1, s_retired_gen2,
            "a second serve/unserve cycle rotating the retired generation again \
             must also restart"
        );
        // A re-resolve with the SAME retired generation is a no-op.
        assert_eq!(stamp(&retired_gen1), s_retired_gen1);
    }

    /// Ruling (7)(b)(ii) rule (2): a re-resolve that ONLY moves the serve
    /// window must restart a live engine — a shared set's serve flip leaves
    /// its group and generations as they were, and the engine's reader
    /// exempts the owner's pseudo-device rows on the window it was built with.
    #[test]
    fn engine_stamp_distinguishes_a_serve_window_only_change() {
        let keys = |served_at: Option<u64>, unserved_at: Option<u64>| FolderEngineKeys {
            folder: "s".into(),
            folder_id: test_ref("s").to_wire(),
            mls_group_id: Some(b"gid".to_vec()),
            content_keys: Some(FolderContentKeys::genesis([1u8; 32], 1_000)),
            served_at,
            unserved_at,
            ..Default::default()
        };
        let m = LocationMode::Always;
        let f = std::path::Path::new("/watch");
        let stamp = |k: &FolderEngineKeys| {
            engine_stamp(Some(k), m.clone(), f, None).expect("the resolution names `s`")
        };
        let unserved = stamp(&keys(None, None));
        let served = stamp(&keys(Some(2_000), None));
        let served_off = stamp(&keys(Some(2_000), Some(3_000)));
        assert_ne!(unserved, served, "serve-on restarts the engine");
        assert_ne!(served, served_off, "serve-off restarts the engine");
        assert_eq!(
            unserved, served_off,
            "a closed window reads as never served: nothing else moved"
        );
    }

    /// A re-resolve that ONLY gains the set's nonce must restart a live engine:
    /// an owner-only set is named by its nest row alone, so its engine is built
    /// at the first reconcile after a provision — before the account host has
    /// mounted the store custody is read through — with no nonce. Without the
    /// nonce in the stamp that engine would keep recording unsigned and holding
    /// every pull below the first signed row.
    #[test]
    fn engine_stamp_distinguishes_a_set_nonce_only_change() {
        let custody_unread = FolderEngineKeys {
            folder: "s".into(),
            folder_id: test_ref("s").to_wire(),
            ..Default::default()
        };
        let custody_read = FolderEngineKeys {
            set_nonce: Some([7u8; 32]),
            ..custody_unread.clone()
        };
        let m = LocationMode::Always;
        let f = std::path::Path::new("/watch");
        let stamp = |keys: &FolderEngineKeys| {
            engine_stamp(Some(keys), m.clone(), f, None).expect("the resolution names `s`")
        };
        assert_ne!(
            stamp(&custody_unread),
            stamp(&custody_read),
            "the resolution reading the set's nonce must restart an engine built without it"
        );
        // A re-resolve with the SAME nonce is a no-op.
        assert_eq!(stamp(&custody_read), stamp(&custody_read));
    }

    #[test]
    fn engine_stamp_distinguishes_actor_scope() {
        // Per-actor sync-state scoping (`file-sync.md` § Multi-account × File
        // Provider, consequence 3): an engine serving actor A's scoped state dir is
        // a DIFFERENT engine than one serving actor B's, even for the same set /
        // binding / folder / mode. Windows re-scopes on the *provision* path (it
        // sends no Unprovision), so a switch between two accounts with a same-named
        // owner-only set on the same folder must NOT leave the outgoing engine's
        // open DB handle serving the incoming scope (the scope-blind stamp let the
        // "live + unchanged" branch keep it — a cross-account DB leak).
        let m = LocationMode::OnDemand;
        let f = std::path::Path::new(r"C:\od");
        let a = "aa".repeat(32);
        let b = "bb".repeat(32);
        let unscoped = engine_stamp(Some(&FolderEngineKeys::default()), m.clone(), f, None);
        let scoped_a = engine_stamp(Some(&FolderEngineKeys::default()), m.clone(), f, Some(&a));
        let scoped_b = engine_stamp(Some(&FolderEngineKeys::default()), m.clone(), f, Some(&b));
        assert_ne!(
            unscoped, scoped_a,
            "activating a scope must restart the engine"
        );
        assert_ne!(
            scoped_a, scoped_b,
            "a switch to another actor must restart the engine"
        );
        assert_eq!(
            scoped_a,
            engine_stamp(Some(&FolderEngineKeys::default()), m.clone(), f, Some(&a)),
            "same scope is a no-op (no needless restart)"
        );
    }

    /// The registration-lifecycle table (`full_teardown_location`): a sync-root
    /// registration comes down ONLY when its binding ends — unbind/removal,
    /// folder swap, mode flip away from on-demand. A key rotation and a plain
    /// stop-the-service keep it (the drop guard's default), and a watcher
    /// (always-resident) engine never has one to tear down.
    #[test]
    fn full_teardown_fires_only_when_the_binding_ends() {
        let od = |folder: &str| -> EngineStamp {
            (
                None,
                None,
                LocationMode::OnDemand,
                PathBuf::from(folder),
                None,
                None,
                None,
                None,
                false,
            )
        };
        let mapping = |folder: &str, mode| EngineMapping {
            path: PathBuf::from(folder),
            folder: "s".into(),
            folder_ref: test_ref("s"),
            mode,
        };

        // Unbound / folder removed → tear down the old root.
        assert_eq!(
            full_teardown_location(&od(r"C:\od"), None),
            Some(PathBuf::from(r"C:\od"))
        );
        // Re-bound to a different folder → tear down the OLD folder's root.
        assert_eq!(
            full_teardown_location(
                &od(r"C:\od"),
                Some(&mapping(r"C:\new", LocationMode::OnDemand))
            ),
            Some(PathBuf::from(r"C:\od"))
        );
        // Mode flip to always-resident → the folder stops being a cfapi root.
        assert_eq!(
            full_teardown_location(
                &od(r"C:\od"),
                Some(&mapping(r"C:\od", LocationMode::Always))
            ),
            Some(PathBuf::from(r"C:\od"))
        );
        // Same folder, still on-demand (a key-rotation restart) → KEEP.
        assert_eq!(
            full_teardown_location(
                &od(r"C:\od"),
                Some(&mapping(r"C:\od", LocationMode::OnDemand))
            ),
            None
        );
        // A retiring always-resident engine registered nothing → never tears down.
        let always: EngineStamp = (
            None,
            None,
            LocationMode::Always,
            PathBuf::from(r"C:\ar"),
            None,
            None,
            None,
            None,
            false,
        );
        assert_eq!(full_teardown_location(&always, None), None);
        assert_eq!(
            full_teardown_location(&always, Some(&mapping(r"C:\ar", LocationMode::OnDemand))),
            None
        );
    }
}
