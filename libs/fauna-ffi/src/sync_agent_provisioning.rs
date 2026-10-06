//! UniFFI surface for the desktop **sync-agent provisioning loop** — the FaunaKit
//! (macOS) adapter over the shared control client
//! (`fauna_client_sync::agent::SyncAgentProvisioner`; `sync-agent.md` § Control
//! plane split + § Credential model; plan D3/D4, milestone A4).
//!
//! ## Why a thin adapter (not a re-implementation)
//!
//! The convergence loop (`fauna_ipc::convergence`), the socket delegate, the
//! renewal-grant setup, and the folder-binding verbs are all **shared Rust**
//! (`fauna_client_sync::agent`) — linux GTK and fauna-tui consume them directly
//! (no FFI hop), and the C# `HydrationSessionService` mirrors the same
//! cross-process contract. This module only adapts the two genuinely
//! platform-specific hooks across the UniFFI boundary via `with_foreign` traits:
//! how to *spawn* the agent ([`FfiAgentSpawner`], a `launchctl` kick /
//! LaunchAgent self-install on macOS), where the app's current *bearer* comes
//! from ([`FfiProvisioningBearerSource`]) — plus the optional agent-up edge
//! observer ([`FfiAgentReachabilityObserver`], the folder-UI reconcile re-fire
//! cue).
//!
//! The agent's **pushed-event** read side rides the same shared-Rust rule: the
//! filter + self-healing listener thread are `fauna_ipc::events` (linux runs
//! them directly); [`spawn_sync_event_listener`] adapts only the notification
//! callback ([`FfiSyncCompleteObserver`]) across the boundary.
//!
//! ## The local seam, per platform
//!
//! The agent seam resolves through one endpoint type,
//! [`fauna_ipc::endpoint::AgentEndpoint`]: a per-user unix socket on macOS/linux,
//! the per-SID named pipe on windows. So the module is `#[cfg(any(unix, windows))]`
//! — present (and identical) on every apple slice for flat-binding consistency,
//! dead code on iOS/watchOS/android (no agent there), and **exported into the
//! windows C# bindings**, which consume it exactly as Swift does since the C#
//! wire-pinned codec retired (`sync-agent.md` § Consumers, ratified 2026-07-24).
//! The Go mail-bridge `--no-default-features` build drops it via the default-on
//! `sync-agent-provisioning` feature gate.

use std::future::Future;
use std::sync::{Arc, Mutex};

use fauna_client::NestClient;
use fauna_client_sync::agent::{
    AgentCapabilityInputs, AgentSpawner, BindingState, LocationBindingsModel,
    ProvisioningBearerSource, ReachabilityObserver, SyncAgentProvisioner,
    signed_out_onboarding_reconcile as shared_signed_out_onboarding_reconcile,
};
#[cfg(feature = "test-helpers")]
use fauna_client_sync::agent_spawner::ChildSpawner;
#[cfg(windows)]
use fauna_client_sync::agent_spawner::WindowsDetachedSpawner;
use fauna_ipc::sync::BearerToken;

use crate::FfiError;

impl From<fauna_client_sync::agent::AgentControlError> for FfiError {
    fn from(e: fauna_client_sync::agent::AgentControlError) -> Self {
        FfiError::General { msg: e.to_string() }
    }
}

/// Swift-provided hook to **launch the agent** when the socket probe finds it
/// absent. On macOS this ensures the `social.fauna.sync-agent` LaunchAgent is
/// installed (`.dmg`-only installs self-install it here) and bootstrapped /
/// kickstarted; the loop grants a spawn grace afterward before leaning on it.
/// Best-effort — a failed spawn just means the next tick retries.
#[uniffi::export(with_foreign)]
pub trait FfiAgentSpawner: Send + Sync {
    /// Start the agent if it is not already running. Called from the convergence
    /// loop's probe step when the socket connect fails.
    fn spawn_agent(&self);
}

/// The **e2e** spawner: direct-spawns the pinned agent binary
/// ([`fauna_client_sync::agent_spawner::AGENT_BIN_ENV`]) as a reaped child of
/// the app, instead of touching the machine-global `social.fauna.sync-agent`
/// LaunchAgent.
///
/// This is the macOS arm of the shape linux already ships — production installs
/// and kicks the platform's service manager, e2e spawns a private child — and
/// it reuses linux's very [`ChildSpawner`] rather than growing a second copy
/// (priority #2; linux reaches it via `SystemdUserUnitSpawner::new(e2e)`).
///
/// It exists because BOTH halves were wrong before: a test launch must never
/// bootstrap the developer's real LaunchAgent (testing.md point 10), but
/// skipping the provisioner outright — what `FaunaMacApp` did under
/// `FaunaE2E.isActive` — left the macOS seat with a bound folder and no sync
/// behind it in either direction (multiseat run 20260724-02).
///
/// Isolation here is by construction, not convention: the agent's control
/// socket is `~/Library/Application Support/Fauna/sync-agent.sock`
/// (`fauna_ipc::unix_transport`) and the child inherits the launch's isolated
/// `HOME`/`CFFIXED_USER_HOME`, so app and agent rendezvous on a per-launch
/// socket — the same mechanism that makes linux's private `XDG_RUNTIME_DIR`
/// work. Pair it with [`AGENT_BIN_ENV`] so the child is the freshly built agent
/// and never the box's installed one.
///
/// **Gated on `test-helpers` (testing.md convention 15), and it is the seam's
/// BEHAVIOUR that earns the gate, not its name.** Its spawn resolves the binary
/// through [`AGENT_BIN_ENV`], which is taken "verbatim and unconditionally" — so
/// an exported constructor for it is a process-execution redirect handed to
/// whoever controls the launch environment, the same class as (and strictly
/// stronger than) the provider base-URL redirect that convention 15's
/// implementation-status section records. Both automated seam witnesses key on a
/// symbol's *name*, and nothing here is named like a seam, so neither would ever
/// have flagged it. Feature-only, never the profile, per the convention's
/// FFI-export rule: the generated Swift/Kotlin/C# face stays a pure function of
/// the feature set. The Swift call site is `FaunaE2E`-guarded already; when
/// apple's production/test recipe split lands it must guard this construction at
/// compile time too.
#[cfg(feature = "test-helpers")]
#[derive(Default, uniffi::Object)]
pub struct FfiChildAgentSpawner {
    child: ChildSpawner,
}

#[cfg(feature = "test-helpers")]
#[uniffi::export]
impl FfiChildAgentSpawner {
    /// Construct the e2e child spawner. Using it on a non-test launch is the
    /// caller's mistake to avoid — production must keep the launchd spawner.
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[cfg(feature = "test-helpers")]
#[uniffi::export]
impl FfiAgentSpawner for FfiChildAgentSpawner {
    fn spawn_agent(&self) {
        self.child.spawn();
    }
}

/// The **production windows** spawner: the shared
/// [`WindowsDetachedSpawner`], exported so the C# app can name it without
/// owning any spawn code of its own.
///
/// It is the counterpart of macOS's Swift `LaunchdSyncAgentSpawner` — but where
/// launchd bootstrapping genuinely belongs on the Swift side, windows' whole
/// resolution (`AGENT_BIN_ENV` pin → `<exe-dir>` → `<exe-dir>\..` →
/// `%ProgramFiles%\Fauna`), its harness-isolation argv and its
/// `CREATE_NO_WINDOW` flag were **already lifted into shared Rust** by the
/// 2026-07-24 design record's D3, precisely so retiring the C# stack loses no
/// behavior. Without this export the retirement could not complete: the
/// provisioner factory takes a `with_foreign` [`FfiAgentSpawner`], so C# would
/// have to re-implement `SpawnSyncAgentDetached` — the exact per-app twin D3
/// deleted (priorities #1/#2).
///
/// **Deliberately NOT `test-helpers`-gated**, unlike [`FfiChildAgentSpawner`].
/// The gate there is earned by *behavior* (that spawner execs
/// `FAUNA_E2E_SYNC_AGENT_BIN` verbatim — a process-execution redirect, testing.md
/// convention 15). This one is the production path, and its own env forwards are
/// compiled out of a release build inside `WindowsDetachedSpawner`
/// (`#[cfg(any(test, debug_assertions, feature = "test-helpers"))]`), so a
/// shipped artifact resolves only real installed layouts and passes no argv.
#[cfg(windows)]
#[derive(Default, uniffi::Object)]
pub struct FfiWindowsDetachedSpawner {
    inner: WindowsDetachedSpawner,
}

#[cfg(windows)]
#[uniffi::export]
impl FfiWindowsDetachedSpawner {
    /// Construct the windows production spawner. One per provisioner; it holds
    /// the live-child gate that stops a slow-binding agent getting a sibling on
    /// every convergence tick.
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[cfg(windows)]
#[uniffi::export]
impl FfiAgentSpawner for FfiWindowsDetachedSpawner {
    fn spawn_agent(&self) {
        AgentSpawner::spawn_agent(&self.inner);
    }
}

/// Swift-provided source of the app's **current nest bearer**, read fresh each
/// convergence tick and pushed to the agent (`RefreshBearer`). Mirrors the File
/// Provider host's [`crate::FfiBearerProvider`]: a fast local read the identity app
/// keeps current. An empty token ⇒ not authenticated (the tick then skips).
#[uniffi::export(with_foreign)]
pub trait FfiProvisioningBearerSource: Send + Sync {
    /// The bearer the app currently holds for this actor's nest connection.
    fn current_bearer(&self) -> FfiProvisioningBearer;
}

/// Swift-provided observer of the **agent-reachability false→true edge**
/// (`ProvisioningDelegate::agent_became_reachable`): the cue to re-drive the
/// folder-binding reconcile (attach-time reconcile alone races the very first
/// agent spawn/bootstrap — the 2026-07-19 A4 review finding). Called from the
/// convergence loop's tokio task; hop to the main actor yourself.
#[uniffi::export(with_foreign)]
pub trait FfiAgentReachabilityObserver: Send + Sync {
    fn on_agent_reachable(&self);
}

/// A nest bearer token plus its expiry — unix seconds on this device's clock,
/// anchored at receipt (`login.md` § Token lifetime on the client's clock): the
/// deadline the app's own mint recorded, never the nest's absolute
/// `expires_at`. The FFI record [`FfiProvisioningBearerSource`] returns; empty
/// `token` ⇒ none (its `expires_at` is then ignored).
#[derive(uniffi::Record)]
pub struct FfiProvisioningBearer {
    pub token: String,
    pub expires_at: u64,
}

/// One agent-side sync folder row ([`Self::list_locations`]) — the UI fields of
/// `fauna_ipc::sync::LocationInfo`. `folder` is `None` for an added-but-unbound
/// folder; `mode` is the wire string (`"always"` | `"on-demand"`).
///
/// [`Self::list_locations`]: FfiSyncAgentProvisioner::list_locations
#[derive(uniffi::Record)]
pub struct FfiAgentLocation {
    pub path: String,
    pub folder: Option<String>,
    /// The bound set's `FolderRef` wire form — `Some` exactly when `folder`
    /// is; the key the bindings model confirms and adopts rows by.
    pub folder_id: Option<String>,
    pub mode: String,
    /// Mirrors `LocationInfo::access_revoked` — the owner withdrew this
    /// binding's write grant, so the agent parked it (no engine runs for it).
    /// The client renders `folder-access-revoked-warning` on a writer's
    /// shared-set row when this is `true` (`file-sync.md` § Multi-writer
    /// shared sets — D4).
    pub access_revoked: bool,
}

/// The `sync-agent-status` health state (`sync-agent.md` § Local agent health) —
/// the FFI face of `fauna_client_sync::agent::AgentHealthState`.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiAgentHealthState {
    /// `GetServiceStatus` itself failed — no local socket / connect error.
    NotRunning,
    /// Reachable, but the running process reports a version older than this
    /// build — an update replaced the on-disk binary without the still-running
    /// agent picking it up.
    RestartPending,
    /// Reachable and running the same version as this build.
    Running,
    /// Reachable, but the agent reports a bound set whose engine is behind the
    /// set's content-key floor, or keyless (its writes are held until custody
    /// holds the generation). Outranks `RestartPending`. Appended last so the earlier
    /// variants keep their UniFFI ordinals.
    KeysPending,
    /// Reachable, but the agent reports the nest holds no grant for this
    /// machine any more, so nothing syncs once the app is closed; signing in
    /// again on this machine re-enrolls it. Outranks `KeysPending`. Appended
    /// last so the earlier variants keep their UniFFI ordinals.
    NotEnrolled,
}

impl From<fauna_client_sync::agent::AgentHealthState> for FfiAgentHealthState {
    fn from(s: fauna_client_sync::agent::AgentHealthState) -> Self {
        use fauna_client_sync::agent::AgentHealthState as S;
        match s {
            S::NotRunning => Self::NotRunning,
            S::RestartPending => Self::RestartPending,
            S::KeysPending => Self::KeysPending,
            S::NotEnrolled => Self::NotEnrolled,
            S::Running => Self::Running,
        }
    }
}

/// A `GetServiceStatus` probe already mapped through the shared health-state
/// derivation ([`Self::state`](FfiAgentStatus::state) via
/// [`FfiSyncAgentProvisioner::agent_health`]) — the `sync-agent-status`/
/// `-version`/`-uptime` global shell elements read straight off this.
/// `version`/`uptime_secs` are empty/`0` while `state` is `NotRunning`.
#[derive(uniffi::Record)]
pub struct FfiAgentStatus {
    pub state: FfiAgentHealthState,
    pub version: String,
    pub uptime_secs: u64,
}

/// The agent's per-device sync signal (`GetServiceStatus` → `SyncStatusInfo`) — the
/// Status page's Connected / Syncing / queue-depth readout.
///
/// Distinct from [`FfiAgentStatus`], which answers "is the agent PROCESS healthy" for the
/// `sync-agent-status` shell element; this answers "what is it DOING". A client needing
/// both makes both calls rather than one growing a union of the two questions.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiAgentSyncStatus {
    /// The agent's transport to the nest is up.
    pub connected: bool,
    /// At least one engine is actively transferring.
    pub syncing: bool,
    /// Files still queued across every engine.
    pub files_pending: u64,
    /// Bytes still queued across every engine.
    pub bytes_pending: u64,
    /// Unix seconds of the last completed transfer / clean pass, or `None` when the
    /// agent has never finished one (`sync-agent.md` § Local agent health — the
    /// two-stamp `last_sync`).
    pub last_sync: Option<u64>,
}

/// Where a rendered binding row stands relative to the agent — the FFI face of
/// `fauna_client_sync::agent::BindingState`.
#[derive(uniffi::Enum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfiBindingState {
    /// The agent has it (seen in `ListLocations`, or a bind push succeeded).
    Confirmed,
    /// Local truth not yet on the agent. Stays **rendered** and re-pushes on
    /// every reconcile until confirmed — a failed push must never vanish.
    PendingBind,
    /// A local remove not yet pushed: hidden from render, re-pushes until the
    /// agent no longer lists it.
    PendingUnbind,
}

impl From<BindingState> for FfiBindingState {
    fn from(s: BindingState) -> Self {
        match s {
            BindingState::Confirmed => Self::Confirmed,
            BindingState::PendingBind => Self::PendingBind,
            BindingState::PendingUnbind => Self::PendingUnbind,
        }
    }
}

/// One folder↔folder row the bindings UI renders
/// ([`FfiLocationBindingsModel::rendered`]).
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiBindingRow {
    pub path: String,
    pub folder: String,
    /// The bound set's `FolderRef` in wire form — the row's identity, and the
    /// key agent rows and engine holds join on (`folder` is a label two sets
    /// can share).
    pub folder_id: String,
    pub state: FfiBindingState,
    /// The owner revoked this binding's write grant, so the agent parked it —
    /// render `folder-access-revoked-warning` rather than letting the folder go
    /// quiet (`file-sync.md` § Multi-writer shared sets — D4).
    pub access_revoked: bool,
    /// How many deletes the **mass-delete floor** is holding on this row's set —
    /// render `folder-location-deletes-held` plus the
    /// `folder-location-apply-deletes-button` verb while it is non-zero, and
    /// neither at `0` (`delete-propagation.md` § A wholesale-vanished folder is
    /// infrastructure failure).
    ///
    /// Kept current by [`FfiLocationBindingsModel::fold_engine_holds`] over
    /// [`FfiSyncAgentProvisioner::list_engine_holds`] — a *watch*, not a
    /// reconcile hook: the hold is derived inside the agent on its own rescan
    /// cadence, so no user gesture produces it.
    pub deletes_held: u64,
    /// How many deletes the delete rail withheld on this row's set because part
    /// of it could not be read — render `folder-location-unreadable` while
    /// non-zero, with **no** action (`delete-propagation.md` § Unreadable is
    /// not absent). Folded by the same
    /// [`FfiLocationBindingsModel::fold_engine_holds`] as the hold.
    #[uniffi(default = 0)]
    pub deletes_skipped_unreadable: u64,
}

/// One set's mass-delete-floor hold, as [`FfiSyncAgentProvisioner::list_engine_holds`]
/// reports it — the `ListEngines` roster narrowed to what the bindings UI needs.
/// Deliberately not the whole `EngineInfo`: the rest of that row (backlog,
/// re-seal progress) has its own consumers and its own additive evolution, and a
/// record carrying fields no caller reads is a record every caller has to
/// re-read on every change.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiEngineHold {
    /// The set the engine serves (`EngineInfo::folder`) — a label.
    pub folder: String,
    /// The served set's `FolderRef` wire form (`EngineInfo::folder_id`) — the
    /// key [`FfiBindingRow::folder_id`] joins on.
    pub folder_id: String,
    pub deletes_held: u64,
    /// The set's unreadable-path count (`EngineInfo::deletes_skipped_unreadable`).
    /// Defaulted so a caller constructing a hold record keeps compiling.
    #[uniffi(default = 0)]
    pub deletes_skipped_unreadable: u64,
}

/// What an [`FfiSyncAgentProvisioner::apply_held_deletes`] actually did — the
/// wire reply, which is the ONLY honest post-apply count: the agent re-derives
/// the missing set at click time, so the number the button rendered may already
/// be wrong (`delete-propagation.md` § Implementation status today, the verb half).
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiHeldDeletesApplied {
    /// Deletes actually recorded to the nest by this call.
    pub applied: u64,
    /// Still held afterwards — `0` retracts the surface; non-zero keeps the
    /// affordance up (a crash mid-apply, or a floor that re-engaged).
    pub remaining_held: u64,
    /// Whether the floor was still engaged when the agent re-derived. `false`
    /// means the files came back (or the vanish was only partial): the call
    /// refuses and touches nothing, which is what stops the verb from becoming a
    /// general force-delete API.
    pub floor_was_active: bool,
}

/// One binding a reconcile pass wants pushed — the arguments of
/// [`FfiSyncAgentProvisioner::bind_location`] as a named record, so the three
/// strings cannot be transposed silently.
#[derive(uniffi::Record, Debug, Clone, PartialEq, Eq)]
pub struct FfiPendingBind {
    pub path: String,
    pub folder: String,
    pub folder_id: String,
}

impl From<fauna_client_sync::agent::PendingBind> for FfiPendingBind {
    fn from(b: fauna_client_sync::agent::PendingBind) -> Self {
        Self {
            path: b.path,
            folder: b.folder,
            folder_id: b.folder_id,
        }
    }
}

/// What a reconcile pass wants pushed to the agent.
#[derive(uniffi::Record, Debug, Default, PartialEq, Eq)]
pub struct FfiReconcileActions {
    /// Push each via [`FfiSyncAgentProvisioner::bind_location`], then
    /// [`FfiLocationBindingsModel::confirm_bind`] on success.
    pub to_bind: Vec<FfiPendingBind>,
    /// Push each via [`FfiSyncAgentProvisioner::unbind_location`], then
    /// [`FfiLocationBindingsModel::confirm_unbind`] on success.
    pub to_unbind: Vec<String>,
}

/// The shared **optimistic** folder-binding state machine
/// (`fauna_client_sync::agent::LocationBindingsModel`) across the UniFFI boundary —
/// optimistic local rows reconciled against the agent's `ListLocations` truth,
/// with union semantics: a failed push stays rendered and re-pushes, an agent
/// config reset re-pushes rather than erasing the user's bindings, and a row
/// bound from another control surface is adopted.
///
/// **Pure — no IO.** The caller drives the socket verbs on
/// [`FfiSyncAgentProvisioner`] and reports outcomes back via
/// [`Self::confirm_bind`] / [`Self::confirm_unbind`]; a failed push simply stays
/// pending for the next reconcile. Drive it at attach, after each user mutation,
/// and on every [`FfiAgentReachabilityObserver::on_agent_reachable`] edge —
/// **never only once**.
///
/// Exported so C# consumes the same state machine linux and fauna-tui link
/// directly, rather than hand-porting a third copy of it (priority #2; the goal
/// doc's "the shared `LocationBindingsModel`'s optimistic semantics",
/// `sync-agent.md` § Implementation status today). Interior mutability because
/// UniFFI objects expose `&self` methods only.
#[derive(uniffi::Object)]
pub struct FfiLocationBindingsModel {
    inner: Mutex<LocationBindingsModel>,
}

#[uniffi::export]
impl FfiLocationBindingsModel {
    /// An empty model — every row arrives by [`Self::reconcile`] adoption or a
    /// user [`Self::add`]. (The `seeded` constructor that took the pre-cutover
    /// `location-map.json` rows, name-only, was retired 2026-09-24 with the
    /// name-keyed binding — the compat-remnant sweep.)
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(LocationBindingsModel::default()),
        })
    }

    /// The rows the UI renders: everything except pending unbinds. A
    /// pending-bind row **renders** — an add made while the agent is down must
    /// stay visible.
    pub fn rendered(&self) -> Vec<FfiBindingRow> {
        self.lock()
            .rendered()
            .into_iter()
            .map(|r| FfiBindingRow {
                path: r.path,
                folder: r.folder,
                folder_id: r.folder_id,
                state: r.state.into(),
                access_revoked: r.access_revoked,
                deletes_held: r.deletes_held,
                deletes_skipped_unreadable: r.deletes_skipped_unreadable,
            })
            .collect()
    }

    /// Fold the agent's per-set mass-delete-floor holds
    /// ([`FfiSyncAgentProvisioner::list_engine_holds`]) onto the rendered rows.
    ///
    /// A **join, not a copy** — the hold is reported per set and rendered per
    /// bound path, so two folders bound to one set both carry it — and a
    /// **mirror**: a set absent from the roster reads `0`, because a stopped
    /// engine, a restarted agent and an agent too old to report the field all
    /// mean "no live engine stands behind this count".
    pub fn fold_engine_holds(&self, holds: Vec<FfiEngineHold>) {
        let engines: Vec<fauna_ipc::sync::EngineInfo> = holds
            .into_iter()
            .map(|h| fauna_ipc::sync::EngineInfo {
                folder: h.folder,
                folder_id: h.folder_id,
                deletes_held: h.deletes_held,
                deletes_skipped_unreadable: h.deletes_skipped_unreadable,
                ..Default::default()
            })
            .collect();
        self.lock().fold_engine_holds(&engines);
    }

    /// Repaint **one** set's hold — what an
    /// [`FfiSyncAgentProvisioner::apply_held_deletes`] reply carries back in
    /// `remaining_held`. Distinct from [`Self::fold_engine_holds`], which is a
    /// whole-roster mirror and would blank every other set.
    pub fn set_engine_hold(&self, folder: String, held: u64) {
        self.lock().set_engine_hold(&folder, held);
    }

    /// Optimistic user add, keyed by the set's identity — the `FolderRef` wire
    /// string [`crate::folders_client::folder_ref_for_row`] produces from the
    /// summary the surface picked the set from. A row that yields no ref is
    /// refused by the caller, never added. Replaces any existing row for the
    /// same path; returns the binding to push.
    pub fn add(&self, path: String, folder: String, folder_id: String) -> FfiPendingBind {
        self.lock().add(path, folder, folder_id).into()
    }

    /// Optimistic user remove, by folder — linux's row key, where a row *is* a
    /// bound set. Returns the paths to unbind.
    pub fn remove_by_set(&self, folder: String) -> Vec<String> {
        self.lock().remove_by_set(&folder)
    }

    /// Optimistic user remove, by path — **windows'** row key, where the per-row
    /// `folder-location-remove-button` acts on one folder. Use this from a per-path
    /// UI: two folders bound to the same set are one [`Self::remove_by_set`] but
    /// two distinct removes here, so the set-keyed verb would silently unbind a
    /// sibling row the user never touched.
    pub fn remove_by_path(&self, path: String) -> Vec<String> {
        self.lock().remove_by_path(&path)
    }

    /// A bind push succeeded.
    pub fn confirm_bind(&self, path: String) {
        self.lock().confirm_bind(&path);
    }

    /// An unbind push succeeded (or the agent stopped listing the row).
    pub fn confirm_unbind(&self, path: String) {
        self.lock().confirm_unbind(&path);
    }

    /// Reconcile against the agent's [`FfiSyncAgentProvisioner::list_locations`]
    /// truth, returning what to push. See the type docs for the union semantics.
    pub fn reconcile(&self, agent_rows: Vec<FfiAgentLocation>) -> FfiReconcileActions {
        let actions = self.lock().reconcile(&agent_location_rows(agent_rows));
        FfiReconcileActions {
            to_bind: actions.to_bind.into_iter().map(Into::into).collect(),
            to_unbind: actions.to_unbind,
        }
    }

    /// Mirror the agent's binding **park** (`access_revoked` on
    /// [`FfiSyncAgentProvisioner::list_locations`]'s rows) onto the rows this
    /// model already holds — the watch half of D4 revocation, called on the
    /// agent-status tick beside [`Self::fold_engine_holds`] (`file-sync.md`
    /// § Multi-writer shared sets → *Revocation*). The park is derived inside
    /// the agent, so no gesture and no reachability edge re-drives
    /// [`Self::reconcile`] when it changes. A mirror of that one flag, never a
    /// reconcile: it pushes nothing and adopts nothing.
    pub fn fold_parks(&self, agent_rows: Vec<FfiAgentLocation>) {
        self.lock().fold_parks(&agent_location_rows(agent_rows));
    }
}

/// The FFI agent rows as the wire `LocationInfo` the shared model reads. Only
/// `path`/`folder`/`folder_id`/`access_revoked` are read by the reconcile and
/// the park fold; the rest of the wire row is irrelevant here, so it is built
/// off `Default` (the struct derives one precisely so additive growth cannot
/// break a hand-listed construction — `LocationInfo`'s own note).
fn agent_location_rows(agent_rows: Vec<FfiAgentLocation>) -> Vec<fauna_ipc::sync::LocationInfo> {
    agent_rows
        .into_iter()
        .map(|r| fauna_ipc::sync::LocationInfo {
            path: r.path,
            folder: r.folder,
            folder_id: r.folder_id,
            mode: r.mode,
            access_revoked: r.access_revoked,
            ..Default::default()
        })
        .collect()
}

impl FfiLocationBindingsModel {
    /// The model is pure and every method is short, so a poisoned lock can only
    /// mean a panic *inside* one of them — recover rather than cascade a second
    /// panic through the FFI boundary, where it would abort instead of unwind.
    fn lock(&self) -> std::sync::MutexGuard<'_, LocationBindingsModel> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Adapter: the shared [`AgentSpawner`] hook over the Swift-provided
/// [`FfiAgentSpawner`].
struct SpawnerAdapter(Arc<dyn FfiAgentSpawner>);

impl AgentSpawner for SpawnerAdapter {
    fn spawn_agent(&self) {
        self.0.spawn_agent();
    }
}

/// Adapter: the shared [`ProvisioningBearerSource`] hook over the Swift-provided
/// [`FfiProvisioningBearerSource`] (a fast synchronous property read app-side).
struct BearerAdapter(Arc<dyn FfiProvisioningBearerSource>);

impl ProvisioningBearerSource for BearerAdapter {
    fn current_bearer(&self) -> impl Future<Output = Option<BearerToken>> + Send {
        let source = Arc::clone(&self.0);
        async move {
            let b = source.current_bearer();
            if b.token.is_empty() {
                None
            } else {
                Some(BearerToken::new(b.token, b.expires_at))
            }
        }
    }
}

/// Adapter: the shared [`ReachabilityObserver`] hook over the Swift-provided
/// [`FfiAgentReachabilityObserver`].
struct ReachabilityAdapter(Arc<dyn FfiAgentReachabilityObserver>);

impl ReachabilityObserver for ReachabilityAdapter {
    fn on_agent_reachable(&self) {
        self.0.on_agent_reachable();
    }
}

/// The macOS/desktop app's **control channel to the external `fauna-sync-agent`**:
/// capability provisioning *and* device-local folder↔folder binding
/// (`sync-agent.md` § Control plane split — the folder *binding* is device-local
/// IPC; the file *set* is nest policy).
///
/// Constructed off the identity session
/// ([`FfiNestClient::sync_agent_provisioner`](crate::FfiNestClient::sync_agent_provisioner)),
/// it (1) mints + registers a `RenewBearer` device grant on the nest so the agent
/// can self-renew its bearer app-dead (`sync-agent.md` § Credential model), then
/// (2) runs the shared convergence loop over the unix socket:
/// probe → spawn-if-absent → `RefreshBearer` → full `ProvisionCapability` on
/// `NoCapability`. Folder binding ([`Self::bind_location`] / [`Self::unbind_location`]
/// / [`Self::list_locations`]) drives the agent's own `config.toml` over the same
/// socket — post-cutover the in-app engine host is one-shot-only, so the agent is
/// the sole owner of resident engines and their folder set. Sign-out /
/// account-switch calls [`Self::unprovision`], which stops the loop and tears down
/// the agent's persisted capability.
///
/// All of the above is the shared `fauna_client_sync::agent::SyncAgentProvisioner`
/// — this object is the UniFFI adapter over it.
#[derive(uniffi::Object)]
pub struct FfiSyncAgentProvisioner {
    /// Shared, not owned: the share plane's agent adapters
    /// (`fauna_sync_engine::share_glue::agent_share_access`) hold the same
    /// provisioner for the plane's lifetime (`crate::share_plane`).
    inner: Arc<SyncAgentProvisioner<Arc<NestClient>, BearerAdapter>>,
}

impl FfiSyncAgentProvisioner {
    /// The share plane's two agent adapters (`GetShareServeInfo` /
    /// `ShareIngest`) over this provisioner — the one call a share-plane host
    /// makes, exactly as tui and linux make it over theirs.
    #[cfg(feature = "p2p-share")]
    pub(crate) fn share_access(&self) -> fauna_sync_engine::share_glue::ReplicaAccess {
        fauna_sync_engine::share_glue::agent_share_access(Arc::clone(&self.inner))
    }

    #[allow(clippy::too_many_arguments)] // capability inputs + three hooks; a struct would just relocate them
    pub(crate) fn build(
        nest: Arc<NestClient>,
        identity_secret: Vec<u8>,
        backup_key: Vec<u8>,
        // Retired owner `BackupKey`s of the identities this account succeeded
        // from — resolve ONCE post-auth via
        // `FfiAccountRegistry::predecessor_backup_keys` and pass the same list
        // here and to any label-custody equivalent, mirroring tui's
        // `session.rs` post-auth hook. Empty for every identity that never
        // succeeded (today's overwhelmingly common case) and fails **closed**
        // until the caller wires it: a successor's pre-succession corpus stays
        // unopenable rather than opening under a wrong root. `sync-agent.md`
        // § Credential model → *Retired owner keys after an identity
        // succession*.
        predecessor_backup_keys: Vec<Vec<u8>>,
        // The **attested** actor ids beside the keys above — resolve ONCE
        // post-auth via `FfiAccountRegistry::attested_predecessor_actor_ids`
        // and pass the same list here and to `start_account_runtime`
        // (`account-data-taxonomy.md` § The generation machinery → *The
        // source of `prior`*, ruled 2026-09-13). Empty for every identity
        // that never succeeded. Validated 32-bytes-each by the shared
        // `SyncAgentProvisioner::new` below — no separate check needed here.
        predecessor_actor_ids: Vec<Vec<u8>>,
        // The app's account registry, for the paired walk
        // (`FfiAccountRegistry::paired_predecessor_keys`). `None` sends no
        // pairs.
        accounts: Option<&crate::accounts_registry::FfiAccountRegistry>,
        device_id: String,
        device_label: String,
        spawner: Arc<dyn FfiAgentSpawner>,
        bearer_source: Arc<dyn FfiProvisioningBearerSource>,
        reachability_observer: Option<Arc<dyn FfiAgentReachabilityObserver>>,
    ) -> Result<Arc<Self>, FfiError> {
        let nest_url = nest.nest_url();
        let predecessor_keys_by_actor =
            accounts.map_or_else(Vec::new, |a| a.paired_predecessor_keys(&identity_secret));
        let inner = SyncAgentProvisioner::new(
            nest,
            AgentCapabilityInputs {
                identity_secret,
                backup_key,
                predecessor_backup_keys,
                // The caller's attested-ids walk, passed through unconditionally —
                // empty is fail-safe at the agent: a successor's
                // predecessor-signed enrollments drop out of its fleet view;
                // nothing is admitted.
                predecessor_actor_ids,
                // Empty until the app hands its registry: its agent then
                // opens with the unpaired keys, which a row signed as a
                // predecessor is never offered (fail-closed — a noted skip).
                predecessor_keys_by_actor,
                device_id,
                device_label,
                nest_url,
            },
            Arc::new(SpawnerAdapter(spawner)),
            Arc::new(BearerAdapter(bearer_source)),
            reachability_observer
                .map(|o| Arc::new(ReachabilityAdapter(o)) as Arc<dyn ReachabilityObserver>),
        )?;
        let inner = Arc::new(inner);
        // The devices page's participation switch asks the agent — the usual
        // engine holder — for the pass that reads the row (`p2p.md`
        // § Per-device participation, (c)); weak, so it lapses with this
        // provisioner.
        #[cfg(feature = "account-runtime")]
        {
            use fauna_client_account_runtime::p2p_participation::{
                EngineHolderNudge, EngineHolderNudgeSlot,
            };
            let holder_nudge: Arc<dyn EngineHolderNudge> = inner.clone();
            EngineHolderNudgeSlot::seat().publish(&holder_nudge);
        }
        Ok(Arc::new(Self { inner }))
    }
}

#[fauna_uniffi_async::export]
impl FfiSyncAgentProvisioner {
    /// Register the renewal grant (one-time) and start the convergence loop. Safe to
    /// call once per active session; a second call while running just pokes the loop.
    pub async fn start(&self) -> Result<(), FfiError> {
        Ok(self.inner.start().await?)
    }

    /// Nudge the loop to converge now (sign-in, a new folder binding, a nest move).
    pub fn poke(&self) {
        self.inner.poke();
    }

    /// Bind a local folder to a nest folder on the agent: an idempotent
    /// `AddLocation` (a fresh path takes the agent's platform default mode —
    /// always-resident on macOS, where a bound location outranks the File
    /// Provider domain; on-demand on windows — the agent's
    /// `LocationMode::fresh_binding_default`; re-adding an existing path is a
    /// no-op that keeps its persisted mode) followed by `SetLocationFolder`.
    /// Two separate exchanges so each checks its own result.
    /// The agent reconciles engines off its own config, so a successful bind
    /// starts the set's engine agent-side; `poke()` is not required.
    ///
    /// Keyed by the set's `FolderRef` wire string
    /// (`crate::folders_client::folder_ref_for_row` produces it) — the
    /// unambiguous key the agent resolves this binding's engine key material,
    /// engine-registry entry and state DB by, since a set name is unique only
    /// per owner (`on-demand-files.md` § Hosting multiple on-demand folders).
    /// A row the resolver yields no ref for is refused by the caller, never
    /// bound by name: the name-keyed bind was retired 2026-09-24 (the
    /// compat-remnant sweep).
    pub async fn bind_location(
        &self,
        path: String,
        folder: String,
        folder_id: String,
    ) -> Result<(), FfiError> {
        Ok(self.inner.bind_location(path, folder, folder_id).await?)
    }

    /// Unbind a folder on the agent (`RemoveLocation`): its engine stops and the
    /// binding is forgotten; the nest folder and the engine's state DB are
    /// untouched, so re-binding resumes instead of re-uploading.
    pub async fn unbind_location(&self, path: String) -> Result<(), FfiError> {
        Ok(self.inner.unbind_location(path).await?)
    }

    /// The agent's per-device sync signal (`GetServiceStatus` → `SyncStatusInfo`) — what
    /// the Status page renders as Connected / Syncing / files+bytes pending / last sync.
    ///
    /// `None` when the agent is unreachable (not running, not installed — the ordinary
    /// case, since on-demand sync is opt-in), so a caller renders its defaults rather
    /// than an error. Use [`Self::agent_health`] for the *process* health tri-state; this
    /// is the work signal.
    pub async fn sync_status(&self) -> Option<FfiAgentSyncStatus> {
        let info = self.inner.get_service_status().await.ok()?;
        Some(FfiAgentSyncStatus {
            connected: info.sync.connected,
            syncing: info.sync.syncing,
            files_pending: info.sync.files_pending,
            bytes_pending: info.sync.bytes_pending,
            last_sync: info.sync.last_sync,
        })
    }

    /// Nudge the agent's resident engine serving `folder` to pull remote changes
    /// **now** (`PullFolderNow`), off its rescan cadence — sent when this client
    /// receives a `PushEvent::SyncChanged` for the set (`file-sync.md` § Remote-change
    /// nudge; the twin of linux `sync_agent::pull_set_now` and tui
    /// `SyncAgentState::pull_set_now`).
    ///
    /// Best-effort agent-side (a set with no resident engine, or one whose pull is
    /// already pending, is a silent no-op there) — but "best-effort" means *latency*,
    /// not *optional*: without it the only delivery path left is the 300 s rescan tick,
    /// so a second device's save takes minutes rather than seconds to appear.
    ///
    /// `folder_hash` is the push's own `folder_hash`, relayed as received: the
    /// agent matches its bindings by it, so a sealed set's nudge (blank
    /// `folder`) still reaches its engine.
    pub async fn pull_folder_now(
        &self,
        folder: String,
        folder_hash: Option<Vec<u8>>,
    ) -> Result<(), FfiError> {
        Ok(self.inner.pull_folder_now(folder, folder_hash).await?)
    }

    /// Set a bound folder's sync mode (`SetLocationSyncMode`) — `always` keeps every
    /// file materialized, `on-demand` serves placeholders that hydrate on access.
    /// Backs the per-row `folder-location-mode-toggle`, which today only **windows**
    /// offers (its cfapi placeholder host is the one shipped on-demand root;
    /// `file-sync.md` § On-Demand Files).
    ///
    /// Orthogonal to binding: re-binding does not reset a folder's mode, and
    /// toggling mode does not re-push a binding — so this is its own verb rather
    /// than an argument of [`Self::bind_location`].
    pub async fn set_location_sync_mode(&self, path: String, mode: String) -> Result<(), FfiError> {
        Ok(self.inner.set_location_sync_mode(path, mode).await?)
    }

    /// Every hosted engine's mass-delete-floor hold (`ListEngines`, narrowed) —
    /// the input to [`FfiLocationBindingsModel::fold_engine_holds`], and the
    /// only channel by which an app learns a bound folder emptied
    /// (`delete-propagation.md` § A wholesale-vanished folder is infrastructure
    /// failure).
    ///
    /// Call it on the app's existing agent-status tick, not on a reconcile hook:
    /// the hold is derived inside the agent on its own rescan cadence, so no
    /// user gesture and no reachability edge produces it. An unreachable agent
    /// errors, and the honest fold for that is an empty roster.
    pub async fn list_engine_holds(&self) -> Result<Vec<FfiEngineHold>, FfiError> {
        Ok(self
            .inner
            .list_engines()
            .await?
            .into_iter()
            .map(|e| FfiEngineHold {
                folder: e.folder,
                folder_id: e.folder_id,
                deletes_held: e.deletes_held,
                deletes_skipped_unreadable: e.deletes_skipped_unreadable,
            })
            .collect())
    }

    /// Apply the deletes the mass-delete floor is holding on `folder` — the
    /// explicit user action behind `folder-location-apply-deletes-button`.
    /// Propagation of a held set is never automatic.
    ///
    /// ⚠ **Pass the set, never the count the UI rendered.** The agent re-derives
    /// what is missing at click time, so a confirm racing a remount deletes
    /// nothing; repaint from the reply
    /// ([`FfiLocationBindingsModel::set_engine_hold`] with its `remaining_held`),
    /// never from the number the button was labelled with.
    pub async fn apply_held_deletes(
        &self,
        folder: String,
    ) -> Result<FfiHeldDeletesApplied, FfiError> {
        let info = self.inner.apply_held_deletes(folder).await?;
        Ok(FfiHeldDeletesApplied {
            applied: info.applied,
            remaining_held: info.remaining_held,
            floor_was_active: info.floor_was_active,
        })
    }

    /// The agent's current sync folders (`ListLocations`) — the truth the
    /// Settings → Sync bindings UI renders post-cutover (the in-app one-shot-only
    /// host has no resident bindings to list).
    pub async fn list_locations(&self) -> Result<Vec<FfiAgentLocation>, FfiError> {
        Ok(self
            .inner
            .list_locations()
            .await?
            .into_iter()
            .map(|f| FfiAgentLocation {
                path: f.path,
                folder: f.folder,
                folder_id: f.folder_id,
                mode: f.mode,
                access_revoked: f.access_revoked,
            })
            .collect())
    }

    /// Stop the loop and tear down the agent's provisioned capability
    /// (`UnprovisionCapability`): the agent deletes its persisted credential-store
    /// record and stops engines. Idempotent — safe on an already-stopped
    /// provisioner. Drives sign-out and account-switch teardown
    /// (consumes the same op).
    pub async fn unprovision(&self) -> Result<(), FfiError> {
        Ok(self.inner.unprovision().await?)
    }

    /// The local agent's own process health (`GetServiceStatus`), mapped
    /// through the shared health-state derivation
    /// (`fauna_client_sync::agent::agent_health_state` — `sync-agent.md` §
    /// Local agent health), *Keys pending* included — the agent reports it on
    /// the same reply — feeds the `sync-agent-status` global shell element. A connect failure (agent not running) is not an error here:
    /// it maps to [`FfiAgentHealthState::NotRunning`], matching how linux and
    /// windows treat the same probe. `local_build_version` is this client's
    /// own compiled-in version (Swift has no `env!` — pass
    /// [`fauna_ffi_build_version`](crate::fauna_ffi_build_version)).
    pub async fn agent_health(&self, local_build_version: String) -> FfiAgentStatus {
        let result = self.inner.get_service_status().await;
        let state =
            fauna_client_sync::agent::agent_health_state(&result, &local_build_version).into();
        let status = result.ok();
        FfiAgentStatus {
            state,
            version: status
                .as_ref()
                .map(|s| s.version.clone())
                .unwrap_or_default(),
            uptime_secs: status.as_ref().map(|s| s.uptime_secs).unwrap_or(0),
        }
    }
}

/// This device's **custodian store** over the agent seam — the desktop half of
/// the reclaim affordance (`docs/goal/ui/backups.md` § Manage backup
/// destinations → *Reclaim this device's copy*).
///
/// Its own `impl` block, and gated, for the same reason
/// [`FfiNestClient::payments`](crate::FfiNestClient::payments) is: the two
/// records it speaks live in the `backup-destinations` module, so a build
/// without that face (the Go mail-bridge's `--no-default-features`) must not
/// carry these two methods either.
///
/// # Why the agent and not this process's disk
///
/// The mobile shells reach their own store directly
/// ([`custodian_store_footprint`](crate::custodian_store_footprint) /
/// [`reclaim_custodian_store`](crate::reclaim_custodian_store)) because the app
/// process *is* the custodian host there. On a desktop the external
/// `fauna-sync-agent` hosts the replica, so the store's location is the agent's
/// to resolve (`sync-agent.md` § Control plane split) and only the agent can
/// promise a reclaim does not delete bytes out from under its own live pull
/// pass. macOS therefore rides this seam, exactly as linux and windows do
/// natively — this block is only the UniFFI face they never needed.
#[cfg(feature = "backup-destinations")]
#[fauna_uniffi_async::export]
impl FfiSyncAgentProvisioner {
    /// What this device's sealed custodian store occupies on disk
    /// (`GetCustodianStore`) — the read behind `backup-orphaned-store-row`.
    ///
    /// Reports only what is there. **Whether it is orphaned** is the caller's
    /// call, made against the destination rows the page already holds through
    /// the shared `fauna_core::data::custodian_store_is_orphaned` — never
    /// re-derived here, and never from `custodian_assignment_for(..).is_none()`,
    /// which answers `None` for two rows naming this device and would offer to
    /// delete live custody.
    pub async fn custodian_store(&self) -> Result<crate::FfiCustodianStoreInfo, FfiError> {
        let info = self.inner.custodian_store().await?;
        Ok(crate::FfiCustodianStoreInfo {
            generations: info.generations,
            files: info.files,
            bytes: info.bytes,
            source_regressions: info
                .source_regressions
                .into_iter()
                .map(|r| crate::FfiCustodianSourceRegression {
                    ledger: r.ledger,
                    held: r.held,
                    served: r.served,
                    observed_at: r.observed_at,
                })
                .collect(),
        })
    }

    /// Free this device's whole sealed custodian store (`ReclaimCustodianStore`)
    /// — the confirmed `backup-destination-reclaim-button` action, and the
    /// `backup-destination-remove-reclaim-checkbox` opt-in.
    ///
    /// Awaited, not fire-and-forget, unlike the binding mutations on this seam:
    /// it is a destructive gesture behind a confirm modal, so the page repaints
    /// on what actually happened — including the
    /// [`still_hosting`](crate::FfiCustodianReclaimOutcome::still_hosting)
    /// refusal, which is a **reported outcome, not an error**: nothing was
    /// deleted, the store is intact, and the page's own orphaned row is both the
    /// honest state and the way to retry. The await is also what lets a test
    /// assert state rather than timing (`testing.md` convention 14).
    pub async fn reclaim_custodian_store(
        &self,
    ) -> Result<crate::FfiCustodianReclaimOutcome, FfiError> {
        let outcome = self.inner.reclaim_custodian_store().await?;
        Ok(crate::FfiCustodianReclaimOutcome {
            still_hosting: outcome.still_hosting,
            freed_files: outcome.freed_files,
            freed_bytes: outcome.freed_bytes,
        })
    }

    /// Restore the signed-in nest from this device's copy — the confirmed
    /// `backup-destination-reseed-confirm-button` action on a desktop
    /// (`backup-destinations.md` § Re-seed → *Where the ceremony runs*: the
    /// agent hosts the store, so the agent runs the ceremony).
    ///
    /// The shared app half, whole: start the agent's job and wait for its own
    /// terminal state (`fauna_client_sync::reseed_wire::await_agent_reseed`),
    /// then the shared post-ceremony duty ([`crate::reseed`]'s `finish`). linux
    /// and tui run the same two calls, so the order cannot drift per app.
    ///
    /// `nest` is the owner's connection to the nest being seeded — what the
    /// re-enrollment writes through. `this_device_id` is the stable sync device
    /// id, hex, the re-enrolled row names. A stop comes back as
    /// [`crate::FfiReseedResult::stopped`], never as an error: nothing was made
    /// live, and the page phrases it.
    pub async fn reseed_custodian_store(
        &self,
        nest: Arc<crate::FfiNestClient>,
        owner_secret: Vec<u8>,
        this_device_id: String,
    ) -> Result<crate::FfiReseedResult, FfiError> {
        use fauna_client_sync::reseed_wire::{AGENT_RESEED_POLL, await_agent_reseed};
        let secret = crate::crypto::secret32(&owner_secret)?;
        // Held in its own zeroize-on-drop type; the one bare copy is the IPC
        // frame's, which zeroizes itself too.
        let key = fauna_core::crypto::NestBackupKey::derive(&secret);
        // The target pre-create runs here, in the seed-holding app, over this
        // connection and the account's custody (`writer-signed-change-records.md`
        // ruling (7)(a)(i)) — the phone host's twin in `custodian_host`.
        let ws = nest.nest_arc();
        let prepare = |names: Vec<String>| async move {
            #[cfg(feature = "folders-author")]
            {
                let files = fauna_client_folders::FoldersClient::new(Arc::clone(&ws));
                let custody = crate::account_runtime::folder_key_store();
                fauna_client_folders::prepare_reseed_targets_logged(&files, &*custody, &names)
                    .await;
            }
            #[cfg(not(feature = "folders-author"))]
            let _ = (&ws, names);
        };
        let result = await_agent_reseed(
            self.inner.as_ref(),
            prepare,
            key.to_bytes(),
            AGENT_RESEED_POLL,
        )
        .await;
        drop(key);
        Ok(match result {
            Ok(outcome) => crate::reseed::finish(&nest, outcome, &this_device_id).await,
            Err(stop) => crate::FfiReseedResult::stopped(stop.to_string()),
        })
    }
}

/// What one poked custodian pull pass did — the FFI twin of
/// [`fauna_ipc::sync::CustodianPassReport`], field-for-field, and the reply
/// [`FfiSyncAgentProvisioner::custodian_run_pass_now`] returns. **Test-only**,
/// same gate as its sole producer below: a shipped build carries neither the
/// record nor the call that fills it.
#[cfg(any(test, feature = "test-helpers"))]
#[derive(uniffi::Record)]
pub struct FfiCustodianPassReport {
    /// Whether this process was hosting a custodian replica at all. `false` is
    /// a meaningful negative, not an error — the device is not enrolled, or the
    /// host loop has not yet re-read the registry.
    pub hosting: bool,
    /// How many backed-up kinds the pass drove.
    pub kinds_run: u32,
    /// Bytes this device holds after the pass, as reported to the nest.
    pub held_bytes: u64,
    /// The pass's own cap verdict (`CAP_STATE_OK` / `CAP_STATE_REACHED`).
    pub cap_state: Option<String>,
    /// The self-audit verdict this pass reported, when it ran one. `None` is
    /// *not audited on this pass*, never a failed audit.
    pub audit_state: Option<String>,
    /// Whether the pass's check-in reached the source nest.
    pub checked_in: bool,
}

/// Run ONE custodian pull pass on this device's hosted replica and report what
/// it did (`CustodianRunPassNow`) — the windows/desktop twin of linux's
/// (`apps/fauna-linux/src/main.rs`'s `custodian_pull_run_now` arm) and tui's
/// (`apps/fauna-tui/src/automation.rs`) direct `SyncAgentProvisioner` calls,
/// and the seam the windows `custodian_pull_run_now` TestAgent command drives.
/// It is the causal barrier the tier_3 enroll→pull→check-in→status proof rests
/// on
/// (`test_a_hosted_custodian_pulls_checks_in_and_the_owners_row_reports_it`) —
/// never a settle-sleep (testing.md convention 14). `now_offset_secs` shifts
/// the clock this one pass runs at (`0` = the real one); see
/// [`fauna_client_sync::agent::SyncAgentProvisioner::custodian_run_pass_now`]
/// for why an offset rather than an absolute `now`.
///
/// **Own impl block, own extra gate.** `custodian_store`/`reclaim_custodian_store`
/// above need only `backup-destinations`; this one ALSO needs
/// `any(test, feature = "test-helpers")` — earned by *behavior* exactly as
/// [`FfiChildAgentSpawner`]'s gate is (this call only exists to drive the
/// windows/tui/linux `custodian_pull_run_now` TestAgent command), not the
/// broader `debug_assertions` its two underlying pieces carry in
/// `fauna_client_sync::agent` and `fauna_ipc::sync` (testing.md convention
/// 15) for THEIR OWN direct-Rust-caller debug builds — mirroring that wider
/// gate here made an ordinary `apple-ffi-host` debug (non-test-helpers)
/// build fail the flavor-diff seam witness (e2e-conventions.md point 15),
/// since that flavor is never built `--release`. `any(test, feature =
/// "test-helpers")` still keeps a shipped `--release` build from exporting
/// either the call or [`FfiCustodianPassReport`].
#[cfg(all(feature = "backup-destinations", any(test, feature = "test-helpers")))]
#[fauna_uniffi_async::export]
impl FfiSyncAgentProvisioner {
    pub async fn custodian_run_pass_now(
        &self,
        now_offset_secs: i64,
    ) -> Result<FfiCustodianPassReport, FfiError> {
        let report = self.inner.custodian_run_pass_now(now_offset_secs).await?;
        Ok(FfiCustodianPassReport {
            hosting: report.hosting,
            kinds_run: report.kinds_run,
            held_bytes: report.held_bytes,
            cap_state: report.cap_state,
            audit_state: report.audit_state,
            checked_in: report.checked_in,
        })
    }
}

/// Swift-provided consumer of the agent's **completed-sync pushed events** —
/// the per-file desktop-notification surface (`sync-agent.md` § Control plane
/// split, seam op 3 "pushed events"; the macOS parity of linux's
/// `notify_sync_complete`). The Synced filter runs in shared Rust
/// (`fauna_ipc::events::synced_filename`), so this only ever sees basenames of
/// completed uploads.
#[uniffi::export(with_foreign)]
pub trait FfiSyncCompleteObserver: Send + Sync {
    /// A file finished uploading (`FileStatusChanged{Synced}`); `filename` is
    /// the basename. Called from the listener's background thread — hop to the
    /// main actor yourself if the surface needs it (`UNUserNotificationCenter`
    /// does not).
    fn on_sync_complete(&self, filename: String);
}

/// Handle over the shared self-healing event-listener thread
/// (`fauna_ipc::events::spawn_event_listener` — the same loop the linux GTK
/// client runs in-process). Hold it for the post-auth session; [`Self::stop`]
/// (or dropping the last reference) ends the thread best-effort.
#[derive(uniffi::Object)]
pub struct FfiSyncAgentEventListener {
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl FfiSyncAgentEventListener {
    fn spawn_at(
        endpoint: fauna_ipc::endpoint::AgentEndpoint,
        observer: Arc<dyn FfiSyncCompleteObserver>,
    ) -> Result<Arc<Self>, FfiError> {
        let stop = fauna_ipc::events::spawn_event_listener_at(endpoint, move |filename| {
            observer.on_sync_complete(filename)
        })
        .map_err(|e| FfiError::General {
            msg: format!("sync event listener spawn failed: {e}"),
        })?;
        Ok(Arc::new(Self { stop }))
    }
}

#[uniffi::export]
impl FfiSyncAgentEventListener {
    /// Signal the listener thread to stop. Best-effort: it may be parked in a
    /// blocking event read and only notices on its next wake (an event or a
    /// socket error). Idempotent; also runs when the object drops.
    pub fn stop(&self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

impl Drop for FfiSyncAgentEventListener {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Spawn the shared agent event listener on this user's default agent endpoint
/// and forward every completed-sync basename to `observer`. Self-healing (retries
/// while the agent is down; reconnects across agent restarts), so it is
/// started once at post-auth alongside the provisioner and stopped on the
/// same teardown paths. E2E launches must not call this — the default endpoint
/// is the *real* user's agent (testing.md § point 10).
///
/// Resolves via [`AgentEndpoint::default_for_user`], not the unix socket path it
/// hard-coded before windows had a consumer: on windows that is the per-SID pipe
/// (or the `FAUNA_E2E_SYNC_PIPE` leaf), which is what makes this the same call on
/// both desktops (design record D6 note 2).
///
/// [`AgentEndpoint::default_for_user`]: fauna_ipc::endpoint::AgentEndpoint::default_for_user
#[uniffi::export]
pub fn spawn_sync_event_listener(
    observer: Arc<dyn FfiSyncCompleteObserver>,
) -> Result<Arc<FfiSyncAgentEventListener>, FfiError> {
    let endpoint =
        fauna_ipc::endpoint::AgentEndpoint::default_for_user().map_err(|e| FfiError::General {
            msg: format!("agent endpoint unresolvable: {e}"),
        })?;
    FfiSyncAgentEventListener::spawn_at(endpoint, observer)
}

/// FaunaKit's call site for `sync-agent.md` § Credential model → *The
/// signed-out reconcile*, shape (a): once per launch when the
/// app determines it has no account and renders the onboarding surface, this
/// nudges a reachable agent still serving THIS machine's just-signed-out
/// account to drop its capability immediately, instead of waiting on the
/// agent's own renewal-loop cadence (bounded at 300 s). linux
/// (`spawn_signed_out_onboarding_reconcile`) and tui (`launch::route`'s
/// `WizardAt` arm) are the built references; the guard that keeps a
/// co-resident sibling account safe lives entirely in
/// [`fauna_client_sync::agent::signed_out_onboarding_reconcile`] — this face
/// only resolves the endpoint.
///
/// Best-effort and silent: an unresolvable agent endpoint (no local
/// `SignedOutMarker`, unreachable agent, or — on iOS/watchOS, which have no
/// agent — `AgentEndpoint::default_for_user()` itself erroring, since neither
/// carries `$XDG_RUNTIME_DIR`) is exactly the "ambiguous read" case the
/// shared guard already treats as a no-op, so this face needs no platform
/// branch of its own; it is present on every apple slice for the same
/// flat-binding-consistency reason the rest of this module is (module doc).
///
/// Callers gate this on **not** an append-style launch (adding a second
/// identity from a running, authenticated session) — see linux's
/// `build_onboarding_window_inner` for why: the agent may still be serving
/// that first, live account.
#[fauna_uniffi_async::export]
pub async fn signed_out_onboarding_reconcile() {
    let Ok(endpoint) = fauna_ipc::endpoint::AgentEndpoint::default_for_user() else {
        return;
    };
    shared_signed_out_onboarding_reconcile(&endpoint).await;
}

/// Whether the local agent is **running** in the sense the `data.sync` e2e state
/// block means it: at least one hosted engine reports `serving`
/// ([`fauna_client_sync::agent::any_engine_serving_cached`], the same derivation
/// linux's `sync_running()` and fauna-tui's `running()` call — one shared answer,
/// not a third hand-rolled one).
///
/// An unreachable agent is `false`, never an error: "not running" is precisely
/// what a caller wants to hear when the connect fails.
///
/// ⚠ **This face does NOT block, and that is load-bearing rather than an
/// optimisation** — it is the reason [`sync_agent_any_engine_serving_cached`] below
/// is safe to build on top of. It used to `spawn_blocking` the 6 s-bounded
/// [`fauna_client_sync::agent::any_engine_serving`], which is what forced windows
/// and macOS to each hand-roll a cache on top; the cache now lives once in the
/// shared crate underneath this face (`e2e-latency-independent-assertions.md` §
/// Implementation status today — convention 14's windows/macOS build-out). macOS's
/// local cache (`SyncEngineServingProbe`) is deleted and
/// windows' (`AppDataSnapshot`'s former `SyncAgentServingCached`) is deleted too
/// — **both e2e state-block assemblies now call the
/// sync twin below directly, so this `async` face has NO remaining callers.** Left
/// in place rather than deleted: it is still a correct, cheap, non-blocking UniFFI
/// export, and a synchronous call site is the exception (windows/macOS' state
/// providers are the only ones that cannot `await`), not the rule — a future
/// `await`-shaped caller (Android/Kotlin, a new async assembly) should reach for
/// this one rather than re-deriving it.
///
/// **Convention 15 gating lives on the CALLER, not here** — this is an ordinary
/// control-plane read, so it stays ungated exactly as the shared function and
/// linux's `sync_running()` do; it is the *state-JSON assembly* that consumes it
/// which carries `#if DEBUG || FAUNA_E2E_AGENT` (C#) / `#[cfg(any(debug_assertions,
/// feature = "e2e-agent"))]` (linux `main.rs::sync_state_json`).
#[fauna_uniffi_async::export]
pub async fn sync_agent_any_engine_serving() -> bool {
    fauna_client_sync::agent::any_engine_serving_cached()
}

/// Synchronous twin of [`sync_agent_any_engine_serving`] — identical
/// non-blocking cache-only read, callable from a context that cannot `await`.
/// **The one both e2e state-block assemblies actually call today** — macOS'
/// `FaunaMacApp.serializeData` and windows' `AppDataSnapshot.GetSyncForState`,
/// neither of which can `await` without cascading `async` through its whole
/// call path (macOS: a raw blocking-socket accept thread bridged onto the main
/// thread synchronously, `onMainActor`'s `DispatchQueue.main.sync`, no Swift
/// Concurrency in scope; windows: `SerializeState` is a plain
/// `Func<Dictionary<string, object?>>` the `TestAgent` holds, not a `Task`-returning
/// one).
#[uniffi::export]
pub fn sync_agent_any_engine_serving_cached() -> bool {
    fauna_client_sync::agent::any_engine_serving_cached()
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::sync::Mutex;

    use super::*;

    /// The FFI enum must mirror the shared `AgentHealthState` variant-for-variant
    /// (`sync-agent.md` § Local agent health) — the derivation logic itself is
    /// pinned in `fauna_client_sync::agent`; this only guards the FFI face's
    /// mapping from silently going stale against it.
    #[test]
    fn ffi_agent_health_state_mirrors_the_shared_enum() {
        use fauna_client_sync::agent::AgentHealthState as S;
        assert_eq!(
            FfiAgentHealthState::from(S::NotRunning),
            FfiAgentHealthState::NotRunning
        );
        assert_eq!(
            FfiAgentHealthState::from(S::RestartPending),
            FfiAgentHealthState::RestartPending
        );
        assert_eq!(
            FfiAgentHealthState::from(S::KeysPending),
            FfiAgentHealthState::KeysPending
        );
        assert_eq!(
            FfiAgentHealthState::from(S::NotEnrolled),
            FfiAgentHealthState::NotEnrolled
        );
        assert_eq!(
            FfiAgentHealthState::from(S::Running),
            FfiAgentHealthState::Running
        );
    }

    fn agent_row(parked: bool) -> FfiAgentLocation {
        FfiAgentLocation {
            path: "/Users/a/Shared".into(),
            folder: Some("photos".into()),
            folder_id: Some("ref:photos".into()),
            mode: "always".into(),
            access_revoked: parked,
        }
    }

    /// The FFI face of the park watch (`file-sync.md` § Multi-writer shared
    /// sets → *Revocation*): a park the agent reports AFTER the reconcile that
    /// adopted the row reaches the rendered row without another reconcile, and
    /// an un-park clears it the same way — the macOS status tick's only way to
    /// learn a demoted writer's binding stopped syncing.
    #[test]
    fn fold_parks_mirrors_the_agents_park_onto_an_adopted_row() {
        let model = FfiLocationBindingsModel::new();
        model.reconcile(vec![agent_row(false)]);
        assert!(!model.rendered()[0].access_revoked);

        model.fold_parks(vec![agent_row(true)]);
        assert!(
            model.rendered()[0].access_revoked,
            "the park must reach the row"
        );

        model.fold_parks(vec![agent_row(false)]);
        assert!(
            !model.rendered()[0].access_revoked,
            "a re-bind's un-park must clear it"
        );
    }

    /// Records every basename forwarded across the (in-test, Rust-side)
    /// foreign-observer boundary.
    #[cfg(unix)]
    #[derive(Default)]
    struct Recording(Mutex<Vec<String>>);

    #[cfg(unix)]
    impl FfiSyncCompleteObserver for Recording {
        fn on_sync_complete(&self, filename: String) {
            self.0.lock().unwrap().push(filename);
        }
    }

    /// The adapter forwards a real pushed event (over a real
    /// `unix_transport::serve` socket) to the observer as a basename — pins
    /// the FFI closure layer on top of the shared-loop test in `fauna_ipc`.
    ///
    /// **`cfg(unix)` because it serves a real unix socket**, not because the
    /// adapter is unix-only — the adapter is cross-platform since the module
    /// de-gated (`sync-agent.md` § Consumers). `fauna_ipc::unix_transport` and
    /// `AgentEndpoint::Unix` both vanish on windows, so this test cannot compile
    /// there; the windows twin of this proof is the live e2e suite. Same split
    /// `fauna_ipc::events` makes between its `filter_tests` (every platform) and
    /// its socket `tests` (unix).
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pushed_synced_event_reaches_the_ffi_observer() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sync-agent.sock");

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let (event_tx, _) = tokio::sync::broadcast::channel::<fauna_ipc::sync::Event>(16);
        let handler = |req: fauna_ipc::sync::Request| async move {
            fauna_ipc::sync::Response {
                id: req.id,
                result: fauna_ipc::sync::ResponseResult::Err("unhandled".into()),
            }
        };
        let sock_srv = sock.clone();
        let event_tx_srv = event_tx.clone();
        let server = tokio::spawn(async move {
            fauna_ipc::unix_transport::serve(&sock_srv, handler, shutdown_rx, event_tx_srv)
                .await
                .unwrap();
        });
        for _ in 0..200 {
            if sock.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        let observer = Arc::new(Recording::default());
        let listener = FfiSyncAgentEventListener::spawn_at(
            fauna_ipc::endpoint::AgentEndpoint::Unix(sock.clone()),
            observer.clone(),
        )
        .unwrap();

        // Push until the listener (connect + park in the blocking read) has one.
        for _ in 0..200 {
            let _ = event_tx.send(fauna_ipc::sync::Event {
                event: fauna_ipc::sync::EventKind::FileStatusChanged {
                    path: "/Users/alice/Fauna/sub/report.bin".into(),
                    status: fauna_ipc::sync::FileStatus::Synced,
                },
            });
            if !observer.0.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        assert_eq!(
            observer.0.lock().unwrap().first().map(String::as_str),
            Some("report.bin")
        );

        listener.stop();
        let _ = shutdown_tx.send(true);
        let _ = server.await;
    }
}
