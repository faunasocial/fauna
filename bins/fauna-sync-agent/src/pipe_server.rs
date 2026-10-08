//! Named pipe server for the FaunaSync service.
//!
//! Listens on `\\.\pipe\fauna-sync` and handles sync-related IPC requests.
//! Follows the same pipe creation, security, and accept loop pattern as
//! the original `fauna-service` pipe server.

use std::sync::Arc;

use zeroize::Zeroize;

use fauna_core::folder_keys::FolderRef;
use fauna_ipc::sync::{
    BackupDestinationStatus, BearerToken, ConnectionState, CustodianReclaimOutcome,
    CustodianSourceRegression, CustodianStoreInfo, EngineInfo, FileDevicesInfo, FileStatus,
    FileStatusInfo, FileVersionsInfo, LocationInfo, LocationStatus, Request, RequestMethod,
    Response, ResponsePayload, ResponseResult, ServiceStatusInfo, ShareTargetInfo, SyncCapability,
    SyncStatusInfo,
};
use fauna_sync_engine::custodian_store::{CustodianStore, custodian_store_root};
use std::time::{Duration, Instant};

use crate::state::SyncServiceState;

/// Handle an IPC request using shared sync service state.
pub async fn handle_request(req: &Request, state: &Arc<SyncServiceState>) -> Response {
    // Which verb we served, by name only — never the payload (`RequestMethod::name`'s doc
    // explains why: two variants carry key material). Without this, diagnosing "who tore the
    // engines down?" means proving a negative across every `reconcile_engines` call site by
    // its side-effects; with it, one log read answers it.
    tracing::debug!(id = req.id, method = req.method.name(), "pipe request");
    let result = match &req.method {
        RequestMethod::GetServiceStatus => handle_get_service_status(state).await,
        RequestMethod::GetSyncStatus => handle_get_sync_status(state).await,
        RequestMethod::AddLocation { path } => handle_add_location(state, path).await,
        RequestMethod::RemoveLocation { path } => handle_remove_location(state, path).await,
        RequestMethod::ListLocations => handle_list_locations(state).await,
        RequestMethod::GetFileStatus { path } => handle_get_file_status(state, path).await,
        RequestMethod::PinFile { path } => handle_pin_file(state, path).await,
        RequestMethod::UnpinFile { path } => handle_unpin_file(state, path).await,
        RequestMethod::FreeSpace { path } => handle_free_space(state, path).await,
        RequestMethod::SetLocationSyncMode { path, mode } => {
            handle_set_location_sync_mode(state, path, mode).await
        }
        RequestMethod::SetLocationFolder {
            path,
            folder,
            folder_id,
        } => handle_set_location_folder(state, path, folder, folder_id).await,
        RequestMethod::ShareFile { path } => handle_share_file(state, path).await,
        RequestMethod::GetFileDevices { path } => handle_get_file_devices(state, path).await,
        RequestMethod::GetFileVersions { path } => handle_get_file_versions(state, path).await,
        RequestMethod::ListFileVersions { path } => handle_list_file_versions(state, path).await,
        RequestMethod::RestoreFileVersion { path, version_num } => {
            handle_restore_file_version(state, path, *version_num).await
        }
        RequestMethod::Configure { nest_url } => handle_configure(state, nest_url.clone()).await,
        RequestMethod::ProvisionCapability(cap) => handle_provision_capability(state, cap).await,
        RequestMethod::RefreshBearer(bearer) => handle_refresh_bearer(state, bearer).await,
        RequestMethod::UnprovisionCapability => handle_unprovision_capability(state).await,
        RequestMethod::AttachApp {
            app,
            notification_identity,
        } => handle_attach_app(state, app.as_deref(), notification_identity.clone()).await,
        RequestMethod::Shutdown => handle_shutdown(state).await,
        RequestMethod::ListEngines => handle_list_engines(state).await,
        RequestMethod::GetBackupStatus => handle_get_backup_status(state).await,
        RequestMethod::GetCustodianStore => handle_get_custodian_store(state).await,
        RequestMethod::ReclaimCustodianStore => handle_reclaim_custodian_store(state).await,
        RequestMethod::ReseedCustodianStore(req) => handle_reseed_custodian_store(state, req).await,
        RequestMethod::GetCustodianReseed => Ok(ResponsePayload::CustodianReseed(
            state.reseed_job.lock().await.clone(),
        )),
        RequestMethod::GetCustodianFolderNames => handle_get_custodian_folder_names(state).await,
        RequestMethod::Pause => handle_set_paused(state, true).await,
        RequestMethod::Resume => handle_set_paused(state, false).await,
        RequestMethod::PullFolderNow {
            folder,
            folder_hash,
        } => {
            handle_pull_folder_now(state, folder, folder_hash.as_ref().map(|h| h.as_slice())).await
        }
        #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
        RequestMethod::CustodianRunPassNow { now_offset_secs } => {
            handle_custodian_run_pass_now(state, *now_offset_secs).await
        }
        RequestMethod::ApplyHeldDeletes { folder } => {
            handle_apply_held_deletes(state, folder).await
        }
        RequestMethod::GetShareServeInfo => handle_get_share_serve_info(state).await,
        RequestMethod::ReconcileAccountRuntime => handle_reconcile_account_runtime(state).await,
        RequestMethod::ShareIngest {
            folder,
            folder_id,
            proven_actor_hex,
            rows,
            spool_dir,
        } => {
            #[cfg(feature = "p2p-share")]
            {
                handle_share_ingest(state, folder, folder_id, proven_actor_hex, rows, spool_dir)
                    .await
            }
            #[cfg(not(feature = "p2p-share"))]
            {
                let _ = (folder, folder_id, proven_actor_hex, rows, spool_dir);
                Err("the share plane is not compiled into this agent".to_string())
            }
        }
    };

    Response {
        id: req.id,
        result: match result {
            Ok(payload) => ResponseResult::Ok(payload),
            Err(msg) => ResponseResult::Err(msg),
        },
    }
}

// ── Service status ──

/// Open a bound set's per-set state DB for a **second read connection** — the
/// same identity-keyed resolution `lookup_entry` uses (`sync_db_path_for_ref`,
/// account-data-plane.md § The ratified decisions); `SyncDb::open`
/// sets WAL + a 5s busy_timeout precisely so this read can run while a live
/// engine holds the write side. `None` when the set has no DB yet (bound but
/// never served) or on an open failure (warned — a status poll must never error
/// a healthy handler).
fn open_set_db_readonly(
    paths: &crate::config::SyncPaths,
    folder: &str,
    folder_ref: FolderRef,
) -> Option<fauna_sync_engine::db::SyncDb> {
    let db_path = paths.sync_db_path_for_ref(folder_ref);
    if !db_path.exists() {
        return None;
    }
    match fauna_sync_engine::db::SyncDb::open(&db_path) {
        Ok(db) => Some(db),
        Err(e) => {
            tracing::warn!(folder, error = %e, "status projection: open set db failed");
            None
        }
    }
}

/// The cross-set backlog aggregate behind BOTH status handlers — one owner, so
/// `GetServiceStatus` and `GetSyncStatus` cannot drift. Folds
/// [`SyncDb::transfer_backlog`](fauna_sync_engine::db::SyncDb::transfer_backlog)
/// over every planned (bound, unrevoked) set: `files_pending`/`bytes_pending`
/// sum; `last_sync` is the freshest per-set "known consistent" stamp (see
/// `TransferBacklog::last_sync_at` — max of last completed transfer and last
/// clean pass). Best-effort per set: a set that can't be read contributes
/// nothing rather than failing the poll.
async fn aggregate_backlog(state: &Arc<SyncServiceState>) -> (u64, u64, Option<u64>) {
    let planned = {
        let config = state.config.read().await;
        crate::engine_driver::plan_engines(&config)
    };
    let mut files_pending = 0u64;
    let mut bytes_pending = 0u64;
    let mut last_sync: Option<i64> = None;
    for m in &planned {
        let Some(db) = open_set_db_readonly(&state.paths, &m.folder, m.folder_ref) else {
            continue;
        };
        match db.transfer_backlog() {
            Ok(b) => {
                files_pending += b.files_pending;
                bytes_pending += b.bytes_pending;
                // Option<i64> orders None < Some(_), so max folds correctly.
                last_sync = last_sync.max(b.last_sync_at());
            }
            Err(e) => {
                tracing::warn!(folder = %m.folder, error = %e, "status projection: backlog read failed")
            }
        }
    }
    (
        files_pending,
        bytes_pending,
        last_sync.map(|t| t.max(0) as u64),
    )
}

/// The one shared `SyncStatusInfo` projection (`sync-agent.md` § Local agent
/// health): `connected` = a capability is provisioned (the "connected to a
/// nest" signal — NOT engine liveness), `syncing` = the engine host is serving,
/// and the backlog trio comes from [`aggregate_backlog`].
async fn sync_status_info(state: &Arc<SyncServiceState>) -> SyncStatusInfo {
    let se = state.engines.lock().await;
    let syncing = se.as_ref().is_some_and(|h| h.is_serving());
    drop(se);

    let capability = state.capability.read().await;
    let connected = capability.is_some();
    drop(capability);

    let (files_pending, bytes_pending, last_sync) = aggregate_backlog(state).await;
    SyncStatusInfo {
        connected,
        syncing,
        files_pending,
        bytes_pending,
        last_sync,
    }
}

async fn handle_get_service_status(
    state: &Arc<SyncServiceState>,
) -> Result<ResponsePayload, String> {
    let sync = sync_status_info(state).await;
    // Under the per-user model device.toml is never read; a provisioned
    // capability IS the "connected to a nest" signal — the same meaning the
    // nested `sync.connected` carries (one projection, one truth).
    let connection = if sync.connected {
        ConnectionState::Connected
    } else {
        ConnectionState::Disconnected
    };

    Ok(ResponsePayload::ServiceStatus(ServiceStatusInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs: state.start_time.elapsed().as_secs(),
        connection,
        sync,
        // The principal-support advertisement (`sync-agent.md` § Credential
        // model → the RULED 2026-08-15 block, decision 4). A **cached** field
        // read, never a slot lookup: this reply is on the app's convergence
        // tick, and a keyring round trip per tick is exactly the blocking I/O
        // an ack path must not carry. `crate::renewal::refresh_store_principal_presence`
        // is what keeps it true, off this path.
        store_principal_actor: state
            .store_principal_actor
            .read()
            .await
            .as_ref()
            .map(hex::encode),
        // The machine's sync device id (decision 2's enrollment target). A
        // field read of the capability already in memory — no keyring, no
        // nest — so it is ack-path safe exactly as the line above is.
        sync_device_id: state
            .capability
            .read()
            .await
            .as_ref()
            .map(|c| c.device_id.clone()),
        // The terminal renewal refusal: the refusal record's in-memory mirror,
        // never the bearer slot — an app's pushed bearer re-arms dialling and
        // leaves this standing until a renewal succeeds (`crate::renewal`).
        // A field read, ack-path safe like the two fields above.
        needs_reenrollment: crate::renewal::standing_refusal(state).is_some(),
        // *Keys pending*, derived where the truth is (`sync-agent.md` § Local
        // agent health): an in-memory read of the agent's own resolution, ack-path
        // safe like every field above.
        keys_pending: crate::content_keys::keys_pending(state).await,
        // The push arm's sink — a cached answer, never the probe itself (a
        // session-bus round trip), so ack-path safe like every field above.
        notification_sink: state.notification_sink.availability(),
        // The on-demand surface — the boot probe's cached answer, ack-path safe
        // like every field above.
        on_demand_available: state.on_demand.get().map(Result::is_ok),
        on_demand_unavailable_reason: state
            .on_demand
            .get()
            .and_then(|probe| probe.err())
            .map(str::to_string),
    }))
}

/// [`RequestMethod::AttachApp`] — the calling app is open for as long as this
/// connection is: the lease is held in the connection's scope and ends when
/// the app closes it (or dies), which is what the push arm reads
/// ([`crate::push_arm`]). A named notification identity is handed to the sink
/// before the reply, so an app that reads the status after attaching sees the
/// sink that identity gives.
async fn handle_attach_app(
    state: &Arc<SyncServiceState>,
    app: Option<&str>,
    notification_identity: Option<String>,
) -> Result<ResponsePayload, String> {
    if !fauna_ipc::conn_scope::hold_for_connection(state.attached_apps.attach()) {
        return Err("AttachApp must arrive on a served connection".into());
    }
    tracing::debug!(app = app.unwrap_or("?"), "an app attached");
    if let Some(identity) = notification_identity {
        let sink = Arc::clone(&state.notification_sink);
        // May block: a platform probe and a small file write.
        if let Err(e) = tokio::task::spawn_blocking(move || sink.adopt_identity(&identity)).await {
            tracing::warn!("push arm: adopting the app's notification identity failed: {e}");
        }
    }
    Ok(ResponsePayload::Empty)
}

// ── Sync status ──

async fn handle_get_sync_status(state: &Arc<SyncServiceState>) -> Result<ResponsePayload, String> {
    Ok(ResponsePayload::SyncStatus(sync_status_info(state).await))
}

// ── Sync folder management ──

async fn handle_add_location(
    state: &Arc<SyncServiceState>,
    path: &str,
) -> Result<ResponsePayload, String> {
    {
        let mut config = state.config.write().await;
        if config.find_location(path).is_none() {
            config.locations.push(crate::config::LocationConfig {
                path: path.to_string(),
                // Platform-scoped: on-demand on windows, always-resident elsewhere
                // (`LocationMode::fresh_binding_default`, user ruling 2026-09-26).
                // Re-adding an existing path keeps its persisted mode untouched.
                mode: crate::config::LocationMode::fresh_binding_default(),
                ..Default::default()
            });
        }
        if let Err(e) = state.paths.save_config(&config) {
            tracing::warn!("config save failed (in-memory state updated): {e}");
        }
    }

    // Adding a location starts no engine and registers no cfapi root: an unbound
    // location is served by nothing whatever its mode (`engine_driver` skips it).
    // The bind verb that follows (`SetLocationFolder`) runs `reconcile_engines`,
    // which is where a windows on-demand binding becomes a cfapi sync root and an
    // always-resident one gets its watcher; `SetLocationSyncMode` re-drives the
    // same reconcile when the user flips the mode later.
    Ok(ResponsePayload::Empty)
}

async fn handle_remove_location(
    state: &Arc<SyncServiceState>,
    path: &str,
) -> Result<ResponsePayload, String> {
    {
        let mut config = state.config.write().await;
        config.locations.retain(|f| f.path != path);
        if let Err(e) = state.paths.save_config(&config) {
            tracing::warn!("config save failed (in-memory state updated): {e}");
        }
    }

    // Reconcile: stops the removed folder's folder (if it was being served);
    // any other on-demand roots keep running. Best-effort.
    if let Err(e) = crate::engine_driver::reconcile_engines(state).await {
        tracing::warn!(error = %e, "hydration reconcile after folder removal failed");
    }

    Ok(ResponsePayload::Empty)
}

async fn handle_list_locations(state: &Arc<SyncServiceState>) -> Result<ResponsePayload, String> {
    let config = state.config.read().await;
    let se = state.engines.lock().await;
    let capability = state.capability.read().await;

    let status = if se.as_ref().is_some_and(|h| h.is_serving()) {
        LocationStatus::Syncing
    } else if capability.is_some() {
        LocationStatus::Paused
    } else {
        LocationStatus::Error("not connected".to_string())
    };
    drop(se);
    drop(capability);

    // A field read like the rest of this reply: the roots themselves keep the
    // map current (`engine_driver::MountReport`).
    let mount_errors = state
        .on_demand_mount_errors
        .lock()
        .map(|errors| errors.clone())
        .unwrap_or_default();

    let folders = config
        .locations
        .iter()
        .map(|f| {
            // The binding as every consumer reads it: label AND ref, or unbound.
            let binding = f.binding();
            // What the folder holds (`SyncDb::tracked_totals` — every tracked
            // non-deleted row), not the backlog. Unbound or never-served
            // folders report honest zeros.
            let (file_count, total_bytes) = binding
                .and_then(|b| open_set_db_readonly(&state.paths, b.folder, b.folder_ref))
                .and_then(|db| match db.tracked_totals() {
                    Ok(t) => Some(t),
                    Err(e) => {
                        tracing::warn!(folder = %f.path, error = %e,
                            "status projection: tracked totals read failed");
                        None
                    }
                })
                .unwrap_or((0, 0));
            LocationInfo {
                path: f.path.clone(),
                file_count,
                total_bytes,
                status: status.clone(),
                mode: f.mode.wire_str().to_string(),
                folder: binding.map(|b| b.folder.to_string()),
                folder_id: binding.map(|b| b.folder_ref.to_wire()),
                access_revoked: f.access_revoked,
                on_demand_mount_error: mount_errors
                    .get(std::path::Path::new(&f.path))
                    .map(|code| code.to_string()),
            }
        })
        .collect();
    Ok(ResponsePayload::Locations(folders))
}

// ── Device-global agent control + live introspection (sync-agent.md § Control plane split) ──

/// [`RequestMethod::ListEngines`] — one [`EngineInfo`] per *bound* folder (from
/// config), with `serving` reflecting whether the running host has that set started.
/// A bound set with no capability provisioned yet, or while the device is paused,
/// lists with `serving: false`. Read-only; an unbound folder is excluded (the host
/// never manufactures a folder name).
async fn handle_list_engines(state: &Arc<SyncServiceState>) -> Result<ResponsePayload, String> {
    let planned = {
        let config = state.config.read().await;
        crate::engine_driver::plan_engines(&config)
    };
    // The host's `started` map is keyed by `engine_key()` (the `FolderRef`
    // when the binding carries one — R1), NOT the set name; keying by name here
    // read `serving: false` for every ref-carrying binding while its engine
    // ran. Flags are collected first so the engines mutex is never held across
    // the per-set DB reads below.
    let serving_flags: Vec<bool> = {
        let running = state.engines.lock().await;
        planned
            .iter()
            .map(|m| {
                running
                    .as_ref()
                    .is_some_and(|r| r.is_serving_set(&m.engine_key()))
            })
            .collect()
    };
    // The identity the sentinels must belong to for a drain to mean anything
    // (`sync-agent.md` § Credential model → *Bound (3)'s enforcement design*,
    // ruling 5's read half). No capability provisioned yet → no actor to check
    // against → every engine answers "cannot tell", which is the fail-closed
    // reading: the keys stay.
    let owner_actor_hex = state
        .capability
        .read()
        .await
        .as_ref()
        .and_then(|c| c.actor_id_array())
        .map(hex::encode);
    // The mass-delete floor's per-set verdict, as last reported by each running
    // engine's progress drain (`file-sync.md` § Files Appear Automatically).
    // Snapshotted once rather than locked per row, like `serving_flags` above.
    // A set with no entry — unbound, not yet served, or no pass completed since
    // the agent started — reads 0: the hold is derived, so "nothing observed
    // yet" and "nothing held" are the same answer to a reader.
    let held_by_set = state.deletes_held.lock().await.clone();
    // The delete rail's unreadable-path count, snapshotted the same way and
    // defaulting the same way (`delete-propagation.md` § Unreadable is not
    // absent).
    let unreadable_by_set = state.deletes_skipped_unreadable.lock().await.clone();
    let engines = planned
        .into_iter()
        .zip(serving_flags)
        .map(|(m, serving)| {
            // Per-set split of the same display-transfer fold the status
            // handlers aggregate ("0 when idle or not yet known" per the
            // EngineInfo doc — a set with no DB yet reports honest zeros).
            //
            // The post-succession re-seal's progress rides the same read: it
            // rests in the same `meta` table of the same DB, and the app cannot
            // read it any other way — the pass runs in THIS process, and the
            // Settings line that renders it is in the app's
            // (`succession-aftermath.md` § Re-key scope: "surfaced with
            // progress"). `None` for every set that never ran a pass.
            //
            // The drain observable rides the same read, and is computed FRESH
            // here rather than read from any record: `corpus_reseal` above says
            // what one pass did, this says what is still owed *now*
            // (`sync-agent.md` § Credential model → *Bound (3)'s enforcement
            // design*, rulings 3 + 4). A set with no DB, an unreadable DB, or no
            // provisioned actor answers `None` — "cannot tell", which the app
            // reads as "not drained".
            let (backlog, corpus_reseal, reseal_drain) =
                open_set_db_readonly(&state.paths, &m.folder, m.folder_ref)
                    .map(|db| {
                        let backlog = match db.transfer_backlog() {
                            Ok(b) => Some((b.files_pending, b.bytes_pending)),
                            Err(e) => {
                                tracing::warn!(folder = %m.folder, error = %e,
                                    "status projection: backlog read failed");
                                None
                            }
                        };
                        let reseal = match db.corpus_reseal_pass() {
                            Ok(pass) => pass,
                            Err(e) => {
                                tracing::warn!(folder = %m.folder, error = %e,
                                    "status projection: corpus re-seal progress read failed");
                                None
                            }
                        };
                        let drain = owner_actor_hex.as_deref().and_then(|actor| {
                            match db.reseal_drain(actor) {
                                Ok(d) => Some(reseal_drain_info(d)),
                                Err(e) => {
                                    tracing::warn!(folder = %m.folder, error = %e,
                                        "status projection: re-seal drain read failed");
                                    None
                                }
                            }
                        });
                        (backlog, reseal.map(corpus_reseal_info), drain)
                    })
                    .unwrap_or((None, None, None));
            let (files_pending, bytes_pending) = backlog.unwrap_or((0, 0));
            let deletes_held = held_by_set.get(&m.engine_key()).copied().unwrap_or(0);
            let deletes_skipped_unreadable =
                unreadable_by_set.get(&m.engine_key()).copied().unwrap_or(0);
            EngineInfo {
                folder_id: m.folder_ref.to_wire(),
                folder: m.folder,
                mode: m.mode.wire_str().to_string(),
                serving,
                files_pending,
                bytes_pending,
                corpus_reseal,
                reseal_drain,
                deletes_held,
                deletes_skipped_unreadable,
            }
        })
        .collect();
    Ok(ResponsePayload::Engines(engines))
}

/// The engine record → wire mirror map, at the one seam that crosses it
/// (`fauna-ipc` deliberately takes no engine dependency, so the two types are
/// structurally identical and translated here — the same split
/// `BackupDestinationStatus` follows).
fn corpus_reseal_info(
    pass: fauna_sync_engine::succession_progress::CorpusResealPass,
) -> fauna_ipc::sync::CorpusResealInfo {
    use fauna_ipc::sync::CorpusResealInfo as Wire;
    use fauna_sync_engine::succession_progress::CorpusResealPass as Pass;
    match pass {
        Pass::Running => Wire::Running,
        Pass::Settled { resealed, owed } => Wire::Settled { resealed, owed },
        Pass::Failed { reason } => Wire::Failed { reason },
    }
}

/// The drain observable's engine record → wire mirror, at the same seam.
fn reseal_drain_info(
    drain: fauna_sync_engine::succession_drain::ResealDrain,
) -> fauna_ipc::sync::ResealDrainInfo {
    fauna_ipc::sync::ResealDrainInfo {
        folded: drain.folded,
        nothing_owed: drain.nothing_owed,
        all_at_rest_classified: drain.all_at_rest_classified,
    }
}

/// [`RequestMethod::GetBackupStatus`] — **retired, always `[]`**. The D6
/// agent-hosted-coordinator milestone was withdrawn 2026-07-23 (segment backup
/// is nest-run; status is the nest's `fauna.backup.status` projection, which
/// apps read directly), so the agent never has rows to report. The op + its
/// codec variants are queued for deletion (`sync-agent.md` § Scope per
/// platform / A1b); until then `[]` keeps old callers rendering "no backups
/// yet" cleanly rather than erroring.
async fn handle_get_backup_status(
    _state: &Arc<SyncServiceState>,
) -> Result<ResponsePayload, String> {
    let rows: Vec<BackupDestinationStatus> = Vec::new();
    Ok(ResponsePayload::BackupStatus(rows))
}

/// [`RequestMethod::Pause`] (`paused = true`) / [`RequestMethod::Resume`]
/// (`paused = false`) — device-global pause of all sync work (and, once the segment
/// coordinator is agent-hosted, backup). Persists the flag in the agent's own config
/// (the uniform cross-platform shape) then reconciles so it takes effect immediately:
/// pause stops every running engine, resume restarts the bound ones. No payload.
async fn handle_set_paused(
    state: &Arc<SyncServiceState>,
    paused: bool,
) -> Result<ResponsePayload, String> {
    {
        let mut config = state.config.write().await;
        config.paused = paused;
        if let Err(e) = state.paths.save_config(&config) {
            tracing::warn!("config save failed (in-memory state updated): {e}");
        }
    }
    if let Err(e) = crate::engine_driver::reconcile_engines(state).await {
        tracing::warn!(error = %e, paused, "reconcile after pause/resume failed");
    }
    Ok(ResponsePayload::Empty)
}

/// Resolve the engine-registry key for an IPC verb that addresses its set by
/// **name** (`PullFolderNow`, `ApplyHeldDeletes`).
///
/// Registration keys the per-engine channel maps by the binding's `FolderRef`
/// wire form — the [`crate::engine_driver::EngineMapping::engine_key`]
/// semantic, the one key that tells two same-named sets apart. A name is only a lookup into this device's bindings: exactly one
/// bound location wearing it → that binding's ref; several → refused loudly
/// rather than routed to whichever engine registered last; none → `Ok(None)`,
/// "not served". There is no bare-name key: the fallback that returned the
/// name itself for a pre-identity binding was retired 2026-09-24 (the
/// compat-remnant sweep) — every binding carries a ref.
async fn resolve_engine_key(
    state: &Arc<SyncServiceState>,
    folder: &str,
) -> Result<Option<String>, String> {
    resolve_engine_key_at(state, folder, None).await
}

/// [`resolve_engine_key`] with the set's hash address, when the caller has
/// one (`PullFolderNow` relays the push's `folder_hash`): a binding matches
/// when the hash of its name equals it, so a sealed set's nudge — whose name
/// is blank on the wire — still finds its engine (`path-sealing.md` § the
/// set-name plane). No hash → the name match above.
async fn resolve_engine_key_at(
    state: &Arc<SyncServiceState>,
    folder: &str,
    folder_hash: Option<&[u8]>,
) -> Result<Option<String>, String> {
    let config = state.config.read().await;
    let names = |name: &str| match folder_hash {
        Some(hash) => fauna_core::path_crypto::set_name_hash(name).as_slice() == hash,
        None => name == folder,
    };
    let mut keys = config
        .locations
        .iter()
        .filter_map(|f| f.binding())
        .filter(|b| names(b.folder))
        .map(|b| b.folder_ref.to_wire());
    let Some(first) = keys.next() else {
        return Ok(None);
    };
    if keys.next().is_some() {
        return Err(format!(
            "folder name '{folder}' is ambiguous on this device (several sync locations \
             wear it); address the set by its folder id"
        ));
    }
    Ok(Some(first))
}

/// How long [`handle_reclaim_custodian_store`] waits for a cancelled stint to
/// finish tearing down before it refuses rather than deleting under a live
/// writer.
///
/// A named, generous budget with a deadline poll, not a settle-sleep
/// (`testing.md` convention 14): the wait ends the instant the stint's own tail
/// clears the slot, and the ceiling exists only so a driver wedged on a dead
/// socket turns into an honest refusal instead of an unbounded IPC call. The
/// teardown itself is two task joins over an already-cancelled token, so the
/// real wait is milliseconds; the ceiling is sized for a laptop under load.
const RECLAIM_TEARDOWN_BUDGET: Duration = Duration::from_secs(30);
const RECLAIM_TEARDOWN_POLL: Duration = Duration::from_millis(50);

/// Where this device's sealed custodian store lives — the agent's one
/// authority on that path (`sync-agent.md` § Control plane split), derived the
/// same way [`crate::custodian::host_stint`] derives it so a read and a write
/// cannot land on different roots.
///
/// `Err` when no capability has been provisioned: without an actor id there is
/// no store to name, and answering "empty" would tell a signed-out app that a
/// full store is not there.
async fn custodian_store_here(state: &Arc<SyncServiceState>) -> Result<CustodianStore, String> {
    let actor_id = {
        let cap = state.capability.read().await;
        cap.as_ref()
            .and_then(|c| c.actor_id_array())
            .ok_or_else(|| {
                // Its own message, deliberately not `NO_CAPABILITY_ERROR_MESSAGE`:
                // that string is a pinned cross-process contract whose prefix the
                // re-provisioning convergence loop matches on, and borrowing it
                // here would let a store read trip a capability re-push.
                "no capability provisioned; this agent cannot name a custodian store".to_string()
            })?
    };
    let root = custodian_store_root(&state.paths.flat_base_dir(), &hex::encode(actor_id))
        .map_err(|e| format!("resolve custodian store root: {e}"))?;
    Ok(CustodianStore::at(root))
}

/// [`RequestMethod::GetCustodianStore`] — measure this device's sealed store.
///
/// Read-only and policy-free: it reports this process's own disk. Whether that
/// store is *orphaned* is the app's call, because the destination rows that
/// decide it are at-rest data this bearer-only process cannot open
/// (`fauna_core::data::custodian_store_is_orphaned`).
///
/// [`CustodianStore::at`] rather than `ensure_root`: a read must not create the
/// directory, and must not re-assert a cloud-backup exclusion for a store that
/// does not exist. A missing root is an empty store, which is the honest answer
/// for a device that never enrolled.
async fn handle_get_custodian_store(
    state: &Arc<SyncServiceState>,
) -> Result<ResponsePayload, String> {
    let store = custodian_store_here(state).await?;
    let footprint = store
        .footprint()
        .await
        .map_err(|e| format!("measure custodian store: {e}"))?;
    // The audit record is read through the same read-only door: a missing
    // record is one with no regressions.
    let source_regressions = store
        .audit_record()
        .await
        .source_regressions
        .into_iter()
        .map(|(ledger, r)| CustodianSourceRegression {
            ledger,
            held: r.held,
            served: r.served,
            observed_at: r.observed_at,
        })
        .collect();
    Ok(ResponsePayload::CustodianStore(CustodianStoreInfo {
        generations: footprint.generations as u64,
        files: footprint.files as u64,
        bytes: footprint.bytes,
        source_regressions,
    }))
}

/// [`RequestMethod::GetCustodianFolderNames`] — the covered-folder display
/// names this device's store learned, the input to a desktop re-seed's target
/// pre-create (`writer-signed-change-records.md` ruling (7)(a)(i)).
/// [`CustodianStore::at`], as for the footprint: a missing store has no names.
async fn handle_get_custodian_folder_names(
    state: &Arc<SyncServiceState>,
) -> Result<ResponsePayload, String> {
    let store = custodian_store_here(state).await?;
    let names = store
        .folder_names()
        .await
        .map_err(|e| format!("read custodian folder names: {e}"))?;
    let mut names: Vec<String> = names.into_values().collect();
    names.sort();
    names.dedup();
    Ok(ResponsePayload::CustodianFolderNames(names))
}

/// Stop this device's live custodian stint, if any, and wait for it to be down —
/// the precondition of every act that must be the store's only writer (a
/// reclaim, a re-seed). `false` when the stint did not stop inside
/// [`RECLAIM_TEARDOWN_BUDGET`], in which case the caller must not proceed.
async fn stop_hosted_custodian(state: &Arc<SyncServiceState>, why: &str) -> bool {
    // Cloned out of the slot, never cancelled under its lock: the stint's own
    // tail takes that same lock to clear itself, so holding it across the wait
    // would deadlock against the teardown being waited for.
    let Some(hosted) = state.hosted_custodian.lock().await.clone() else {
        return true;
    };
    tracing::info!("custodian: stopping this device's replica for a {why}");
    hosted.cancel.cancel();
    let deadline = Instant::now() + RECLAIM_TEARDOWN_BUDGET;
    while state.hosted_custodian.lock().await.is_some() {
        if Instant::now() >= deadline {
            tracing::warn!(
                "custodian: the replica did not stop within {RECLAIM_TEARDOWN_BUDGET:?} for a {why}"
            );
            return false;
        }
        tokio::time::sleep(RECLAIM_TEARDOWN_POLL).await;
    }
    true
}

/// [`RequestMethod::ReseedCustodianStore`] — start the re-seed ceremony over
/// this device's store, or report the one already running.
///
/// Answers at once with the job's state; the job itself outlives the request
/// (see the variant's docs for why). The `Running` swap happens under the slot's
/// lock, so two presses racing each other start one job.
async fn handle_reseed_custodian_store(
    state: &Arc<SyncServiceState>,
    req: &fauna_ipc::sync::CustodianReseedRequest,
) -> Result<ResponsePayload, String> {
    use fauna_ipc::sync::CustodianReseedState;
    let key = req
        .nest_backup_key_array()
        .ok_or_else(|| "re-seed: the NestBackupKey must be exactly 32 bytes".to_string())?;
    {
        let mut job = state.reseed_job.lock().await;
        if matches!(*job, CustodianReseedState::Running) {
            return Ok(ResponsePayload::CustodianReseed(
                CustodianReseedState::Running,
            ));
        }
        *job = CustodianReseedState::Running;
    }
    let state = Arc::clone(state);
    tokio::spawn(async move {
        let finished = if stop_hosted_custodian(&state, "re-seed").await {
            crate::custodian::run_reseed(&state, &key).await
        } else {
            CustodianReseedState::Failed {
                phase: "store".into(),
                detail: "this device's backup replica did not stop; nothing was sent".into(),
            }
        };
        *state.reseed_job.lock().await = finished;
    });
    Ok(ResponsePayload::CustodianReseed(
        CustodianReseedState::Running,
    ))
}

/// [`RequestMethod::ReclaimCustodianStore`] — free this device's whole sealed
/// store, after stopping its writer.
///
/// # The one guarantee this handler adds
///
/// The app decides *whether* to reclaim (it holds the destination rows; see the
/// variant's docs). What only this process can promise is that the bytes are
/// not deleted out from under its own custodian stint: a pull pass writes blobs
/// and then records them, so a sweep racing it can delete freshly written blobs
/// whose index row lands a moment later — leaving a store that reports healthy
/// and fails its next audit.
///
/// So a live stint is **stopped first**, through the published token rather than
/// by dropping anything (`host_stint`'s teardown rule), and the reclaim waits
/// for the slot to empty — the stint's own tail is what clears it, so an empty
/// slot is proof the driver and both push pumps are down. A teardown that does
/// not finish inside [`RECLAIM_TEARDOWN_BUDGET`] refuses
/// ([`CustodianReclaimOutcome::still_hosting`]) rather than racing it.
///
/// In the ordinary flow there is nothing to stop: the affordance is offered only
/// for a store no destination row claims, and a stint exists only for a row that
/// does. This is the wind-down window between the two.
async fn handle_reclaim_custodian_store(
    state: &Arc<SyncServiceState>,
) -> Result<ResponsePayload, String> {
    let store = custodian_store_here(state).await?;

    // A running re-seed is reading this store to rebuild the owner's nest from
    // it: the one moment deleting it would destroy the copy being restored.
    // Refused the same way a stint that will not stop is — nothing deleted.
    let reseeding = matches!(
        *state.reseed_job.lock().await,
        fauna_ipc::sync::CustodianReseedState::Running
    );
    if reseeding || !stop_hosted_custodian(state, "reclaim").await {
        tracing::warn!(
            reseeding,
            "custodian: reclaim refused — the store still has a writer; nothing was deleted"
        );
        return Ok(ResponsePayload::CustodianStoreReclaimed(
            CustodianReclaimOutcome {
                still_hosting: true,
                ..Default::default()
            },
        ));
    }

    let report = store
        .reclaim_all()
        .await
        .map_err(|e| format!("reclaim custodian store: {e}"))?;
    tracing::info!(
        files = report.files,
        bytes = report.bytes,
        "custodian: reclaimed this device's sealed store"
    );
    Ok(ResponsePayload::CustodianStoreReclaimed(
        CustodianReclaimOutcome {
            still_hosting: false,
            freed_files: report.files as u64,
            freed_bytes: report.bytes,
        },
    ))
}

/// [`RequestMethod::CustodianRunPassNow`] — **test-only**: run exactly one
/// custodian pull pass on the replica this process is already hosting, and reply
/// only once it has finished.
///
/// The reply *is* the causal barrier (`testing.md` convention 14): when it
/// arrives, the pass has pulled, sealed, stored, audited-if-due and written its
/// check-in, or definitively failed — so a caller asserts state and never
/// timing. Without it the first pass is `PERIODIC_INTERVAL` away, because
/// `CustodianPull::run_loop` deliberately mutes the interval's immediate first
/// tick, and a test would have to sleep out fifteen minutes.
///
/// It drives the **published** host rather than assembling one. Assembling a
/// second host here would be the silent bug this slot exists to prevent: a
/// second host means a second store root under a different base, so the device
/// would grow two sealed stores and file two devices' worth of check-ins against
/// one registry row.
///
/// Not hosting is a report, not an error — see
/// [`CustodianPassReport::hosting`](fauna_ipc::sync::CustodianPassReport::hosting).
///
/// `now_offset_secs` is convention 14's fake clock for the one cadence inside a
/// pass that a test cannot otherwise reach: the self-audit's 24-hour debounce
/// (`fauna_client_backup::audit::AUDIT_MIN_INTERVAL_SECS`). It is applied to the
/// **pass's own `now` parameter** — the one the scheduler already supplies
/// (`CustodianPull::run_once`'s "the caller supplies it so a scheduler and a
/// test drive the same code") — so an offset pass runs the identical code an
/// unoffset one does, and no second path exists to rot. It is not stored
/// anywhere: the next unoffset poke is back at the real clock.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
async fn handle_custodian_run_pass_now(
    state: &Arc<SyncServiceState>,
    now_offset_secs: i64,
) -> Result<ResponsePayload, String> {
    // Cloned out of the slot rather than awaited under its lock: a pass takes
    // network time, and holding the slot across it would block the stint's own
    // teardown from clearing it.
    let hosted = state.hosted_custodian.lock().await.clone();
    let Some(crate::state::HostedCustodian { host, .. }) = hosted else {
        return Ok(ResponsePayload::CustodianPassReport(
            fauna_ipc::sync::CustodianPassReport::default(),
        ));
    };

    // Saturating, not wrapping: a nonsense offset must land at a clock extreme
    // the pass simply reads as very old or very new, never wrap into a
    // plausible-looking timestamp on the other side of the epoch.
    let now = fauna_core::data::Timestamp::now_secs_or_zero().saturating_add(now_offset_secs);
    let reports = host.run_all_kinds(now).await;
    let audit = host.audit_verdict().await;
    Ok(ResponsePayload::CustodianPassReport(fold_pass_report(
        &reports, audit,
    )))
}

/// Fold one pass's per-kind reports into the wire report.
///
/// Split out from the handler because every rule that can be silently wrong
/// lives here, and none of them needs a nest to exercise.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
fn fold_pass_report(
    reports: &[fauna_sync_engine::custodian_pull::PullReport],
    audit: Option<fauna_client_backup::custodian::SelfAudit>,
) -> fauna_ipc::sync::CustodianPassReport {
    use fauna_client_backup::custodian::CapState;

    fauna_ipc::sync::CustodianPassReport {
        hosting: true,
        kinds_run: reports.len() as u32,
        // The freshest figure, not the largest: each pass reports what the store
        // holds *after* it, and a pass that reclaimed generations honestly holds
        // less than the one before it.
        held_bytes: reports.last().map(|r| r.held_bytes).unwrap_or(0),
        // Read from the verdict, never re-derived from `held >= cap`: a pass that
        // stopped at its cap ends *below* the cap, so inference renders a stopped
        // backup as healthy-with-room (`backup-destinations.md` § Third
        // destination kind). Any kind at its cap makes the device cap-reached.
        cap_state: if reports.is_empty() {
            None
        } else if reports.iter().any(|r| r.cap_state == CapState::Reached) {
            Some(CapState::Reached.as_wire().to_string())
        } else {
            Some(CapState::Ok.as_wire().to_string())
        },
        // Absence stays *not yet audited*. Substituting a pass here would render
        // an unverified copy as verified; substituting a failure would raise a
        // fleet-wide false data-loss alarm.
        audit_state: audit.map(|a| a.state().as_wire().to_string()),
        // The check-in is written inside `run_once`, and a failed check-in makes
        // that call return `Err` — so a kind that reached `reports` at all
        // definitively checked in.
        checked_in: !reports.is_empty(),
    }
}

/// [`RequestMethod::PullFolderNow`] — best-effort remote-change nudge: signal
/// the resident engine serving `folder` (if any) to pull now, off its rescan
/// cadence. A client sends this on receiving a `PushEvent::SyncChanged`. Drops
/// silently when the set is not bound or not resident (no wake sender
/// registered), a pull is already pending (bounded channel full), or the name
/// is ambiguous on this
/// device (warned — the rescan tick is the correctness backstop, so a missed
/// nudge costs only latency). No payload. Per `file-sync.md` § Remote-change
/// nudge.
async fn handle_pull_folder_now(
    state: &Arc<SyncServiceState>,
    folder: &str,
    folder_hash: Option<&[u8]>,
) -> Result<ResponsePayload, String> {
    let key = match resolve_engine_key_at(state, folder, folder_hash).await {
        Ok(Some(k)) => k,
        Ok(None) => {
            tracing::debug!(folder, "pull-now nudge dropped: no binding wears this name");
            return Ok(ResponsePayload::Empty);
        }
        Err(e) => {
            tracing::warn!(folder, error = %e, "pull-now nudge dropped: ambiguous name");
            return Ok(ResponsePayload::Empty);
        }
    };
    let nudged = {
        let senders = state.wake_senders.lock().await;
        match senders.get(&key) {
            // `try_send`: non-blocking + coalescing. `Full` ⇒ a pull is already
            // pending (fine); `Closed` ⇒ the engine is tearing down (fine).
            Some(tx) => tx.try_send(()).is_ok(),
            None => false,
        }
    };
    tracing::debug!(folder, nudged, "remote-change pull-now nudge");
    Ok(ResponsePayload::Empty)
}

/// [`RequestMethod::ApplyHeldDeletes`] — route the user-confirmed propagation
/// of a mass-delete-floor hold to the resident engine serving `folder`
/// (`delete-propagation.md` § the mass-delete floor). Unlike the nudge this is
/// invoke-and-reply: the engine re-derives the hold NOW, applies it through
/// the ordinary delete path, and the outcome comes back for the app to render.
/// Errors — never silently drops (a dropped confirm would read downstream as
/// "the apply happened", the exact disguise convention 11 bans) — when the set
/// has no resident engine, the engine is tearing down, or it stops before the
/// apply runs.
async fn handle_apply_held_deletes(
    state: &Arc<SyncServiceState>,
    folder: &str,
) -> Result<ResponsePayload, String> {
    let Some(key) = resolve_engine_key(state, folder).await? else {
        return Err(format!(
            "folder '{folder}' is not being served by a resident engine"
        ));
    };
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    {
        let senders = state.engine_cmd_senders.lock().await;
        let Some(tx) = senders.get(&key) else {
            return Err(format!(
                "folder '{folder}' is not being served by a resident engine"
            ));
        };
        // try_send: capacity 2 queues the command behind whatever the loop is
        // mid-way through (busy ≠ full); Full means a confirm is already
        // queued — tell the caller instead of stacking a third.
        tx.try_send(
            fauna_sync_engine::always_resident::EngineCommand::ApplyHeldDeletes { reply: reply_tx },
        )
        .map_err(|e| match e {
            tokio::sync::mpsc::error::TrySendError::Full(_) => {
                format!("an apply is already pending for '{folder}'")
            }
            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                format!("the engine serving '{folder}' is shutting down")
            }
        })?;
    }
    // The apply does real per-row work (nest round-trips); it blocks only this
    // connection. A dropped sender (engine exited before running the command)
    // resolves the oneshot with RecvError — an error, never a hang.
    let outcome = reply_rx
        .await
        .map_err(|_| format!("the engine serving '{folder}' stopped before the apply ran"))?
        .map_err(|e| format!("apply held deletes failed: {e}"))?;
    Ok(ResponsePayload::HeldDeletesApplied(
        fauna_ipc::sync::HeldDeletesAppliedInfo {
            applied: outcome.applied,
            remaining_held: outcome.remaining_held,
            floor_was_active: outcome.floor_was_active,
        },
    ))
}

/// How long the participation nudge waits on the pass before replying anyway:
/// the app holds its gesture on the reply, and a pass stuck on a nest round
/// trip must not hold it longer than this — the pass runs on regardless.
const RECONCILE_NUDGE_TIMEOUT: Duration = Duration::from_secs(10);

/// [`RequestMethod::ReconcileAccountRuntime`] — run one full pass of the
/// account store this agent has mounted, now: the devices page's
/// participation switch rested the device-local row, and the pass's
/// `peer_leg::ensure_bound` is what drops (or binds) the same-account listener
/// (`p2p.md` § Per-device participation → *Enforcement*, (c) Promptness).
/// Reads the mounted store slot ([`SyncServiceState::mounted_store`]), never
/// the config. A quiet `Empty` when nothing is mounted or this runtime is not
/// the engine holder — an app's runtime holds it and ran its own pass — and
/// whatever the pass reports: its outcome is its report's, and the row is the
/// fact the next pass reads either way.
async fn handle_reconcile_account_runtime(
    state: &Arc<SyncServiceState>,
) -> Result<ResponsePayload, String> {
    let Some(handle) = state
        .mounted_store
        .lock()
        .ok()
        .and_then(|held| held.clone())
    else {
        tracing::debug!("account pass nudge: no store mounted");
        return Ok(ResponsePayload::Empty);
    };
    if !handle.is_engine_holder() {
        tracing::debug!("account pass nudge: an app's runtime holds the engine");
        return Ok(ResponsePayload::Empty);
    }
    match tokio::time::timeout(RECONCILE_NUDGE_TIMEOUT, handle.reconcile_now()).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => {
            tracing::warn!(error = %format!("{e:#}"), "account pass nudge: the pass failed")
        }
        Err(_) => tracing::warn!("account pass nudge: the pass outlived the reply"),
    }
    Ok(ResponsePayload::Empty)
}

/// [`RequestMethod::GetShareServeInfo`] — where each bound folder's state DB
/// and local tree live, so the APP process serves the share plane by
/// cross-process WAL read (the same sanctioned second-connection pattern this
/// file's status projection uses — [`open_set_db_readonly`]). Path resolution
/// stays this binary's one authority (`sync_db_path_for_ref`, identical
/// to every engine's own); only folders whose DB exists report — a set never
/// served has nothing to serve.
async fn handle_get_share_serve_info(
    state: &Arc<SyncServiceState>,
) -> Result<ResponsePayload, String> {
    let config = state.config.read().await;
    let rows = config
        .locations
        .iter()
        .filter_map(|f| {
            let binding = f.binding()?;
            let db_path = state.paths.sync_db_path_for_ref(binding.folder_ref);
            if !db_path.exists() {
                return None;
            }
            Some(fauna_ipc::sync::ShareServeFolderInfo {
                folder: binding.folder.to_string(),
                folder_id: binding.folder_ref.to_wire(),
                watch_dir: f.path.clone(),
                db_path: db_path.to_string_lossy().into_owned(),
            })
        })
        .collect();
    Ok(ResponsePayload::ShareServeInfo(rows))
}

/// [`RequestMethod::ShareIngest`] — route one accepted page of peer-served
/// share rows to the resident engine serving the set, exactly the
/// [`handle_apply_held_deletes`] shape: invoke-and-reply through the engine's
/// own command channel (the engine is never driven from two tasks), errors
/// never silent drops. Routing is by the set's ref, never its name (a misrouted
/// ingest writes one set's rows into another's state).
#[cfg(feature = "p2p-share")]
async fn handle_share_ingest(
    state: &Arc<SyncServiceState>,
    folder: &str,
    folder_id: &str,
    proven_actor_hex: &str,
    rows: &[fauna_ipc::sync::ByteBuf],
    spool_dir: &str,
) -> Result<ResponsePayload, String> {
    // Canonicalised through the parse, so the key is byte-identical to the
    // one the engine registered under (`EngineMapping::engine_key`).
    let key = FolderRef::parse(folder_id)
        .ok_or_else(|| format!("'{folder_id}' is not a folder id (folder '{folder}')"))?
        .to_wire();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    {
        let senders = state.engine_cmd_senders.lock().await;
        let Some(tx) = senders.get(&key) else {
            return Err(format!(
                "folder '{folder}' is not being served by a resident engine"
            ));
        };
        tx.try_send(
            fauna_sync_engine::always_resident::EngineCommand::ShareIngest {
                proven_actor_hex: proven_actor_hex.to_string(),
                rows: rows.iter().map(|b| b.to_vec()).collect(),
                spool_dir: std::path::PathBuf::from(spool_dir),
                reply: reply_tx,
            },
        )
        .map_err(|e| match e {
            tokio::sync::mpsc::error::TrySendError::Full(_) => {
                format!("an ingest is already pending for '{folder}'")
            }
            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                format!("the engine serving '{folder}' is shutting down")
            }
        })?;
    }
    let (report, cursor) = reply_rx
        .await
        .map_err(|_| format!("the engine serving '{folder}' stopped before the ingest ran"))?
        .map_err(|e| format!("share ingest failed: {e}"))?;
    Ok(ResponsePayload::ShareIngested(
        fauna_ipc::sync::ShareIngestOutcome {
            refused: report.refused as u32,
            overlaid: report.overlaid as u32,
            materialized: report.materialized as u32,
            already_current: report.already_current as u32,
            skipped: report
                .skipped
                .into_iter()
                .map(|(p, r)| (p, r.to_string()))
                .collect(),
            cursor,
        },
    ))
}

// ── File status ──

/// Resolve an absolute path to its tracked [`SyncEntry`](fauna_sync_engine::db::SyncEntry),
/// opening the per-folder state DB of the bound on-demand folder serving it.
///
/// The multi-root hydration host keeps one state DB per bound folder, keyed by
/// folder-relative paths; the shell extension queries by absolute path. This
/// maps absolute → [`ResolvedFolder`](crate::path_map::ResolvedFolder) via
/// [`resolve_to_folder_rel`] and reads that **binding's** DB
/// (`sync_db_path_for_ref` — identity-keyed, the same resolution the engine
/// itself uses). `Ok(None)` when the
/// path is not under a served on-demand folder or has no row yet (→
/// `NotTracked`); `Err` only on a real DB error. The config read lock is released
/// before the DB is opened.
async fn lookup_entry(
    state: &Arc<SyncServiceState>,
    abs_path: &str,
) -> Result<Option<fauna_sync_engine::db::SyncEntry>, String> {
    let resolved = {
        let config = state.config.read().await;
        crate::path_map::resolve_to_folder_rel(&config, abs_path)
    };
    let Some(crate::path_map::ResolvedFolder {
        folder_ref, rel, ..
    }) = resolved
    else {
        return Ok(None);
    };
    let db_path = state.paths.sync_db_path_for_ref(folder_ref);
    // The folder may be bound but not yet served (no DB created) — NotTracked.
    if !db_path.exists() {
        return Ok(None);
    }
    let db = fauna_sync_engine::db::SyncDb::open(&db_path).map_err(|e| format!("open db: {e}"))?;
    db.get_entry(&rel).map_err(|e| format!("db error: {e}"))
}

/// Aggregate overlay status for a **folder** path — the severity fold over its
/// tracked descendants' states (`path_map::folder_status_from_states`, the
/// USER-ratified rule in `apps/windows.md` § Shell Extension). Folders carry no
/// `SyncDb` row, so [`lookup_entry`] returns `None` for them; this is the fallback
/// that gives them a badge from what's inside.
///
/// `Ok(None)` — rendered as `NotTracked`, an unbadged folder — when the path is not
/// under a served on-demand folder, its folder has no DB yet, or the folder has
/// no tracked descendant (an empty folder, or an untracked file, both of which
/// simply have no rows beneath them). Thin OS/DB-glue like [`lookup_entry`]; the
/// pure fold + the descendant query are unit-tested in `path_map` and
/// `fauna_sync_engine::db`, and the end-to-end handler path in `producer_integration`.
async fn folder_status(
    state: &Arc<SyncServiceState>,
    abs_path: &str,
) -> Result<Option<FileStatus>, String> {
    let resolved = {
        let config = state.config.read().await;
        crate::path_map::resolve_to_folder_rel(&config, abs_path)
    };
    let Some(crate::path_map::ResolvedFolder {
        folder_ref, rel, ..
    }) = resolved
    else {
        return Ok(None);
    };
    let db_path = state.paths.sync_db_path_for_ref(folder_ref);
    if !db_path.exists() {
        return Ok(None);
    }
    let db = fauna_sync_engine::db::SyncDb::open(&db_path).map_err(|e| format!("open db: {e}"))?;
    let states = db
        .descendant_states(&rel)
        .map_err(|e| format!("db error: {e}"))?;
    Ok(crate::path_map::folder_status_from_states(states))
}

async fn handle_get_file_status(
    state: &Arc<SyncServiceState>,
    path: &str,
) -> Result<ResponsePayload, String> {
    let info = match lookup_entry(state, path).await? {
        Some(entry) => FileStatusInfo {
            path: path.to_string(),
            // `effective_for_size`: a 0-byte placeholder is present-and-empty, so it badges
            // `Synced`, not `CloudOnly` — cfapi never fires a FETCH_DATA to flip its row.
            status: crate::path_map::file_status_from_state(
                entry.state.effective_for_size(entry.size_bytes),
            ),
            size_bytes: entry.size_bytes as u64,
            is_pinned: entry.pinned,
        },
        // No row of its own — it may be a folder, which carries a badge folded from
        // its tracked descendants (folders have no `SyncDb` row). A path with no
        // descendants (an empty folder, or an untracked file) folds to `None` and
        // renders `NotTracked`, exactly as before.
        None => FileStatusInfo {
            path: path.to_string(),
            status: folder_status(state, path)
                .await?
                .unwrap_or(FileStatus::NotTracked),
            size_bytes: 0,
            is_pinned: false,
        },
    };
    Ok(ResponsePayload::FileStatus(info))
}

async fn handle_pin_file(
    _state: &Arc<SyncServiceState>,
    path: &str,
) -> Result<ResponsePayload, String> {
    tracing::info!(path, "pin file requested");
    #[cfg(windows)]
    {
        fauna_cfapi::set_pin_state(std::path::Path::new(path), true)
            .map_err(|e| format!("cfapi pin failed: {e}"))?;
        Ok(ResponsePayload::Empty)
    }
    #[cfg(target_os = "linux")]
    {
        route_row_verb(_state, path, |rel, reply| {
            fauna_sync_engine::always_resident::EngineCommand::SetPinned {
                rel,
                pinned: true,
                reply,
            }
        })
        .await
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = path;
        Err("pinning is unsupported on this platform".to_string())
    }
}

async fn handle_unpin_file(
    _state: &Arc<SyncServiceState>,
    path: &str,
) -> Result<ResponsePayload, String> {
    tracing::info!(path, "unpin file requested");
    #[cfg(windows)]
    {
        fauna_cfapi::set_pin_state(std::path::Path::new(path), false)
            .map_err(|e| format!("cfapi unpin failed: {e}"))?;
        Ok(ResponsePayload::Empty)
    }
    #[cfg(target_os = "linux")]
    {
        route_row_verb(_state, path, |rel, reply| {
            fauna_sync_engine::always_resident::EngineCommand::SetPinned {
                rel,
                pinned: false,
                reply,
            }
        })
        .await
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = path;
        Err("unpinning is unsupported on this platform".to_string())
    }
}

/// Hand a per-file verb of an on-demand root whose platform keeps no pin or
/// dehydrate of its own — the linux FUSE root — to that root's loop: resolve
/// `abs_path` to its serving folder and folder-relative path, route the
/// [`EngineCommand`](fauna_sync_engine::always_resident::EngineCommand) `make`
/// builds through the engine's command channel, and wait for its answer
/// (`on-demand-files.md` § Linux FUSE binding, the dehydrate rule: the bytes move
/// on the loop that owns them). Errors — never a silent drop — for a path no
/// on-demand binding serves, a root not running, or a loop that stops first.
#[cfg(target_os = "linux")]
async fn route_row_verb(
    state: &Arc<SyncServiceState>,
    abs_path: &str,
    make: impl FnOnce(
        String,
        tokio::sync::oneshot::Sender<anyhow::Result<()>>,
    ) -> fauna_sync_engine::always_resident::EngineCommand,
) -> Result<ResponsePayload, String> {
    let resolved = {
        let config = state.config.read().await;
        crate::path_map::resolve_to_folder_rel(&config, abs_path)
    };
    let Some(crate::path_map::ResolvedFolder {
        folder_ref, rel, ..
    }) = resolved
    else {
        return Err(format!("{abs_path} is not in an on-demand folder"));
    };
    let key = folder_ref.to_wire();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let tx = state.engine_cmd_senders.lock().await.get(&key).cloned();
    let Some(tx) = tx else {
        return Err(format!("the folder serving {abs_path} is not running"));
    };
    // `send` (not `try_send`): the verb waits its turn behind a command the loop
    // is still answering, which is what a user who clicked twice expects.
    tx.send(make(rel, reply_tx))
        .await
        .map_err(|_| format!("the folder serving {abs_path} is shutting down"))?;
    reply_rx
        .await
        .map_err(|_| format!("the folder serving {abs_path} stopped before answering"))?
        .map_err(|e| e.to_string())?;
    Ok(ResponsePayload::Empty)
}

/// Record the file at `abs_path` as a `Placeholder` in its serving folder's per-root
/// state DB — the symmetric reverse of [`SyncEngine::mark_hydrated`] (Placeholder→Synced),
/// run after a cfapi dehydrate frees a file's bytes. A no-op when `abs_path` is not under a
/// served on-demand folder, or the folder is bound but not yet served (no DB), or the row
/// is already gone. Thin OS-glue (DB open via the resolved data-root); the pure routing
/// (`resolve_to_folder_rel`) and the `SyncState`→`FileStatus` mapping are tested in
/// `path_map`.
///
/// Not `cfg(windows)` (its only non-test caller, `handle_free_space`, is): like
/// `path_map`, kept platform-free so the identity-keyed DB routing
/// is pinned by unit tests on every platform, not just Windows.
///
/// [`SyncEngine::mark_hydrated`]: fauna_sync_engine::engine::SyncEngine::mark_hydrated
#[cfg_attr(not(windows), allow(dead_code))]
async fn mark_placeholder(state: &Arc<SyncServiceState>, abs_path: &str) -> Result<(), String> {
    let resolved = {
        let config = state.config.read().await;
        crate::path_map::resolve_to_folder_rel(&config, abs_path)
    };
    let Some(crate::path_map::ResolvedFolder {
        folder_ref, rel, ..
    }) = resolved
    else {
        return Ok(());
    };
    let db_path = state.paths.sync_db_path_for_ref(folder_ref);
    if !db_path.exists() {
        return Ok(());
    }
    let db = fauna_sync_engine::db::SyncDb::open(&db_path).map_err(|e| format!("open db: {e}"))?;
    db.update_state(&rel, fauna_sync_engine::db::SyncState::Placeholder)
        .map_err(|e| format!("db error: {e}"))
}

/// Is freeing `abs_path`'s local bytes provably lossless by its serving folder's own
/// record — the engine's one dehydrate gate (`SyncEngine::is_dehydration_safe_in`: row
/// `Synced`, disk hash == the recorded content) over the per-folder state DB? Fail-closed:
/// a path no served on-demand folder resolves, a folder with no DB yet, or any read error
/// answers `false`. The hash runs off the async runtime (a large file takes seconds).
#[cfg_attr(not(windows), allow(dead_code))]
async fn dehydration_safe(state: &Arc<SyncServiceState>, abs_path: &str) -> bool {
    let resolved = {
        let config = state.config.read().await;
        crate::path_map::resolve_to_folder_rel(&config, abs_path)
    };
    let Some(crate::path_map::ResolvedFolder {
        folder_ref, rel, ..
    }) = resolved
    else {
        return false;
    };
    let db_path = state.paths.sync_db_path_for_ref(folder_ref);
    let full_path = std::path::PathBuf::from(abs_path);
    tokio::task::spawn_blocking(move || {
        if !db_path.exists() {
            return false;
        }
        fauna_sync_engine::db::SyncDb::open(&db_path).is_ok_and(|db| {
            fauna_sync_engine::engine::SyncEngine::is_dehydration_safe_in(&db, &rel, &full_path)
        })
    })
    .await
    .unwrap_or(false)
}

async fn handle_free_space(
    state: &Arc<SyncServiceState>,
    path: &str,
) -> Result<ResponsePayload, String> {
    tracing::info!(path, "free space requested");
    #[cfg(windows)]
    {
        // Gate on the engine's record before the platform's own refusal: cfapi's not-in-sync bit is the first-line guard, but it is only as
        // good as every in-sync assertion ever made, and freeing an unrecorded edit is
        // irrecoverable. A file already cloud-only has nothing local to lose.
        let p = std::path::Path::new(path);
        if !fauna_sync_engine::placeholder::path_is_cloud_placeholder(p)
            && !dehydration_safe(state, path).await
        {
            return Err(
                "not freeing space: this device may hold the only copy of this file's \
                 content — it has not finished syncing, or its folder keeps no content \
                 on the server"
                    .to_string(),
            );
        }
        fauna_cfapi::dehydrate_placeholder(p)
            .map_err(|e| format!("cfapi dehydrate failed: {e}"))?;

        // Symmetric reverse of the hydrate success arm (bridge.rs `run_hydration_loop`:
        // `mark_hydrated` + emit `FileStatusChanged { Synced }`). The bytes are now freed,
        // so record the file back to `Placeholder` in its serving folder's per-root DB
        // and push a `FileStatusChanged { CloudOnly }` so Explorer overlays flip
        // Synced → CloudOnly live (matching the `GetFileStatus` re-query, which maps
        // Placeholder → CloudOnly — no badge flicker).
        if let Err(e) = mark_placeholder(state, path).await {
            // Best-effort, like `mark_hydrated`: the cfapi dehydrate already succeeded.
            tracing::warn!(path, error = %e, "recording dehydrated file as Placeholder failed");
        }
        let event = {
            let config = state.config.read().await;
            crate::path_map::dehydrate_status_event(&config, path)
        };
        if let Some(event) = event {
            let _ = state.ipc_event_tx.send(event);
        }
        Ok(ResponsePayload::Empty)
    }
    // linux: the FUSE root's loop gates, records and frees (the bytes are its).
    #[cfg(target_os = "linux")]
    {
        route_row_verb(state, path, |rel, reply| {
            fauna_sync_engine::always_resident::EngineCommand::FreeSpace { rel, reply }
        })
        .await
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (state, path);
        Err("freeing space is unsupported on this platform".to_string())
    }
}

async fn handle_set_location_sync_mode(
    state: &Arc<SyncServiceState>,
    path: &str,
    mode: &str,
) -> Result<ResponsePayload, String> {
    let new_mode = match mode {
        "always" => crate::config::LocationMode::Always,
        "on-demand" => crate::config::LocationMode::OnDemand,
        _ => return Err(format!("unknown mode: {mode}")),
    };

    // Scope the config write guard so it is released before `reconcile_engines`
    // (which read-locks config — holding the write guard across it would deadlock).
    {
        let mut config = state.config.write().await;
        let folder = config
            .find_location_mut(path)
            .ok_or_else(|| format!("folder not found: {path}"))?;
        folder.mode = new_mode;
        if let Err(e) = state.paths.save_config(&config) {
            tracing::warn!("config save failed: {e}");
        }
    }

    tracing::info!(path, mode, "folder sync mode changed");

    // Reconcile: switching to on-demand serves the root (if bound to a folder);
    // switching back to always stops it. Best-effort.
    if let Err(e) = crate::engine_driver::reconcile_engines(state).await {
        tracing::warn!(error = %e, "hydration reconcile after sync-mode change failed");
    }
    Ok(ResponsePayload::Empty)
}

/// Bind an already-added location to a nest folder (multi-root on-demand) —
/// the one bind verb, keyed by the set's identity.
///
/// `folder_id` must parse as a `FolderRef`; anything else is refused before
/// the config is touched, so a binding is never recorded that no engine, key
/// lookup or state DB could resolve. `folder` is stored as the label.
///
/// Records the device-local location↔folder binding, mirroring
/// `handle_set_location_sync_mode`'s shape: find the folder, mutate + persist, then
/// reconcile. Binding a folder to an on-demand folder makes it serveable, so the
/// reconcile starts that root (if the device + capability preconditions hold).
/// Returns `folder not found` if the path isn't a known sync folder.
async fn handle_set_location_folder(
    state: &Arc<SyncServiceState>,
    path: &str,
    folder: &str,
    folder_id: &str,
) -> Result<ResponsePayload, String> {
    let folder_ref = FolderRef::parse(folder_id).ok_or_else(|| {
        format!("'{folder_id}' is not a folder id (binding {path} to '{folder}')")
    })?;
    {
        let mut config = state.config.write().await;
        let location = config
            .find_location_mut(path)
            .ok_or_else(|| format!("folder not found: {path}"))?;
        // Label and ref are written together — `LocationConfig::binding` reads
        // them as one — and the ref in its canonical wire form, so the stored
        // key is byte-identical to the one the engine registers under.
        location.folder = Some(folder.to_string());
        location.folder_id = Some(folder_ref.to_wire());
        // A (re-)bind is the recovery path out of a D4 `access-revoked` park:
        // the client only reaches here after its eager bind-time
        // `write_token.get` verify (D3) got a *yes* from the authoritative nest,
        // so the grant is live again and the binding may run. If it is not, the
        // very next mint/record re-parks it — fail-closed either way, and never
        // a park that outlives the revocation it recorded.
        location.access_revoked = false;
        if let Err(e) = state.paths.save_config(&config) {
            tracing::warn!("config save failed (in-memory state updated): {e}");
        }
    }

    tracing::info!(path, folder, folder_id, "folder bound to folder");

    // Before the reconcile, so the engine's first seat read already sees the
    // place it just gained.
    enrol_bound_place(state, folder, &folder_ref).await;

    // Reconcile: binding a folder to an on-demand folder makes that root
    // serveable. Best-effort.
    if let Err(e) = crate::engine_driver::reconcile_engines(state).await {
        tracing::warn!(error = %e, "hydration reconcile after folder binding failed");
    }
    Ok(ResponsePayload::Empty)
}

/// **A local presence writes the place it needs** (`file-sync.md` § 4): a bind
/// enrols this device at the default point on a folder it holds no place in,
/// through the shared `FoldersClient::ensure_place`. The agent is the one bind
/// seam every desktop shares (linux, tui and windows all bind through
/// `SetLocationFolder`), so this is the only desktop call site.
///
/// Best-effort and bounded by [`NEST_VERB_TIMEOUT`]: the binding is already
/// recorded, and until the place lands the engine's `Absent` default governs
/// the seat. Only an own-nest set (`FolderRef::Local`) has a roster this
/// device can join — a cross-nest set's roster lives on its home nest under
/// its owner's account. A same-nest set shared *with* this account is not
/// this account's roster either: the nest answers `not_found`, which is
/// expected and logged quietly.
async fn enrol_bound_place(state: &Arc<SyncServiceState>, folder: &str, folder_ref: &FolderRef) {
    if !matches!(folder_ref, FolderRef::Local(_)) {
        return;
    }
    let Some(device_id) = state
        .capability
        .read()
        .await
        .as_ref()
        .map(|cap| cap.device_id.clone())
    else {
        return;
    };
    // Outer `Err` = no control plane; inner = the nest's answer.
    let call = async {
        let client = state.nest_rpc_client().await?;
        Ok::<_, String>(
            fauna_client_folders::FoldersClient::new(client)
                .ensure_place(folder, &device_id)
                .await,
        )
    };
    match tokio::time::timeout(NEST_VERB_TIMEOUT, call).await {
        Ok(Ok(Ok(outcome))) => tracing::info!(folder, ?outcome, "bound folder's place ensured"),
        Ok(Ok(Err(fauna_client::NestClientError::Rpc(rpc))))
            if rpc.code == "fauna.folders.not_found" =>
        {
            tracing::debug!(
                folder,
                "bound folder has no own roster here; no place to enrol"
            )
        }
        Ok(Ok(Err(e))) => tracing::warn!(folder, error = %e, "place enrol on bind failed"),
        Ok(Err(e)) => tracing::warn!(folder, error = %e, "place enrol on bind: no nest"),
        Err(_) => tracing::warn!(folder, "place enrol on bind timed out"),
    }
}

// ── Context menu / sharing ──

/// `ShareFile`: resolve an Explorer path to where the app should open to share
/// it — never a link. The agent is seedless and a share token must be signed by
/// the account key, so the leaf hands off to the app
/// (`docs/goal/behavior/share-links.md` § Windows Explorer's Share leaf); the
/// mechanism is `apps/windows.md` § Shell Extension → *The Share hand-off*.
/// An `Err` means "no Share here" — the shell hides the leaf.
async fn handle_share_file(
    state: &Arc<SyncServiceState>,
    path: &str,
) -> Result<ResponsePayload, String> {
    let resolved = {
        let config = state.config.read().await;
        crate::path_map::resolve_to_folder_rel(&config, path)
    };
    let Some(crate::path_map::ResolvedFolder {
        folder_ref, rel, ..
    }) = resolved
    else {
        return Err("not in a bound on-demand folder".to_string());
    };
    let public_audience = state
        .public_audience
        .lock()
        .await
        .get(&folder_ref.to_wire())
        .copied()
        .unwrap_or(false);
    let is_dir = std::path::Path::new(path).is_dir();
    share_target(&folder_ref, &rel, is_dir, public_audience).map(ResponsePayload::ShareTarget)
}

/// The `ShareFile` decision, pure so it is pinned without a pipe or an engine.
///
/// - The set's own root (`rel` empty) → a folder target (the member-share
///   picker), whatever its audience.
/// - A sub-folder → refused: it is not a shareable unit, and opening its
///   containing set's picker would silently widen what the user right-clicked.
/// - A file → a target only while the set's engine holds the public-audience
///   arm, the same owner-attested fact the nest serves by (`share_link_eligible`).
/// - A cross-nest set → refused: the app's routes name a set by this nest's
///   `folders.id`, which a foreign set does not have.
fn share_target(
    folder_ref: &FolderRef,
    rel: &str,
    is_dir: bool,
    public_audience: bool,
) -> Result<ShareTargetInfo, String> {
    let FolderRef::Local(folder_id) = *folder_ref else {
        return Err("a set on another nest is shared from its owner's app".to_string());
    };
    if rel.is_empty() {
        return Ok(ShareTargetInfo {
            folder_id,
            path: None,
        });
    }
    if is_dir {
        return Err("a sub-folder is not a shareable unit".to_string());
    }
    if !public_audience {
        return Err("only a file in a public folder can carry a share link".to_string());
    }
    Ok(ShareTargetInfo {
        folder_id,
        path: Some(rel.to_string()),
    })
}

async fn handle_get_file_devices(
    _state: &Arc<SyncServiceState>,
    path: &str,
) -> Result<ResponsePayload, String> {
    // Stub: return 1 device (this device) until nest API endpoint exists
    Ok(ResponsePayload::FileDevices(FileDevicesInfo {
        path: path.to_string(),
        device_count: 1,
    }))
}

async fn handle_get_file_versions(
    state: &Arc<SyncServiceState>,
    path: &str,
) -> Result<ResponsePayload, String> {
    let info = match lookup_entry(state, path).await? {
        Some(entry) => FileVersionsInfo {
            path: path.to_string(),
            version_count: entry.version_num as u32,
            latest_timestamp: (entry.remote_mtime > 0).then_some(entry.remote_mtime as u64),
        },
        None => FileVersionsInfo {
            path: path.to_string(),
            version_count: 0,
            latest_timestamp: None,
        },
    };
    Ok(ResponsePayload::FileVersions(info))
}

/// A right-click must never hang Explorer. The `fauna.files.versions.*` kinds carry
/// a 5 s per-kind deadline inside `NestClient`, but that does not bound the WS
/// *connect* on the first call, so the whole operation gets its own ceiling.
///
/// Kept **below** the shell client's `fauna_ipc::sync_pipe_client::REQUEST_TIMEOUT`
/// (6 s), so a slow nest surfaces as this handler's error reply rather than as the
/// caller giving up on the pipe.
const NEST_VERB_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

/// Who reads, for the version verbs — writer-signed change records, ruling
/// (3): every listed or restored version is judged under this agent's own
/// actor id and the set nonces its content-key resolution holds (the same map
/// its record signing binds to, `custodian`'s re-seed leg). No capability or
/// no resolution for this account yet → no nonces: a signed version cannot
/// verify yet and is held (absent, judged again on the next listing).
async fn version_reader_seat(
    state: &Arc<SyncServiceState>,
) -> fauna_client_sync::row_judge::ReaderSeat {
    // The account's attested predecessor ids ride the capability
    // (`SyncCapability::predecessor_actor_ids`), so a version a retired
    // identity signed is judged as the account's own (ruling (8)(b)).
    let (own, predecessors) = match state.capability.read().await.as_ref() {
        Some(cap) => (cap.actor_id_array(), cap.predecessor_actor_ids()),
        None => (None, Vec::new()),
    };
    let nonces = match own {
        Some(actor) => state
            .content_keys
            .read()
            .await
            .as_ref()
            .filter(|r| r.is_for(actor))
            .map(|r| {
                fauna_client_sync::SetNonceSource::ByFolder(Arc::new(r.set_lineages_by_name()))
            }),
        None => None,
    };
    fauna_client_sync::row_judge::ReaderSeat {
        own,
        nonces,
        predecessors,
        ..Default::default()
    }
}

/// `ListFileVersions` — the file's real, retroactive version history from the nest
/// (`file-sync.md` § File Versions), unlike `GetFileVersions` which reports only the
/// local `SyncDb` entry's counter.
///
/// A path outside every served on-demand folder is not an error: it simply has no
/// history (Explorer offers the menu on any file).
async fn handle_list_file_versions(
    state: &Arc<SyncServiceState>,
    path: &str,
) -> Result<ResponsePayload, String> {
    let resolved = {
        let config = state.config.read().await;
        crate::path_map::resolve_to_folder_rel(&config, path)
    };
    // The version verbs are nest-side and name-addressed — only the DB-reading
    // sites need the binding's `folder_ref`.
    let Some(crate::path_map::ResolvedFolder { folder, rel, .. }) = resolved else {
        return Ok(ResponsePayload::FileVersionList(
            crate::versions::empty_version_list(path),
        ));
    };

    let seat = version_reader_seat(state).await;
    // The connect() inside `nest_rpc_client` must be INSIDE the ceiling: on the very
    // first right-click after login the WS handshake has not happened yet, and an
    // unreachable nest would otherwise block Explorer for the TCP timeout.
    let call = async {
        let client = state.nest_rpc_client().await?;
        let sync = fauna_client_sync::SyncClient::new(client);
        crate::versions::list_file_versions(&sync, path, &folder, &rel, &seat).await
    };

    let info = match tokio::time::timeout(NEST_VERB_TIMEOUT, call).await {
        Ok(Ok(info)) => info,
        // No invalidation on failure: the retained client's supervisor reconnects
        // on its own, and `nest_rpc_client` rebuilds it only once it has stopped
        // (`SyncServiceState::nest_rpc`).
        Ok(Err(e)) => return Err(e),
        Err(_elapsed) => {
            return Err(format!(
                "listing versions timed out after {}s",
                NEST_VERB_TIMEOUT.as_secs()
            ));
        }
    };
    Ok(ResponsePayload::FileVersionList(info))
}

/// `RestoreFileVersion` — restore `path` to `version_num` (`file-sync.md` § Restore).
///
/// Two halves, in this order, and **the order is load-bearing**:
///
/// 1. **Record on the nest** (the source of truth): look the historical version up,
///    then record an ordinary `modify` re-pointing the file at its manifest — via the
///    shared [`SyncClient::restore_version`], so this can never drift from the Media
///    library's restore.
/// 2. **Re-point this device's own local copy.** Catch-up deliberately skips a
///    device's own changes, and Explorer's bytes come from the local `SyncDb` row's
///    `manifest_hash` — so recording alone would leave *this* machine, the very one
///    the user is looking at, showing the pre-restore content.
///
/// The reverse order would serve content the nest never agreed to. A crash (or a
/// timeout) between the two leaves the nest correct and this device's row stale; see
/// [`apply_restore_locally`] for what that costs and how the user recovers.
/// The re-point record's `path_sealed`, minted from the agent's own key
/// material (S8 D2): the set's engine keys as the agent resolved them from
/// custody (`crate::content_keys` — carrying the fail-closed bound-but-keyless
/// decision) plus the capability `BackupKey`, through the pinned
/// `FileDownloadKeys::label_seal_root`. This is the identical selection every
/// engine this agent builds uses (the shared `engine_lifecycle::build_engine` keys the same
/// pair), so a re-point can never seal under a root the set's readers don't
/// hold. Best-effort: `None` records plaintext-only, an S8 backfill row —
/// including the bound-keyless arm, where sealing under the only key we *do*
/// hold (the owner's) would be exactly the root no roster member could open,
/// and a set the resolution does not name at all.
fn seal_repoint_for(
    cap: &fauna_ipc::sync::SyncCapability,
    resolved: Option<&crate::content_keys::ResolvedContentKeys>,
    folder_ref: FolderRef,
    rel: &str,
) -> Option<Vec<u8>> {
    let resolved = resolved.filter(|r| cap.actor_id_array().is_some_and(|a| r.is_for(a)))?;
    let engine_keys = resolved.keys_for(folder_ref)?;
    let keys = fauna_core::file_download::FileDownloadKeys {
        backup_key: cap
            .backup_key_array()
            .map(|k| fauna_core::crypto::BackupKey::from_bytes(k).into()),
        mls_group_id: engine_keys.mls_group_id,
        content_keys: engine_keys.content_keys,
        ..Default::default()
    };
    let root = keys.label_seal_root().ok().flatten()?;
    fauna_core::label_custody::seal_path(&root, rel).ok()
}

/// The restore's byte seam ([`crate::versions::InheritedReseal`]) over the
/// set's own **running engine**: the re-seal of a version a retired identity
/// signed is routed down the engine's command channel
/// (`SyncServiceState::engine_cmd_senders`) and answered on the loop that owns
/// the set's bytes and keys — this handler holds no engine, and a second one
/// for the set would open the running engine's state DB.
///
/// A set no engine is running for (bound but not yet served, or stopping) has
/// no byte seam here: the version is refused with the inherited refusal, which
/// names the Media page's restore.
struct EngineReseal<'a> {
    state: &'a Arc<SyncServiceState>,
    folder_ref: FolderRef,
    /// Raised while the engine moves the bytes: a whole file's re-seal is not
    /// bounded by [`NEST_VERB_TIMEOUT`], which is sized for one RPC.
    moving_bytes: &'a std::sync::atomic::AtomicBool,
}

impl crate::versions::InheritedReseal for EngineReseal<'_> {
    async fn reseal(
        &self,
        rel: &str,
        manifest_hash: [u8; 32],
        signed_as: Option<[u8; 32]>,
    ) -> Result<fauna_sync_engine::engine::ResealedVersion, String> {
        use std::sync::atomic::Ordering;
        let tx = self
            .state
            .engine_cmd_senders
            .lock()
            .await
            .get(&self.folder_ref.to_wire())
            .cloned()
            .ok_or_else(|| crate::versions::RESTORE_INHERITED_REFUSED.to_string())?;
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.moving_bytes.store(true, Ordering::Relaxed);
        // `send`, not `try_send`: the re-seal waits its turn behind a command
        // the loop is still answering.
        let outcome = async {
            tx.send(
                fauna_sync_engine::always_resident::EngineCommand::ResealInheritedVersion {
                    rel: rel.to_string(),
                    manifest_hash,
                    signed_as,
                    reply,
                },
            )
            .await
            .map_err(|_| "the folder's sync engine is shutting down".to_string())?;
            answer
                .await
                .map_err(|_| "the folder's sync engine stopped before answering".to_string())?
                .map_err(|e| format!("{e:#}"))
        }
        .await;
        self.moving_bytes.store(false, Ordering::Relaxed);
        outcome
    }
}

async fn handle_restore_file_version(
    state: &Arc<SyncServiceState>,
    path: &str,
    version_num: i64,
) -> Result<ResponsePayload, String> {
    let resolved = {
        let config = state.config.read().await;
        crate::path_map::resolve_to_folder_rel(&config, path)
    };
    // Unlike `ListFileVersions` — where "no history" is a fine answer for any file
    // Explorer happens to offer the menu on — restoring a file we do not serve is an
    // error: there is no folder to record the change against.
    let Some(crate::path_map::ResolvedFolder {
        folder,
        folder_ref,
        rel,
    }) = resolved
    else {
        return Err("file is not in a synced folder".to_string());
    };

    // The recording device must be one of the actor's registered, write-capable sync
    // devices; the capability carries this agent's id (the shared record self-heals a
    // never-registered one). The re-point's `path_sealed` is minted here too: the
    // agent's own custody-resolved per-set engine keys (bound sets, incl. the
    // fail-closed bound-but-keyless decision) + the capability `BackupKey`
    // (owner-only sets), through the pinned
    // `FileDownloadKeys::label_seal_root` — the identical selection every engine
    // this agent builds uses (the shared `engine_lifecycle::build_engine`), so a re-point can
    // never seal under a root the set's readers don't hold (S8 D2). Best-effort:
    // `None` records plaintext-only, an S8 backfill row.
    let (device_id, path_sealed, recorded_as) = {
        let cap = state.capability.read().await;
        let cap = cap
            .as_ref()
            .ok_or_else(|| "nest capability not provisioned".to_string())?;
        let resolved = state.content_keys.read().await;
        (
            cap.device_id.clone(),
            seal_repoint_for(cap, resolved.as_ref(), folder_ref, &rel),
            // The restore records as the current identity, so the re-pointed
            // entry's head is signed as it (ruling (11)(d)'s persisted signer).
            cap.actor_id_array(),
        )
    };

    let seat = version_reader_seat(state).await;
    // The connect() must be INSIDE the ceiling, for the reason `handle_list_file_versions`
    // documents: the first right-click after login has not yet handshaked.
    let moving_bytes = std::sync::atomic::AtomicBool::new(false);
    let reseal = EngineReseal {
        state,
        folder_ref,
        moving_bytes: &moving_bytes,
    };
    let call = async {
        let client = state.nest_rpc_client().await?;
        let sync = fauna_client_sync::SyncClient::new(client);
        crate::versions::restore_file_version(
            &sync,
            &folder,
            &device_id,
            &rel,
            version_num,
            path_sealed,
            &seat,
            &reseal,
        )
        .await
    };
    tokio::pin!(call);

    // The ceiling bounds the RPCs (the lookup, the record), not the re-seal of
    // an inherited version between them: a window that closes while the engine
    // is moving the bytes opens another.
    let restored = loop {
        match tokio::time::timeout(NEST_VERB_TIMEOUT, &mut call).await {
            Ok(Ok(restored)) => break restored,
            Ok(Err(e)) => return Err(e),
            Err(_elapsed) if moving_bytes.load(std::sync::atomic::Ordering::Relaxed) => {}
            Err(_elapsed) => {
                // The record may in fact have landed — a timeout is not a rollback.
                // Retrying is safe: history is append-only, so a duplicate restore
                // records another (identical) head and is itself reversible.
                return Err(format!(
                    "restoring version timed out after {}s",
                    NEST_VERB_TIMEOUT.as_secs()
                ));
            }
        }
    };

    tracing::info!(
        path,
        version_num,
        recorded_seq = restored.recorded_seq,
        "restore recorded on nest"
    );

    #[cfg(windows)]
    apply_restore_locally(state, path, folder_ref, &rel, &restored, recorded_as).await;
    #[cfg(not(windows))]
    let _ = (&folder_ref, &rel, &restored, recorded_as);

    Ok(ResponsePayload::Empty)
}

/// Re-point `rel`'s local row at the restored version.
///
/// `SyncDb` has no narrow re-point setter — `update_state` cannot touch
/// `manifest_hash` — so this read-modify-writes through `upsert_entry`, whose
/// `ON CONFLICT` clause deliberately omits `pinned`, leaving the user's pin intact.
///
/// The row written here is exactly the row `record_placeholders_from_changes` would
/// fold from the nest's new head (`local_hash`/`remote_hash` = `None`, `local_mtime`
/// = 0, `state` = `Placeholder`), so a *different* device that learns of this restore
/// through catch-up lands in the identical state. The entry's head is stamped as signed
/// by `recorded_as`, the identity the restore recorded as
/// (`writer-signed-change-records.md` ruling (11)(d)): an entry with no signer recorded
/// opens under no owner root, so a re-point that named none would strand the restored
/// bytes.
///
/// `Ok(false)` = there is no local copy to re-point (the folder is bound but not yet
/// served, or the path has no row): per § Restore, a client with no local file has
/// nothing to apply.
///
/// Not `cfg(windows)` (its only non-test caller, `apply_restore_locally`, is): like
/// `path_map`, kept platform-free so the identity-keyed DB routing
/// is pinned by unit tests on every platform, not just Windows.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) async fn repoint_entry(
    state: &Arc<SyncServiceState>,
    folder_ref: FolderRef,
    rel: &str,
    restored: &crate::versions::RestoredVersion,
    recorded_as: Option<[u8; 32]>,
) -> Result<bool, String> {
    let db_path = state.paths.sync_db_path_for_ref(folder_ref);
    if !db_path.exists() {
        return Ok(false);
    }
    let db = fauna_sync_engine::db::SyncDb::open(&db_path).map_err(|e| format!("open db: {e}"))?;
    let Some(entry) = db.get_entry(rel).map_err(|e| format!("db error: {e}"))? else {
        return Ok(false);
    };
    let now = fauna_core::data::Timestamp::now_secs_or_zero();
    db.upsert_entry(
        rel,
        None, // local_hash: the bytes are about to be freed
        None, // remote_hash: carried by neither the version projection nor changes.list
        Some(fauna_core::data::ContentHash::from_digest_raw(
            restored.manifest_hash,
        )),
        fauna_sync_engine::db::SyncState::Placeholder,
        0,   // local_mtime: nothing on disk once dehydrated
        now, // remote_mtime: the restore record is the new head, recorded just now
        restored.size_bytes,
        // The legacy local counter `GetFileVersions` reports. The real, retroactive
        // history is nest-side (`ListFileVersions`), so leave it undisturbed.
        entry.version_num,
        restored.content_key_version,
    )
    .map_err(|e| format!("db error: {e}"))?;
    if let Some(signer) = recorded_as {
        db.set_head_signed_as(
            rel,
            &fauna_core::data::ContentHash::from_digest_raw(restored.manifest_hash),
            &signer,
        )
        .map_err(|e| format!("db error: {e}"))?;
    }
    Ok(true)
}

/// Re-point this device's own copy at the just-restored version, then drop the stale
/// cached bytes so the next open re-hydrates the historical ones
/// (`file-sync.md` § Restore, *the recording device must re-point its own local copy*).
///
/// Best-effort, and run *after* the authoritative nest record. The two steps are
/// ordered so the **durable** row is corrected before the **volatile** on-disk cache:
///
/// * row first — `SyncEngine::download_file_bytes` resolves the hydration manifest from
///   it, so once re-pointed every later fetch serves the restored bytes;
/// * `dehydrate_placeholder` second — it frees the stale bytes, turning the next open
///   into a cfapi `FETCH_DATA` against the re-pointed row.
///
/// A failed dehydrate therefore leaves a *correct* row and a stale cache: the user's own
/// **Free up space**, or any later eviction, completes the restore. The reverse order
/// would leave a placeholder-on-disk pointing at the **old** manifest — actively
/// re-materializing the pre-restore bytes, which is strictly worse.
///
/// A failed re-point skips the dehydrate for the same reason: dehydrating against a
/// stale row re-downloads the pre-restore content.
#[cfg(windows)]
async fn apply_restore_locally(
    state: &Arc<SyncServiceState>,
    abs_path: &str,
    folder_ref: FolderRef,
    rel: &str,
    restored: &crate::versions::RestoredVersion,
    recorded_as: Option<[u8; 32]>,
) {
    match repoint_entry(state, folder_ref, rel, restored, recorded_as).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::debug!(path = abs_path, "no local copy to re-point after restore");
            return;
        }
        Err(e) => {
            // The nest record has landed and is authoritative; this device now shows
            // stale content until the user restores again (append-only history makes
            // that safe and idempotent).
            tracing::warn!(path = abs_path, error = %e, "re-pointing local row after restore failed");
            return;
        }
    }

    if let Err(e) = fauna_cfapi::dehydrate_placeholder(std::path::Path::new(abs_path)) {
        tracing::warn!(path = abs_path, error = %e, "dehydrating restored file failed");
        return;
    }

    // Same event the `FreeSpace` verb pushes — `CloudOnly` is what the `GetFileStatus`
    // re-query will now report for this `Placeholder` row, so the badge does not flicker.
    let event = {
        let config = state.config.read().await;
        crate::path_map::dehydrate_status_event(&config, abs_path)
    };
    if let Some(event) = event {
        let _ = state.ipc_event_tx.send(event);
    }
}

// ── Configure ──

async fn handle_configure(
    state: &Arc<SyncServiceState>,
    nest_url: Option<String>,
) -> Result<ResponsePayload, String> {
    tracing::info!(?nest_url, "configure requested");
    // No-op: the agent sources its nest URL live from the provisioned capability
    // (`SyncCapability.nest_url`), not from any persisted config or `device.toml`,
    // so there is nothing to reconnect here.
    let _ = state;
    Ok(ResponsePayload::Empty)
}

// ── On-demand hydration capability handoff ──

async fn handle_provision_capability(
    state: &Arc<SyncServiceState>,
    cap: &SyncCapability,
) -> Result<ResponsePayload, String> {
    // Validate the key length before storing so state never holds a malformed capability.
    // `Zeroizing` wipes this transient 32-byte copy when it leaves scope.
    let key = zeroize::Zeroizing::new(
        cap.backup_key_array()
            .ok_or_else(|| "capability backup_key must be exactly 32 bytes".to_string())?,
    );
    let actor_id = cap
        .actor_id_array()
        .ok_or_else(|| "capability actor_id must be exactly 32 bytes".to_string())?;
    let owned = SyncCapability::new(
        key.to_vec(),
        actor_id.to_vec(),
        cap.nest_url.clone(),
        cap.device_id.clone(),
        BearerToken::new(cap.bearer.token.clone(), cap.bearer.expires_at),
    )
    // The account's retired owner keys after an identity succession
    // (`sync-agent.md` § Credential model). Carried forward because this
    // handler rebuilds the capability field by field, so a field not named here
    // is silently dropped on every provision/reconnect — and dropping *these* would put a
    // successor's corpus back to unopenable, which reads as corruption rather
    // than as a missing key.
    .with_predecessor_backup_keys(&cap.predecessor_backup_keys())
    // The attested predecessor ids — the account host's R14 `prior`
    // (`account_host::HostInputs::attested_predecessors`). Same field-by-field
    // reason; dropping these would silently empty the agent's fleet view of
    // every predecessor-signed enrollment on the next reconnect.
    .with_predecessor_actor_ids(&cap.predecessor_actor_ids())
    // The retired keys paired with their identities — the per-signer bound's
    // input (`mls-group-key-material.md` § M2 → *Writer-signed change
    // records*, ruling (8)(c)). Same field-by-field reason; dropping these
    // would turn every predecessor-signed row into a noted skip on the next
    // reconnect.
    .with_predecessor_keys_by_actor(&cap.predecessor_keys_by_actor())
    // The provision stamp, for the same field-by-field reason — and this one
    // fails in the *opposite* direction, which is why it is easy to miss:
    // dropping it makes the capability read as provisioned at time 0, so the
    // signed-out reconcile would revoke a freshly-provisioned capability against
    // a marker its own previous sign-out left behind, silently stopping a
    // signed-in user's sync. The agent never invents a stamp of its own, since
    // only the identity-holding app knows when it minted this.
    .with_provisioned_at_ms(cap.provisioned_at_ms);
    let expires_at = owned.bearer.expires_at;

    // Per-actor state scoping (`file-sync.md` § Multi-account × File Provider,
    // consequence 3): scope the shared paths to the provisioned actor BEFORE
    // publishing the capability, and reload the binding store from the scoped
    // location — a switch must never leave the outgoing account's bindings or DBs
    // visible to the incoming one. Every desktop app un-provisions before
    // provisioning the incoming account (consequence-1 ordering), but a lost or
    // late un-provision must not matter here: this provision itself re-scopes and
    // the trailing `reconcile_engines` rebuilds engines under the new scope — the
    // scope-aware `EngineStamp` forces that rebuild even for a same-named
    // owner-only set (else the outgoing engine's open DB handle would leak across
    // accounts).
    #[cfg(any(target_os = "macos", windows, target_os = "linux"))]
    if crate::service::apply_actor_scope(&state.paths, &actor_id) {
        let config = state.paths.load_config().unwrap_or_else(|e| {
            tracing::warn!("scoped config load failed, using defaults: {e}");
            crate::config::SyncConfig::default()
        });
        *state.config.write().await = config;
        state.invalidate_nest_rpc().await;
    }

    // Persist BEFORE publishing to the slot (sync-agent.md § Credential model):
    // the record is what survives to the next app-dead boot. No-op in unit
    // tests (`credentials: None`).
    if let Some(store) = &state.credentials {
        crate::credentials::persist_capability(store, &owned);
    }
    *state.capability.write().await = Some(owned);
    // The account host keys its mount on this slot: an account switch that
    // arrives as a provision (no un-provision first) must not leave the
    // outgoing account mounted until the host's next recheck. The custodian
    // stint keys on the same slot's device id.
    crate::account_host::recheck_now(state);
    crate::custodian::recheck_now(state);
    // NB: never log the key or bearer.
    tracing::info!(nest_url = %cap.nest_url, ?expires_at, "on-demand hydration capability provisioned");

    // Refresh the principal-support advertisement now that the actor is known
    // (and, on an account switch, now that it has CHANGED — the answer is
    // actor-qualified, so a stale one would tell an incoming account that this
    // agent renews as its principal when the key belongs to the outgoing one).
    crate::renewal::refresh_store_principal_presence(state).await;
    // A provision never clears a standing refusal — only a renewal that
    // succeeds does — but it is the sign an app is signed in, so the renewal
    // loop re-reads the machine's evidence now.
    crate::renewal::wake_if_refused(state);

    // The capability is the last precondition for the hydration host (device config +
    // bound on-demand folder + capability). Best-effort: a missing precondition just
    // leaves the host unstarted until the next provision / on-demand / bind change.
    if let Err(e) = crate::engine_driver::reconcile_engines(state).await {
        tracing::warn!(error = %e, "hydration start deferred after capability provision");
    }
    Ok(ResponsePayload::Empty)
}

async fn handle_refresh_bearer(
    state: &Arc<SyncServiceState>,
    bearer: &BearerToken,
) -> Result<ResponsePayload, String> {
    let mut guard = state.capability.write().await;
    match guard.as_mut() {
        Some(cap) => {
            cap.bearer.token.zeroize(); // zeroize the superseded bearer before overwrite
            cap.bearer.token = bearer.token.clone();
            cap.bearer.expires_at = bearer.expires_at;
            // Keep the persisted record on the current bearer, so an app-dead
            // reboot resumes fresh instead of waiting out a renewal round-trip.
            if let Some(store) = &state.credentials {
                crate::credentials::persist_capability(store, cap);
            }
            tracing::info!(expires_at = ?bearer.expires_at, "on-demand hydration bearer refreshed");
            // The pushed bearer re-arms dialling and leaves a standing refusal
            // reported; the renewal loop re-reads the machine's evidence now.
            crate::renewal::wake_if_refused(state);
            Ok(ResponsePayload::Empty)
        }
        // The "no capability provisioned" prefix is a cross-process contract: the
        // app's HydrationSessionService matches it to decide a full re-provision
        // (a restarted agent has no capability until the app pushes one again).
        // Rewording it downgrades that trigger to unreachable-vs-error guessing.
        None => Err("no capability provisioned; cannot refresh bearer".to_string()),
    }
}

/// Tear down the provisioned capability (sign-out / account switch /
/// revocation — sync-agent.md § Credential model): delete the persisted
/// record, clear the in-memory slot, and stop engines. Idempotent — an
/// un-provisioned agent just re-runs the (empty) reconcile.
///
/// **The reply is the receipt** (`sync-agent.md` § Control plane split): `Ok`
/// only once this process's account-store mount is down, because the app's
/// next step is to erase that store. A mount still up when the budget elapses
/// is reported as a refusal, never as success — the app then proceeds exactly
/// as it does for an unreachable agent.
async fn handle_unprovision_capability(
    state: &Arc<SyncServiceState>,
) -> Result<ResponsePayload, String> {
    if unprovision_now(state).await {
        Ok(ResponsePayload::Empty)
    } else {
        Err(format!(
            "capability un-provisioned, but the account store was still mounted after \
             {:?}; its teardown is still running",
            crate::account_host::UNMOUNT_BUDGET
        ))
    }
}

/// The teardown body itself, shared by the IPC handler above and the renewal
/// loop's signed-out reconcile (`crate::renewal`) — the second caller is the
/// whole point: a teardown that only ever runs when the message *arrives*
/// cannot heal a message that was lost, so the agent must be able to reach this
/// on its own evidence.
///
/// Returns whether the account host's mount is down — `true` also when there
/// was none. It waits for that ([`crate::account_host::unmounted_within`]): the
/// app erases the store once this replies, and the host otherwise notices the
/// capability is gone only at its next recheck.
pub(crate) async fn unprovision_now(state: &Arc<SyncServiceState>) -> bool {
    if let Some(store) = &state.credentials {
        crate::credentials::delete_capability(store);
    }
    *state.capability.write().await = None;
    // The refusal record described that capability's machine grant.
    crate::renewal::forget_refusal(state);
    crate::account_host::recheck_now(state);
    // The custodian replica goes with the capability: without this its stint
    // pulled on, every check-in refused, until the next rediscovery tick.
    crate::custodian::recheck_now(state);
    state.invalidate_nest_rpc().await;

    // The per-actor scope leaves with the capability — the next provision re-scopes
    // to its own actor. The in-memory config resets so a signed-out agent serves no
    // account's bindings; a (rare) mutation in the signed-out window goes to the flat
    // location and is re-pushed by the app's folder reconcile after the next
    // provision. macOS + windows + linux, on every layout (the `--data-dir`
    // override scopes too since 2026-09-26 — `service::apply_actor_scope`):
    // windows now sends UnprovisionCapability too (the consequence-1 teardown
    // glue, `SignOutHandler`/`SwitchAccountHandler` ->
    // `CapabilityProvisioner.UnprovisionAsync`), and linux's `sync_agent::teardown()`
    // always has (sign-out `main.rs:512`, account-switch `main.rs:675`), so this
    // clears on all three platforms' teardown paths.
    #[cfg(any(target_os = "macos", windows, target_os = "linux"))]
    {
        state.paths.set_actor_scope(None);
        *state.config.write().await = crate::config::SyncConfig::default();
    }

    tracing::info!("capability un-provisioned; stopping engines");
    // With the capability gone, reconcile serves no engines (same predicate the
    // pause path relies on).
    if let Err(e) = crate::engine_driver::reconcile_engines(state).await {
        tracing::warn!(error = %e, "engine teardown reconcile failed after un-provision");
    }

    let unmounted =
        crate::account_host::unmounted_within(state, crate::account_host::UNMOUNT_BUDGET).await;
    if !unmounted {
        tracing::warn!(
            budget = ?crate::account_host::UNMOUNT_BUDGET,
            "un-provision: the account store is still mounted; replying without the receipt"
        );
    }
    unmounted
}

// ── Shutdown ──

async fn handle_shutdown(state: &Arc<SyncServiceState>) -> Result<ResponsePayload, String> {
    // Wakeup-only — same channel, same argument as `run_agent`'s own
    // shutdown send in service.rs: every subscriber
    // is minted once at startup and outlives this call by construction.
    let _ = state.shutdown_tx.send(true);
    Ok(ResponsePayload::Empty)
}

// ── Pipe server (Windows only) ──

/// Windows single-instance guard — the named-mutex twin of unix's
/// kernel-arbitrated `fauna_ipc::unix_transport::InstanceLock` (flock-based).
///
/// Without it, a spawner racing a slow-binding agent (the convergence loop
/// re-probes on every tick/poke) can start a second agent process whose only
/// stop is `run_pipe_server`'s `CreateNamedPipeW` with
/// `FILE_FLAG_FIRST_PIPE_INSTANCE` rejecting the SECOND agent's pipe
/// *creation* — a later, louder exit than never starting engines at all.
/// This mutex makes the common same-session duplicate exit cleanly having
/// touched nothing.
///
/// A named kernel mutex, scoped to the per-user pipe name exactly like the
/// unix lock is scoped to the per-user socket path, is the natural windows
/// idiom: `CreateMutexW` either creates-and-owns a new object or opens the
/// existing one (surfaced via `ERROR_ALREADY_EXISTS`), and the OS releases it
/// automatically once the owning process's last handle closes — on a clean
/// exit or a crash alike — so there is no stale-lock state to reason about,
/// matching the unix flock's crash-safety.
///
/// ## Scope limit — `Local\` is per logon session, not per user
///
/// The mutex name lives in the `Local\` namespace, so it excludes duplicates
/// only *within one logon session*. The pipe and sync DB it guards are
/// per-user **machine-wide**, so a second concurrent logon session of the
/// same user (RDP + console — routine on Windows Pro/Server) acquires a
/// different `Local\` object and passes this gate. That duplicate is instead
/// stopped by the machine-global `FILE_FLAG_FIRST_PIPE_INSTANCE` create
/// failing, which `service::run_agent`'s run-loop tail observes and turns
/// into agent termination (pinned by `pipe_transport_integration::
/// duplicate_agent_in_second_logon_session_terminates_instead_of_degrading`).
/// `Global\` was considered and rejected: creating `Global\` objects needs
/// `SeCreateGlobalPrivilege`, which the standard-user identity this agent
/// runs as does not hold.
#[cfg(windows)]
#[derive(Debug)]
pub struct InstanceLock {
    handle: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl InstanceLock {
    /// Try to become the single agent instance for `pipe_name`.
    ///
    /// Returns `AddrInUse` when another live process already holds the mutex
    /// for this user — the caller should log and exit cleanly, mirroring the
    /// unix `InstanceLock::acquire` contract exactly (same error kind, same
    /// "first, before restoring the capability or starting engines" placement
    /// in `service::run_agent`).
    pub fn acquire(pipe_name: &str) -> std::io::Result<Self> {
        use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
        use windows::Win32::System::Threading::CreateMutexW;
        use windows::core::PCWSTR;

        let name = fauna_ipc::sync::mutex_name_for_pipe(pipe_name);
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

        unsafe {
            let handle = CreateMutexW(None, true, PCWSTR(wide.as_ptr()))
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            // `CreateMutexW` returns Ok (a handle to the EXISTING object) even
            // when one already existed — `ERROR_ALREADY_EXISTS` is the only
            // signal that we did not just create it, so it must be read
            // before any other Win32 call can overwrite the thread's
            // last-error slot.
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    format!("another sync-agent instance holds {name} — exiting as duplicate"),
                ));
            }
            Ok(Self { handle })
        }
    }
}

#[cfg(windows)]
impl Drop for InstanceLock {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
pub async fn run_pipe_server(
    state: Arc<SyncServiceState>,
    event_tx: tokio::sync::broadcast::Sender<fauna_ipc::sync::Event>,
    shutdown: tokio::sync::watch::Receiver<bool>,
    pipe_name: &str,
) -> anyhow::Result<()> {
    let handler = move |req: fauna_ipc::sync::Request| {
        let state = state.clone();
        async move { handle_request(&req, &state).await }
    };
    fauna_ipc::pipe_transport::serve(pipe_name, handler, shutdown, event_tx).await
}

#[cfg(all(test, windows))]
mod instance_lock_tests {
    use super::InstanceLock;

    /// tier_1: the single-instance guard. A second acquirer must fail while
    /// the first holds the lock — the named-mutex twin of the unix flock
    /// test (`fauna_ipc::unix_transport::instance_lock_excludes_second_acquirer_until_released`)
    /// — and must succeed once the first releases (drop == handle close, so a
    /// crashed agent never leaves a stale lock: the OS reclaims the named
    /// kernel object once the last handle to it closes).
    ///
    /// A pipe name keyed on this process's PID keeps concurrent `cargo test`
    /// runs on the same machine from colliding on the same named mutex.
    #[test]
    fn instance_lock_excludes_second_acquirer_until_released() {
        let pipe_name = format!(r"\\.\pipe\fauna-sync-test.{}", std::process::id());

        let first = InstanceLock::acquire(&pipe_name).expect("first acquire must succeed");
        let second = InstanceLock::acquire(&pipe_name);
        let err = second.expect_err("second acquire must fail while the first instance is live");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::AddrInUse,
            "duplicate → AddrInUse: {err}"
        );

        drop(first);
        InstanceLock::acquire(&pipe_name)
            .expect("acquire after release must succeed (crash leaves no stale lock)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_ipc::sync::{
        BearerToken, Request, RequestMethod, ResponsePayload, ResponseResult, SyncCapability,
    };

    fn test_state() -> Arc<SyncServiceState> {
        test_state_with_config(crate::config::SyncConfig::default())
    }

    // ── ShareFile: the Explorer Share hand-off's target (apps/windows.md
    //    § Shell Extension → The Share hand-off, step 1) ──

    #[test]
    fn share_target_offers_a_file_only_in_a_public_set() {
        let set = FolderRef::Local(42);
        assert_eq!(
            share_target(&set, "photos/a.jpg", false, true),
            Ok(ShareTargetInfo {
                folder_id: 42,
                path: Some("photos/a.jpg".into())
            })
        );
        assert!(
            share_target(&set, "photos/a.jpg", false, false).is_err(),
            "a private set's file is never a link target"
        );
    }

    #[test]
    fn share_target_offers_the_set_root_whatever_its_audience_but_never_a_sub_folder() {
        let set = FolderRef::Local(42);
        for public in [false, true] {
            assert_eq!(
                share_target(&set, "", true, public),
                Ok(ShareTargetInfo {
                    folder_id: 42,
                    path: None
                })
            );
            assert!(
                share_target(&set, "photos", true, public).is_err(),
                "a sub-folder would widen what the user picked"
            );
        }
    }

    #[test]
    fn share_target_refuses_a_cross_nest_set() {
        assert!(share_target(&FolderRef::Foreign([7; 32]), "", true, true).is_err());
        assert!(share_target(&FolderRef::Foreign([7; 32]), "a.jpg", false, true).is_err());
    }

    /// The handler end to end over real state: an unbound path hides the leaf,
    /// and a public set's file answers the target only once its engine has
    /// reported the arm — the entry the progress drain writes.
    #[tokio::test]
    async fn share_file_reads_the_engines_reported_audience() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("site");
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("index.jpg");
        std::fs::write(&file, b"x").unwrap();
        let state = test_state_with_config(crate::config::SyncConfig {
            locations: vec![crate::config::LocationConfig {
                path: root.to_str().unwrap().into(),
                mode: crate::config::LocationMode::OnDemand,
                folder: Some("site".into()),
                folder_id: Some(tref("site").to_wire()),
                ..Default::default()
            }],
            ..crate::config::SyncConfig::default()
        });
        let key = {
            let config = state.config.read().await;
            crate::path_map::resolve_to_folder_rel(&config, file.to_str().unwrap())
                .expect("resolves")
                .folder_ref
        };

        assert!(
            handle_share_file(&state, r"C:\elsewhere\x.jpg")
                .await
                .is_err()
        );
        assert!(
            handle_share_file(&state, file.to_str().unwrap())
                .await
                .is_err(),
            "no engine report yet — fail-closed"
        );
        state
            .public_audience
            .lock()
            .await
            .insert(key.to_wire(), true);
        match handle_share_file(&state, file.to_str().unwrap()).await {
            Ok(ResponsePayload::ShareTarget(t)) => assert_eq!(t.path.as_deref(), Some("index.jpg")),
            other => panic!("expected a file target, got {other:?}"),
        }
    }

    /// The fixtures' stable per-name identity: one set per name unless a test
    /// builds a same-named twin by hand.
    fn tref(folder: &str) -> FolderRef {
        FolderRef::Local(folder.bytes().fold(0i64, |acc, b| {
            acc.wrapping_mul(31).wrapping_add(i64::from(b))
        }))
    }

    /// A state holding one location bound to `folder` (under its [`tref`]) —
    /// what a name-addressed verb resolves its engine key through.
    fn test_state_bound_to(folder: &str) -> Arc<SyncServiceState> {
        test_state_with_config(crate::config::SyncConfig {
            locations: vec![crate::config::LocationConfig {
                path: format!("/data/{folder}"),
                folder: Some(folder.into()),
                folder_id: Some(tref(folder).to_wire()),
                ..Default::default()
            }],
            ..crate::config::SyncConfig::default()
        })
    }

    // ── the re-seed job (`RequestMethod::ReseedCustodianStore`) ─────────────

    fn reseed_state(payload: ResponsePayload) -> fauna_ipc::sync::CustodianReseedState {
        match payload {
            ResponsePayload::CustodianReseed(state) => state,
            other => panic!("expected CustodianReseed, got {other:?}"),
        }
    }

    fn reseed_request() -> fauna_ipc::sync::CustodianReseedRequest {
        fauna_ipc::sync::CustodianReseedRequest::new([7; 32])
    }

    /// A second press while a job runs starts nothing: the slot answers
    /// `Running`, and no second ceremony races the first over one store.
    #[tokio::test]
    async fn a_reseed_press_while_one_runs_starts_nothing() {
        use fauna_ipc::sync::CustodianReseedState;
        let state = test_state();
        *state.reseed_job.lock().await = CustodianReseedState::Running;
        let reply = handle_reseed_custodian_store(&state, &reseed_request())
            .await
            .unwrap();
        assert_eq!(reseed_state(reply), CustodianReseedState::Running);
    }

    /// The one moment deleting the store would destroy the copy being
    /// restored: a reclaim during a re-seed refuses and deletes nothing.
    #[tokio::test]
    async fn a_reclaim_during_a_reseed_refuses() {
        use fauna_ipc::sync::CustodianReseedState;
        // A temp data dir, never the default: a provisioned state names a real
        // store root, and this test must not be able to delete one.
        let dir = tempfile::tempdir().unwrap();
        let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let state = SyncServiceState::new(
            crate::config::SyncConfig::default(),
            shutdown_tx,
            event_tx,
            crate::config::SyncPaths::new(Some(dir.path().to_path_buf())),
        );
        let cap = SyncCapability::new(
            vec![9u8; 32],
            vec![0x42u8; 32],
            "https://nest.example".into(),
            "dev-test".into(),
            BearerToken::new("b".into(), 5),
        );
        *state.capability.write().await = Some(cap);
        *state.reseed_job.lock().await = CustodianReseedState::Running;
        match handle_reclaim_custodian_store(&state).await.unwrap() {
            ResponsePayload::CustodianStoreReclaimed(outcome) => {
                assert!(outcome.still_hosting, "a reclaim must refuse mid-re-seed");
                assert_eq!(outcome.freed_files, 0);
            }
            other => panic!("expected CustodianStoreReclaimed, got {other:?}"),
        }
    }

    /// An agent with no capability has no store to name and no nest to seed:
    /// the job ends `Failed` at the `store` phase, having sent nothing, and the
    /// read verb reports exactly that.
    #[tokio::test]
    async fn a_reseed_on_an_unprovisioned_agent_fails_at_the_store() {
        use fauna_ipc::sync::CustodianReseedState;
        let state = test_state();
        let started = handle_reseed_custodian_store(&state, &reseed_request())
            .await
            .unwrap();
        assert_eq!(reseed_state(started), CustodianReseedState::Running);
        // Wait on the job's own terminal state, never on a clock.
        let finished = loop {
            let now = state.reseed_job.lock().await.clone();
            if now != CustodianReseedState::Running {
                break now;
            }
            tokio::task::yield_now().await;
        };
        match finished {
            CustodianReseedState::Failed { phase, .. } => assert_eq!(phase, "store"),
            other => panic!("expected Failed at store, got {other:?}"),
        }
        let read = handle_request(
            &Request {
                id: 1,
                method: RequestMethod::GetCustodianReseed,
            },
            &state,
        )
        .await;
        match read.result {
            ResponseResult::Ok(payload) => assert!(matches!(
                reseed_state(payload),
                CustodianReseedState::Failed { .. }
            )),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    fn test_state_with_config(config: crate::config::SyncConfig) -> Arc<SyncServiceState> {
        let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        // None of these unit tests persist config, so the default (no-override)
        // paths are never written to disk.
        SyncServiceState::new(
            config,
            shutdown_tx,
            event_tx,
            crate::config::SyncPaths::new(None),
        )
    }

    /// The re-point seal mint (S8 D2), all four custody arms, exact bytes by
    /// recomputing each derivation: an owner-only set (resolved unbound) seals
    /// under the capability `BackupKey`'s owner root; a bound set with keys in
    /// custody seals under the M2 content root its roster holds; a
    /// bound-but-keyless set mints **nothing** — sealing under the owner key we
    /// do hold would be exactly the root no roster member could open; and a set the resolution does not name at all mints
    /// nothing for the same reason — nothing vouches it is owner-only.
    #[test]
    fn repoint_seal_follows_the_agents_custody_per_set() {
        let content = fauna_core::folder_keys::FolderContentKeys::genesis([4u8; 32], 1_000);
        let resolved = crate::content_keys::ResolvedContentKeys::for_tests(
            vec![
                fauna_core::folder_keys::FolderEngineKeys {
                    folder: "docs".into(),
                    folder_id: tref("docs").to_wire(),
                    ..Default::default()
                },
                fauna_core::folder_keys::FolderEngineKeys {
                    folder: "shared".into(),
                    folder_id: tref("shared").to_wire(),
                    mls_group_id: Some(b"gid".to_vec()),
                    content_keys: Some(content.clone()),
                    ..Default::default()
                },
                fauna_core::folder_keys::FolderEngineKeys {
                    folder: "keyless".into(),
                    folder_id: tref("keyless").to_wire(),
                    mls_group_id: Some(b"gid2".to_vec()),
                    ..Default::default()
                },
            ],
            &[],
        );
        let cap = SyncCapability::new(
            vec![7u8; 32],
            vec![1u8; 32],
            "https://nest.example".into(),
            "dd".repeat(32),
            BearerToken::new("tok".into(), 4_000_000_000),
        );

        let expect = |root: &fauna_core::path_crypto::LabelRoot| {
            fauna_core::path_crypto::seal_convergent(
                root,
                &fauna_core::sync::path_hash("2026/eviction_notice.pdf"),
                fauna_core::path_crypto::LabelField::SyncChangePath,
                "2026/eviction_notice.pdf".as_bytes(),
            )
            .unwrap()
            .to_bytes()
            .unwrap()
        };

        // Owner-only (resolved unbound) → the capability BackupKey's owner root.
        let owner_root = fauna_core::path_crypto::LabelRoot::owner_of(
            &fauna_core::crypto::BackupKey::from_bytes([7u8; 32]),
        );
        assert_eq!(
            seal_repoint_for(
                &cap,
                Some(&resolved),
                tref("docs"),
                "2026/eviction_notice.pdf"
            ),
            Some(expect(&owner_root))
        );

        // Bound with keys in custody → the content root, NOT the owner's.
        let content_root = fauna_core::path_crypto::LabelRoot::content_key(
            *content.current_key(),
            content.current_version(),
        );
        assert_eq!(
            seal_repoint_for(
                &cap,
                Some(&resolved),
                tref("shared"),
                "2026/eviction_notice.pdf"
            ),
            Some(expect(&content_root))
        );

        // Bound-but-keyless → fail closed to plaintext-only.
        assert_eq!(
            seal_repoint_for(
                &cap,
                Some(&resolved),
                tref("keyless"),
                "2026/eviction_notice.pdf"
            ),
            None
        );

        // Not in the resolution at all → withheld, plaintext-only: the owner root
        // is a guess no served/bound reader could open.
        assert_eq!(
            seal_repoint_for(
                &cap,
                Some(&resolved),
                tref("not-yet-resolved"),
                "2026/x.pdf"
            ),
            None
        );
        // Nothing resolved yet → the same.
        assert_eq!(
            seal_repoint_for(&cap, None, tref("docs"), "2026/x.pdf"),
            None
        );
    }

    /// TDD — `ListEngines` lists one row per *bound* folder (unbound folders are
    /// excluded), with `serving: false` when no engine is running yet (the unit state
    /// has no capability provisioned) and the mode as the shared wire string.
    #[tokio::test]
    async fn list_engines_lists_bound_sets_not_yet_serving() {
        let config = crate::config::SyncConfig {
            locations: vec![
                crate::config::LocationConfig {
                    path: "/data/docs".into(),
                    mode: crate::config::LocationMode::OnDemand,
                    folder: Some("documents".into()),
                    folder_id: Some(tref("documents").to_wire()),
                    ..Default::default()
                },
                // Unbound folder — excluded (the host never manufactures a set name).
                crate::config::LocationConfig {
                    path: "/data/loose".into(),
                    mode: crate::config::LocationMode::Always,
                    folder: None,
                    ..Default::default()
                },
            ],
            ..crate::config::SyncConfig::default()
        };
        let state = test_state_with_config(config);
        let resp = handle_request(
            &Request {
                id: 1,
                method: RequestMethod::ListEngines,
            },
            &state,
        )
        .await;
        match resp.result {
            ResponseResult::Ok(ResponsePayload::Engines(engines)) => {
                assert_eq!(engines.len(), 1, "only the bound set is listed");
                assert_eq!(engines[0].folder, "documents");
                assert_eq!(engines[0].mode, "on-demand");
                assert!(
                    !engines[0].serving,
                    "no capability provisioned → not serving"
                );
            }
            other => panic!("expected Engines, got {other:?}"),
        }
    }

    /// TDD — the three status handlers project the REAL per-set backlog from the
    /// bound sets' state DBs instead of hardcoding zeros: `files_pending` /
    /// `bytes_pending` are the display-transfer fold summed across sets,
    /// `last_sync` is the freshest engine stamp, and `ListEngines` carries the
    /// per-set split of the same numbers. `GetServiceStatus` and `GetSyncStatus`
    /// agree because they share one projection (and one meaning of `connected`:
    /// capability provisioned — none here, so `false` while the backlog is real).
    /// One set is ref-carrying, so the aggregate resolves the identity-keyed DB
    /// namespace (R1), not just the name-keyed one.
    #[tokio::test]
    async fn status_handlers_project_the_seeded_backlog() {
        use fauna_sync_engine::db::SyncState as S;

        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::config::SyncPaths::new(Some(tmp.path().to_path_buf()));
        let photos_ref = fauna_core::folder_keys::FolderRef::Local(9).to_wire();
        let config = crate::config::SyncConfig {
            locations: vec![
                crate::config::LocationConfig {
                    path: "/data/docs".into(),
                    mode: crate::config::LocationMode::Always,
                    folder: Some("documents".into()),
                    folder_id: Some(tref("documents").to_wire()),
                    ..Default::default()
                },
                crate::config::LocationConfig {
                    path: "/data/photos".into(),
                    mode: crate::config::LocationMode::OnDemand,
                    folder: Some("photos".into()),
                    folder_id: Some(photos_ref.clone()),
                    ..Default::default()
                },
            ],
            ..crate::config::SyncConfig::default()
        };

        // documents (name-keyed): two files mid-transfer (10 + 20 bytes), one
        // synced (99 — counted by the folder totals, never by the backlog). A
        // completed transfer stamps this set's last_sync.
        let docs_path = paths.sync_db_path_for_ref(tref("documents"));
        seed_row_at(&docs_path, "up.txt", S::LocallyModified, 10);
        seed_row_at(&docs_path, "down.txt", S::RemotelyModified, 20);
        seed_row_at(&docs_path, "done.txt", S::Synced, 99);
        fauna_sync_engine::db::SyncDb::open(&docs_path)
            .unwrap()
            .mark_transfer_completed()
            .unwrap();
        // photos (identity-keyed): one upload in flight (5 bytes), no transfer
        // ever completed.
        let photos_path = paths.sync_db_path_for_ref(FolderRef::parse(&photos_ref).unwrap());
        seed_row_at(&photos_path, "cat.jpg", S::Uploading, 5);

        let state = test_state_with_config_and_paths(config, paths);

        // GetSyncStatus — the cross-set aggregate.
        let resp = handle_request(
            &Request {
                id: 1,
                method: RequestMethod::GetSyncStatus,
            },
            &state,
        )
        .await;
        match resp.result {
            ResponseResult::Ok(ResponsePayload::SyncStatus(s)) => {
                assert!(!s.connected, "no capability provisioned");
                assert!(!s.syncing, "no engines running");
                assert_eq!(s.files_pending, 3, "2 documents + 1 photos");
                assert_eq!(s.bytes_pending, 35, "10 + 20 + 5");
                assert!(s.last_sync.is_some(), "documents stamped a transfer");
            }
            other => panic!("expected SyncStatus, got {other:?}"),
        }

        // GetServiceStatus — same projection, same numbers (one shared helper).
        let resp = handle_request(
            &Request {
                id: 2,
                method: RequestMethod::GetServiceStatus,
            },
            &state,
        )
        .await;
        match resp.result {
            ResponseResult::Ok(ResponsePayload::ServiceStatus(s)) => {
                assert_eq!(s.sync.files_pending, 3);
                assert_eq!(s.sync.bytes_pending, 35);
                assert!(s.sync.last_sync.is_some());
                assert!(!s.sync.connected);
            }
            other => panic!("expected ServiceStatus, got {other:?}"),
        }

        // ListEngines — the per-set split of the same fold.
        let resp = handle_request(
            &Request {
                id: 3,
                method: RequestMethod::ListEngines,
            },
            &state,
        )
        .await;
        match resp.result {
            ResponseResult::Ok(ResponsePayload::Engines(engines)) => {
                assert_eq!(engines.len(), 2);
                let docs = engines.iter().find(|e| e.folder == "documents").unwrap();
                assert_eq!((docs.files_pending, docs.bytes_pending), (2, 30));
                let photos = engines.iter().find(|e| e.folder == "photos").unwrap();
                assert_eq!((photos.files_pending, photos.bytes_pending), (1, 5));
            }
            other => panic!("expected Engines, got {other:?}"),
        }

        // ListLocations — tracked totals (all non-deleted rows, not just the
        // backlog): documents holds 3 files / 129 bytes, photos 1 / 5.
        let resp = handle_request(
            &Request {
                id: 4,
                method: RequestMethod::ListLocations,
            },
            &state,
        )
        .await;
        match resp.result {
            ResponseResult::Ok(ResponsePayload::Locations(folders)) => {
                let docs = folders.iter().find(|f| f.path == "/data/docs").unwrap();
                assert_eq!((docs.file_count, docs.total_bytes), (3, 129));
                let photos = folders.iter().find(|f| f.path == "/data/photos").unwrap();
                assert_eq!((photos.file_count, photos.total_bytes), (1, 5));
            }
            other => panic!("expected Locations, got {other:?}"),
        }
    }

    /// TDD — `ListEngines` carries the mass-delete floor's per-set verdict
    /// (`file-sync.md` § Files Appear Automatically → the captured follow-on:
    /// *"surfacing that count on the sync-agent status projection"*). Before
    /// this, a held set existed only as a warning in the agent log and a field
    /// on the `ReconcileStats` the resident loop discards, so no app could tell
    /// a user their folder had vanished.
    ///
    /// Both directions matter and both are asserted here: the set that reported
    /// a hold carries it, and the set beside it — same agent, same answer —
    /// reads 0. A projection that leaked one set's hold onto its siblings would
    /// tell a user their working folders had emptied too.
    #[tokio::test]
    async fn list_engines_carries_the_per_set_mass_delete_hold() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::config::SyncPaths::new(Some(tmp.path().to_path_buf()));
        let config = crate::config::SyncConfig {
            locations: vec![
                crate::config::LocationConfig {
                    path: "/data/docs".into(),
                    mode: crate::config::LocationMode::Always,
                    folder: Some("documents".into()),
                    folder_id: Some(tref("documents").to_wire()),
                    ..Default::default()
                },
                crate::config::LocationConfig {
                    path: "/data/photos".into(),
                    mode: crate::config::LocationMode::Always,
                    folder: Some("photos".into()),
                    folder_id: Some(tref("photos").to_wire()),
                    ..Default::default()
                },
            ],
            ..crate::config::SyncConfig::default()
        };
        let state = test_state_with_config_and_paths(config, paths);

        // Before any engine has reported: the honest nothing-observed-yet
        // reading, which is also what a restarted agent answers.
        let engines = list_engines_now(&state).await;
        assert!(
            engines.iter().all(|e| e.deletes_held == 0),
            "an agent that has observed no pass must not claim a hold"
        );

        // The documents folder's volume goes away; its engine's progress drain
        // records what the floor held (the write `drive_progress_notifications`
        // performs on `ProgressEvent::DeletesHeld`).
        state
            .deletes_held
            .lock()
            .await
            .insert(tref("documents").to_wire(), 3);

        let engines = list_engines_now(&state).await;
        let docs = engines.iter().find(|e| e.folder == "documents").unwrap();
        assert_eq!(
            docs.deletes_held, 3,
            "the held count reaches the projection"
        );
        let photos = engines.iter().find(|e| e.folder == "photos").unwrap();
        assert_eq!(
            photos.deletes_held, 0,
            "a hold is per-set — it must not bleed onto a healthy sibling"
        );

        // The drive is remounted: the next pass reports 0 and the surface
        // clears. Pinned because the hold is derived — nothing retracts it but
        // a newer report.
        state
            .deletes_held
            .lock()
            .await
            .insert(tref("documents").to_wire(), 0);
        let engines = list_engines_now(&state).await;
        let docs = engines.iter().find(|e| e.folder == "documents").unwrap();
        assert_eq!(docs.deletes_held, 0, "a zero report clears the surface");
    }

    /// The unreadable-path count reaches `ListEngines` per set, apart from the
    /// hold: a set whose subtree cannot be read reports it, a healthy sibling
    /// reads 0, and the hold field is untouched by it.
    #[tokio::test]
    async fn list_engines_carries_the_per_set_unreadable_count() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::config::SyncPaths::new(Some(tmp.path().to_path_buf()));
        let config = crate::config::SyncConfig {
            locations: vec![
                crate::config::LocationConfig {
                    path: "/data/docs".into(),
                    mode: crate::config::LocationMode::Always,
                    folder: Some("documents".into()),
                    folder_id: Some(tref("documents").to_wire()),
                    ..Default::default()
                },
                crate::config::LocationConfig {
                    path: "/data/photos".into(),
                    mode: crate::config::LocationMode::Always,
                    folder: Some("photos".into()),
                    folder_id: Some(tref("photos").to_wire()),
                    ..Default::default()
                },
            ],
            ..crate::config::SyncConfig::default()
        };
        let state = test_state_with_config_and_paths(config, paths);

        let engines = list_engines_now(&state).await;
        assert!(
            engines.iter().all(|e| e.deletes_skipped_unreadable == 0),
            "an agent that has observed no pass must not claim an unreadable path"
        );

        state
            .deletes_skipped_unreadable
            .lock()
            .await
            .insert(tref("photos").to_wire(), 7);
        let engines = list_engines_now(&state).await;
        let photos = engines.iter().find(|e| e.folder == "photos").unwrap();
        assert_eq!(photos.deletes_skipped_unreadable, 7);
        assert_eq!(
            photos.deletes_held, 0,
            "an unreadable path is not a hold — it must not surface as one"
        );
        let docs = engines.iter().find(|e| e.folder == "documents").unwrap();
        assert_eq!(docs.deletes_skipped_unreadable, 0, "per-set, no bleed");
    }

    /// `ListEngines` reduced to its rows, for tests that call it repeatedly.
    async fn list_engines_now(
        state: &std::sync::Arc<crate::state::SyncServiceState>,
    ) -> Vec<fauna_ipc::sync::EngineInfo> {
        let resp = handle_request(
            &Request {
                id: 1,
                method: RequestMethod::ListEngines,
            },
            state,
        )
        .await;
        match resp.result {
            ResponseResult::Ok(ResponsePayload::Engines(engines)) => engines,
            other => panic!("expected Engines, got {other:?}"),
        }
    }

    /// TDD — `ListEngines` carries each set's **post-succession corpus re-seal**
    /// progress, read from the same per-set DB the backlog comes from
    /// (`succession-aftermath.md` § Re-key scope: the re-seal is "surfaced with
    /// progress").
    ///
    /// ⚠ This is the pin the leg most needs on this side of the process
    /// boundary. The pass runs *here*, in the agent, and the line renders in the
    /// app — so if this handler stopped reading the record, every app-side test
    /// would stay green (they all build their own `EngineInfo` rows) while no
    /// user ever saw the line again. That is exactly the shape of the
    /// finding: a funnel nothing feeds passes every test that starts from the
    /// funnel.
    ///
    /// Both a set **with** a record and a set **without** one are asserted: the
    /// absent case is the ordinary state (a bound set never rests under an owner
    /// root, and no non-successor records anything), and mapping it to some
    /// stand-in value rather than `None` would put an aftermath line on every
    /// account in the fleet.
    #[tokio::test]
    async fn list_engines_carries_each_sets_corpus_reseal_progress() {
        use fauna_sync_engine::succession_progress::CorpusResealPass;

        let tmp = tempfile::tempdir().unwrap();
        let paths = crate::config::SyncPaths::new(Some(tmp.path().to_path_buf()));
        let config = crate::config::SyncConfig {
            locations: vec![
                crate::config::LocationConfig {
                    path: "/data/docs".into(),
                    mode: crate::config::LocationMode::Always,
                    folder: Some("documents".into()),
                    folder_id: Some(tref("documents").to_wire()),
                    ..Default::default()
                },
                crate::config::LocationConfig {
                    path: "/data/photos".into(),
                    mode: crate::config::LocationMode::OnDemand,
                    folder: Some("photos".into()),
                    folder_id: Some(tref("photos").to_wire()),
                    ..Default::default()
                },
            ],
            ..crate::config::SyncConfig::default()
        };

        // documents ran a pass that moved 7 files and left 2 owed; photos has
        // never run one.
        let docs_path = paths.sync_db_path_for_ref(tref("documents"));
        fauna_sync_engine::db::SyncDb::open(&docs_path)
            .unwrap()
            .record_corpus_reseal_pass(&CorpusResealPass::Settled {
                resealed: 7,
                owed: 2,
            })
            .unwrap();
        // Give photos a DB so the read genuinely happens and returns None,
        // rather than the whole open failing and hiding the mapping.
        fauna_sync_engine::db::SyncDb::open(paths.sync_db_path_for_ref(tref("photos"))).unwrap();

        let state = test_state_with_config_and_paths(config, paths);
        let resp = handle_request(
            &Request {
                id: 1,
                method: RequestMethod::ListEngines,
            },
            &state,
        )
        .await;
        match resp.result {
            ResponseResult::Ok(ResponsePayload::Engines(engines)) => {
                let docs = engines.iter().find(|e| e.folder == "documents").unwrap();
                assert_eq!(
                    docs.corpus_reseal,
                    Some(fauna_ipc::sync::CorpusResealInfo::Settled {
                        resealed: 7,
                        owed: 2
                    }),
                    "the recorded pass must reach the wire with its counts intact — \
                     they are what the app renders as progress"
                );
                let photos = engines.iter().find(|e| e.folder == "photos").unwrap();
                assert_eq!(
                    photos.corpus_reseal, None,
                    "a set that never ran a pass reports nothing, so an ordinary \
                     account renders no aftermath line at all"
                );
            }
            other => panic!("expected Engines, got {other:?}"),
        }
    }

    /// `PullFolderNow` routes the remote-change nudge to the registered
    /// per-engine wake channel (the IPC → `wake_senders` → engine link of
    /// `file-sync.md` § Remote-change nudge). A registered set is signalled
    /// exactly once; an unknown set is a silent best-effort no-op (the rescan
    /// tick is the backstop). Both return `Ok(Empty)`.
    #[tokio::test]
    async fn pull_folder_now_signals_the_registered_wake_channel() {
        let state = test_state_bound_to("documents");
        let (wake_tx, mut wake_rx) = tokio::sync::mpsc::channel::<()>(1);
        state
            .wake_senders
            .lock()
            .await
            .insert(tref("documents").to_wire(), wake_tx);

        // The registered set → its wake channel fires exactly once.
        let resp = handle_request(
            &Request {
                id: 1,
                method: RequestMethod::PullFolderNow {
                    folder: "documents".into(),
                    folder_hash: None,
                },
            },
            &state,
        )
        .await;
        assert!(matches!(
            resp.result,
            ResponseResult::Ok(ResponsePayload::Empty)
        ));
        assert_eq!(
            wake_rx.try_recv(),
            Ok(()),
            "the resident engine's wake channel was signalled"
        );

        // An unknown set is a best-effort no-op (still Ok) and signals nothing.
        let resp = handle_request(
            &Request {
                id: 2,
                method: RequestMethod::PullFolderNow {
                    folder: "no-such-set".into(),
                    folder_hash: None,
                },
            },
            &state,
        )
        .await;
        assert!(matches!(
            resp.result,
            ResponseResult::Ok(ResponsePayload::Empty)
        ));
        assert!(
            wake_rx.try_recv().is_err(),
            "no spurious signal for an unknown set"
        );
    }

    /// The restore's byte seam routes the re-seal of an inherited version to
    /// the set's running engine — the path, the historical manifest and the
    /// identity it was signed as, verbatim — and hands back the engine's
    /// answer; a set no engine runs for is refused with the inherited refusal,
    /// and an engine's refusal surfaces as its own words.
    #[tokio::test]
    async fn the_restore_byte_seam_routes_to_the_sets_running_engine() {
        use crate::versions::InheritedReseal;
        let state = test_state_bound_to("documents");
        let moving_bytes = std::sync::atomic::AtomicBool::new(false);
        let seam = EngineReseal {
            state: &state,
            folder_ref: tref("documents"),
            moving_bytes: &moving_bytes,
        };

        // No engine running for the set: no byte seam here.
        let err = seam
            .reseal("report.txt", [7; 32], Some([9; 32]))
            .await
            .unwrap_err();
        assert_eq!(err, crate::versions::RESTORE_INHERITED_REFUSED);

        let (cmd_tx, mut cmd_rx) =
            tokio::sync::mpsc::channel::<fauna_sync_engine::always_resident::EngineCommand>(2);
        state
            .engine_cmd_senders
            .lock()
            .await
            .insert(tref("documents").to_wire(), cmd_tx);
        let resealed = fauna_sync_engine::engine::ResealedVersion {
            manifest_hash: fauna_core::data::ContentHash::from_digest_raw([0xee; 32]),
            size_bytes: 5000,
        };
        // Stand in for the engine's loop: answer the first ask with a new
        // head, the second with a refusal.
        let responder = tokio::spawn(async move {
            for answer in [Ok(resealed), Err(anyhow::anyhow!("does not open"))] {
                let Some(
                    fauna_sync_engine::always_resident::EngineCommand::ResealInheritedVersion {
                        rel,
                        manifest_hash,
                        signed_as,
                        reply,
                    },
                ) = cmd_rx.recv().await
                else {
                    panic!("expected a ResealInheritedVersion command");
                };
                assert_eq!(
                    (rel.as_str(), manifest_hash, signed_as),
                    ("report.txt", [7; 32], Some([9; 32]))
                );
                let _ = reply.send(answer);
            }
        });

        assert_eq!(
            seam.reseal("report.txt", [7; 32], Some([9; 32])).await,
            Ok(resealed)
        );
        let err = seam
            .reseal("report.txt", [7; 32], Some([9; 32]))
            .await
            .unwrap_err();
        assert!(err.contains("does not open"), "got: {err}");
        responder.await.unwrap();
        assert!(
            !moving_bytes.load(std::sync::atomic::Ordering::Relaxed),
            "lowered once the engine answered"
        );
    }

    /// `ApplyHeldDeletes` routes the user-confirmed hold propagation to the
    /// registered per-engine command channel and maps the engine's verdict onto
    /// the wire (`delete-propagation.md` § the mass-delete floor). Unlike the
    /// nudge, this is invoke-and-reply — the caller renders the outcome.
    #[tokio::test]
    async fn apply_held_deletes_routes_to_the_registered_engine_and_replies() {
        let state = test_state_bound_to("documents");
        let (cmd_tx, mut cmd_rx) =
            tokio::sync::mpsc::channel::<fauna_sync_engine::always_resident::EngineCommand>(2);
        state
            .engine_cmd_senders
            .lock()
            .await
            .insert(tref("documents").to_wire(), cmd_tx);

        // Stand in for the resident loop: answer the one command with a canned
        // engine verdict, so the test pins the ROUTING + wire mapping.
        let responder = tokio::spawn(async move {
            let Some(fauna_sync_engine::always_resident::EngineCommand::ApplyHeldDeletes { reply }) =
                cmd_rx.recv().await
            else {
                panic!("expected an ApplyHeldDeletes command");
            };
            let _ = reply.send(Ok(fauna_sync_engine::engine::AppliedHeldDeletes {
                applied: 3,
                remaining_held: 1,
                floor_was_active: true,
            }));
        });

        let resp = handle_request(
            &Request {
                id: 1,
                method: RequestMethod::ApplyHeldDeletes {
                    folder: "documents".into(),
                },
            },
            &state,
        )
        .await;
        responder.await.unwrap();
        match resp.result {
            ResponseResult::Ok(ResponsePayload::HeldDeletesApplied(info)) => {
                assert_eq!(info.applied, 3);
                assert_eq!(
                    info.remaining_held, 1,
                    "partial-with-retry survives the wire"
                );
                assert!(info.floor_was_active);
            }
            other => panic!("expected HeldDeletesApplied, got {other:?}"),
        }
    }

    /// routing: the engine registry keys by ref, so a name-addressed
    /// verb resolves through this device's bindings — a bound location
    /// resolves to its ref, a name worn by SEVERAL locations is refused (a
    /// bare-name lookup would reach whichever same-named engine registered
    /// last), and a name no binding wears resolves to nothing — never to the
    /// bare name as a key (retired 2026-09-24 with the name-keyed binding).
    #[tokio::test]
    async fn engine_routing_resolves_through_the_bindings_and_refuses_an_ambiguous_name() {
        let mut config = crate::config::SyncConfig::default();
        config.locations.push(crate::config::LocationConfig {
            path: "/one".into(),
            folder: Some("docs".into()),
            folder_id: Some(FolderRef::Local(1).to_wire()),
            ..Default::default()
        });
        // A label with no ref is not a binding and resolves nothing.
        config.locations.push(crate::config::LocationConfig {
            path: "/label-only".into(),
            folder: Some("other".into()),
            ..Default::default()
        });
        let state = test_state_with_config(config.clone());

        assert_eq!(
            resolve_engine_key(&state, "docs").await.unwrap(),
            Some(FolderRef::Local(1).to_wire())
        );
        assert_eq!(
            resolve_engine_key(&state, "other").await.unwrap(),
            None,
            "no binding wears the name → not served, never the bare name as a key"
        );

        // A second same-named location makes the name unroutable.
        config.locations.push(crate::config::LocationConfig {
            path: "/two".into(),
            folder: Some("docs".into()),
            folder_id: Some(FolderRef::Local(2).to_wire()),
            ..Default::default()
        });
        let state = test_state_with_config(config);
        let err = resolve_engine_key(&state, "docs").await.unwrap_err();
        assert!(err.contains("ambiguous"), "{err}");
    }

    /// A sealed set's nudge carries no plaintext name — only the push's hash
    /// address — and still resolves to the binding whose name hashes to it;
    /// the hash wins over whatever `folder` says.
    #[tokio::test]
    async fn a_hash_addressed_nudge_resolves_the_binding_whose_name_hashes_to_it() {
        let mut config = crate::config::SyncConfig::default();
        config.locations.push(crate::config::LocationConfig {
            path: "/one".into(),
            folder: Some("Holiday Photos".into()),
            folder_id: Some(FolderRef::Local(1).to_wire()),
            ..Default::default()
        });
        let state = test_state_with_config(config);
        let hash = fauna_core::path_crypto::set_name_hash("Holiday Photos");
        assert_eq!(
            resolve_engine_key_at(&state, "", Some(&hash))
                .await
                .unwrap(),
            Some(FolderRef::Local(1).to_wire())
        );
        let other = fauna_core::path_crypto::set_name_hash("Work");
        assert_eq!(
            resolve_engine_key_at(&state, "Holiday Photos", Some(&other))
                .await
                .unwrap(),
            None,
            "a hash no binding's name produces names nothing, whatever the name says"
        );
    }

    /// A folder with no resident engine ERRORS — never a silent drop. A dropped
    /// confirm would read downstream as "the apply happened" (the convention-11
    /// disguise); the nudge may drop because the rescan tick is its backstop,
    /// but nothing retries a user's confirm.
    #[tokio::test]
    async fn apply_held_deletes_errors_when_no_resident_engine_serves_the_set() {
        let state = test_state();
        let resp = handle_request(
            &Request {
                id: 1,
                method: RequestMethod::ApplyHeldDeletes {
                    folder: "no-such-set".into(),
                },
            },
            &state,
        )
        .await;
        match resp.result {
            ResponseResult::Err(msg) => assert!(
                msg.contains("not being served"),
                "the refusal names the condition: {msg}"
            ),
            other => panic!("expected an error, got {other:?}"),
        }
    }

    /// TDD — `Pause`/`Resume` persist the device-global flag to config (and reply
    /// `Empty`), so a paused agent stays paused across a restart. Uses an override
    /// data-root so the save actually hits a temp `config.toml`.
    #[tokio::test]
    async fn pause_resume_persist_the_flag() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = crate::config::SyncPaths::new(Some(tmp.path().to_path_buf()));
        let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let state = SyncServiceState::new(
            crate::config::SyncConfig::default(),
            shutdown_tx,
            event_tx,
            paths.clone(),
        );

        let resp = handle_request(
            &Request {
                id: 1,
                method: RequestMethod::Pause,
            },
            &state,
        )
        .await;
        assert!(matches!(
            resp.result,
            ResponseResult::Ok(ResponsePayload::Empty)
        ));
        assert!(state.config.read().await.paused, "in-memory paused set");
        assert!(
            paths.load_config().unwrap().paused,
            "paused persisted to config.toml"
        );

        let resp = handle_request(
            &Request {
                id: 2,
                method: RequestMethod::Resume,
            },
            &state,
        )
        .await;
        assert!(matches!(
            resp.result,
            ResponseResult::Ok(ResponsePayload::Empty)
        ));
        assert!(!state.config.read().await.paused, "resume clears in-memory");
        assert!(
            !paths.load_config().unwrap().paused,
            "resume persisted to config.toml"
        );
    }

    /// The retired `GetBackupStatus` answers cleanly with `[]` — never an
    /// error — until the op's codec variants are deleted (D6 withdrawn;
    /// status is the nest's projection).
    #[tokio::test]
    async fn get_backup_status_is_empty_not_an_error() {
        let state = test_state();
        let resp = handle_request(
            &Request {
                id: 1,
                method: RequestMethod::GetBackupStatus,
            },
            &state,
        )
        .await;
        match resp.result {
            ResponseResult::Ok(ResponsePayload::BackupStatus(rows)) => assert!(rows.is_empty()),
            other => panic!("expected BackupStatus, got {other:?}"),
        }
    }

    /// TDD — status handlers derive "connected" from the provisioned capability, NOT device_config.
    ///
    /// RED (pre-migration): handlers read `state.device_config.is_some()`, which is always
    /// `None` under the per-user model → always `Disconnected` even with a capability.
    /// GREEN (post-migration): handlers read `state.capability.is_some()`.
    #[tokio::test]
    async fn service_status_connected_iff_capability_provisioned() {
        let state = test_state();

        // No capability provisioned → Disconnected.
        let req = Request {
            id: 1,
            method: RequestMethod::GetServiceStatus,
        };
        let resp = handle_request(&req, &state).await;
        match resp.result {
            ResponseResult::Ok(ResponsePayload::ServiceStatus(info)) => {
                assert_eq!(
                    info.connection,
                    ConnectionState::Disconnected,
                    "no capability → Disconnected"
                );
            }
            other => panic!("expected ServiceStatus, got {other:?}"),
        }

        // Provision a capability → Connected.
        let cap = SyncCapability::new(
            vec![1u8; 32],
            vec![2u8; 32],
            "https://nest.example".into(),
            "d1".into(),
            BearerToken::new("tok".into(), 4_000_000_000),
        );
        *state.capability.write().await = Some(cap);

        let req2 = Request {
            id: 2,
            method: RequestMethod::GetServiceStatus,
        };
        let resp2 = handle_request(&req2, &state).await;
        match resp2.result {
            ResponseResult::Ok(ResponsePayload::ServiceStatus(info)) => {
                assert_eq!(
                    info.connection,
                    ConnectionState::Connected,
                    "provisioned capability → Connected"
                );
            }
            other => panic!("expected ServiceStatus, got {other:?}"),
        }
    }

    /// The status reply reports the on-demand surface as the boot probe answered
    /// it, and "cannot tell" until it has — a field read, never the probe itself.
    #[tokio::test]
    async fn service_status_reports_the_on_demand_probe() {
        async fn on_demand(state: &Arc<SyncServiceState>) -> (Option<bool>, Option<String>) {
            let req = Request {
                id: 1,
                method: RequestMethod::GetServiceStatus,
            };
            match handle_request(&req, state).await.result {
                ResponseResult::Ok(ResponsePayload::ServiceStatus(info)) => {
                    (info.on_demand_available, info.on_demand_unavailable_reason)
                }
                other => panic!("expected ServiceStatus, got {other:?}"),
            }
        }

        let unprobed = test_state();
        assert_eq!(on_demand(&unprobed).await, (None, None), "not probed yet");

        let available = test_state();
        available.on_demand.set(Ok(())).unwrap();
        assert_eq!(on_demand(&available).await, (Some(true), None));

        let unavailable = test_state();
        unavailable
            .on_demand
            .set(Err(fauna_ipc::sync::ON_DEMAND_REASON_NO_FUSERMOUNT))
            .unwrap();
        assert_eq!(
            on_demand(&unavailable).await,
            (
                Some(false),
                Some(fauna_ipc::sync::ON_DEMAND_REASON_NO_FUSERMOUNT.to_string())
            )
        );
    }

    /// TDD — GetSyncStatus `connected` field reflects capability presence, NOT device_config.
    #[tokio::test]
    async fn sync_status_connected_iff_capability_provisioned() {
        let state = test_state();

        let req = Request {
            id: 1,
            method: RequestMethod::GetSyncStatus,
        };
        let resp = handle_request(&req, &state).await;
        match resp.result {
            ResponseResult::Ok(ResponsePayload::SyncStatus(info)) => {
                assert!(!info.connected, "no capability → not connected");
            }
            other => panic!("expected SyncStatus, got {other:?}"),
        }

        let cap = SyncCapability::new(
            vec![3u8; 32],
            vec![4u8; 32],
            "https://nest.example".into(),
            "d1".into(),
            BearerToken::new("tok".into(), 4_000_000_000),
        );
        *state.capability.write().await = Some(cap);

        let req2 = Request {
            id: 2,
            method: RequestMethod::GetSyncStatus,
        };
        let resp2 = handle_request(&req2, &state).await;
        match resp2.result {
            ResponseResult::Ok(ResponsePayload::SyncStatus(info)) => {
                assert!(info.connected, "provisioned capability → connected");
            }
            other => panic!("expected SyncStatus, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_locations_reports_bound_folder() {
        use crate::config::{LocationConfig, LocationMode, SyncConfig};
        // The Sync-folders page renders each row's bound folder (ui.yaml
        // § sync `folder-location-fileset`); `ListLocations` must therefore carry
        // the device-local folder↔folder binding, not just path + mode.
        let mut config = SyncConfig::default();
        config.locations.push(LocationConfig {
            path: r"C:\Users\alice\Docs".to_string(),
            mode: LocationMode::OnDemand,
            folder: Some("documents".to_string()),
            folder_id: Some(tref("documents").to_wire()),
            ..Default::default()
        });
        config.locations.push(LocationConfig {
            path: r"D:\Scratch".to_string(),
            mode: LocationMode::Always,
            folder: None,
            ..Default::default()
        });
        let state = test_state_with_config(config);

        let req = Request {
            id: 1,
            method: RequestMethod::ListLocations,
        };
        let resp = handle_request(&req, &state).await;
        let folders = match resp.result {
            ResponseResult::Ok(ResponsePayload::Locations(f)) => f,
            other => panic!("expected Locations payload, got {other:?}"),
        };
        assert_eq!(folders.len(), 2);
        let docs = folders
            .iter()
            .find(|f| f.path == r"C:\Users\alice\Docs")
            .expect("docs folder present");
        assert_eq!(docs.folder.as_deref(), Some("documents"));
        assert_eq!(
            docs.folder_id,
            Some(tref("documents").to_wire()),
            "the binding's ref rides beside its label"
        );
        let scratch = folders
            .iter()
            .find(|f| f.path == r"D:\Scratch")
            .expect("scratch folder present");
        assert_eq!(scratch.folder, None, "unbound folder reports None");
        assert_eq!(scratch.folder_id, None);
    }

    /// A location carrying a label but no ref — the retired name-keyed binding
    /// — is reported UNBOUND, label included: an app must never render (or
    /// adopt) a binding no engine, key lookup or state DB can resolve.
    #[tokio::test]
    async fn list_locations_reports_a_label_without_a_ref_as_unbound() {
        let mut config = crate::config::SyncConfig::default();
        config.locations.push(crate::config::LocationConfig {
            path: "/label-only".into(),
            folder: Some("documents".into()),
            ..Default::default()
        });
        let state = test_state_with_config(config);
        let resp = handle_request(
            &Request {
                id: 1,
                method: RequestMethod::ListLocations,
            },
            &state,
        )
        .await;
        let ResponseResult::Ok(ResponsePayload::Locations(rows)) = resp.result else {
            panic!("expected Locations");
        };
        assert_eq!(rows[0].folder, None);
        assert_eq!(rows[0].folder_id, None);
    }

    /// `AddLocation` starts a FRESH path in the platform default mode —
    /// on-demand on windows, always-resident elsewhere
    /// (`LocationMode::fresh_binding_default`; user ruling 2026-09-26) — and
    /// re-adding a path that already carries a persisted mode changes nothing:
    /// the default is for new binds only, never a migration. Pinned here, on
    /// the agent, because the windows e2e witness
    /// (`test_folder_location_mode_toggle.py`) runs against the app's in-memory
    /// channel and so proves the app's default, not the agent's.
    #[tokio::test]
    async fn add_location_starts_a_fresh_path_in_the_platform_default_mode() {
        use crate::config::{LocationConfig, LocationMode, SyncConfig};
        let mut config = SyncConfig::default();
        // A pre-existing binding in the NON-default mode: re-adding it must not
        // flip it to the fresh-binding default.
        let kept = if cfg!(windows) {
            LocationMode::Always
        } else {
            LocationMode::OnDemand
        };
        config.locations.push(LocationConfig {
            path: "/data/kept".into(),
            mode: kept.clone(),
            ..Default::default()
        });
        let state = test_state_with_config(config);

        for (id, path) in [(1, "/data/fresh"), (2, "/data/kept")] {
            let resp = handle_request(
                &Request {
                    id,
                    method: RequestMethod::AddLocation { path: path.into() },
                },
                &state,
            )
            .await;
            assert!(
                matches!(resp.result, ResponseResult::Ok(ResponsePayload::Empty)),
                "AddLocation {path} must succeed: {resp:?}"
            );
        }

        let config = state.config.read().await;
        let fresh = config
            .find_location("/data/fresh")
            .expect("fresh path recorded");
        assert_eq!(
            fresh.mode,
            LocationMode::fresh_binding_default(),
            "a fresh path takes the platform default"
        );
        let expected_wire = if cfg!(windows) { "on-demand" } else { "always" };
        assert_eq!(
            fresh.mode.wire_str(),
            expected_wire,
            "windows binds on-demand by default; every other platform always-resident"
        );
        let kept_row = config
            .find_location("/data/kept")
            .expect("kept path still recorded");
        assert_eq!(
            kept_row.mode, kept,
            "re-adding an existing path keeps its persisted mode"
        );
        assert_eq!(
            config.locations.len(),
            2,
            "re-adding never duplicates a row"
        );
    }

    /// The one bind verb refuses a `folder_id` that is not a `FolderRef`,
    /// before touching the config: a binding no engine could resolve is never
    /// recorded.
    #[tokio::test]
    async fn set_location_folder_refuses_an_unparseable_ref() {
        let mut config = crate::config::SyncConfig::default();
        config.locations.push(crate::config::LocationConfig {
            path: "/d".into(),
            ..Default::default()
        });
        let state = test_state_with_config(config);
        let resp = handle_request(
            &Request {
                id: 1,
                method: RequestMethod::SetLocationFolder {
                    path: "/d".into(),
                    folder: "docs".into(),
                    folder_id: "docs".into(),
                },
            },
            &state,
        )
        .await;
        assert!(
            matches!(&resp.result, ResponseResult::Err(e) if e.contains("not a folder id")),
            "{:?}",
            resp.result
        );
        let config = state.config.read().await;
        assert_eq!(config.locations[0].folder, None, "nothing was recorded");
        assert_eq!(config.locations[0].folder_id, None);
    }

    #[tokio::test]
    async fn provision_capability_stores_in_state() {
        let state = test_state();
        let cap = SyncCapability::new(
            vec![9u8; 32],
            vec![0x42u8; 32],
            "https://nest.example".into(),
            "dev-test".into(),
            BearerToken::new("b".into(), 5),
        );
        let req = Request {
            id: 1,
            method: RequestMethod::ProvisionCapability(cap),
        };
        let resp = handle_request(&req, &state).await;
        assert!(
            matches!(resp.result, ResponseResult::Ok(ResponsePayload::Empty)),
            "expected Ok(Empty), got {:?}",
            resp.result
        );
        let guard = state.capability.read().await;
        let stored = guard
            .as_ref()
            .expect("capability should be Some after provision");
        assert_eq!(stored.backup_key_array(), Some([9u8; 32]));
        assert_eq!(stored.actor_id_array(), Some([0x42u8; 32]));
        assert_eq!(stored.bearer.token, "b");
        assert_eq!(stored.bearer.expires_at, 5);
    }

    /// The A2 persistence loop, over a file-backend store (never the OS
    /// keychain): provision persists, refresh re-persists the new bearer, a
    /// fresh boot reloads it, and un-provision deletes the record and clears
    /// the slot — the mandate's own cycle (sync-agent.md § Credential model).
    #[tokio::test]
    async fn capability_persists_reloads_and_unprovisions() {
        let dir = std::env::temp_dir().join(format!("sync-agent-persist-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store =
            std::sync::Arc::new(fauna_credential_store::CredentialStore::with_file_backend(
                "fauna-sync-agent-test",
                dir.clone(),
            ));
        let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let state = SyncServiceState::new_with_credentials(
            crate::config::SyncConfig::default(),
            shutdown_tx,
            event_tx,
            crate::config::SyncPaths::new(None),
            Some(store.clone()),
        );

        // Provision → persisted.
        let cap = SyncCapability::new(
            vec![9u8; 32],
            vec![0x42u8; 32],
            "https://nest.example".into(),
            "dev-test".into(),
            BearerToken::new("tok-1".into(), 5),
        );
        let resp = handle_request(
            &Request {
                id: 1,
                method: RequestMethod::ProvisionCapability(cap),
            },
            &state,
        )
        .await;
        assert!(matches!(
            resp.result,
            ResponseResult::Ok(ResponsePayload::Empty)
        ));
        let persisted =
            crate::credentials::load_capability(&store).expect("provision persists the capability");
        assert_eq!(persisted.bearer.token, "tok-1");

        // Refresh → the persisted record carries the NEW bearer (an app-dead
        // reboot must resume fresh).
        let resp = handle_request(
            &Request {
                id: 2,
                method: RequestMethod::RefreshBearer(BearerToken::new("tok-2".into(), 99)),
            },
            &state,
        )
        .await;
        assert!(matches!(
            resp.result,
            ResponseResult::Ok(ResponsePayload::Empty)
        ));
        let persisted = crate::credentials::load_capability(&store).unwrap();
        assert_eq!(persisted.bearer.token, "tok-2");
        assert_eq!(persisted.bearer.expires_at, 99);

        // Un-provision → record deleted, slot cleared. Idempotent on re-run.
        for id in [3, 4] {
            let resp = handle_request(
                &Request {
                    id,
                    method: RequestMethod::UnprovisionCapability,
                },
                &state,
            )
            .await;
            assert!(
                matches!(resp.result, ResponseResult::Ok(ResponsePayload::Empty)),
                "un-provision (id {id}) must succeed, got {:?}",
                resp.result
            );
        }
        assert!(crate::credentials::load_capability(&store).is_none());
        assert!(state.capability.read().await.is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The per-actor scope on the `--data-dir` override, end to end through the
    /// handlers: a provision scopes the paths and the binding store lands under
    /// `<root>/<actor>/`; an un-provision clears the scope and the in-memory
    /// config while the scoped file survives on disk for that account's next
    /// sign-in; the same actor's re-provision reloads its bindings; a DIFFERENT
    /// actor's provision sees none of them. Until 2026-09-26 the override stayed
    /// flat, so every account an e2e module signed in shared one `config.toml`
    /// and one `fsid-local-N.db` — two fresh nests both mint `local:N`, so a
    /// later account inherited the earlier one's pull anchor and skipped its own
    /// nest's first changes.
    #[cfg(any(target_os = "macos", windows, target_os = "linux"))]
    #[tokio::test]
    async fn data_dir_override_scopes_bindings_per_actor_across_provisions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().to_path_buf();
        let state = test_state_with_config_and_paths(
            crate::config::SyncConfig::default(),
            crate::config::SyncPaths::new(Some(root.clone())),
        );
        let location = root.join("docs-a");
        std::fs::create_dir_all(&location).unwrap();
        let location = location.to_string_lossy().into_owned();

        fn cap_for(actor: u8) -> SyncCapability {
            SyncCapability::new(
                vec![actor.wrapping_add(0x40); 32],
                vec![actor; 32],
                "https://nest.example".into(),
                hex::encode([actor; 32]),
                BearerToken::new(format!("tok-{actor}"), 9_999),
            )
        }
        async fn ok(state: &Arc<SyncServiceState>, id: u64, method: RequestMethod) {
            let resp = handle_request(&Request { id, method }, state).await;
            assert!(
                matches!(resp.result, ResponseResult::Ok(ResponsePayload::Empty)),
                "request {id} must succeed, got {:?}",
                resp.result
            );
        }

        // Actor A provisions: the paths scope under the override root.
        let hex_a = hex::encode([0xa1u8; 32]);
        ok(&state, 1, RequestMethod::ProvisionCapability(cap_for(0xa1))).await;
        assert_eq!(state.paths.actor_scope().as_deref(), Some(hex_a.as_str()));
        assert_eq!(
            state.paths.config_path(),
            root.join(&hex_a).join("config.toml")
        );

        // A's binding lands in A's scoped store, never flat at the root.
        ok(
            &state,
            2,
            RequestMethod::AddLocation {
                path: location.clone(),
            },
        )
        .await;
        assert!(root.join(&hex_a).join("config.toml").is_file());
        assert!(
            !root.join("config.toml").exists(),
            "nothing is written flat at the override root once scoped"
        );

        // Un-provision: scope and in-memory config clear; the file stays.
        ok(&state, 3, RequestMethod::UnprovisionCapability).await;
        assert_eq!(state.paths.actor_scope(), None);
        assert!(state.config.read().await.locations.is_empty());
        assert!(root.join(&hex_a).join("config.toml").is_file());

        // A signs back in: its binding comes back from its own store.
        ok(&state, 4, RequestMethod::ProvisionCapability(cap_for(0xa1))).await;
        let paths: Vec<String> = state
            .config
            .read()
            .await
            .locations
            .iter()
            .map(|l| l.path.clone())
            .collect();
        assert_eq!(paths, vec![location.clone()]);

        // Actor B provisions (the windows switch shape — no un-provision
        // first): B's store is its own and holds none of A's bindings.
        let hex_b = hex::encode([0xb2u8; 32]);
        ok(&state, 5, RequestMethod::ProvisionCapability(cap_for(0xb2))).await;
        assert_eq!(state.paths.actor_scope().as_deref(), Some(hex_b.as_str()));
        assert!(
            state.config.read().await.locations.is_empty(),
            "B must not inherit A's bindings"
        );
        assert!(root.join(&hex_a).join("config.toml").is_file());
    }

    /// The mandate's own test (sync-agent.md § Credential model; plan D8): a **fresh
    /// agent process — no app, no re-provision — resumes from the persisted
    /// capability**. Distinct from `capability_persists_reloads_and_unprovisions`,
    /// which reuses one state; here the provisioning state (and its store handle) is
    /// dropped *entirely* before a brand-new `SyncServiceState` runs the app-dead
    /// boot-reload path `service::run_agent` runs (load → publish → reconcile). This is
    /// the state-level half of the D8 harness — it proves the resume decision without
    /// a live nest; the full-stack socket/binary round-trip (engines actually serving
    /// bytes) is the queued unix tier_3 leg.
    #[tokio::test]
    async fn fresh_agent_resumes_from_persisted_capability_with_no_app() {
        let dir = std::env::temp_dir().join(format!("sync-agent-resume-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        // ── Agent process #1: provision, then "die" (drop state + store handle). ──
        {
            let store =
                std::sync::Arc::new(fauna_credential_store::CredentialStore::with_file_backend(
                    "fauna-sync-agent-test",
                    dir.clone(),
                ));
            let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
            let (event_tx, _) = tokio::sync::broadcast::channel(16);
            let state = SyncServiceState::new_with_credentials(
                crate::config::SyncConfig::default(),
                shutdown_tx,
                event_tx,
                crate::config::SyncPaths::new(None),
                Some(store.clone()),
            );
            let cap = SyncCapability::new(
                vec![9u8; 32],
                vec![0x42u8; 32],
                "https://nest.example".into(),
                "dev-resume".into(),
                BearerToken::new("boot-tok".into(), 4242),
            );
            let resp = handle_request(
                &Request {
                    id: 1,
                    method: RequestMethod::ProvisionCapability(cap),
                },
                &state,
            )
            .await;
            assert!(matches!(
                resp.result,
                ResponseResult::Ok(ResponsePayload::Empty)
            ));
            // `state` + `store` drop here — the agent process is gone; only the
            // on-disk credential-store record survives.
        }

        // ── Agent process #2: a brand-new state over the SAME store, NO provision. ──
        let store =
            std::sync::Arc::new(fauna_credential_store::CredentialStore::with_file_backend(
                "fauna-sync-agent-test",
                dir.clone(),
            ));
        let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        let state = SyncServiceState::new_with_credentials(
            crate::config::SyncConfig::default(),
            shutdown_tx,
            event_tx,
            crate::config::SyncPaths::new(None),
            Some(store.clone()),
        );
        // A fresh state's slot is empty until the boot-reload runs — nothing has
        // pushed a capability into this process.
        assert!(
            state.capability.read().await.is_none(),
            "a fresh agent state has no capability before boot-reload"
        );

        // The app-dead boot-reload path (`service::run_agent`): load the persisted
        // record → publish it to the slot → reconcile engines. With no bound folders
        // reconcile is a no-op (no nest contacted), so the resume decision is proven
        // deterministically.
        let restored = crate::credentials::load_capability(&store)
            .expect("a fresh agent must find the persisted capability in the store");
        *state.capability.write().await = Some(restored);
        crate::engine_driver::reconcile_engines(&state)
            .await
            .expect("reconcile after restore must succeed (no folders → no-op)");

        // Resumed: the capability is live in the fresh process with no app and no
        // re-provision — including the bearer the app-dead self-renewal loop
        // plans on (`renewal::run`).
        let live = state.capability.read().await;
        let cap = live
            .as_ref()
            .expect("capability resumed from the store after restart");
        assert_eq!(cap.bearer.token, "boot-tok");
        assert_eq!(cap.bearer.expires_at, 4242);
        assert_eq!(cap.nest_url, "https://nest.example");
        assert_eq!(cap.device_id, "dev-resume");
        drop(live);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn provision_capability_rejects_wrong_key_length() {
        let state = test_state();
        // Valid 32-byte actor_id so the backup_key length is the rejected field.
        let cap = SyncCapability::new(
            vec![0u8; 16],
            vec![0u8; 32],
            "https://nest.example".into(),
            "dev-test".into(),
            BearerToken::new("b".into(), 4_000_000_000),
        );
        let req = Request {
            id: 2,
            method: RequestMethod::ProvisionCapability(cap),
        };
        let resp = handle_request(&req, &state).await;
        assert!(
            matches!(resp.result, ResponseResult::Err(_)),
            "expected Err for wrong key length, got {:?}",
            resp.result
        );
        let guard = state.capability.read().await;
        assert!(
            guard.is_none(),
            "capability should still be None after rejected provision"
        );
    }

    #[tokio::test]
    async fn refresh_bearer_updates_existing() {
        let state = test_state();
        // First provision
        let cap = SyncCapability::new(
            vec![9u8; 32],
            vec![0x42u8; 32],
            "https://nest.example".into(),
            "dev-test".into(),
            BearerToken::new("b".into(), 5),
        );
        let provision_req = Request {
            id: 3,
            method: RequestMethod::ProvisionCapability(cap),
        };
        let provision_resp = handle_request(&provision_req, &state).await;
        assert!(matches!(provision_resp.result, ResponseResult::Ok(_)));

        // Now refresh the bearer
        let refresh_req = Request {
            id: 4,
            method: RequestMethod::RefreshBearer(BearerToken::new("b2".into(), 9)),
        };
        let refresh_resp = handle_request(&refresh_req, &state).await;
        assert!(
            matches!(
                refresh_resp.result,
                ResponseResult::Ok(ResponsePayload::Empty)
            ),
            "expected Ok(Empty), got {:?}",
            refresh_resp.result
        );

        let guard = state.capability.read().await;
        let stored = guard.as_ref().expect("capability should still be Some");
        assert_eq!(stored.bearer.token, "b2");
        assert_eq!(stored.bearer.expires_at, 9);
        // Key must be unchanged
        assert_eq!(stored.backup_key_array(), Some([9u8; 32]));
    }

    #[tokio::test]
    async fn refresh_bearer_without_capability_errors() {
        let state = test_state();
        let req = Request {
            id: 5,
            method: RequestMethod::RefreshBearer(BearerToken::new("x".into(), 4_000_000_000)),
        };
        let resp = handle_request(&req, &state).await;
        match &resp.result {
            // The prefix is matched by the app's HydrationSessionService as its
            // full-re-provision trigger (see handle_refresh_bearer) — pin it.
            ResponseResult::Err(msg) => assert!(
                msg.starts_with("no capability provisioned"),
                "error must keep the app-matched prefix, got {msg:?}"
            ),
            other => panic!("expected Err when no capability provisioned, got {other:?}"),
        }
        let guard = state.capability.read().await;
        assert!(guard.is_none(), "capability should still be None");
    }

    /// `ListFileVersions` on a path outside every served on-demand folder must
    /// answer `FileVersionList` (empty), never an error — Explorer offers the
    /// "Fauna" submenu on any file, so `EnumSubCommands` always gets *some*
    /// response. Exercises the actual dispatch entry point (`handle_request`),
    /// not just `versions::list_file_versions` directly (which `versions_tier3.rs`
    /// already proves against a real nest) — this is the one link in the
    /// DLL→pipe→service→nest chain that function alone doesn't cover: this
    /// handler's own `path_map::resolve_to_folder_rel` gate. No capability is
    /// provisioned, so a wrong implementation that skipped the gate and called
    /// through to `nest_rpc_client()` would fail loudly ("nest capability not
    /// provisioned") rather than silently returning empty for the wrong reason.
    #[tokio::test]
    async fn list_file_versions_for_untracked_path_returns_empty_no_nest_call() {
        let state = test_state();
        let req = Request {
            id: 1,
            method: RequestMethod::ListFileVersions {
                path: r"C:\Users\alice\Desktop\untracked.txt".into(),
            },
        };
        let resp = handle_request(&req, &state).await;
        match resp.result {
            ResponseResult::Ok(ResponsePayload::FileVersionList(info)) => {
                assert!(
                    info.versions.is_empty(),
                    "an untracked path has no history: {info:?}"
                );
                assert_eq!(info.folder, "");
            }
            other => panic!("expected FileVersionList, got {other:?}"),
        }
    }

    // ──: the IPC readers must resolve the same state DB the engine
    //    writes — identity-keyed when the binding carries a `FolderRef` (R1),
    //    name-keyed only for a pre-identity binding. A test over a ref-LESS binding
    //    passes against name-keyed readers and proves nothing, so every pin below
    //    drives a ref-carrying on-demand binding. ──

    /// State whose `SyncPaths` point at a real temp data-root, so the per-set DB
    /// seeded on disk is the one the handlers open.
    fn test_state_with_config_and_paths(
        config: crate::config::SyncConfig,
        paths: crate::config::SyncPaths,
    ) -> Arc<SyncServiceState> {
        let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        SyncServiceState::new(config, shutdown_tx, event_tx, paths)
    }

    /// One on-demand folder at `C:\od-docs` bound to set "docs" carrying `fs_ref`.
    fn ref_carrying_config(fs_ref: &str) -> crate::config::SyncConfig {
        crate::config::SyncConfig {
            locations: vec![crate::config::LocationConfig {
                path: r"C:\od-docs".into(),
                mode: crate::config::LocationMode::OnDemand,
                folder: Some("docs".into()),
                folder_id: Some(fs_ref.to_string()),
                ..Default::default()
            }],
            ..crate::config::SyncConfig::default()
        }
    }

    /// Seed one row at `rel` in the DB at `db_path` (same shape as
    /// `producer_integration::seed_db_row`, but at an explicit path so a test can
    /// seed the identity-keyed location the engine really writes).
    fn seed_row_at(
        db_path: &std::path::Path,
        rel: &str,
        st: fauna_sync_engine::db::SyncState,
        size_bytes: i64,
    ) {
        std::fs::create_dir_all(db_path.parent().unwrap()).expect("create db dir");
        let db = fauna_sync_engine::db::SyncDb::open(db_path).expect("open per-folder db");
        db.upsert_entry(rel, None, None, None, st, 0, 0, size_bytes, 1, None)
            .expect("seed sync entry");
    }

    /// A file's overlay status must come from the DB the engine actually writes.
    /// Red-first: the name-keyed reader looked in `fs-docs.db` (absent — the
    /// engine keeps a ref-carrying binding's state in the `fsid-` namespace) and
    /// answered `NotTracked` for a tracked, hydrated file.
    #[tokio::test]
    async fn get_file_status_reads_the_identity_keyed_db_for_a_ref_carrying_binding() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = crate::config::SyncPaths::new(Some(tmp.path().to_path_buf()));
        let fs_ref = fauna_core::folder_keys::FolderRef::Local(7).to_wire();
        let db_path = paths.sync_db_path_for_ref(FolderRef::parse(&fs_ref).unwrap());
        seed_row_at(
            &db_path,
            "sub/a.txt",
            fauna_sync_engine::db::SyncState::Synced,
            4096,
        );
        let state = test_state_with_config_and_paths(ref_carrying_config(&fs_ref), paths);

        let resp = handle_request(
            &Request {
                id: 1,
                method: RequestMethod::GetFileStatus {
                    path: r"C:\od-docs\sub\a.txt".into(),
                },
            },
            &state,
        )
        .await;
        match resp.result {
            ResponseResult::Ok(ResponsePayload::FileStatus(info)) => {
                assert_eq!(
                    info.status,
                    fauna_ipc::sync::FileStatus::Synced,
                    "the row lives in the identity-keyed DB; a name-keyed read \
                     reports NotTracked"
                );
                assert_eq!(info.size_bytes, 4096);
            }
            other => panic!("expected FileStatus, got {other:?}"),
        }
    }

    /// A folder's badge folds descendant states from the same identity-keyed DB.
    /// Red-first: the name-keyed fold saw no DB → unbadged (`NotTracked`).
    #[tokio::test]
    async fn folder_badge_folds_descendants_from_the_identity_keyed_db() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = crate::config::SyncPaths::new(Some(tmp.path().to_path_buf()));
        let fs_ref = fauna_core::folder_keys::FolderRef::Local(7).to_wire();
        let db_path = paths.sync_db_path_for_ref(FolderRef::parse(&fs_ref).unwrap());
        seed_row_at(
            &db_path,
            "sub/a.txt",
            fauna_sync_engine::db::SyncState::Placeholder,
            4096,
        );
        let state = test_state_with_config_and_paths(ref_carrying_config(&fs_ref), paths);

        let resp = handle_request(
            &Request {
                id: 1,
                method: RequestMethod::GetFileStatus {
                    path: r"C:\od-docs\sub".into(),
                },
            },
            &state,
        )
        .await;
        match resp.result {
            ResponseResult::Ok(ResponsePayload::FileStatus(info)) => {
                assert_eq!(
                    info.status,
                    fauna_ipc::sync::FileStatus::CloudOnly,
                    "one cloud-only descendant in the identity-keyed DB folds to \
                     CloudOnly; a name-keyed fold sees no DB and unbadges"
                );
            }
            other => panic!("expected FileStatus, got {other:?}"),
        }
    }

    /// After a dehydrate, `mark_placeholder` must flip the row in the DB the
    /// engine writes. Red-first: the name-keyed writer hit the `!db_path.exists()`
    /// guard and silently no-opped — the row stayed `Synced` while the bytes were
    /// gone, so the overlay lied until the next full rescan.
    #[tokio::test]
    async fn mark_placeholder_updates_the_identity_keyed_db() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = crate::config::SyncPaths::new(Some(tmp.path().to_path_buf()));
        let fs_ref = fauna_core::folder_keys::FolderRef::Local(7).to_wire();
        let db_path = paths.sync_db_path_for_ref(FolderRef::parse(&fs_ref).unwrap());
        seed_row_at(
            &db_path,
            "sub/a.txt",
            fauna_sync_engine::db::SyncState::Synced,
            4096,
        );
        let state = test_state_with_config_and_paths(ref_carrying_config(&fs_ref), paths);

        mark_placeholder(&state, r"C:\od-docs\sub\a.txt")
            .await
            .expect("mark_placeholder");

        let db = fauna_sync_engine::db::SyncDb::open(&db_path).expect("open db");
        let entry = db
            .get_entry("sub/a.txt")
            .expect("db read")
            .expect("row exists");
        assert_eq!(
            entry.state,
            fauna_sync_engine::db::SyncState::Placeholder,
            "the dehydrate must be recorded in the identity-keyed DB; the \
             name-keyed writer no-opped and left the row Synced"
        );
    }

    /// **The shell's *Free up space* verb answers the engine's one dehydrate
    /// gate** — recorded content only. A file whose disk bytes are the recorded content
    /// may be freed; a newer save on top of it may not (freeing it is irrecoverable),
    /// and neither may a path no served folder resolves (fail-closed), nor this
    /// device's own record in a folder not read as full. The platform's
    /// not-in-sync refusal is the first-line guard; this is the gate that does not
    /// depend on every in-sync assertion ever made being right.
    #[tokio::test]
    async fn the_free_space_gate_frees_only_the_recorded_content() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().join("od-docs");
        std::fs::create_dir_all(&root).expect("create root");
        let paths = crate::config::SyncPaths::new(Some(tmp.path().join("data")));
        let fs_ref = fauna_core::folder_keys::FolderRef::Local(7).to_wire();
        let db_path = paths.sync_db_path_for_ref(FolderRef::parse(&fs_ref).unwrap());
        let mut config = ref_carrying_config(&fs_ref);
        config.locations[0].path = root.to_string_lossy().to_string();
        let state = test_state_with_config_and_paths(config, paths);

        const RECORDED: &[u8] = b"E1: the content whose record reached the nest";
        let file = root.join("a.txt");
        std::fs::write(&file, RECORDED).expect("write E1");
        std::fs::create_dir_all(db_path.parent().unwrap()).expect("create db dir");
        {
            let db = fauna_sync_engine::db::SyncDb::open(&db_path).expect("open db");
            db.upsert_entry(
                "a.txt",
                Some(fauna_core::data::ContentHash::of_raw(RECORDED)),
                None,
                Some(fauna_core::data::ContentHash::of_raw(b"head manifest")),
                fauna_sync_engine::db::SyncState::Synced,
                0,
                0,
                RECORDED.len() as i64,
                1,
                None,
            )
            .expect("seed row");
            db.stamp_recorded_content_from_local(
                "a.txt",
                fauna_sync_engine::db::ProofOrigin::OwnRecord,
            )
            .expect("stamp the recorded content");
        }
        let abs = file.to_string_lossy().to_string();
        let set_residency = |metadata_only: bool| {
            fauna_sync_engine::db::SyncDb::open(&db_path)
                .expect("reopen db")
                .set_residency_reading(metadata_only)
                .expect("persist the residency reading");
        };

        // A holder keeps what it wrote: this device's own record frees only where
        // the folder's engine last read the residency as full — the verb holds
        // nothing but the state DB, so it reads the persisted reading.
        assert!(
            !dehydration_safe(&state, &abs).await,
            "no residency reading yet: an own record is kept"
        );
        set_residency(true);
        assert!(
            !dehydration_safe(&state, &abs).await,
            "metadata-only: the nest holds no byte of an own record — never freed"
        );
        set_residency(false);
        assert!(
            dehydration_safe(&state, &abs).await,
            "the recorded content may be freed"
        );

        std::fs::write(&file, b"E2: a newer save, never recorded").expect("write E2");
        assert!(
            !dehydration_safe(&state, &abs).await,
            "a save newer than the recorded content must never be freed"
        );

        let elsewhere = tmp.path().join("elsewhere.txt");
        std::fs::write(&elsewhere, RECORDED).expect("write an unserved file");
        assert!(
            !dehydration_safe(&state, &elsewhere.to_string_lossy()).await,
            "a path no served folder resolves fails closed"
        );
    }

    /// After a version restore, `repoint_entry` must re-point the row in the DB
    /// the engine writes. Red-first: the name-keyed writer returned `Ok(false)`
    /// ("no local copy"), `apply_restore_locally` logged at debug and skipped the
    /// dehydrate — the nest-side restore landed while this device kept serving
    /// pre-restore content with no signal at all (the worst of the four).
    #[tokio::test]
    async fn repoint_entry_re_points_the_row_in_the_identity_keyed_db() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let paths = crate::config::SyncPaths::new(Some(tmp.path().to_path_buf()));
        let fs_ref = fauna_core::folder_keys::FolderRef::Local(7).to_wire();
        let db_path = paths.sync_db_path_for_ref(FolderRef::parse(&fs_ref).unwrap());
        seed_row_at(
            &db_path,
            "sub/a.txt",
            fauna_sync_engine::db::SyncState::Synced,
            4096,
        );
        let state = test_state_with_config_and_paths(ref_carrying_config(&fs_ref), paths);

        let restored = crate::versions::RestoredVersion {
            manifest_hash: [0xAA; 32],
            size_bytes: 2048,
            content_key_version: Some(3),
            recorded_seq: 9,
        };
        let repointed = repoint_entry(
            &state,
            FolderRef::parse(&fs_ref).unwrap(),
            "sub/a.txt",
            &restored,
            Some([0x5C; 32]),
        )
        .await
        .expect("repoint_entry");
        assert!(
            repointed,
            "the row lives in the identity-keyed DB; the name-keyed writer \
             answered Ok(false) = \"no local copy\""
        );

        let db = fauna_sync_engine::db::SyncDb::open(&db_path).expect("open db");
        let entry = db
            .get_entry("sub/a.txt")
            .expect("db read")
            .expect("row exists");
        assert_eq!(
            entry.state,
            fauna_sync_engine::db::SyncState::Placeholder,
            "re-pointed row is a placeholder awaiting re-hydration"
        );
        assert_eq!(
            entry.manifest_hash,
            Some(fauna_core::data::ContentHash::from_digest_raw([0xAA; 32])),
            "the row must carry the restored manifest"
        );
        assert_eq!(entry.size_bytes, 2048);
        assert_eq!(entry.content_key_version, Some(3));
        assert_eq!(
            entry.head_signed_as,
            Some([0x5C; 32]),
            "the re-pointed head is stamped as the identity the restore recorded as; \
             an entry with no signer opens under no owner root"
        );
    }
}

// ── The test-only custodian pass poke ─────────────────────────────────────────
//
// Gated with the IPC variant it serves (`testing.md` convention 15).

#[cfg(all(test, any(debug_assertions, feature = "test-helpers")))]
mod custodian_poke_tests {
    use super::*;
    use fauna_client_backup::custodian::{AuditState, CapState, SelfAudit};
    use fauna_ipc::sync::{Request, RequestMethod, ResponsePayload, ResponseResult};
    use fauna_sync_engine::custodian_pull::PullReport;

    fn state() -> Arc<SyncServiceState> {
        let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        SyncServiceState::new(
            crate::config::SyncConfig::default(),
            shutdown_tx,
            event_tx,
            crate::config::SyncPaths::new(None),
        )
    }

    fn report(held: u64, cap: CapState) -> PullReport {
        PullReport {
            held_bytes: held,
            cap_state: cap,
            ..PullReport::default()
        }
    }

    /// A device that is not hosting answers with a **report**, not an error.
    ///
    /// This is the arm the tier_3 proof polls on: enrollment and the host loop's
    /// registry re-read are `IDLE_RECHECK_SECS` apart, and a test that treated
    /// "not yet hosting" as a failure would have to sleep that interval out
    /// instead of retrying the poke — precisely the wall-clock brittleness
    /// convention 14 forbids. Same choice the nest hook's `owners_run: 0` makes.
    #[tokio::test]
    async fn not_hosting_is_a_report_rather_than_an_error() {
        let state = state();
        let resp = handle_request(
            &Request {
                id: 1,
                method: RequestMethod::CustodianRunPassNow { now_offset_secs: 0 },
            },
            &state,
        )
        .await;
        match resp.result {
            ResponseResult::Ok(ResponsePayload::CustodianPassReport(r)) => {
                assert!(!r.hosting, "an unenrolled device is not hosting");
                assert_eq!(r.kinds_run, 0);
                assert!(
                    !r.checked_in,
                    "a pass that never ran cannot have checked in"
                );
                assert_eq!(
                    r.cap_state, None,
                    "no pass ran, so there is no cap verdict to report — reporting \
                     CAP_STATE_OK here would assert a healthy verdict nothing produced"
                );
            }
            other => panic!("expected a CustodianPassReport, got {other:?}"),
        }
    }

    /// `cap_state` is read from the pass's own verdict, and is `Reached` if
    /// **any** kind reached it.
    ///
    /// The rule this pins is the one `backup-destinations.md` § Third destination
    /// kind states twice: cap-reached is read, never inferred from
    /// `held >= cap`. A pass that stopped at its cap ends *below* it (a segment
    /// larger than the remaining headroom stops the pass without filling it), so
    /// the held figures here are deliberately small — anything re-deriving the
    /// verdict from them would report OK and render a stopped backup as
    /// healthy-with-room.
    #[test]
    fn cap_reached_comes_from_the_verdict_not_from_held_versus_cap() {
        let folded = fold_pass_report(
            &[report(10, CapState::Ok), report(12, CapState::Reached)],
            None,
        );
        assert_eq!(
            folded.cap_state.as_deref(),
            Some(CapState::Reached.as_wire()),
            "one kind at its cap makes the device cap-reached, however small the \
             held figures are"
        );
        assert_eq!(folded.kinds_run, 2);
        assert!(
            folded.checked_in,
            "the check-in is inside the pass a report proves ran"
        );

        let all_ok = fold_pass_report(&[report(10, CapState::Ok)], None);
        assert_eq!(all_ok.cap_state.as_deref(), Some(CapState::Ok.as_wire()));
    }

    /// `held_bytes` is the freshest pass's figure, not the largest.
    ///
    /// Each pass reports what the store holds **after** it, and the kinds run in
    /// sequence, so the last report is the current one. Folding with `max` would
    /// be silently wrong in exactly the case the field exists to show: a pass
    /// that reclaimed generations leaves *fewer* bytes held, and a max-fold would
    /// keep reporting the pre-reclaim figure.
    #[test]
    fn held_bytes_is_the_last_passs_figure_not_the_largest() {
        let folded = fold_pass_report(
            &[report(900, CapState::Ok), report(300, CapState::Ok)],
            None,
        );
        assert_eq!(folded.held_bytes, 300);
    }

    /// An un-audited pass reports **no** audit state rather than a pass.
    ///
    /// The audit is debounced to `AUDIT_MIN_INTERVAL`, so most passes carry no
    /// fresh verdict. `None` must stay *not yet audited*: reading absence as a
    /// pass renders an unverified copy as verified, and reading it as a failure
    /// raises a fleet-wide false data-loss alarm — the two misreadings
    /// `backup-destinations.md` § Third destination kind exists to prevent.
    #[test]
    fn an_unaudited_pass_reports_no_verdict_in_either_direction() {
        let folded = fold_pass_report(&[report(1, CapState::Ok)], None);
        assert_eq!(folded.audit_state, None);

        let failed = fold_pass_report(
            &[report(1, CapState::Ok)],
            Some(SelfAudit::failed(Some(1_000))),
        );
        assert_eq!(
            failed.audit_state.as_deref(),
            Some(AuditState::Failed.as_wire()),
            "a failing verdict reaches the poke's caller — the tier_3 audit pin \
             asserts the owner's page off exactly this"
        );
    }
}

#[cfg(all(test, unix))]
mod attach_tests {
    use super::*;
    use fauna_ipc::sync::{Request, RequestMethod, ResponsePayload, ResponseResult};

    /// tier_1, over the REAL unix socket server: an app's `AttachApp` lease
    /// holds exactly as long as its connection — the push arm's "is an app
    /// open?" answer (`push_arm`; `sync-agent.md` § Scope per platform). A
    /// client that closes (the stand-in for an exiting or crashed app; the
    /// kernel closes the socket either way) ends the lease with no request.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_attach_lease_lasts_exactly_as_long_as_the_apps_connection() {
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel(4);
        let state = SyncServiceState::new(
            crate::config::SyncConfig::default(),
            shutdown_tx.clone(),
            event_tx.clone(),
            crate::config::SyncPaths::new(None),
        );
        let dir = std::env::temp_dir().join(format!("fauna-attach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("a.sock");
        let served = Arc::clone(&state);
        let server_socket = socket.clone();
        let server = tokio::spawn(async move {
            let handler = move |req: Request| {
                let state = Arc::clone(&served);
                async move { handle_request(&req, &state).await }
            };
            fauna_ipc::unix_transport::serve(&server_socket, handler, shutdown_rx, event_tx).await
        });

        // A raw socket, not `SyncPipeClient`: that client's reader thread holds
        // a cloned fd, so only the process exiting closes its socket — which is
        // exactly the app's lease (its process lifetime). Dropping this stream
        // is the in-test stand-in for that exit.
        let client_socket = socket.clone();
        let client = tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let mut stream = loop {
                match std::os::unix::net::UnixStream::connect(&client_socket) {
                    Ok(s) => break s,
                    Err(e) => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "agent socket never came up: {e}"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
            };
            let frame = fauna_ipc::encode_frame(&Request {
                id: 1,
                method: RequestMethod::AttachApp {
                    app: Some("tui".into()),
                    notification_identity: None,
                },
            })
            .unwrap();
            stream.write_all(&frame).unwrap();
            let payload = fauna_ipc::sync_pipe_client::read_frame(&mut stream).unwrap();
            let resp: fauna_ipc::sync::Response = fauna_ipc::decode_payload(&payload).unwrap();
            assert!(matches!(
                resp.result,
                ResponseResult::Ok(ResponsePayload::Empty)
            ));
            stream
        })
        .await
        .unwrap();
        assert!(
            state.attached_apps.any(),
            "the reply is sent only after the lease is held"
        );

        drop(client);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while state.attached_apps.any() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the lease outlived the app's connection"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        shutdown_tx.send(true).unwrap();
        let _ = server.await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// [`RequestMethod::ReconcileAccountRuntime`]: the participation switch's
/// cross-process arm (`p2p.md` § Per-device participation → *Enforcement*,
/// (c) Promptness).
#[cfg(test)]
mod reconcile_account_runtime_tests {
    use super::*;
    use fauna_ipc::sync::{Request, RequestMethod, ResponsePayload, ResponseResult};

    fn test_state() -> Arc<SyncServiceState> {
        let (shutdown_tx, _rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        SyncServiceState::new(
            crate::config::SyncConfig::default(),
            shutdown_tx,
            event_tx,
            crate::config::SyncPaths::new(None),
        )
    }

    /// A requester for a runtime with no nest: every call fails as a transport
    /// fault the pump absorbs, so a pass runs and reports, and nothing leaves
    /// the process.
    #[derive(Clone)]
    struct NoNest;

    #[derive(Debug)]
    struct NoNestError(&'static str);

    impl std::fmt::Display for NoNestError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "no nest in this test: {}", self.0)
        }
    }

    impl fauna_protocol::RpcErrorClass for NoNestError {
        fn is_rejection(&self) -> bool {
            false
        }
    }

    impl fauna_protocol::RpcRequester for NoNest {
        type Error = NoNestError;

        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            _payload: Req,
        ) -> Result<Reply, NoNestError>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            Err(NoNestError(kind))
        }
    }

    impl fauna_protocol::KeyedRpcRequester for NoNest {
        async fn request_keyed<Req, Reply>(
            &self,
            kind: &'static str,
            _idempotency_key: [u8; 16],
            _payload: Req,
        ) -> Result<Reply, NoNestError>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            Err(NoNestError(kind))
        }
    }

    /// An account runtime over a store root of its own, the only one on it —
    /// so it takes the engine-holder role, as the agent's mount does on a
    /// desktop with no app holding it. Its backstop is an hour away: a pass
    /// counted in the test is one something asked for.
    async fn holding_runtime(
        base: &std::path::Path,
    ) -> fauna_sync_engine::account_runtime::AccountStoreHandle {
        use fauna_sync_engine::account_runtime as rt;
        let root = fauna_core::identity::ActorKeypair::from_secret([0x2au8; 32]);
        let actor_id_hex = root.actor_id_hex();
        rt::AccountStoreRuntime::start(rt::AccountRuntimeParams {
            store_backup_exclusion: rt::CloudBackupExclusion::NotApplicable {
                platform: "test".into(),
            },
            store_root: rt::StoreRoot::at(base.join("state")),
            actor_id_hex,
            rpc: NoNest,
            process_rpc: None,
            principal: rt::RuntimePrincipal::SeedHolding(root.into()),
            credentials: fauna_credential_store::CredentialStore::with_file_backend(
                rt::CRED_NAMESPACE,
                base.join("creds"),
            ),
            reconnects: None,
            pushes: None,
            backstop_interval: std::time::Duration::from_secs(3600),
            memberships: None,
            trusted_escrow_holders: rt::fixed_holders(Vec::new()),
            attested_predecessors: Default::default(),
            linked_nests: None,
            owed_nests: None,
            peer_transport: None,
            enrollment_target_device_id: "ab".repeat(32),
        })
        .await
        .expect("an offline runtime starts")
    }

    fn reconcile_request() -> Request {
        Request {
            id: 1,
            method: RequestMethod::ReconcileAccountRuntime,
        }
    }

    /// The verb runs a full pass of the store the agent has mounted before it
    /// replies — the pass whose ensure step reads the participation row, so
    /// the same-account listener drops within the app's gesture.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reconcile_account_runtime_runs_a_pass_of_the_mounted_store() {
        let dir = tempfile::tempdir().unwrap();
        let handle = holding_runtime(dir.path()).await;
        assert!(
            handle.is_engine_holder(),
            "the only runtime on its root holds"
        );
        let state = test_state();
        *state.mounted_store.lock().unwrap() = Some(handle.clone());

        let (_, before) = handle.pump_cycles();
        let reply = handle_request(&reconcile_request(), &state).await;
        assert!(matches!(
            reply.result,
            ResponseResult::Ok(ResponsePayload::Empty)
        ));
        let (_, after) = handle.pump_cycles();
        assert!(
            after > before,
            "the reply comes after a completed pass ({before} → {after})"
        );

        *state.mounted_store.lock().unwrap() = None;
        handle.shutdown().await;
    }

    /// Nothing mounted — signed out, or the stint not yet up — is not the
    /// gesture's failure: the row rests in the store, and the mount's first
    /// pass reads it.
    #[tokio::test]
    async fn reconcile_account_runtime_with_no_store_mounted_is_a_quiet_no_op() {
        let state = test_state();
        let reply = handle_request(&reconcile_request(), &state).await;
        assert!(matches!(
            reply.result,
            ResponseResult::Ok(ResponsePayload::Empty)
        ));
    }
}
