//! The desktops' **presence roots** — the planning half
//! (`docs/goal/behavior/on-demand-files-shared-sets.md` § Shared sets on a
//! capability host, decision 3d).
//!
//! The desktop agent runs the same presence plan every set-level on-demand
//! platform runs (decision 3: the sets this device is a delivery seat for, own
//! or shared with the account, toggle-enabled, minus the sets it presents
//! another way) and presents each set the plan names as an on-demand root no
//! bound location reaches: `Fauna/<display name>` under the user's home. This
//! module turns the plan's input into [`EngineMapping`]s of the presence kind —
//! pure, so every rule is pinned by a tier-1 test:
//!
//! - **where** — `<home>/Fauna/<display name>`; a clash between two names on
//!   one device (case-insensitive on windows) suffixed ` (2)`, ` (3)` in plan
//!   order (rule 1);
//! - **what subtracts** — a set bound to a location in EITHER mode (parked
//!   included: the bind still stands) has no presence root (rule 2);
//! - **the backing** — linux mounts its view over a PRIVATE backing directory
//!   in the agent's per-account state dir; windows has none (rule 3);
//! - **the reader flag** — from the set's row (rule 4);
//! - **the drain gate** — a root the plan no longer names, or whose display
//!   name moved, keeps serving where it is until its engine records nothing
//!   un-recorded (rule 5) — one root per set, so a renamed set moves only once
//!   its old root has drained.
//!
//! [`reconcile_presence_roots`] (spawned by every engine reconcile and every
//! toggle) publishes the plan, and the engine reconcile SERVES it
//! (`crate::engine_driver::reconcile_resolved`) — on linux a FUSE view at the
//! presence path over the private backing, on windows a cfapi sync root
//! registered at the presence path itself, a reader's refused by the OS
//! (`crate::cfapi_host::protect_reader_root`) — and the pass removes a retired
//! root's directory, backing and state DB once it has drained
//! ([`removal_for`]).

// Apple presents its sets through the File Provider, never this agent: the
// pass and the toggle verb's re-plan are desktop-only.
#![cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fauna_core::folder_keys::{ActorScopedFolderRef, FolderRef};
use fauna_folders_machine::on_demand_presence::{
    HeldGroupScope, OnDemandPrefs, PresenceSet, desired_presences,
};

use crate::config::{LocationMode, SyncConfig, SyncPaths};
use crate::engine_driver::EngineMapping;
use crate::state::SyncServiceState;

/// The directory under the user's home every presence root lives in — the
/// desktops' counterpart of apple's system-managed Fauna location.
pub const PRESENCE_DIR: &str = "Fauna";

/// The presence half of an [`EngineMapping`]: what a presence root carries that
/// a bound location does not. `None` on a mapping = a bound location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceRoot {
    /// The set's actor-scoped identifier (`ActorScopedFolderRef::to_wire`) —
    /// the toggle's key and the identity the drain gate follows.
    pub scoped_id: String,
    /// The private directory the linux view is mounted over, in the agent's
    /// per-account state dir; `None` on windows, where cfapi keeps a root's
    /// bytes in the root itself.
    pub backing: Option<PathBuf>,
    /// The account only reads the set (the row's `reader`): the root is built
    /// `ReaderHosting::ReadOnly`, with no write half.
    pub read_only: bool,
}

/// Everything one presence plan reads, gathered by the caller so the planner
/// stays pure.
pub struct PresenceInputs<'a> {
    /// The provisioned account.
    pub actor_id: [u8; 32],
    /// The plan's sets, in plan order (`presence_sets_over`).
    pub sets: &'a [PresenceSet],
    /// The agent's bindings — every bound set subtracts.
    pub config: &'a SyncConfig,
    /// The device-local per-set toggle.
    pub prefs: &'a OnDemandPrefs,
    /// The user's home (`$HOME`, `%USERPROFILE%`).
    pub home: &'a Path,
    /// Where the private backings live (`<per-account state dir>/presence`);
    /// `None` on a platform that keeps no backing.
    pub backing_root: Option<&'a Path>,
    /// Names clash case-insensitively (windows).
    pub case_insensitive: bool,
    /// The presence mappings planned last — what is presented now.
    pub prior: &'a [EngineMapping],
    /// How many rels each presented root (by scoped id) holds that the nest
    /// never recorded. A root absent here holds none.
    pub unrecorded: &'a BTreeMap<String, u32>,
}

/// The presence mappings to serve: one per set the plan names, plus every
/// presented root still draining. Order: draining roots that keep their path
/// first (their paths are taken), then the plan's sets in plan order.
#[must_use]
pub fn plan_presence_roots(inputs: &PresenceInputs<'_>) -> Vec<EngineMapping> {
    // Rule 2: a set bound to a location in either mode is presented by the bind.
    let bound: BTreeSet<String> = inputs
        .config
        .locations
        .iter()
        .filter_map(|l| l.binding().map(|b| b.folder_ref.to_wire()))
        .collect();
    let desired = desired_presences(inputs.actor_id, inputs.sets, &bound, inputs.prefs);

    let draining = |scoped_id: &str| inputs.unrecorded.get(scoped_id).is_some_and(|n| *n > 0);
    let prior_by_id: BTreeMap<&str, &EngineMapping> = inputs
        .prior
        .iter()
        .filter_map(|m| m.presence.as_ref().map(|p| (p.scoped_id.as_str(), m)))
        .collect();

    let mut taken: BTreeSet<String> = BTreeSet::new();
    let key = |path: &Path| {
        let s = path.to_string_lossy().into_owned();
        if inputs.case_insensitive {
            s.to_lowercase()
        } else {
            s
        }
    };
    let mut out = Vec::new();

    // Rule 5, the drain gate: a presented root the plan drops — or would move
    // to a new name — keeps serving at its path while it holds un-recorded
    // rels. A dropped set keeps the reader flag it was presented with; one
    // still desired takes the row's current flag (a grant or a demotion
    // reaches a draining root too).
    let desired_by_id: BTreeMap<&str, &PresenceSet> = desired
        .iter()
        .map(|d| (d.scoped_id.as_str(), &d.set))
        .collect();
    let mut kept: BTreeSet<String> = BTreeSet::new();
    for (scoped_id, prior) in &prior_by_id {
        if !draining(scoped_id) {
            continue;
        }
        let mut mapping = (*prior).clone();
        if let (Some(set), Some(presence)) =
            (desired_by_id.get(scoped_id), mapping.presence.as_mut())
        {
            presence.read_only = set.read_only;
        }
        taken.insert(key(&mapping.path));
        kept.insert((*scoped_id).to_string());
        out.push(mapping);
    }

    let parent = inputs.home.join(PRESENCE_DIR);
    for presence in &desired {
        if kept.contains(&presence.scoped_id) {
            continue;
        }
        let Some(folder_ref) = FolderRef::parse(&presence.set.folder_id) else {
            continue; // `desired_presences` already parsed it; unreachable
        };
        let base = dir_name(&presence.set.name);
        let mut n = 1u32;
        let path = loop {
            let name = if n == 1 {
                base.clone()
            } else {
                format!("{base} ({n})")
            };
            let candidate = parent.join(name);
            if taken.insert(key(&candidate)) {
                break candidate;
            }
            n += 1;
        };
        out.push(EngineMapping {
            path,
            folder: presence.set.set_name.clone(),
            folder_ref,
            mode: LocationMode::OnDemand,
            presence: Some(PresenceRoot {
                scoped_id: presence.scoped_id.clone(),
                backing: inputs
                    .backing_root
                    .map(|root| root.join(&presence.scoped_id)),
                read_only: presence.set.read_only,
            }),
        });
    }
    out
}

/// The presented roots the plan retires now — presented last time, not in
/// `planned`, so drained — whose mount (linux) or registration (windows), and
/// then directory and backing, the driver removes.
#[must_use]
pub fn retired_presence_roots(
    prior: &[EngineMapping],
    planned: &[EngineMapping],
) -> Vec<EngineMapping> {
    prior
        .iter()
        .filter(|m| m.presence.is_some())
        .filter(|m| {
            !planned
                .iter()
                .any(|p| p.presence == m.presence && p.path == m.path)
        })
        .cloned()
        .collect()
}

/// A display name as ONE directory component. The plan's display names are
/// set names a person typed (`docs (alice)`); a separator or a dot-only name
/// would put the root somewhere other than directly under `Fauna`, so each
/// separator becomes `_` and an empty or dot-only name becomes `_`.
fn dir_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    if cleaned.trim_matches('.').trim().is_empty() {
        "_".to_string()
    } else {
        cleaned
    }
}

/// The toggle store's file: device-local, keyed by the actor-scoped id, so it
/// lives in the flat base beside the config rather than under one account's
/// scope.
pub(crate) fn prefs_path(paths: &SyncPaths) -> PathBuf {
    paths.flat_base_dir().join("presence-prefs.json")
}

/// How long one plan read may take before the pass is skipped.
const PLAN_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// [`RequestMethod::SetPresenceEnabled`](fauna_ipc::sync::RequestMethod::SetPresenceEnabled)
/// — set one set's device-local toggle, persist it, and re-plan.
///
/// # Errors
/// An identifier the parser refuses, or a store that will not read or write.
pub(crate) async fn set_presence_enabled(
    state: &Arc<SyncServiceState>,
    scoped_id: &str,
    enabled: bool,
) -> Result<(), String> {
    let scoped = ActorScopedFolderRef::parse(scoped_id)
        .ok_or_else(|| "SetPresenceEnabled: not an actor-scoped set identifier".to_string())?;
    {
        let _serial = state.presence_planned.lock().await;
        let path = prefs_path(&state.paths);
        let mut prefs = OnDemandPrefs::load(&path)
            .map_err(|e| format!("the on-demand toggle store did not read: {e:#}"))?;
        prefs.set_enabled(&scoped, enabled);
        prefs
            .save(&path)
            .map_err(|e| format!("the on-demand toggle store did not write: {e:#}"))?;
    }
    spawn_reconcile(state);
    Ok(())
}

/// Re-plan the presence roots off the caller's path: the plan reads the nest
/// (the list and each own folder's roster), which must never hold up an
/// engine reconcile or an IPC reply. Desktops only — apple presents its sets
/// through the File Provider, never through this agent.
pub(crate) fn spawn_reconcile(state: &Arc<SyncServiceState>) {
    #[cfg(any(target_os = "linux", windows))]
    {
        let state = Arc::clone(state);
        tokio::spawn(async move { reconcile_presence_roots(&state).await });
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    let _ = state;
}

/// One pass of the presence plan: read it, plan the roots, record them, and —
/// where the platform serves them — start, restart or stop their engines and
/// remove the roots it retired. A failed read skips the pass and keeps what was
/// planned — never a teardown on a state the agent could not see (the doors'
/// error rule).
pub(crate) async fn reconcile_presence_roots(state: &Arc<SyncServiceState>) {
    let mut planned = state.presence_planned.lock().await;
    let inputs = match read_plan_inputs(state).await {
        Ok(inputs) => inputs,
        Err(e) => {
            tracing::info!(
                error = %format!("{e:#}"),
                "presence roots: the plan did not read; keeping what is planned"
            );
            return;
        }
    };
    tracing::debug!(
        sets = inputs.sets.len(),
        shared = inputs
            .sets
            .iter()
            .filter(|s| s.role != fauna_folders_machine::on_demand_presence::PresenceRole::Own)
            .count(),
        delivered = inputs.sets.iter().filter(|s| s.this_device_accepts).count(),
        "presence roots: the plan read"
    );
    let state_dir = state.paths.base_dir();
    // The drain gate's input: what each SERVED root's engine has not recorded,
    // off its state DB. Only the desktops serve a presence root; elsewhere none
    // holds an un-recorded rel, so every retirement is drained.
    let unrecorded = if SERVED {
        unrecorded_counts(&planned, &state_dir)
    } else {
        BTreeMap::new()
    };
    let config = state.config.read().await;
    let backing_root = (!cfg!(windows)).then(|| state_dir.join("presence"));
    let next = plan_presence_roots(&PresenceInputs {
        actor_id: inputs.actor_id,
        sets: &inputs.sets,
        config: &config,
        prefs: &inputs.prefs,
        home: &inputs.home,
        backing_root: backing_root.as_deref(),
        case_insensitive: cfg!(windows),
        prior: &planned,
        unrecorded: &unrecorded,
    });
    drop(config);
    let retired = retired_presence_roots(&planned, &next);
    let changed = next != *planned || !retired.is_empty();
    if changed {
        tracing::info!(
            roots = next.len(),
            retired = retired.len(),
            served = SERVED,
            "presence roots planned"
        );
        for m in &next {
            tracing::debug!(
                path = %fauna_core::log_redact::log_path(m.path.to_string_lossy().as_ref()),
                folder = %fauna_core::log_redact::log_folder_name(&m.folder),
                read_only = m.presence.as_ref().is_some_and(|p| p.read_only),
                "presence root"
            );
        }
    }
    *planned = next.clone();
    if let Ok(mut serving) = state.presence_serving.lock() {
        *serving = next.clone();
    }
    if SERVED && changed {
        // A group set a root presents is named by the content-key resolution
        // only once it re-reads (`content_keys::bound_group_scopes`); every other
        // set is already named by the folder list.
        state.content_keys_wake.notify_one();
        if let Err(e) = crate::engine_driver::reconcile_resolved(state).await {
            tracing::warn!(error = %format!("{e:#}"), "presence roots: the engine reconcile failed");
        }
        let bound: BTreeSet<String> = {
            let config = state.config.read().await;
            crate::engine_driver::plan_engines(&config)
                .iter()
                .map(EngineMapping::engine_key)
                .collect()
        };
        for root in retired {
            let removal = removal_for(&root, &next, &bound);
            if removal != Removal::default() {
                tokio::spawn(remove_retired_root(state_dir.clone(), root, removal));
            }
        }
    }
}

/// Whether this platform's driver serves a presence root: linux's FUSE view,
/// windows' cfapi sync root at the presence path.
pub(crate) const SERVED: bool = cfg!(any(target_os = "linux", windows));

/// Each served root's un-recorded count, by scoped id, off its state DB
/// (`fauna_sync_engine::db::set_park`, the read the app's park row makes). A DB
/// that will not read counts as holding one — fail closed: a root is never
/// retired over a state the agent could not see.
fn unrecorded_counts(served: &[EngineMapping], state_dir: &Path) -> BTreeMap<String, u32> {
    served
        .iter()
        .filter_map(|m| m.presence.as_ref().map(|p| (p, m.folder_ref)))
        .filter_map(|(p, folder_ref)| {
            let n = match fauna_sync_engine::db::set_park(state_dir, folder_ref) {
                Ok(read) => read.unrecorded,
                Err(e) => {
                    tracing::warn!(
                        error = %format!("{e:#}"),
                        "presence roots: a root's state DB did not read; it keeps serving"
                    );
                    1
                }
            };
            (n > 0).then(|| (p.scoped_id.clone(), n))
        })
        .collect()
}

/// What retiring one presence root removes — the decision, pure. Nothing at a
/// path a root still planned stands on (an access flip retires the old mapping
/// and restarts the engine at the same path); the backing and the set's state
/// DB only when no planned root and no bound location serves the set (a
/// renamed root keeps both, and a bound engine owns the state DB).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Removal {
    /// The presence path, removed once unmounted and only if empty.
    pub mount_point: Option<PathBuf>,
    /// The private backing, removed whole.
    pub backing: Option<PathBuf>,
    /// The set's state DB, so a set presented again starts from the nest
    /// rather than from rows whose files went with the backing.
    pub state_db: Option<FolderRef>,
}

pub(crate) fn removal_for(
    retired: &EngineMapping,
    planned: &[EngineMapping],
    bound_keys: &BTreeSet<String>,
) -> Removal {
    let Some(presence) = retired.presence.as_ref() else {
        return Removal::default();
    };
    if planned.iter().any(|m| m.path == retired.path) {
        return Removal::default();
    }
    let still_presented = planned.iter().any(|m| {
        m.presence
            .as_ref()
            .is_some_and(|p| p.scoped_id == presence.scoped_id)
    });
    let key = retired.engine_key();
    let set_gone = !still_presented && !bound_keys.contains(&key);
    Removal {
        mount_point: Some(retired.path.clone()),
        backing: presence.backing.clone().filter(|_| !still_presented),
        state_db: set_gone.then_some(retired.folder_ref),
    }
}

/// How long a retired root's binding may take to come down after its engine is
/// stopped — linux's view unmounted, windows' sync root unregistered — before
/// the removal gives up (the directory and backing are then left; the next
/// boot's ghost sweep takes a leftover binding down).
const UNMOUNT_WAIT: Duration = Duration::from_secs(30);

/// Carry out `removal` for `root`: wait for its binding to come down (the
/// stopped engine's guard unmounts the view, or unregisters the sync root —
/// the engine reconcile marked it for full teardown), read the drain once more
/// — a write through the binding between the plan and its teardown is
/// un-recorded and keeps everything where it is — then remove the presence
/// path, the backing and the state DB.
async fn remove_retired_root(state_dir: PathBuf, root: EngineMapping, removal: Removal) {
    let deadline = tokio::time::Instant::now() + UNMOUNT_WAIT;
    while still_bound(&root.path) {
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(
                path = %fauna_core::log_redact::log_path(&root.path.to_string_lossy()),
                "presence roots: a retired root's binding is still up; leaving its directory \
                 and backing in place"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let drained = fauna_sync_engine::db::set_park(&state_dir, root.folder_ref)
        .is_ok_and(|read| read.unrecorded == 0);
    let removal = if drained {
        removal
    } else {
        tracing::warn!(
            path = %fauna_core::log_redact::log_path(&root.path.to_string_lossy()),
            "presence roots: a retired root holds un-recorded content after its unmount; \
             keeping its backing and state DB (the set presented again serves them)"
        );
        Removal {
            mount_point: removal.mount_point,
            ..Removal::default()
        }
    };
    remove_root_files(&removal, root.folder_ref, &state_dir);
}

/// Remove what `removal` names, for the set `folder_ref`. The presence path
/// keeps everything that is not provably a copy of a version the nest recorded
/// ([`remove_recorded_copies`]) and goes only once nothing is left in it: on
/// linux it is empty once unmounted (the bytes are in the backing), on windows
/// it holds the hydrated copies cfapi kept in the root, and a user who put
/// something there keeps it. The backing and the state DB are the agent's own.
/// Each failure is logged and left.
pub(crate) fn remove_root_files(removal: &Removal, folder_ref: FolderRef, state_dir: &Path) {
    let warn = |what: &str, e: &std::io::Error| {
        tracing::warn!(error = %e, what, "presence roots: a retired root's removal step failed");
    };
    if let Some(path) = &removal.mount_point {
        // A directory the agent no longer serves never stays refused to its
        // owner: a reader's root is released first, whatever stays in it.
        #[cfg(windows)]
        crate::cfapi_host::release_reader_root(path);
        remove_recorded_copies(path, folder_ref, state_dir);
        if let Err(e) = std::fs::remove_dir(path)
            && e.kind() != std::io::ErrorKind::NotFound
            && e.kind() != std::io::ErrorKind::DirectoryNotEmpty
        {
            warn("presence path", &e);
        }
    }
    if let Some(backing) = &removal.backing
        && let Err(e) = std::fs::remove_dir_all(backing)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        warn("backing", &e);
    }
    if let Some(folder_ref) = removal.state_db {
        let db = folder_ref.state_db_path(state_dir);
        for suffix in ["", "-wal", "-shm"] {
            let mut file = db.clone().into_os_string();
            file.push(suffix);
            if let Err(e) = std::fs::remove_file(&file)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                warn("state DB", &e);
            }
        }
    }
}

/// Remove every file under `root` the set's state DB proves a copy of a version
/// the nest recorded — the disk's bytes are the recorded head's and no rule
/// keeps the body on this holder
/// (`fauna_sync_engine::engine::SyncEngine::is_dehydration_safe_in`, the gate
/// every free-up-space path answers) — then every directory left empty below
/// `root`. Anything else stays where it is: a file the engine never recorded,
/// one changed since, an ignored one, and every body of a group set (peers are
/// its only source, so none is ever safe to drop). A retired root never costs
/// the user a byte the nest cannot give back. Returns how many files it
/// removed.
pub(crate) fn remove_recorded_copies(
    root: &Path,
    folder_ref: FolderRef,
    state_dir: &Path,
) -> usize {
    if matches!(folder_ref, FolderRef::Group(_)) || !root.is_dir() {
        return 0;
    }
    let db_path = folder_ref.state_db_path(state_dir);
    if !db_path.exists() {
        return 0; // nothing was ever recorded here: nothing is proven
    }
    let db = match fauna_sync_engine::db::SyncDb::open(&db_path) {
        Ok(db) => db,
        Err(e) => {
            tracing::warn!(
                error = %format!("{e:#}"),
                "presence roots: a retired root's state DB did not open; keeping its files"
            );
            return 0;
        }
    };
    fn walk(db: &fauna_sync_engine::db::SyncDb, root: &Path, dir: &Path, removed: &mut usize) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                walk(db, root, &path, removed);
                let _ = std::fs::remove_dir(&path); // only if left empty
                continue;
            }
            if !kind.is_file() {
                continue; // a link is never the engine's
            }
            let Some(rel) = path
                .strip_prefix(root)
                .ok()
                .and_then(|r| r.to_str())
                .map(|r| r.replace('\\', "/"))
            else {
                continue;
            };
            if fauna_sync_engine::engine::SyncEngine::is_dehydration_safe_in(db, &rel, &path)
                && std::fs::remove_file(&path).is_ok()
            {
                *removed += 1;
            }
        }
    }
    let mut removed = 0;
    walk(&db, root, root, &mut removed);
    removed
}

/// Is this binding still up on `path` right now — a mount of it (linux), its
/// sync root still registered with the shell or the filter (windows)?
fn still_bound(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/mountinfo").is_ok_and(|mountinfo| {
            !crate::fuse_host::ghost_mount_points(&mountinfo, &[path.to_path_buf()]).is_empty()
        })
    }
    #[cfg(windows)]
    {
        crate::cfapi_host::still_registered(path)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = path;
        false
    }
}

/// What one pass reads, gathered before the config lock is taken.
struct PlanRead {
    actor_id: [u8; 32],
    sets: Vec<PresenceSet>,
    prefs: OnDemandPrefs,
    home: PathBuf,
}

async fn read_plan_inputs(state: &Arc<SyncServiceState>) -> anyhow::Result<PlanRead> {
    let (actor_id, backup_key, device_id_hex) = {
        let cap = state.capability.read().await;
        let cap = cap
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no capability provisioned"))?;
        let (Some(actor_id), Some(backup_key)) = (cap.actor_id_array(), cap.backup_key_array())
        else {
            anyhow::bail!("malformed capability");
        };
        (actor_id, backup_key, cap.device_id.to_ascii_lowercase())
    };
    let home = user_home().ok_or_else(|| anyhow::anyhow!("no absolute home directory"))?;
    let prefs = OnDemandPrefs::load(&prefs_path(&state.paths))?;
    let groups = held_group_scopes(state, actor_id).await?;
    let nest = state
        .nest_rpc_client()
        .await
        .map_err(|e| anyhow::anyhow!(e))?;
    // The list renders each set's sealed name, so the client carries the
    // account's label custody — the owner's `BackupKey` and the resolver over
    // the shared sets' custody. Without it a set whose name a writer's engine
    // has sealed renders as nothing and drops out of the plan, its root
    // retired (`FoldersClient::list_owned_and_shared`).
    let resolver: Arc<dyn fauna_core::folder_keys::FolderKeyResolver> =
        Arc::new(fauna_client_folders::NestFolderKeyResolver::new(
            Arc::clone(&nest),
            Arc::clone(&state.folder_keys),
        ));
    let client = fauna_client_folders::FoldersClient::new(nest).with_label_custody(
        fauna_core::label_custody::LabelCustody::new(
            Some(resolver),
            Some(fauna_core::crypto::BackupKey::from_bytes(backup_key)),
        ),
    );
    let sets = tokio::time::timeout(
        PLAN_READ_TIMEOUT,
        fauna_folders_machine::on_demand_presence::presence_sets_over(
            &client,
            &device_id_hex,
            &*state.folder_keys,
            &groups,
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("the plan read timed out"))??;
    Ok(PlanRead {
        actor_id,
        sets,
        prefs,
        home,
    })
}

/// The account's held group sets, off the store this agent has mounted,
/// through the strict reader the app-process door reads
/// (`fauna_sync_engine::offline_share::held_group_scope_views`). No store
/// mounted, or a scope that will not read, is an error — a pass that answered
/// fewer sets would retire their roots. A build with no share plane holds no
/// group set.
async fn held_group_scopes(
    state: &Arc<SyncServiceState>,
    actor_id: [u8; 32],
) -> anyhow::Result<Vec<HeldGroupScope>> {
    #[cfg(feature = "p2p-share")]
    {
        let store = state
            .mounted_store
            .lock()
            .ok()
            .and_then(|held| held.clone())
            .ok_or_else(|| anyhow::anyhow!("no account store mounted"))?;
        let views = fauna_sync_engine::offline_share::held_group_scope_views(
            &store,
            &fauna_core::identity::ActorId(actor_id),
        )
        .await?;
        Ok(views
            .into_iter()
            .map(|view| HeldGroupScope {
                scope_id: view.scope_id,
                name: view.name,
                short_id: view.short_id,
            })
            .collect())
    }
    #[cfg(not(feature = "p2p-share"))]
    {
        let _ = (state, actor_id);
        Ok(Vec::new())
    }
}

/// The user's home: `%USERPROFILE%` on windows, `$HOME` elsewhere — absolute
/// or nothing (a relative home would plant the roots under the agent's cwd).
pub(crate) fn user_home() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocationConfig;
    use fauna_folders_machine::on_demand_presence::PresenceRole;

    const ACTOR: [u8; 32] = [7; 32];

    fn set(id: i64, name: &str, read_only: bool) -> PresenceSet {
        PresenceSet {
            name: name.to_string(),
            set_name: name.to_string(),
            folder_id: FolderRef::Local(id).to_wire(),
            this_device_accepts: true,
            role: if read_only {
                PresenceRole::SharedWithMe
            } else {
                PresenceRole::Own
            },
            read_only,
        }
    }

    fn scoped(id: i64) -> String {
        FolderRef::Local(id).scoped_to(ACTOR).to_wire()
    }

    struct Fixture {
        sets: Vec<PresenceSet>,
        config: SyncConfig,
        prefs: OnDemandPrefs,
        prior: Vec<EngineMapping>,
        unrecorded: BTreeMap<String, u32>,
        case_insensitive: bool,
        backing: Option<PathBuf>,
    }

    impl Fixture {
        fn new(sets: Vec<PresenceSet>) -> Self {
            Self {
                sets,
                config: SyncConfig::default(),
                prefs: OnDemandPrefs::default(),
                prior: Vec::new(),
                unrecorded: BTreeMap::new(),
                case_insensitive: false,
                backing: Some(PathBuf::from("/state/presence")),
            }
        }

        fn plan(&self) -> Vec<EngineMapping> {
            plan_presence_roots(&PresenceInputs {
                actor_id: ACTOR,
                sets: &self.sets,
                config: &self.config,
                prefs: &self.prefs,
                home: Path::new("/home/u"),
                backing_root: self.backing.as_deref(),
                case_insensitive: self.case_insensitive,
                prior: &self.prior,
                unrecorded: &self.unrecorded,
            })
        }
    }

    /// Each root's path with `/` separators, whatever the platform joined it with
    /// (windows joins `Fauna` and the display name onto the home with `\`).
    fn paths(plan: &[EngineMapping]) -> Vec<String> {
        plan.iter()
            .map(|m| m.path.to_string_lossy().replace('\\', "/"))
            .collect()
    }

    #[test]
    fn each_set_gets_a_root_under_home_fauna_with_its_backing_and_mode() {
        let plan = Fixture::new(vec![set(1, "docs", false)]).plan();
        assert_eq!(plan.len(), 1);
        let m = &plan[0];
        assert_eq!(m.path, PathBuf::from("/home/u/Fauna/docs"));
        assert_eq!(m.folder, "docs");
        assert_eq!(m.folder_ref, FolderRef::Local(1));
        assert_eq!(m.mode, LocationMode::OnDemand);
        let p = m.presence.as_ref().unwrap();
        assert_eq!(p.scoped_id, scoped(1));
        assert_eq!(
            p.backing,
            Some(PathBuf::from("/state/presence").join(scoped(1)))
        );
        assert!(!p.read_only);
    }

    #[test]
    fn windows_keeps_no_backing() {
        let mut f = Fixture::new(vec![set(1, "docs", false)]);
        f.backing = None;
        assert_eq!(f.plan()[0].presence.as_ref().unwrap().backing, None);
    }

    #[test]
    fn a_bound_set_in_either_mode_has_no_root_parked_included() {
        let mut f = Fixture::new(vec![
            set(1, "a", false),
            set(2, "b", false),
            set(3, "c", false),
            set(4, "d", false),
        ]);
        let bind = |id: i64, mode: LocationMode, revoked: bool| {
            let mut location = LocationConfig {
                path: format!("/x/{id}"),
                mode,
                folder: Some(format!("s{id}")),
                folder_id: Some(FolderRef::Local(id).to_wire()),
                ..Default::default()
            };
            location.access_revoked = revoked;
            location
        };
        f.config.locations = vec![
            bind(1, LocationMode::Always, false),
            bind(2, LocationMode::OnDemand, false),
            bind(3, LocationMode::Always, true),
        ];
        assert_eq!(paths(&f.plan()), vec!["/home/u/Fauna/d"]);
    }

    #[test]
    fn a_toggled_off_set_has_no_root() {
        let mut f = Fixture::new(vec![set(1, "a", false), set(2, "b", false)]);
        f.prefs
            .set_enabled(&FolderRef::Local(1).scoped_to(ACTOR), false);
        assert_eq!(paths(&f.plan()), vec!["/home/u/Fauna/b"]);
    }

    #[test]
    fn a_set_whose_place_does_not_accept_has_no_root() {
        let mut s = set(1, "a", false);
        s.this_device_accepts = false;
        assert!(Fixture::new(vec![s]).plan().is_empty());
    }

    #[test]
    fn a_readers_root_carries_the_reader_flag() {
        let plan = Fixture::new(vec![set(1, "docs (alice)", true)]).plan();
        assert_eq!(plan[0].path, PathBuf::from("/home/u/Fauna/docs (alice)"));
        assert!(plan[0].presence.as_ref().unwrap().read_only);
    }

    #[test]
    fn clashing_names_take_suffixes_in_plan_order() {
        let f = Fixture::new(vec![
            set(1, "docs", false),
            set(2, "docs", false),
            set(3, "docs", false),
        ]);
        assert_eq!(
            paths(&f.plan()),
            vec![
                "/home/u/Fauna/docs",
                "/home/u/Fauna/docs (2)",
                "/home/u/Fauna/docs (3)"
            ]
        );
    }

    #[test]
    fn names_clash_case_insensitively_only_on_windows() {
        let mut f = Fixture::new(vec![set(1, "Docs", false), set(2, "docs", false)]);
        assert_eq!(
            paths(&f.plan()),
            vec!["/home/u/Fauna/Docs", "/home/u/Fauna/docs"]
        );
        f.case_insensitive = true;
        assert_eq!(
            paths(&f.plan()),
            vec!["/home/u/Fauna/Docs", "/home/u/Fauna/docs (2)"]
        );
    }

    #[test]
    fn a_separator_never_leaves_the_fauna_directory() {
        let f = Fixture::new(vec![set(1, "a/b", false), set(2, "..", false)]);
        assert_eq!(
            paths(&f.plan()),
            vec!["/home/u/Fauna/a_b", "/home/u/Fauna/_"]
        );
    }

    #[test]
    fn a_dropped_set_keeps_serving_until_it_drains() {
        let mut f = Fixture::new(vec![set(1, "a", false), set(2, "b", false)]);
        f.prior = f.plan();
        f.sets.truncate(1); // set 2 leaves the plan
        f.unrecorded.insert(scoped(2), 3);
        let held = f.plan();
        assert_eq!(paths(&held), vec!["/home/u/Fauna/b", "/home/u/Fauna/a"]);
        assert!(retired_presence_roots(&f.prior, &held).is_empty());

        f.unrecorded.insert(scoped(2), 0);
        let drained = f.plan();
        assert_eq!(paths(&drained), vec!["/home/u/Fauna/a"]);
        let retired = retired_presence_roots(&f.prior, &drained);
        assert_eq!(paths(&retired), vec!["/home/u/Fauna/b"]);
    }

    #[test]
    fn a_draining_roots_path_is_taken_by_no_new_root() {
        let mut f = Fixture::new(vec![set(2, "b", false)]);
        f.prior = f.plan();
        f.unrecorded.insert(scoped(2), 1);
        f.sets = vec![set(5, "b", false)]; // set 2 dropped, a new "b" arrives
        assert_eq!(
            paths(&f.plan()),
            vec!["/home/u/Fauna/b", "/home/u/Fauna/b (2)"]
        );
    }

    #[test]
    fn a_renamed_set_moves_once_its_old_root_drains() {
        let mut f = Fixture::new(vec![set(1, "old", false)]);
        f.prior = f.plan();
        f.sets = vec![set(1, "new", false)];
        f.unrecorded.insert(scoped(1), 2);
        assert_eq!(paths(&f.plan()), vec!["/home/u/Fauna/old"]);
        f.unrecorded.clear();
        let moved = f.plan();
        assert_eq!(paths(&moved), vec!["/home/u/Fauna/new"]);
        assert_eq!(
            paths(&retired_presence_roots(&f.prior, &moved)),
            vec!["/home/u/Fauna/old"]
        );
    }

    #[test]
    fn a_demotion_reaches_a_draining_root() {
        let mut f = Fixture::new(vec![set(1, "a", false)]);
        f.prior = f.plan();
        f.sets = vec![set(1, "a", true)];
        f.unrecorded.insert(scoped(1), 1);
        assert!(f.plan()[0].presence.as_ref().unwrap().read_only);
    }

    #[test]
    fn a_kept_root_is_not_retired() {
        let mut f = Fixture::new(vec![set(1, "a", false)]);
        f.prior = f.plan();
        let again = f.plan();
        assert_eq!(again, f.prior);
        assert!(retired_presence_roots(&f.prior, &again).is_empty());
    }

    /// A set the plan drops is removed whole once drained: its presence path,
    /// its backing and its state DB.
    #[test]
    fn a_dropped_root_is_removed_whole() {
        let mut f = Fixture::new(vec![set(1, "a", false)]);
        f.prior = f.plan();
        f.sets.clear();
        let next = f.plan();
        let retired = retired_presence_roots(&f.prior, &next);
        assert_eq!(retired.len(), 1);
        assert_eq!(
            removal_for(&retired[0], &next, &BTreeSet::new()),
            Removal {
                mount_point: Some(PathBuf::from("/home/u/Fauna/a")),
                backing: Some(PathBuf::from("/state/presence").join(scoped(1))),
                state_db: Some(FolderRef::Local(1)),
            }
        );
    }

    /// An access flip retires the old mapping at the SAME path: nothing is
    /// removed — the engine restarts there and remounts.
    #[test]
    fn an_access_flip_removes_nothing() {
        let mut f = Fixture::new(vec![set(1, "a", false)]);
        f.prior = f.plan();
        f.sets = vec![set(1, "a", true)];
        let next = f.plan();
        let retired = retired_presence_roots(&f.prior, &next);
        assert_eq!(retired.len(), 1, "the flip retires the old mapping");
        assert_eq!(
            removal_for(&retired[0], &next, &BTreeSet::new()),
            Removal::default()
        );
    }

    /// A renamed set's old path goes; the backing and the state DB the new
    /// root serves from stay.
    #[test]
    fn a_renamed_root_keeps_its_backing_and_state_db() {
        let mut f = Fixture::new(vec![set(1, "old", false)]);
        f.prior = f.plan();
        f.sets = vec![set(1, "new", false)];
        let next = f.plan();
        let retired = retired_presence_roots(&f.prior, &next);
        assert_eq!(
            removal_for(&retired[0], &next, &BTreeSet::new()),
            Removal {
                mount_point: Some(PathBuf::from("/home/u/Fauna/old")),
                ..Removal::default()
            }
        );
    }

    /// A set the user binds subtracts its root: the bound engine owns the
    /// state DB, so only the presence path and the backing go.
    #[test]
    fn a_bound_sets_root_leaves_the_state_db_to_the_bind() {
        let mut f = Fixture::new(vec![set(1, "a", false)]);
        f.prior = f.plan();
        f.sets.clear();
        let next = f.plan();
        let retired = retired_presence_roots(&f.prior, &next);
        let bound = BTreeSet::from([FolderRef::Local(1).to_wire()]);
        let removal = removal_for(&retired[0], &next, &bound);
        assert!(removal.mount_point.is_some() && removal.backing.is_some());
        assert_eq!(removal.state_db, None);
    }

    /// The removal itself: the empty presence path, the backing whole, the
    /// state DB and its WAL pair; a presence path the user filled while it
    /// was unmounted is kept.
    #[test]
    fn removing_a_root_takes_its_files_and_keeps_a_users_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let mount_point = tmp.path().join("Fauna").join("a");
        let backing = state_dir.join("presence").join("x");
        std::fs::create_dir_all(&mount_point).unwrap();
        std::fs::create_dir_all(backing.join("sub")).unwrap();
        std::fs::write(backing.join("sub/f.txt"), b"x").unwrap();
        let folder_ref = FolderRef::Local(1);
        let db = folder_ref.state_db_path(&state_dir);
        std::fs::write(&db, b"db").unwrap();
        let mut wal = db.clone().into_os_string();
        wal.push("-wal");
        std::fs::write(&wal, b"wal").unwrap();

        remove_root_files(
            &Removal {
                mount_point: Some(mount_point.clone()),
                backing: Some(backing.clone()),
                state_db: Some(folder_ref),
            },
            folder_ref,
            &state_dir,
        );
        assert!(!mount_point.exists());
        assert!(!backing.exists());
        assert!(!db.exists());
        assert!(!Path::new(&wal).exists());

        std::fs::create_dir_all(&mount_point).unwrap();
        std::fs::write(mount_point.join("mine.txt"), b"keep").unwrap();
        remove_root_files(
            &Removal {
                mount_point: Some(mount_point.clone()),
                ..Removal::default()
            },
            folder_ref,
            &state_dir,
        );
        assert!(mount_point.join("mine.txt").exists());
    }

    /// A retired root whose binding keeps its bytes in the root itself (windows'
    /// cfapi) loses only the files its state DB proves copies of a recorded
    /// version: a changed copy, a file never recorded, a body this holder wrote
    /// and keeps, and every group set's body stay — and so does the directory
    /// holding them.
    #[test]
    fn a_retired_root_drops_only_its_proven_copies() {
        use fauna_sync_engine::db::{ProofOrigin, SyncDb, SyncState};
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        let root = tmp.path().join("Fauna").join("docs");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("emptied")).unwrap();
        let folder_ref = FolderRef::Local(7);
        let db = SyncDb::open(folder_ref.state_db_path(&state_dir)).unwrap();
        let record = |rel: &str, body: &[u8], origin: ProofOrigin| {
            let path = root.join(rel);
            std::fs::write(&path, body).unwrap();
            let hash = fauna_core::chunker_stream::content_hash_streaming(&path).unwrap();
            db.upsert_entry(
                rel,
                Some(hash),
                Some(hash),
                Some(hash),
                SyncState::Synced,
                0,
                0,
                body.len() as i64,
                1,
                None,
            )
            .unwrap();
            db.stamp_recorded_content_from_local(rel, origin).unwrap();
        };
        record("copy.txt", b"fetched", ProofOrigin::Fetched);
        record("emptied/deep.txt", b"fetched too", ProofOrigin::Fetched);
        record("sub/changed.txt", b"recorded", ProofOrigin::Fetched);
        record("own.txt", b"written here", ProofOrigin::OwnRecord);
        std::fs::write(root.join("sub/changed.txt"), b"changed since").unwrap();
        std::fs::write(root.join("never.txt"), b"never recorded").unwrap();
        drop(db);

        assert_eq!(remove_recorded_copies(&root, folder_ref, &state_dir), 2);
        assert!(!root.join("copy.txt").exists(), "a proven copy goes");
        assert!(
            !root.join("emptied").exists(),
            "a directory left empty goes"
        );
        assert!(
            root.join("sub/changed.txt").exists(),
            "a changed copy stays"
        );
        assert!(
            root.join("never.txt").exists(),
            "a file never recorded stays"
        );
        assert!(
            root.join("own.txt").exists(),
            "a body the holder keeps stays"
        );

        let group = FolderRef::Group([3; 32]);
        let kept = tmp.path().join("group");
        std::fs::create_dir_all(&kept).unwrap();
        std::fs::write(kept.join("g.txt"), b"peers only").unwrap();
        assert_eq!(remove_recorded_copies(&kept, group, &state_dir), 0);
        assert!(
            kept.join("g.txt").exists(),
            "a group set's body is never dropped"
        );
    }
}
