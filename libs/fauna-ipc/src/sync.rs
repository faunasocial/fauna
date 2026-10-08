//! IPC message types for the FaunaSync service.
//!
//! FaunaSync handles file synchronization: adding/removing sync folders,
//! tracking file status, managing folders, and cloud file operations.

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// Re-exported for [`RequestMethod::ShareIngest`] builders/handlers — the
/// opaque-bytes carriage type, so peers of this crate need no `serde_bytes`
/// dep line of their own.
pub use serde_bytes::ByteBuf;

/// Return the per-user sync pipe name for the given SID string.
///
/// Pure function — testable on any platform.
///
/// # Example
/// ```
/// let name = fauna_ipc::sync::pipe_name_for_sid("S-1-5-21-1-2-3-1001");
/// assert_eq!(name, r"\\.\pipe\fauna-sync.S-1-5-21-1-2-3-1001");
/// ```
pub fn pipe_name_for_sid(sid: &str) -> String {
    format!(r"\\.\pipe\fauna-sync.{sid}")
}

/// Derive the windows single-instance mutex name from a per-user sync pipe
/// name ([`pipe_name_for_sid`]'s output) — the named-mutex twin of the unix
/// `InstanceLock`'s socket-path scoping. Kernel object names forbid
/// backslashes past the `Local\` namespace prefix, so this keys on the pipe
/// name's leaf (the SID in production, an arbitrary test/e2e name under
/// `--pipe-name` otherwise) rather than reusing the pipe name verbatim.
///
/// Splits on the LAST `\` first to isolate the leaf, THEN on the last `.`
/// within just that leaf — never on the whole string. `pipe_name` always
/// carries the `\\.\` machine-prefix, which contains its own `.`; splitting
/// the whole string on `.` walks past that prefix dot on any leaf with no
/// dot of its own (every non-production e2e/test pipe name, e.g.
/// `fauna-sync-test-a`), returning a suffix with embedded backslashes that
/// `CreateMutexW` rejects as a nested object-manager path
/// (`ERROR_PATH_NOT_FOUND`, 0x80070003).
///
/// Pure function — testable on any platform, same as [`pipe_name_for_sid`].
///
/// # Example
/// ```
/// let name = fauna_ipc::sync::mutex_name_for_pipe(r"\\.\pipe\fauna-sync.S-1-5-21-1-2-3-1001");
/// assert_eq!(name, r"Local\FaunaSyncAgent.S-1-5-21-1-2-3-1001");
/// ```
pub fn mutex_name_for_pipe(pipe_name: &str) -> String {
    let leaf = pipe_name.rsplit('\\').next().unwrap_or(pipe_name);
    let suffix = leaf.rsplit('.').next().unwrap_or(leaf);
    format!(r"Local\FaunaSyncAgent.{suffix}")
}

/// Return the per-user sync pipe name for the calling process's user SID.
///
/// Resolves the process token → `TOKEN_USER` → `ConvertSidToStringSidW` via
/// [`crate::win_token::current_user_sid_string`] and passes the result to
/// [`pipe_name_for_sid`].
#[cfg(windows)]
pub fn current_user_pipe_name() -> std::io::Result<String> {
    let sid = crate::win_token::current_user_sid_string()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(pipe_name_for_sid(&sid))
}

/// Non-Windows fallback: the per-user pipe naming requires a Windows token SID,
/// so off-Windows (dev/CI builds of the agent that never actually serve a pipe)
/// this returns the legacy fixed sync pipe name. Behavior-preserving for the
/// `#[cfg(not(windows))]` agent path; the production agent is Windows-only.
#[cfg(not(windows))]
pub fn current_user_pipe_name() -> std::io::Result<String> {
    Ok(r"\\.\pipe\fauna-sync".to_string())
}

// ── RefreshBearer no-capability contract (cross-process) ──

/// The exact error-message **prefix** the agent returns from a `RefreshBearer`
/// request when it currently holds no provisioned capability (a fresh start, or a
/// restart before the identity-holding client has re-provisioned it). A caller
/// running the provisioning convergence loop matches on this prefix to decide it
/// must push a full `ProvisionCapability` rather than treat the refresh as a
/// transient failure.
///
/// This is a **cross-process contract**: the agent writes it and the Rust
/// convergence loop (`crate::convergence`) — the one every desktop app runs —
/// matches on it, and the two can be different releases. Rewording it silently
/// breaks the re-provision trigger, so it is pinned by a test and must never
/// change.
pub const NO_CAPABILITY_ERROR_PREFIX: &str = "no capability provisioned";

/// The full error message the agent returns for the no-capability `RefreshBearer`
/// case. Always begins with [`NO_CAPABILITY_ERROR_PREFIX`] (asserted by a test), so
/// the producer and every prefix-matcher share one source of truth.
pub const NO_CAPABILITY_ERROR_MESSAGE: &str = "no capability provisioned; cannot refresh bearer";

/// The agent's answer to a request whose method it cannot decode — a verb a
/// newer app names and this agent predates. The refusal is per request: the
/// connection stays up (`frame_io::handle_conn`; `transport.md` § Rule 3 in
/// full). [`ResponseResult`] is the fixed Ok/Err envelope, so the refusal is
/// this exact `Err` string, which every release of the app already decodes;
/// match it with [`is_unsupported_method_refusal`], never by hand.
pub const UNSUPPORTED_METHOD_ERROR_MESSAGE: &str =
    "unsupported method: this agent predates the request";

/// Whether an `Err` reply's message is the agent's
/// [`UNSUPPORTED_METHOD_ERROR_MESSAGE`] refusal.
pub fn is_unsupported_method_refusal(message: &str) -> bool {
    message == UNSUPPORTED_METHOD_ERROR_MESSAGE
}

// ── Request envelope ──

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    pub method: RequestMethod,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum RequestMethod {
    // Sync folder management
    GetSyncStatus,
    AddLocation {
        path: String,
    },
    RemoveLocation {
        path: String,
    },
    ListLocations,

    // File status (used by shell extension)
    GetFileStatus {
        path: String,
    },
    PinFile {
        path: String,
    },
    UnpinFile {
        path: String,
    },
    FreeSpace {
        path: String,
    },

    // Cloud files
    SetLocationSyncMode {
        path: String,
        mode: String,
    },

    // Context menu / sharing
    ShareFile {
        path: String,
    },
    GetFileDevices {
        path: String,
    },
    GetFileVersions {
        path: String,
    },

    // Service control
    GetServiceStatus,
    Configure {
        nest_url: Option<String>,
    },
    Shutdown,

    // On-demand hydration capability handoff (app→helper; see `SyncCapability`)
    ProvisionCapability(SyncCapability),
    RefreshBearer(BearerToken),

    // Location ↔ folder binding (multi-root on-demand). Binds an already-added
    // location to a nest folder; does NOT start the hydration host (that is the
    // host slice). See `docs/goal/behavior/file-sync.md`
    // § On-Demand Files (*Hosting multiple on-demand folders*).
    //
    // Wire note (applies to every variant here): serde's externally-tagged repr
    // keys a variant by its **name**, so adding a variant is additive wherever it
    // sits, and *renaming* one changes the wire.
    //
    // What that costs is now bounded by there being exactly ONE encoder of this
    // enum: the hand-written C# codec that used to mirror the listing was retired
    // 2026-07-24 and deleted 2026-08-05 (`IpcMessages.cs` — Swift and C# reach
    // this seam through `fauna-ffi`'s `FfiSyncAgentProvisioner`, and linux/tui
    // link the Rust client directly; `sync-agent.md` § Control plane split,
    // "There is no per-app IPC codec"). Every peer therefore ships from the same
    // commit as the agent, and the only skew left is across an in-place
    // upgrade: an app talking to a *still-running older agent* (linux, the
    // macOS `.dmg` channel), or a still-loaded older windows shell extension
    // talking to a newer agent — windows the service or Explorer restart
    // closes. Inside one, an unknown verb is refused for that one request
    // (`UNSUPPORTED_METHOD_ERROR_MESSAGE`) and costs a rejected call, never
    // data or the connection. The phase-1a leg-2 rename (`AddSyncFolder` →
    // `AddLocation`, …) spent exactly that window, deliberately.
    /// Bind an already-added location to a nest folder — the **one** bind
    /// verb, keyed by the set's **identity**: `folder_id` is the
    /// `fauna_core::folder_keys::FolderRef` wire form, the unambiguous key the
    /// agent resolves engine key material, the engine-registry entry and the
    /// per-set state DB by. `folder` is the set's name, carried as a label.
    ///
    /// Both travel in one exchange, so one reconcile starts the engine under
    /// its final key. The agent refuses a `folder_id` that does not parse.
    ///
    /// The name-keyed form of this verb (no `folder_id`) and the separate
    /// `SetLocationFolderRef` variant that added the ref beside it were
    /// collapsed into this one shape 2026-09-24 (the compat-remnant sweep at
    /// its universal scope, `version-compatibility.md` § Dimension 2): set
    /// names are unique only per owner, and no pre-identity app is left to
    /// bind by name.
    SetLocationFolder {
        path: String,
        folder: String,
        folder_id: String,
    },

    // Nest-backed per-file version history (file-sync.md § File Versions). Distinct
    // from `GetFileVersions`, which only reports the *local* SyncDb entry's counter:
    // this lists the real, retroactive history projected over `sync_changes`.
    ListFileVersions {
        path: String,
    },

    // Restore `path` to the version whose `sync_changes` seq is `version_num`
    // (file-sync.md § Restore). Records an ordinary `modify` re-pointing the
    // historical manifest, then re-points this device's own local copy — catch-up
    // skips a device's own changes, so the recording device owns that second step.
    //
    // `version_num` is the `seq` carried verbatim from `ListFileVersions`, never a
    // dense 1..N ordinal.
    RestoreFileVersion {
        path: String,
        version_num: i64,
    },

    // ── Device-global agent control + live introspection ──
    //
    // sync-agent.md § Control plane split (D3.4). All additive: an agent older
    // than the app refuses an op it cannot name for that one request
    // (`UNSUPPORTED_METHOD_ERROR_MESSAGE`), and the connection stays up.
    /// Per-set engine introspection: for each bound folder the agent hosts, its
    /// mode, whether it is actively serving, and its upload backlog. Drives the
    /// desktop apps' / fauna-tui's agent-status view. Read-only.
    ListEngines,

    /// Live segment-backup progress, projected agent-side so the Backups page reads
    /// real progress from the always-running agent rather than an in-app driver
    /// handle (sync-agent.md § Control plane split #4; `behavior/backup-destinations.md` status read).
    GetBackupStatus,

    /// Device-global pause of ALL sync + backup work, persisted in the agent's own
    /// state (the removed headless daemon's `paused` flag, now the uniform
    /// shape). Per-set pause is deferred until a real need appears (D3.4). No
    /// payload.
    Pause,

    /// Device-global resume — the inverse of [`Self::Pause`]. No payload.
    Resume,

    /// Tear down the provisioned capability: stop engines, clear the in-memory
    /// slot, and delete the persisted credential-store record (sync-agent.md
    /// § Credential model — sign-out / account-switch / revocation drive it;
    /// the windows account-switch teardown consumes the same op).
    /// Idempotent — un-provisioning an un-provisioned agent is a no-op success.
    /// Additive variant (2026-07-19); no payload.
    UnprovisionCapability,

    /// **An app is open on this machine for as long as this connection is.**
    /// Sent once on a connection the app opens at launch and holds until it
    /// exits; the agent counts the connection as an attached app until it
    /// closes — a clean exit and a crash alike, since the kernel closes the
    /// socket either way (`fauna_ipc::conn_scope`). The agent's `ws-device`
    /// notification arm posts a banner only while no app is attached, because
    /// an attached app owns the machine's banners under its own focus rule
    /// (`common.md` § Push Notifications → *Transports*). Replies
    /// [`ResponsePayload::Empty`]. Additive variant (2026-10-01): an agent
    /// that cannot name the verb answers `UNSUPPORTED_METHOD` for that one
    /// request and keeps the connection up, which the app's attachment loop
    /// reads as "not attached" and retries.
    AttachApp {
        /// Which app is attaching (`"tui"`, `"linux"`, `"windows"`) — what a
        /// tapped banner will launch. Optional so the field set can grow
        /// additively.
        #[serde(default)]
        app: Option<String>,
        /// The platform identity the agent posts this app's banners under
        /// while no app is attached — windows: the AUMID of the toast
        /// registration the app itself carries, so an agent's toast is the
        /// app's (its name, its icon, and a tap activates it). `None` where the
        /// platform's sink needs no identity (linux) or the app has none. The
        /// agent keeps the last one it was handed across its own restarts.
        /// Additive (2026-10-08): an older agent ignores it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notification_identity: Option<String>,
    },

    /// Nudge the resident engine serving `folder` to pull remote changes
    /// **now**, off its rescan cadence (`file-sync.md` § Remote-change nudge).
    /// The client sends this when it receives a `PushEvent::SyncChanged` for the
    /// set. `folder` is looked up among this device's bindings to the one
    /// binding's `FolderRef` — never used as an engine key itself; a name
    /// several bindings wear is refused. Best-effort: if the set is not bound,
    /// not resident (no engine), or a pull is already pending, it is silently
    /// dropped — the rescan tick is the
    /// correctness backstop, so a missed nudge costs only latency. Additive
    /// variant (2026-07-23); no reply payload.
    ///
    /// `folder_hash` is the push's own hash address
    /// (`SyncChangedPayload::folder_hash`), relayed as received: when present
    /// the agent matches its bindings by it (each binding's name hashed), so a
    /// sealed set's nudge — whose `folder` is blank — still finds its engine
    /// (`path-sealing.md` § the set-name plane). Additive field.
    PullFolderNow {
        folder: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        folder_hash: Option<ByteBuf>,
    },

    /// User-confirmed propagation of a **mass-delete-floor hold**
    /// (`delete-propagation.md` § the mass-delete floor: *"propagation of a
    /// held set is an explicit user action"*): the app's *"your folder emptied
    /// — apply N deletions"* affordance sends this after an explicit confirm.
    /// Routed to the resident engine serving `folder` (looked up among this
    /// device's bindings to its `FolderRef`, like [`Self::PullFolderNow`];
    /// ambiguous names are refused), which re-derives the hold NOW and applies it through the ordinary delete path — a stale
    /// displayed count is never consumed. Replies
    /// [`ResponsePayload::HeldDeletesApplied`]; errors when the set is not
    /// being served by a resident engine. Additive variant (2026-08-15) —
    /// self-gating: an agent reporting no hold has `deletes_held == 0`, so an
    /// app never offers the affordance against it.
    ApplyHeldDeletes {
        folder: String,
    },

    /// The share leg's serve-info read (slice E — `p2p.md` § Cross-user
    /// shared-set transfer): where each bound folder's state DB and local
    /// tree live, so the APP process can serve the transfer plane by
    /// cross-process WAL read (the sanctioned second-connection pattern) —
    /// path resolution stays the agent's one authority
    /// (`sync_db_path_for_binding`), never re-derived app-side. Replies
    /// [`ResponsePayload::ShareServeInfo`]. Additive variant (2026-08-18).
    GetShareServeInfo,

    /// Run one full pass of the account store this agent has mounted, now —
    /// the cross-process arm of the devices page's participation switch
    /// (`p2p.md` § Per-device participation → *Enforcement*, (c) Promptness).
    /// The same-account peer listener lives in the engine holder's pump, which
    /// on a desktop is usually this agent; the app writes the device-local row
    /// first and then sends this, so the pass whose ensure step reads the row
    /// runs within the gesture rather than at the 300 s backstop. Structurally
    /// local: it names this process's own pump and carries nothing. A no-op
    /// `Empty` when no store is mounted or this agent is not the engine holder
    /// (an app's runtime holds it and ran its own pass); the pass's own outcome
    /// is its report's, never the reply's. Best-effort on the caller's side:
    /// the row is the fact the next full pass reads either way. Additive unit
    /// variant (2026-10-06); replies [`ResponsePayload::Empty`].
    ReconcileAccountRuntime,

    /// One page of ACCEPTED peer-served share rows handed to the resident
    /// engine serving `folder` for provisional ingest (slice E): the app owns
    /// the peer channel, the engine owns the state writes, and this verb is
    /// the boundary. `rows` are canonical dag-cbor
    /// `fauna.peer.share` change encodings (opaque here — this crate stays
    /// protocol-free); bodies come from the caller-populated `spool_dir`
    /// (`spool/manifests/<hex>` + `spool/chunks/<hex>`; a spool miss skips
    /// that row, never the page). Replies
    /// [`ResponsePayload::ShareIngested`]; errors when the set has no
    /// resident engine. Additive variant (2026-08-18).
    ShareIngest {
        folder: String,
        /// The set's `FolderRef` wire form — the agent routes by it, never
        /// by the name: two sets can share a name, and a misrouted ingest
        /// writes one set's rows into another's state. Required
        /// since the 2026-09-24 compat-remnant sweep retired the name-only
        /// caller's config lookup.
        folder_id: String,
        proven_actor_hex: String,
        rows: Vec<serde_bytes::ByteBuf>,
        spool_dir: String,
    },

    /// What this device's sealed **custodian store** occupies on disk right now
    /// (`docs/goal/ui/backups.md` § Manage backup destinations → *Reclaim this
    /// device's copy*). Replies [`ResponsePayload::CustodianStore`].
    ///
    /// # Why the agent is asked and not the disk
    ///
    /// The desktop store lives under the sync agent's per-user data dir
    /// (`behavior/backup-destinations.md` § Third destination kind), and
    /// `../../docs/goal/architecture/apps/sync-agent.md` § Control plane split
    /// keeps path resolution as the agent's one authority — the same rule that
    /// makes `GetShareServeInfo` a verb rather than an app-side re-derivation.
    /// An app that recomputed the root would be right until the day the layout
    /// moved, and then silently report an empty store over a full one.
    ///
    /// It carries no policy in either direction: the reply is a measurement of
    /// this process's own disk, which is what makes it structurally local and
    /// so IPC-eligible. **Whether that store is orphaned is not asked here** —
    /// that needs the destination rows, which only the seed-holding app can
    /// open, and it decides with the shared
    /// `fauna_core::data::custodian_store_is_orphaned`.
    ///
    /// Additive variant (2026-08-21); errors only when this agent holds no
    /// provisioned capability, since without an actor it cannot name a store.
    GetCustodianStore,

    /// Free this device's whole sealed custodian store — the
    /// `backup-destination-reclaim-button` action, after its confirm modal
    /// (`docs/goal/ui/backups.md` § Manage backup destinations → *Reclaim this
    /// device's copy*). Replies [`ResponsePayload::CustodianStoreReclaimed`].
    ///
    /// **The app decides *whether*; this decides *safely*.** Removing a
    /// client-device destination deliberately keeps the local store (3c-ii), so
    /// an orphaned store is an ordinary state and the orphan verdict needs the
    /// destination rows — at-rest data this bearer-only process cannot open. The
    /// app makes that call. What this agent adds is the one guarantee only it
    /// can give: it does not delete a store out from under its own writer. A
    /// live custodian stint is **stopped first** and the reclaim proceeds once
    /// it is down; a stint that will not stop in time refuses
    /// ([`CustodianReclaimOutcome::still_hosting`]) rather than racing it.
    ///
    /// Idempotent: reclaiming an already-empty store frees nothing and succeeds,
    /// so a repeated gesture cannot fail the page.
    ///
    /// Additive variant (2026-08-21).
    ReclaimCustodianStore,

    /// Seed the nest this agent is provisioned against from this device's
    /// sealed custodian store: the re-seed ceremony, the confirmed
    /// `backup-destination-reseed-button` action (`docs/goal/behavior/
    /// backup-destinations.md` § Third destination kind → *Re-seed*, and its
    /// *Where the ceremony runs* ruling). Replies
    /// [`ResponsePayload::CustodianReseed`] with the job's state at once.
    ///
    /// **A job, not a call.** A real corpus takes minutes and a verb round trip
    /// is bounded at seconds (`sync_pipe_client::REQUEST_TIMEOUT`), so this
    /// *starts* the ceremony on the agent and returns; [`Self::GetCustodianReseed`]
    /// reads how it went. The agent is the one writer of the store, so the job
    /// runs here: a live custodian stint is stopped first, exactly as a
    /// reclaim stops it. Starting while a job runs starts nothing and answers
    /// `Running`, so a repeated press cannot run two ceremonies over one store.
    ///
    /// Carries the owner's `NestBackupKey` for this one job, never persisted
    /// (`docs/goal/architecture/owner-key-material.md` § Path A-sibling-0).
    ///
    /// Additive variant (2026-09-26).
    ReseedCustodianStore(CustodianReseedRequest),

    /// Read the re-seed job's state. Replies [`ResponsePayload::CustodianReseed`].
    /// Read-only; `Idle` when no job ran since the agent started.
    ///
    /// Additive variant (2026-09-26).
    GetCustodianReseed,

    /// The display names this device's sealed custodian store learned for its
    /// covered-folder sets (`fauna_sync_engine::custodian_store::CustodianStore::
    /// folder_names`). Replies [`ResponsePayload::CustodianFolderNames`].
    ///
    /// The read behind a desktop re-seed's **target pre-create**
    /// (`docs/goal/architecture/writer-signed-change-records.md` ruling
    /// (7)(a)(i)): the app holds the seed, so it creates each target set, but
    /// the names live in the store this agent owns. Read-only; a store that
    /// does not exist answers no names.
    ///
    /// Additive variant (2026-10-02).
    GetCustodianFolderNames,

    /// **Test-only.** Run exactly one custodian pull pass on the driver this
    /// process is *already hosting*, synchronously, and reply
    /// [`ResponsePayload::CustodianPassReport`] only once it has finished.
    ///
    /// # Why this is not the thing § Control plane split forbids
    ///
    /// `../../docs/goal/architecture/apps/sync-agent.md` § Control plane split
    /// forbids carrying a destination's *policy* over this seam — the
    /// custodian's capacity cap is its worked example — and its general test is
    /// that a value is IPC-eligible only when **structurally local**. The one
    /// value this variant carries is a **test clock**, not a policy: it
    /// configures nothing, it cannot create or re-point a host, it does not
    /// survive the call, and the only thing it can reach is the host *this*
    /// process is running, which makes it structurally local by construction.
    /// It is the scheduler poke the periodic arm would otherwise make a test
    /// sleep 15 minutes for (`fauna_sync_engine::segment_backup::PERIODIC_INTERVAL`,
    /// whose first tick `CustodianPull::run_loop` deliberately mutes) — the
    /// sanctioned alternative to the wall-clock brittleness `testing.md`
    /// convention 14 forbids, and the exact shape the nest's own
    /// `POST /api/v1/test/backup/run-now` hook takes for the same reason.
    ///
    /// Compiled out of release artifacts (`testing.md` convention 15) by this
    /// crate's own e2e seam feature — the one tui's `e2e-agent` already forwards
    /// through `fauna-client-sync/test-helpers`, so no new forwarding chain is
    /// minted for it. `RequestMethod` is a plain
    /// externally-tagged serde enum, so this variant is **name**-keyed on the
    /// wire and its absence shifts nothing: a release agent cannot name it and
    /// refuses that one request with [`UNSUPPORTED_METHOD_ERROR_MESSAGE`],
    /// which is the correct answer.
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    CustodianRunPassNow {
        /// Seconds to add to the agent's wall clock for **this pass only** —
        /// convention 14's fake clock, in the `now_offset_secs` spelling the
        /// app-side pokes beside it already use (`backup_audit_run_now`,
        /// `atproto_delegation_advance_clock`).
        ///
        /// `0` is an ordinary pass at the real clock. A non-zero offset exists
        /// because the pass is not one behavior but several on different
        /// cadences, and the slowest of them is a **day**: the custodian's
        /// self-audit is debounced to
        /// `fauna_client_backup::audit::AUDIT_MIN_INTERVAL_SECS` (24 h), so the
        /// second audit of a store — the first one that can ever observe rot
        /// that appeared *after* enrollment — is unreachable at the real clock
        /// by anything but sleeping a day.
        ///
        /// Deliberately an **offset**, not an absolute `now`: the caller does
        /// not have to know the agent's clock, every pass still runs through
        /// the same `run_all_kinds(now)` parameter the scheduler uses (so no
        /// second code path exists to rot), and a `0` default keeps the frame
        /// meaning exactly what it meant before this field.
        #[serde(default)]
        now_offset_secs: i64,
    },
}

impl RequestMethod {
    /// The verb's name — **and nothing else**. This is what the pipe server logs per
    /// request, so it must never widen into the payload: `ProvisionCapability` carries the
    /// owner's `BackupKey` and `RefreshBearer` a live nest bearer, so a `{:?}` of the method
    /// would spill key material into a plaintext log file. (Both types redact themselves in
    /// `Debug`, but the guarantee we want here is structural, not a property of a type someone
    /// might later change.)
    ///
    /// The name is also the **wire key** — variants are encoded by name, not discriminant,
    /// and a rename breaks an agent of another release — so these strings deliberately
    /// match the variant names exactly, and the exhaustive match makes a new verb a compile
    /// error rather than a silently unlogged one.
    pub fn name(&self) -> &'static str {
        match self {
            Self::GetSyncStatus => "GetSyncStatus",
            Self::AddLocation { .. } => "AddLocation",
            Self::RemoveLocation { .. } => "RemoveLocation",
            Self::ListLocations => "ListLocations",
            Self::GetFileStatus { .. } => "GetFileStatus",
            Self::PinFile { .. } => "PinFile",
            Self::UnpinFile { .. } => "UnpinFile",
            Self::FreeSpace { .. } => "FreeSpace",
            Self::SetLocationSyncMode { .. } => "SetLocationSyncMode",
            Self::ShareFile { .. } => "ShareFile",
            Self::GetFileDevices { .. } => "GetFileDevices",
            Self::GetFileVersions { .. } => "GetFileVersions",
            Self::GetServiceStatus => "GetServiceStatus",
            Self::Configure { .. } => "Configure",
            Self::Shutdown => "Shutdown",
            Self::ProvisionCapability(_) => "ProvisionCapability",
            Self::RefreshBearer(_) => "RefreshBearer",
            Self::SetLocationFolder { .. } => "SetLocationFolder",
            Self::ListFileVersions { .. } => "ListFileVersions",
            Self::RestoreFileVersion { .. } => "RestoreFileVersion",
            Self::ListEngines => "ListEngines",
            Self::GetBackupStatus => "GetBackupStatus",
            Self::Pause => "Pause",
            Self::Resume => "Resume",
            Self::UnprovisionCapability => "UnprovisionCapability",
            Self::AttachApp { .. } => "AttachApp",
            Self::PullFolderNow { .. } => "PullFolderNow",
            Self::ApplyHeldDeletes { .. } => "ApplyHeldDeletes",
            Self::GetShareServeInfo => "GetShareServeInfo",
            Self::ReconcileAccountRuntime => "ReconcileAccountRuntime",
            Self::ShareIngest { .. } => "ShareIngest",
            Self::GetCustodianStore => "GetCustodianStore",
            Self::ReclaimCustodianStore => "ReclaimCustodianStore",
            Self::ReseedCustodianStore(_) => "ReseedCustodianStore",
            Self::GetCustodianReseed => "GetCustodianReseed",
            Self::GetCustodianFolderNames => "GetCustodianFolderNames",
            #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
            Self::CustodianRunPassNow { .. } => "CustodianRunPassNow",
        }
    }
}

// ── App→helper capability handoff (on-demand hydration) ──
//
// The identity-holding WinUI app provisions the user-session sync helper with the
// least-privilege capability it needs to hydrate on-demand files — never the identity
// seed. Phase 1 = the owner's `BackupKey` (opens owner-only file chunks for decrypt) +
// a renewable nest bearer. See `docs/goal/behavior/file-sync.md` § On-Demand Files and
// `docs/goal/architecture/key-material-hierarchy.md` rule #7. Held in memory only and
// re-provisioned each login; every cleartext copy is zeroized on drop and redacted in Debug.

/// A renewable nest bearer token plus its expiry. Zeroized on drop; redacted in `Debug`.
#[derive(Serialize, Deserialize)]
pub struct BearerToken {
    /// The bearer credential the helper presents to the nest.
    pub token: String,
    /// Unix-seconds expiry **on this machine's clock**, anchored at receipt by
    /// whichever holder minted the bearer (`login.md` § Token lifetime on the
    /// client's clock) — the deadline the agent's renewal loop plans on. Always
    /// present: every app's bearer source publishes the expiry of the bearer it
    /// minted (`fauna_nest_http::BearerSource::bearer_with_expiry`).
    pub expires_at: u64,
}

impl BearerToken {
    pub fn new(token: String, expires_at: u64) -> Self {
        Self { token, expires_at }
    }
}

impl Drop for BearerToken {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

impl std::fmt::Debug for BearerToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BearerToken")
            .field(
                "token",
                &format_args!("<{} chars redacted>", self.token.len()),
            )
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// The Phase-1 on-demand hydration capability: the owner's `BackupKey` + the owner's
/// public `actor_id` + the nest URL + the device UUID + a renewable bearer.
#[derive(Serialize, Deserialize)]
pub struct SyncCapability {
    /// Owner's `BackupKey`, exactly 32 raw bytes (encoded as a CBOR byte string). Opens
    /// owner-only file chunks for decrypt-on-hydrate. Zeroized on drop; never logged.
    #[serde(with = "serde_bytes")]
    backup_key: Vec<u8>,
    /// Owner's public `actor_id`, exactly 32 raw bytes (encoded as a CBOR byte string).
    /// Public material — drives the bearer-only `AuthClient`'s WS URL path so the helper
    /// can open its authenticated nest connection without the identity keypair. Not a
    /// secret, but kept private behind [`actor_id_array`](Self::actor_id_array) to mirror
    /// the `backup_key` accessor shape.
    #[serde(with = "serde_bytes")]
    actor_id: Vec<u8>,
    /// Full nest base URL the app chose (e.g. `https://example.com`). Used verbatim by the
    /// agent — never reconstructed with a port by the agent. Public routing info; not a secret.
    pub nest_url: String,
    /// Device UUID from the app's vault. Public routing info; not a secret.
    pub device_id: String,
    /// Renewable nest bearer (self-zeroizing, self-redacting).
    pub bearer: BearerToken,
    /// Retired owner `BackupKey`s of the identities this account **succeeded
    /// from**, nearest hop first — flattened 32-byte keys, so `len % 32 == 0`
    /// (`sync-agent.md` § Credential model → *Retired owner keys after an
    /// identity succession*, ratified 2026-08-04).
    ///
    /// A succession moves corpus *ownership* but no *seal*, so the agent's
    /// engines meet chunks sealed under a key the successor no longer derives;
    /// on a fresh device — the case a succession exists for — that is **every**
    /// chunk. These are the read candidates that open them, offered beside
    /// `backup_key` and never instead of it.
    ///
    /// ⚠ **Read-only, and the agent must keep it that way.** They reach
    /// `FileDownloadKeys::predecessor_backup_keys` and nothing else; no seal root
    /// consults that field, so nothing the agent uploads can land back under a
    /// retired key. Same trust boundary as `backup_key` (the same per-user
    /// pipe/socket the app already pushes that over) — zeroized on drop,
    /// redacted in `Debug`, kept private behind
    /// [`predecessor_backup_keys`](Self::predecessor_backup_keys).
    ///
    /// Empty = the overwhelmingly common case (an identity that never succeeded).
    /// `#[serde(default)]` keeps it
    /// additive both directions: an agent that does not know the key
    /// ignores it and simply cannot open a predecessor-sealed
    /// corpus — fail-closed.
    ///
    /// Flattened rather than `Vec<Vec<u8>>` so the zeroize on drop is one
    /// contiguous wipe with no per-element buffer left behind by a reallocating
    /// outer vector.
    #[serde(default, with = "serde_bytes")]
    predecessor_backup_keys: Vec<u8>,
    /// The **attested** actor ids of the identities this account succeeded
    /// from, nearest hop first — flattened 32-byte ids, so `len % 32 == 0`
    /// (`account-data-taxonomy.md` § The generation machinery → *The source of
    /// `prior`*, ruled 2026-09-13; `sync-agent-credentials.md` § Credential
    /// model owns the field).
    ///
    /// The seedless host's `prior` for the generation machinery's fleet view — the
    /// succession-crossing signer allow-list an enrollment cert verifies
    /// against. The host holds no account registry, so it cannot attest a
    /// predecessor itself; the identity-holding app hands it the ids of
    /// `AccountRegistry::predecessor_backup_keys_by_actor` — exactly the rows
    /// whose seeds that device holds — and the host trusts the app over this
    /// pipe as it already trusts it for `backup_key`. The ledger's own
    /// `prior_actor_ids` is deliberately NOT read for this: it is
    /// writer-asserted, and carried across a succession unmarked.
    ///
    /// **A separate field from the keys, and never dropped with them.** The
    /// keys are read candidates under bound (3)'s drop license; the ids are
    /// public material the trust needs for the device's life (a
    /// predecessor-signed `Enrolled` row is never re-signed), so an app pushes
    /// them on every provision regardless of the license, and a consumer must
    /// not pair the two lists positionally.
    ///
    /// Empty = the overwhelmingly common case (an identity that never
    /// succeeded) — then the host's set is
    /// empty, which is fail-safe: predecessor-signed enrollments drop out of
    /// its view and nothing is admitted. `#[serde(default)]` keeps it additive
    /// both directions. Public material (an actor id is a public key) — no
    /// zeroize obligation, shown by count in `Debug`.
    #[serde(default, with = "serde_bytes")]
    predecessor_actor_ids: Vec<u8>,
    /// The retired owner keys **paired with the identities they belong to**,
    /// nearest hop first — flattened 64-byte records, each the 32-byte actor
    /// id then that identity's 32-byte key, so `len % 64 == 0`
    /// (`sync-agent-credentials.md` § Credential model owns the field).
    ///
    /// The per-signer bound needs it (`mls-group-key-material.md` § M2 →
    /// *Writer-signed change records*, ruling (8)(c)): a row signed as
    /// predecessor A is offered only A's own root and its predecessors', so
    /// every open must know which key is whose. The two lists above cannot
    /// say — the ids ride on every push and the keys only under bound (3)'s
    /// license, so they are never paired positionally — and this field is
    /// that pairing, carried as one record per hop so no positional slip can
    /// hand one identity another's root. Resolve it from
    /// `AccountRegistry::predecessor_backup_keys_by_actor`.
    ///
    /// **Key material, under bound (3) exactly like
    /// [`predecessor_backup_keys`](Self::predecessor_backup_keys):** pushed
    /// with those keys and dropped with them, zeroized on drop, redacted in
    /// `Debug`. `#[serde(default)]` keeps it additive both directions: an
    /// agent that does not know it opens with the unpaired keys, which a row
    /// signed as a predecessor is never offered (fail-closed — that row is a
    /// noted skip); an app that does not send it leaves a new agent in the
    /// same state.
    #[serde(default, with = "serde_bytes", skip_serializing_if = "Vec::is_empty")]
    predecessor_keys_by_actor: Vec<u8>,
    /// Unix **milliseconds** at which the identity-holding app minted this
    /// capability — the ordering input of the signed-out reconcile
    /// ([`SignedOutMarker`], `sync-agent.md` § Credential model → *The
    /// signed-out reconcile*).
    ///
    /// Compared against a marker's `signed_out_at_ms` and nothing else: it
    /// orders a provision against a sign-out **on one machine's own clock**, so
    /// it needs no cross-machine agreement and is never a security decision on
    /// its own (the marker only ever *removes* authority, never grants it).
    ///
    /// **Required** — every capability the app pushes is stamped
    /// (`fauna_client_sync::agent`'s builder), and the agent carries the stamp
    /// through every rebuild, so a capability record or IPC message without it
    /// is not one this software wrote and fails to decode. [`Self::new`] leaves
    /// it `0` until [`Self::with_provisioned_at_ms`] stamps it; `0` reads as
    /// **older than any marker** — the fail-safe direction.
    pub provisioned_at_ms: u64,
}

impl SyncCapability {
    pub fn new(
        backup_key: Vec<u8>,
        actor_id: Vec<u8>,
        nest_url: String,
        device_id: String,
        bearer: BearerToken,
    ) -> Self {
        Self {
            backup_key,
            actor_id,
            nest_url,
            device_id,
            bearer,
            predecessor_backup_keys: Vec::new(),
            predecessor_actor_ids: Vec::new(),
            predecessor_keys_by_actor: Vec::new(),
            provisioned_at_ms: 0,
        }
    }

    /// Stamp the provision time (builder-style). The app calls this on every
    /// capability it pushes; see [`SignedOutMarker`].
    pub fn with_provisioned_at_ms(mut self, at_ms: u64) -> Self {
        self.provisioned_at_ms = at_ms;
        self
    }

    /// Attach the account's retired owner `BackupKey`s (builder-style, same
    /// reason as [`Self::with_provisioned_at_ms`]). Each key must be 32 bytes;
    /// they are flattened in the order given, nearest predecessor first.
    pub fn with_predecessor_backup_keys(mut self, keys: &[[u8; 32]]) -> Self {
        self.predecessor_backup_keys = keys.concat();
        self
    }

    /// Attach the account's attested predecessor actor ids (builder-style,
    /// same reason as [`Self::with_provisioned_at_ms`]). Each id must be 32
    /// bytes; they are flattened in the order given, nearest predecessor first.
    /// Independent of [`Self::with_predecessor_backup_keys`] — see the field.
    pub fn with_predecessor_actor_ids(mut self, ids: &[[u8; 32]]) -> Self {
        self.predecessor_actor_ids = ids.concat();
        self
    }

    /// Attach the retired owner keys paired with their identities
    /// (builder-style), `(actor id, key)` per hop, nearest predecessor first —
    /// see the field. Pushed and dropped with
    /// [`Self::with_predecessor_backup_keys`].
    pub fn with_predecessor_keys_by_actor(mut self, pairs: &[([u8; 32], [u8; 32])]) -> Self {
        let mut flat = Vec::with_capacity(pairs.len() * 64);
        for (id, key) in pairs {
            flat.extend_from_slice(id);
            flat.extend_from_slice(key);
        }
        self.predecessor_keys_by_actor.zeroize();
        self.predecessor_keys_by_actor = flat;
        self
    }

    /// The `BackupKey` as a fixed 32-byte array, or `None` if the provisioned key is the
    /// wrong length. Step 2 feeds this to `fauna_core::crypto::BackupKey::from_bytes`.
    ///
    /// The returned `[u8; 32]` is an un-zeroized copy of the key; the caller must contain
    /// it (e.g. wrap in `zeroize::Zeroizing`) so the cleartext does not linger on the stack.
    pub fn backup_key_array(&self) -> Option<[u8; 32]> {
        self.backup_key.as_slice().try_into().ok()
    }

    /// The account's retired owner `BackupKey`s, un-flattened, nearest hop first.
    ///
    /// Empty for every identity that never succeeded. A **malformed** blob (length not a multiple of
    /// 32) yields empty rather than a partial list: a truncated tail would present
    /// as "this key does not open the corpus", which is indistinguishable from
    /// corruption — the exact confusion the whole leg exists to remove — whereas
    /// empty is the honest, fail-closed "this agent has no retired keys".
    ///
    /// The returned copies are un-zeroized; the caller must contain them, mirroring
    /// [`backup_key_array`](Self::backup_key_array).
    pub fn predecessor_backup_keys(&self) -> Vec<[u8; 32]> {
        if !self.predecessor_backup_keys.len().is_multiple_of(32) {
            return Vec::new();
        }
        // `as_chunks` discards no bytes here — the guard above already proved the
        // length is an exact multiple, so its remainder is empty by construction.
        self.predecessor_backup_keys.as_chunks::<32>().0.to_vec()
    }

    /// The account's attested predecessor actor ids, un-flattened, nearest hop
    /// first — the seedless host's fleet-view `prior`.
    ///
    /// Empty for every identity that never succeeded. A **malformed** blob (length not a
    /// multiple of 32) yields empty rather than a partial list, for the trust's
    /// own reason: a truncated chain would admit some predecessor-signed
    /// enrollments and silently drop others, where empty is the honest,
    /// fail-safe "this host attests no predecessor". Public material.
    pub fn predecessor_actor_ids(&self) -> Vec<[u8; 32]> {
        if !self.predecessor_actor_ids.len().is_multiple_of(32) {
            return Vec::new();
        }
        self.predecessor_actor_ids.as_chunks::<32>().0.to_vec()
    }

    /// The retired owner keys paired with their identities, `(actor id, key)`
    /// per hop, nearest first. A **malformed** blob (length not a multiple of
    /// 64) yields empty, for [`Self::predecessor_backup_keys`]' reason. The key
    /// halves are un-zeroized copies; the caller must contain them.
    pub fn predecessor_keys_by_actor(&self) -> Vec<([u8; 32], [u8; 32])> {
        if !self.predecessor_keys_by_actor.len().is_multiple_of(64) {
            return Vec::new();
        }
        self.predecessor_keys_by_actor
            .as_chunks::<64>()
            .0
            .iter()
            .map(|record| {
                let (id, key) = record.split_at(32);
                (
                    id.try_into().expect("split at 32 of 64"),
                    key.try_into().expect("split at 32 of 64"),
                )
            })
            .collect()
    }

    /// The owner's public `actor_id` as a fixed 32-byte array, or `None` if the provisioned
    /// value is the wrong length. Fed to `AuthClient::bearer_only` so the helper's WS URL
    /// path resolves to the owner's actor. Public material — no zeroize obligation.
    pub fn actor_id_array(&self) -> Option<[u8; 32]> {
        self.actor_id.as_slice().try_into().ok()
    }
}

impl Drop for SyncCapability {
    fn drop(&mut self) {
        self.backup_key.zeroize();
        // Retired owner keys are key material like the current one — a rule
        // stated on `backup_key` alone would silently fail to cover its sibling.
        self.predecessor_backup_keys.zeroize();
        self.predecessor_keys_by_actor.zeroize();
        // `actor_id` is public material (no secret), and `bearer` zeroizes via its own Drop.
    }
}

impl std::fmt::Debug for SyncCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncCapability")
            .field(
                "backup_key",
                &format_args!("<{} bytes redacted>", self.backup_key.len()),
            )
            .field("actor_id", &format_args!("<{} bytes>", self.actor_id.len()))
            .field("nest_url", &self.nest_url)
            .field("device_id", &self.device_id)
            .field("bearer", &self.bearer)
            .field(
                "predecessor_backup_keys",
                &format_args!("<{} bytes redacted>", self.predecessor_backup_keys.len()),
            )
            .field(
                "predecessor_actor_ids",
                &format_args!("<{} ids>", self.predecessor_actor_ids.len() / 32),
            )
            .field(
                "predecessor_keys_by_actor",
                &format_args!("<{} bytes redacted>", self.predecessor_keys_by_actor.len()),
            )
            .finish()
    }
}

// ── The signed-out reconcile (sync-agent.md § Credential model) ──

/// The credential-store namespace (`application` attribute / Keychain service)
/// the agent persists its capability under — and, since the signed-out
/// reconcile, the namespace the **app** writes [`SignedOutMarker`] into.
///
/// It lives here, in the crate that already owns the app↔agent contract, rather
/// than in either process: two processes agreeing on a store location *is* a
/// contract, and a private constant on each side is how they drift apart.
/// (`FAUNA_KEYRING_APP` overrides it for both, identically, because both build
/// their store through `CredentialStore::new` — so an e2e run's redirect moves
/// the capability and its marker together, never one without the other.)
pub const CRED_NAMESPACE: &str = "fauna-sync-agent";

/// Account key of the persisted capability record.
pub const CAPABILITY_KEY: &str = "capability/v1";

/// Account key of the persisted sign-out marker ([`SignedOutMarker`]).
pub const SIGNED_OUT_KEY: &str = "signed-out/v1";

/// A durable record that an account **signed out on this machine** — the
/// reconcile that `sync-agent.md` § Credential model owes consequence 1 of
/// `on-demand-files.md` § Multi-account × File Provider.
///
/// **Why it exists.** Tearing the agent down is one best-effort local RPC
/// (`SyncAgentProvisioner::unprovision`), and what that RPC must revoke is
/// built to outlive everything: the capability persists in the secure store,
/// the bearer self-renews app-dead off a grant that deliberately carries **no
/// expiry**, and both resume across reboots. So a single lost message — a
/// wedged (not dead) agent at the teardown instant, a connect timeout under
/// load — used to leave a signed-out account's engines serving indefinitely,
/// with nothing to ever correct it. The app writes this marker **before** it
/// sends that message, so the durable record of the sign-out does not depend on
/// the message arriving.
///
/// **Why it is ordered, not a flag.** The agent must never mistake a *stale*
/// marker for a live sign-out: signing back in re-provisions, and a leftover
/// marker that vetoed that would silently stop sync — the exact breach the
/// no-expiry grant exists to prevent, merely inverted. So the marker carries the
/// instant of the sign-out and a capability carries the instant of its
/// provision ([`SyncCapability::provisioned_at_ms`]); the veto applies only to a
/// capability minted *at or before* the sign-out ([`capability_is_signed_out`]).
/// A re-provision therefore outranks the marker by construction, and clearing
/// the marker is an optimization rather than a correctness requirement — which
/// matters, because `SecretStore::set`/`delete` are infallible and a failed
/// clear is undetectable.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedOutMarker {
    /// The `actor_id` that signed out, 32 raw bytes. Public material.
    #[serde(with = "serde_bytes")]
    actor_id: Vec<u8>,
    /// Unix milliseconds of the sign-out, on this machine's clock.
    pub signed_out_at_ms: u64,
}

impl SignedOutMarker {
    pub fn new(actor_id: Vec<u8>, signed_out_at_ms: u64) -> Self {
        Self {
            actor_id,
            signed_out_at_ms,
        }
    }

    /// The signed-out `actor_id` as a fixed 32-byte array, or `None` if the
    /// stored value is the wrong length.
    pub fn actor_id_array(&self) -> Option<[u8; 32]> {
        self.actor_id.as_slice().try_into().ok()
    }

    /// The at-rest record: hex over canonical dag-cbor, the same framing the
    /// capability record uses. Owned here so the writing app and the reading
    /// agent cannot frame it differently.
    pub fn encode_record(&self) -> Option<String> {
        fauna_cbor::encode_canonical(self).ok().map(hex::encode)
    }

    /// Inverse of [`encode_record`](Self::encode_record); `None` on any
    /// malformed record (callers treat that as "no marker", never as a veto).
    pub fn decode_record(record: &str) -> Option<Self> {
        let bytes = hex::decode(record).ok()?;
        fauna_cbor::decode_strict::<Self>(&bytes).ok()
    }
}

impl std::fmt::Debug for SignedOutMarker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignedOutMarker")
            .field("actor_id", &format_args!("<{} bytes>", self.actor_id.len()))
            .field("signed_out_at_ms", &self.signed_out_at_ms)
            .finish()
    }
}

/// Does `marker` revoke `cap`? The whole decision of the signed-out reconcile,
/// pure so both the agent's boot path and its renewal loop ask it the same way.
///
/// True iff the marker names **this capability's actor** and the capability was
/// provisioned at or before the sign-out. Both halves are load-bearing:
///
/// * **Same actor** — a marker for account A must not disturb a capability
///   serving account B. The agent's slot is single (provision is
///   delete-then-add), so a second app signed in as B is the live one, and a
///   sibling app sitting at A's onboarding screen must not tear B's sync down.
/// * **At or before** — a re-provision after the sign-out is the user signing
///   back in, and it outranks the marker.
///
/// The tie (`==`) revokes deliberately. A same-millisecond provision and
/// sign-out is only reachable when both happen at once, where a live app's next
/// tick re-provisions with a later stamp — so revoking on a tie costs at most
/// one tick of sync, while serving on a tie is unbounded.
pub fn capability_is_signed_out(cap: &SyncCapability, marker: &SignedOutMarker) -> bool {
    match (cap.actor_id_array(), marker.actor_id_array()) {
        (Some(cap_actor), Some(marker_actor)) if cap_actor == marker_actor => {
            cap.provisioned_at_ms <= marker.signed_out_at_ms
        }
        // A malformed actor id on either side names no account, so it revokes
        // nothing — the marker is only ever allowed to *remove* authority, and a
        // record it cannot attribute must not remove anyone's.
        _ => false,
    }
}

/// What one poked custodian pull pass did — the reply to
/// [`RequestMethod::CustodianRunPassNow`], and the assertion surface of the
/// tier_3 `enroll → pull → check-in → status` proof
/// (`../../docs/goal/behavior/backup-destinations.md` § Implementation status today).
///
/// **Test-only**, and shaped so a test never has to sleep: because the reply is
/// sent only after the pass completes, every field below describes a pass that
/// has already finished writing its check-in.
#[cfg(any(test, debug_assertions, feature = "test-helpers"))]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CustodianPassReport {
    /// Whether this process was hosting a custodian replica at all.
    ///
    /// `false` is a **meaningful negative, not an error** (the same choice the
    /// nest hook's `owners_run: 0` makes): the device is not enrolled, or the
    /// host loop has not yet re-read the registry — `IDLE_RECHECK_SECS` after
    /// enrollment, worst case. A test therefore polls *this* until it flips
    /// instead of sleeping out that interval, which is what keeps the proof
    /// latency-independent (`testing.md` convention 14).
    pub hosting: bool,
    /// How many backed-up kinds the pass drove. `run_all_kinds` walks
    /// `BACKED_UP_KINDS` itself, so this grows with a per-kind rollout and is
    /// deliberately not a fixed expectation for a caller to hard-code.
    pub kinds_run: u32,
    /// Bytes this device holds after the pass, as reported to the nest.
    pub held_bytes: u64,
    /// The pass's own cap verdict (`CAP_STATE_OK` / `CAP_STATE_REACHED`), read
    /// from the verdict and **never** inferred from `held >= cap` — a pass that
    /// stopped at its cap ends *below* it
    /// (`../../docs/goal/behavior/backup-destinations.md` § Third destination kind).
    pub cap_state: Option<String>,
    /// The self-audit verdict this pass reported, when it ran one
    /// (`AUDIT_STATE_OK` / `AUDIT_STATE_FAILED`). `None` is *not audited on this
    /// pass* — the audit is debounced to `AUDIT_MIN_INTERVAL` — and never a pass.
    pub audit_state: Option<String>,
    /// Whether the pass's check-in reached the source nest. A pass that pulled
    /// bytes but could not check in leaves the owner's page unchanged, so the
    /// two are reported separately rather than folded into one boolean.
    pub checked_in: bool,
}

// ── Response envelope ──

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub result: ResponseResult,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ResponseResult {
    Ok(ResponsePayload),
    Err(String),
}

impl crate::RefuseUndecodedRequest for Response {
    fn refuse_undecoded_request(id: u64) -> Self {
        Response {
            id,
            result: ResponseResult::Err(UNSUPPORTED_METHOD_ERROR_MESSAGE.into()),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ResponsePayload {
    Empty,
    SyncStatus(SyncStatusInfo),
    Locations(Vec<LocationInfo>),
    FileStatus(FileStatusInfo),
    ServiceStatus(ServiceStatusInfo),
    FileDevices(FileDevicesInfo),
    FileVersions(FileVersionsInfo),
    /// Reply to [`RequestMethod::ShareFile`]: where the app should open to share
    /// the item. The agent never mints a link — it is seedless and a token must
    /// be signed by the account key (`share-links.md` § Windows Explorer's Share
    /// leaf) — so it resolves the Explorer path and the shell opens the app there
    /// (`apps/windows.md` § Shell Extension → *The Share hand-off*).
    ShareTarget(ShareTargetInfo),
    FileVersionList(FileVersionListInfo),
    /// Reply to [`RequestMethod::ListEngines`] — one row per hosted folder.
    Engines(Vec<EngineInfo>),
    /// Reply to [`RequestMethod::GetBackupStatus`] — one row per backup destination.
    BackupStatus(Vec<BackupDestinationStatus>),
    /// Reply to [`RequestMethod::ApplyHeldDeletes`].
    HeldDeletesApplied(HeldDeletesAppliedInfo),
    /// Reply to [`RequestMethod::GetShareServeInfo`] — one row per bound
    /// folder with a state DB on disk.
    ShareServeInfo(Vec<ShareServeFolderInfo>),
    /// Reply to [`RequestMethod::ShareIngest`].
    ShareIngested(ShareIngestOutcome),
    /// Reply to [`RequestMethod::GetCustodianStore`].
    CustodianStore(CustodianStoreInfo),
    /// Reply to [`RequestMethod::ReclaimCustodianStore`].
    CustodianStoreReclaimed(CustodianReclaimOutcome),
    /// Reply to [`RequestMethod::ReseedCustodianStore`] and
    /// [`RequestMethod::GetCustodianReseed`].
    CustodianReseed(CustodianReseedState),
    /// Reply to [`RequestMethod::GetCustodianFolderNames`], sorted.
    CustodianFolderNames(Vec<String>),
    /// **Test-only.** Reply to [`RequestMethod::CustodianRunPassNow`].
    #[cfg(any(test, debug_assertions, feature = "test-helpers"))]
    CustodianPassReport(CustodianPassReport),
}

/// What this device's sealed custodian store occupies — the reply to
/// [`RequestMethod::GetCustodianStore`], and the wire form of
/// `fauna_sync_engine::custodian_store::StoreFootprint`.
///
/// `bytes` is **disk** truth and `generations` is **index** truth, and the two
/// disagreeing is not a contradiction: an interrupted store write leaves blobs
/// on disk that no index row names, and that space is exactly what the reclaim
/// affordance gives back. So `bytes > 0` — not `generations > 0` — is what
/// "this device holds a sealed store" means.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodianStoreInfo {
    /// Generations the store's index calls held.
    pub generations: u64,
    /// Blob + manifest files on disk, recorded or orphaned alike.
    pub files: u64,
    /// Bytes those files occupy.
    pub bytes: u64,
    /// The source regressions standing on the store's audit record — each a
    /// pull pass refused because the source served a saved counter below the
    /// ledger the store holds (`segment-backup-protocol.md` § Client-device
    /// custodian (pull) → *The pull never tombstones against a source below
    /// its copy*). The app folds them into this device's own destination row
    /// (`fauna_client_backup::audit::run_audit_pass`). `#[serde(default)]`
    /// is the additive discipline: a reply without the key decodes as none.
    #[serde(default)]
    pub source_regressions: Vec<CustodianSourceRegression>,
}

/// One refused pull on [`CustodianStoreInfo`] — the wire form of
/// `fauna_sync_engine::custodian_store::SourceRegression` with its key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodianSourceRegression {
    /// The held ledger's own path in the store — one per set and family.
    pub ledger: String,
    /// The generation of the ledger the store holds.
    pub held: u32,
    /// The lower counter the source served.
    pub served: u32,
    /// Unix seconds of the first pass that found the source there.
    pub observed_at: i64,
}

impl CustodianStoreInfo {
    /// Is there anything here to reclaim? — the `store_holds_bytes` input to
    /// `fauna_core::data::custodian_store_is_orphaned`.
    pub fn holds_bytes(&self) -> bool {
        self.bytes > 0
    }
}

/// What a [`RequestMethod::ReclaimCustodianStore`] did.
///
/// A refusal is a **reported outcome, not an error**, for the reason every
/// custodian report on this seam is: the caller is a page that must say
/// something specific to the user, and an error string it has to pattern-match
/// is how two apps end up rendering the same refusal differently.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodianReclaimOutcome {
    /// This device is still hosting a custodian replica and would not stop, so
    /// **nothing was deleted**. The agent stops a live stint before reclaiming;
    /// this is the answer when that teardown did not complete, which means a
    /// pull pass may still be writing into the store.
    ///
    /// Not expected in the ordinary flow — the affordance is only offered for a
    /// store no destination row claims, and a stint exists only for a row that
    /// does — so it is the wind-down window, and retrying is the whole remedy.
    pub still_hosting: bool,
    /// Files deleted (blobs + manifests, orphans included).
    pub freed_files: u64,
    /// Bytes those files occupied.
    pub freed_bytes: u64,
}

/// What [`RequestMethod::ReseedCustodianStore`] carries.
#[derive(Serialize, Deserialize)]
pub struct CustodianReseedRequest {
    /// The owner's `NestBackupKey`, exactly 32 raw bytes. Granted to the target
    /// and used to re-seal the held corpus; zeroized on drop, never logged,
    /// never persisted.
    ///
    /// The key is the whole request: a covered folder's display name is not
    /// the app's to supply — the agent's store holds it, recorded by the pull
    /// off the owner's coverage listing (2026-09-29), so the ceremony never
    /// depends on a caller passing a map it has no source for.
    #[serde(with = "serde_bytes")]
    nest_backup_key: Vec<u8>,
}

impl CustodianReseedRequest {
    pub fn new(nest_backup_key: [u8; 32]) -> Self {
        Self {
            nest_backup_key: nest_backup_key.to_vec(),
        }
    }

    /// The key, or `None` when the frame carried the wrong length.
    pub fn nest_backup_key_array(&self) -> Option<zeroize::Zeroizing<[u8; 32]>> {
        let bytes: [u8; 32] = self.nest_backup_key.as_slice().try_into().ok()?;
        Some(zeroize::Zeroizing::new(bytes))
    }
}

impl Drop for CustodianReseedRequest {
    fn drop(&mut self) {
        self.nest_backup_key.zeroize();
    }
}

impl std::fmt::Debug for CustodianReseedRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustodianReseedRequest")
            .field(
                "nest_backup_key",
                &format_args!("<{} bytes redacted>", self.nest_backup_key.len()),
            )
            .finish()
    }
}

/// The re-seed job, as [`RequestMethod::GetCustodianReseed`] reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CustodianReseedState {
    /// No job since the agent started.
    Idle,
    /// A job is running.
    Running,
    /// The ceremony reached its end; each set's result is in the report.
    Finished(CustodianReseedReport),
    /// The ceremony stopped before materializing anything, or lost its
    /// connection mid-materialize. Re-running is always safe (every phase
    /// resumes), and `phase` says where it stopped: `store` (no store to read,
    /// or the stint would not stop), `grant`, `delivery` or `transport`.
    Failed { phase: String, detail: String },
    /// A state a newer agent names that this build does not, carried whole
    /// (`transport.md` § Rule 3 in full). It reads as [`Self::Failed`], never
    /// as running, so an older app's poll ends instead of waiting on a job it
    /// cannot read (`fauna_client_sync::reseed_wire::job_from_state`). Never
    /// written by this build.
    #[serde(untagged)]
    Unknown(fauna_cbor::CarriedValue),
}

/// The wire form of `fauna_client_backup::reseed::ReseedOutcome`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodianReseedReport {
    /// One entry per delivered set, in materialize order.
    pub sets: Vec<CustodianReseedSet>,
    /// Segments delivered without a sidecar.
    pub sidecarless_segments: Vec<u32>,
    /// Covered-folder paths delivered without a sealed name.
    pub folder_paths_without_seal: Vec<String>,
    /// Sum of the delivered paths' plaintext sizes.
    pub plaintext_bytes: u64,
}

/// One delivered set's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodianReseedSet {
    pub set_name: String,
    /// `true` for a covered-folder set, `false` for a reserved segment set.
    pub folder: bool,
    /// A covered folder's display name, as the agent's store held it — what
    /// the set was materialized under, and how the app labels its line.
    /// `None` on a segment set and on a folder set the store held no name for.
    #[serde(default)]
    pub folder_display_name: Option<String>,
    pub outcome: CustodianReseedSetOutcome,
}

/// What phase 3 did with one set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CustodianReseedSetOutcome {
    Materialized {
        segments: Vec<u32>,
        records: u64,
    },
    AlreadyLive,
    /// `code` is `SetRefusal::code`: the nest's wire code, or a local one.
    Refused {
        code: String,
        detail: String,
    },
    /// An outcome a newer agent names that this build does not, carried whole
    /// (`transport.md` § Rule 3 in full). It reads as [`Self::Refused`] —
    /// nothing is claimed for the set. Never written by this build.
    #[serde(untagged)]
    Unknown(fauna_cbor::CarriedValue),
}

/// One bound folder's serve-side locations
/// ([`RequestMethod::GetShareServeInfo`]): everything the app process needs
/// to open the second read connection and range-read the local tree.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShareServeFolderInfo {
    /// The nest folder name (the set the agent serves).
    ///
    /// ⚠ Names are unique only per owner, so this alone cannot say WHICH
    /// set a row is — [`folder_id`](Self::folder_id) is the unambiguous key
    /// and the join consumers match on it first.
    pub folder: String,
    /// The set's `FolderRef` wire form — every binding carries one since
    /// the 2026-09-24 compat-remnant sweep retired the name-keyed binding.
    pub folder_id: String,
    /// The bound local tree — the byte half's range-read root.
    pub watch_dir: String,
    /// The set's per-folder state DB (rows + retained manifests + cursors).
    pub db_path: String,
}

/// Outcome of one [`RequestMethod::ShareIngest`] page — the wire mirror of
/// the engine's ingest report plus the advanced pull cursor.
///
/// `Default` is deliberate: this wire struct grows additively, and hand-listed
/// fixtures break on every addition. Construct with `..Default::default()`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShareIngestOutcome {
    /// Rows the engine's provenance re-check (or the decode) refused.
    pub refused: u32,
    /// Overlay rows landed or refreshed (deletes included).
    pub overlaid: u32,
    /// Provisional rows whose bytes were fetched and written.
    pub materialized: u32,
    /// Rows already current locally.
    pub already_current: u32,
    /// `(path, reason)` — rows whose materialization was skipped; their
    /// overlay rows still stand unmaterialized.
    pub skipped: Vec<(String, String)>,
    /// The per-peer pull cursor after this page — the pump's next `since`.
    pub cursor: i64,
}

/// Outcome of a user-confirmed [`RequestMethod::ApplyHeldDeletes`] — the wire
/// mirror of the engine's own verdict. Three renderable shapes: applied in
/// full (`applied > 0, remaining_held == 0`), partial-with-retry
/// (`remaining_held > 0` — some records failed; re-invoking resumes), and
/// nothing-was-held (`floor_was_active == false` — the files came back, or a
/// partial state ordinary reconcile owns; clear the surface).
///
/// `Default` is deliberate: this wire struct grows additively, and hand-listed
/// fixtures break on every addition. Construct with `..Default::default()`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct HeldDeletesAppliedInfo {
    /// Rows whose delete record reached the nest.
    pub applied: u64,
    /// Rows still held (each survives for retry).
    pub remaining_held: u64,
    /// Whether a hold was actually active when the verb ran; `false` means
    /// nothing was applied by design (the re-derive-now rule).
    pub floor_was_active: bool,
}

// ── Server-pushed events ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub event: EventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventKind {
    SyncProgress(SyncProgressInfo),
    FileStatusChanged { path: String, status: FileStatus },
    ConnectionStateChanged(ConnectionState),
}

// ── Data types ──

/// `Default` is deliberate, for the same reason [`LocationInfo`] carries one:
/// this wire struct grows additively, and hand-listed fixtures break on every
/// addition. Construct with `..Default::default()`. The default is the honest
/// nothing-known-yet reading (disconnected, idle, nothing pending).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncStatusInfo {
    pub connected: bool,
    pub syncing: bool,
    pub files_pending: u64,
    pub bytes_pending: u64,
    pub last_sync: Option<u64>,
}

/// The [`LocationInfo::mode`] a **fresh** binding starts in on this platform —
/// the one home of the per-platform default (`on-demand-files.md` § On-Demand
/// Files → *The choice is the user's*; user ruling 2026-09-26): `"on-demand"`
/// on windows, `"always"` everywhere else. The agent's `AddLocation` gives a new
/// path this mode (`fauna-sync-agent`'s `LocationMode::fresh_binding_default`
/// delegates here), and an app's optimistic binding row renders it until the
/// agent's `ListLocations` reports the row — so the two cannot disagree and the
/// mode switch does not flicker between the bind and the reconcile. An app and
/// the agent it drives always share a box, so `cfg!` is the agent's answer too.
pub fn fresh_binding_mode() -> &'static str {
    if cfg!(windows) { "on-demand" } else { "always" }
}

/// `Default` is deliberate: this wire struct grows additively, and hand-listed
/// fixtures break on every addition. Construct with `..Default::default()`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LocationInfo {
    pub path: String,
    pub file_count: u64,
    pub total_bytes: u64,
    pub status: LocationStatus,
    pub mode: String,
    /// Nest folder this location is bound to, or `None` if unbound. Mirrors the
    /// device-local `LocationConfig::binding`'s label; the Sync-folders page
    /// renders it per row (ui.yaml § sync `folder-location-fileset`). `None` is *not*
    /// a default name — an unbound on-demand folder is logged + skipped, never
    /// served under a manufactured folder name.
    ///
    /// A **label**: two sets can share a name, so an app that must know which
    /// set a location is bound to reads [`folder_id`](Self::folder_id).
    pub folder: Option<String>,
    /// The bound set's `FolderRef` wire form — `Some` exactly when
    /// [`folder`](Self::folder) is. What an app adopting an agent-side binding
    /// it did not write itself keys the row by (2026-09-24, with the retired
    /// name-keyed binding).
    #[serde(default)]
    pub folder_id: Option<String>,
    /// The bound set's write grant was **revoked** by the nest that owns it, so
    /// this binding is parked: no engine runs for it, and the client renders it
    /// as such (`file-sync.md` § Multi-writer shared sets — D4, *fail-closed AND
    /// loud*; `folder-access-revoked-warning`).
    ///
    /// The folder and its contents are untouched — this says the folder stopped
    /// being *tracked*, never that anything was removed. Clearing it is a
    /// re-bind, which re-verifies the grant against the authoritative nest.
    ///
    /// `#[serde(default)]` keeps it additive across an app/agent version skew
    /// (they ship in lockstep, but the socket must tolerate the upgrade window):
    /// an agent that does not set it decodes as `false` = live.
    #[serde(default)]
    pub access_revoked: bool,
    /// Why this **on-demand** binding's root is running without its placeholder
    /// surface — one of the `ON_DEMAND_MOUNT_*` codes, set while the root's
    /// mount attempt has failed (`on-demand-files.md` § Linux FUSE binding, the
    /// lifecycle rule: the one refusal the boot probe cannot see is per
    /// location). The hydrated files keep syncing two-way and no placeholder is
    /// listed; an app renders its line for the code on the binding's row.
    /// `None` — a mounted root, an always-resident binding, every other
    /// platform — renders nothing. A code and not
    /// an enum for the reason `on_demand_unavailable_reason` is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_demand_mount_error: Option<String>,
}

/// [`LocationInfo::on_demand_mount_error`]: the mount helper refused this
/// location (a confined `fusermount3` admits a mount point only under the
/// user's home, `/mnt`, `/media`, `/run/user/<uid>` or `/tmp`).
pub const ON_DEMAND_MOUNT_REFUSED: &str = "mount-refused";
/// [`LocationInfo::on_demand_mount_error`]: the mount failed for any other
/// reason (the bound directory could not be opened, the helper failed).
pub const ON_DEMAND_MOUNT_FAILED: &str = "mount-failed";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum LocationStatus {
    /// Nothing pending — also the `Default`, which exists so [`LocationInfo`]
    /// can derive one (see its note on fixture-shape conflicts).
    #[default]
    Synced,
    Syncing,
    Error(String),
    Paused,
    /// A status a newer agent names that this build does not, carried whole
    /// (`transport.md` § Rule 3 in full). It renders as an error state, never
    /// as Synced. Never written by this build.
    #[serde(untagged)]
    Unknown(fauna_cbor::CarriedValue),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[repr(u32)]
pub enum FileStatus {
    Synced = 0,
    Syncing = 1,
    CloudOnly = 2,
    Error = 3,
    NotTracked = 4,
    /// A status a newer agent names that this build does not
    /// (`transport.md` § Rule 3 in full): no overlay badge and no context
    /// menu. Never written back — a frame holding it fails to encode.
    #[serde(other, skip_serializing)]
    Unknown = 5,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileStatusInfo {
    pub path: String,
    pub status: FileStatus,
    pub size_bytes: u64,
    pub is_pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncProgressInfo {
    pub folder: String,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[repr(u32)]
pub enum ConnectionState {
    Connected = 0,
    Connecting = 1,
    /// The default: a state we have not yet observed reads as disconnected, never
    /// as connected.
    #[default]
    Disconnected = 2,
    /// A state a newer agent names that this build does not
    /// (`transport.md` § Rule 3 in full); it reads as [`Self::Disconnected`].
    /// Never written back — a frame holding it fails to encode.
    #[serde(other, skip_serializing)]
    Unknown = 3,
}

/// `Default` is deliberate — see [`SyncStatusInfo`]. Construct with
/// `..Default::default()`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServiceStatusInfo {
    pub version: String,
    pub uptime_secs: u64,
    pub connection: ConnectionState,
    pub sync: SyncStatusInfo,
    /// **The principal-support advertisement** — hex actor id of the account
    /// this agent renews for as the machine's *store principal*, or `None`.
    ///
    /// Live status only since the RULED 2026-09-28 block
    /// (`sync-agent-credentials.md` § Credential model, decision 3): the store
    /// principal is the machine's one renewal credential, so this decides no
    /// mint. It names an actor only when this agent holds that actor's
    /// principal key in the shared T10 slot **and** the enrollment's
    /// registration latch shows the grant registered — a claim asserted by the
    /// process that will actually do the renewing. Its consumer is the
    /// signed-out onboarding reconcile, which matches the marker's actor
    /// against it (`fauna_client_sync::agent::signed_out_onboarding_reconcile`).
    ///
    /// Actor-qualified rather than a bare bool, because the agent's slot is
    /// single: an answer about account A must never read as one about B.
    ///
    /// `None` from an agent that has not resolved its principal — the
    /// cold-launch window, an idle one, or one whose grant is not registered
    /// yet (`serde(default)` also covers any post-sweep additive skew).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub store_principal_actor: Option<String>,
    /// **The machine's sync device id** — the hex id this agent presents as
    /// [`SyncCapability::device_id`], i.e. the machine's named `sync_devices`
    /// row: the app's own derived id, which the app pushed and which the
    /// enrollment targets unconditionally (`sync-agent-credentials.md`
    /// § Credential model → the RULED 2026-09-28 block, decision 3).
    ///
    /// Live status only: the two halves of `this_device_row`'s *enrolled wins*
    /// rule agree by construction, so this decides no enrollment target any
    /// more.
    ///
    /// `None` from a provisioned-but-idle agent (and, under `serde(default)`,
    /// across any post-sweep additive skew) reads as "cannot tell".
    /// A field read of the in-memory capability, never a keyring or nest round
    /// trip: this reply rides the app's convergence tick (e2e convention 11's
    /// no-blocking-I/O corollary).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_device_id: Option<String>,
    /// **The nest refused this machine's renewal for good** — the store
    /// principal was answered `fauna.auth.not_registered` (the device was
    /// removed from the devices list, the nest was factory-reset, or no
    /// enrollment ever registered). `true` from that refusal until a renewal
    /// under the principal succeeds: an app's pushed bearer keeps sync going
    /// while that app runs and does not clear it (`sync-agent-credentials.md`
    /// § Credential model → *A refused renewal is terminal*). An app reading
    /// `true` knows app-dead sync is stopped and why — the *Not enrolled*
    /// reading; signing in again on the machine is the fix. `false` (absent,
    /// under `serde(default)`) reads as "not known to be refused".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_reenrollment: bool,
    /// ***Keys pending*** — some bound set's engine holds a content-key
    /// generation behind its row's `content_key_floor`, or was built keyless
    /// because custody could not be read (`sync-agent.md` § Local agent health →
    /// *Keys pending*, the agent-side derivation). True from the first re-resolve
    /// edge that found it so until an edge lands the generation; writes to such a
    /// set are held meanwhile, never sealed under the older generation. The
    /// third input of the app's `agent_health_state()`. A field read of the
    /// agent's in-memory resolution, never a nest round trip.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub keys_pending: bool,
    /// **Can this agent post an OS notification here?** — the `ws-device`
    /// push arm's sink (`sync-agent.md` § Local agent health; `common.md`
    /// § Push Notifications → *Transports*). `Some(true)` when the agent's
    /// notification arm is built for this platform and a desktop session is
    /// present; `Some(false)` when it is not (a headless agent, tui over SSH,
    /// or a platform whose arm is still unbuilt) — the app's push control then
    /// renders its inline "no notification sink" line. A field, not a sixth
    /// `AgentHealthState`: it describes one feature's sink, not the agent's
    /// health. `None` (not probed yet, or none) reads as "cannot
    /// tell", which renders no failure line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_sink: Option<bool>,
    /// **Can this agent serve an on-demand binding here?** — whether the
    /// platform's placeholder surface is usable by this agent
    /// (`on-demand-files.md` § Linux FUSE binding, the lifecycle rule).
    /// windows: always `Some(true)` (cfapi). linux: the boot probe's answer —
    /// an openable `/dev/fuse` and the `fuse3` package's mount helper on
    /// `PATH`. macOS: `Some(false)` — on-demand there is the File Provider
    /// extension's, never this agent's. An app renders its mode control
    /// disabled with the reason when this is `Some(false)`; `None` (the agent
    /// has not probed yet) reads as "cannot tell".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_demand_available: Option<bool>,
    /// Why [`Self::on_demand_available`] is `Some(false)` — one of the
    /// `ON_DEMAND_REASON_*` codes, which an app maps to its own localized
    /// line. A code and not an enum so an agent newer than the app can name a
    /// reason the app has no line for (it renders its generic one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_demand_unavailable_reason: Option<String>,
}

/// [`ServiceStatusInfo::on_demand_unavailable_reason`]: `/dev/fuse` cannot be
/// opened (no FUSE in this kernel or sandbox).
pub const ON_DEMAND_REASON_NO_FUSE_DEVICE: &str = "no-fuse-device";
/// [`ServiceStatusInfo::on_demand_unavailable_reason`]: the `fuse3` package's
/// `fusermount3` helper is not on `PATH`.
pub const ON_DEMAND_REASON_NO_FUSERMOUNT: &str = "no-fusermount3";
/// [`ServiceStatusInfo::on_demand_unavailable_reason`]: this platform's
/// on-demand surface is not hosted by the agent at all.
pub const ON_DEMAND_REASON_NOT_AGENT_HOSTED: &str = "not-agent-hosted";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDevicesInfo {
    pub path: String,
    pub device_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileVersionsInfo {
    pub path: String,
    pub version_count: u32,
    pub latest_timestamp: Option<u64>,
}

/// One entry of a file's nest-side version history (`fauna.files.versions.list`).
///
/// Metadata only — the shell extension renders it and, on restore, hands
/// `version_num` back. The manifest hash / content-key generation a restore must
/// carry verbatim (`file-sync.md` § Restore) stay service-side: the service
/// re-reads them via `fauna.files.versions.get` rather than round-tripping secrets
/// of the content plane through Explorer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileVersionEntry {
    /// The recording `sync_changes` row's **`seq`** — stable, unique, never
    /// renumbered, and **not** a dense `1..N` ordinal (`file-sync.md` § File
    /// Versions). Display ordinals are derived from list position.
    pub version_num: i64,
    pub size_bytes: i64,
    /// Unix seconds, as recorded by the nest.
    pub created_at: i64,
}

/// A file's version history, oldest → newest (the nest's list order).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileVersionListInfo {
    /// The absolute Windows path the shell extension asked about, echoed back.
    pub path: String,
    /// The folder serving `path`. Empty when the path resolves to no served
    /// on-demand folder (then `versions` is empty too).
    pub folder: String,
    pub versions: Vec<FileVersionEntry>,
}

/// A [`ResponsePayload::ShareTarget`]: the bound set and, for a file, the
/// folder-relative path — exactly what `fauna_core::app_route::AppRoute` names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareTargetInfo {
    /// The bound set's durable id (`folders.id` — a `FolderRef::Local`).
    pub folder_id: i64,
    /// The file's forward-slash folder-relative path; `None` for the set's own
    /// root (Share on a bound folder).
    pub path: Option<String>,
}

/// One row of [`ResponsePayload::Engines`] — a folder the agent currently hosts,
/// with its mode, whether it is actively serving, and its upload backlog. Populated
/// by the agent from its running `EngineHost`; consumed by fauna-tui and the desktop
/// apps' agent-status views (`sync-agent.md` § Control plane split, `ListEngines`).
///
/// `Default` is deliberate, for the same reason [`LocationInfo`] carries one: this
/// wire struct grows additively, and hand-listed fixtures break on every addition.
/// Construct with `..Default::default()`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EngineInfo {
    /// The nest folder this engine serves — a label; two engines can share it.
    pub folder: String,
    /// The served set's `FolderRef` wire form — the engine's key, and what
    /// an app joins this row to its location rows by (2026-09-24).
    pub folder_id: String,
    /// The set's mode as the wire string every app shares
    /// (`"always"` | `"on-demand"`; the exact values [`LocationInfo::mode`] emits,
    /// via the shared `LocationMode::wire_str` namer).
    pub mode: String,
    /// Whether the engine is currently running / serving (vs. planned but not started).
    pub serving: bool,
    /// Files still queued for upload for this set (`0` when idle or not yet known).
    pub files_pending: u64,
    /// Bytes still queued for upload for this set (`0` when idle or not yet known).
    pub bytes_pending: u64,
    /// What this set's **post-succession corpus re-seal** has done so far, or
    /// `None` when it has never run one — which is the reading for every identity
    /// that never succeeded, and for every bound set (whose chunks rest under
    /// content keys, not an owner root).
    ///
    /// Additive both directions (`#[serde(default)]`), exactly like
    /// [`SyncCapability::predecessor_backup_keys`], whose window this reports the
    /// closing of: an agent that omits the field leaves a client reading `None`
    /// (honest — it does not know), and a client that does not know the field
    /// ignores it.
    #[serde(default)]
    pub corpus_reseal: Option<CorpusResealInfo>,
    /// Whether this engine still needs the retired owner keys — the observable
    /// `sync-agent.md` § Credential model **bound (3)** is enforced on, computed
    /// fresh in the `ListEngines` handler from the live pending list plus the
    /// fold marker (never from [`Self::corpus_reseal`], which is a *report*).
    ///
    /// `None` means **cannot tell**, and every reader must treat it exactly like
    /// "not drained": a set whose DB could
    /// not be opened has no answer. Additive both directions
    /// (`#[serde(default)]`), like [`Self::corpus_reseal`] beside it.
    ///
    /// ⚠ Not a license on its own — one engine draining says nothing about the
    /// others this agent hosts. The conjunction over the whole roster is taken
    /// in exactly one place, `fauna_client_sync::agent::predecessor_keys_may_be_dropped`.
    #[serde(default)]
    pub reseal_drain: Option<ResealDrainInfo>,
    /// How many deletes the **mass-delete floor** held on this set's last
    /// reconcile pass — `0` when it did not engage, which is every ordinary
    /// set (`file-sync.md` § Files Appear Automatically, ratified 2026-08-02).
    ///
    /// Non-zero means *"the root is there and every tracked file vanished at
    /// once"* — an unmounted volume or a folder removed under the engine — and
    /// **nothing was recorded**: the nest still holds the set. The app renders
    /// it as "folder emptied — N deletions held" so the user can reconnect the
    /// folder, or explicitly apply the deletions.
    ///
    /// `u64` and not `Option<u64>`, unlike the two fields above: they
    /// distinguish *cannot tell* from *nothing owed*, whereas an agent that
    /// reports no hold and an agent that omits the field are the same
    /// thing to a reader — both mean "do not warn". So `0` is the honest
    /// default in both cases, and `#[serde(default)]` makes an omission
    /// read as it.
    ///
    /// ⚠ **Derived, never stored** — the value lives in the agent's memory for
    /// exactly as long as the engine that reported it (`ProgressEvent::
    /// DeletesHeld` → `SyncServiceState::deletes_held`). A restarted agent
    /// answers `0` until its first reconcile, which is the honest
    /// nothing-observed-yet reading and keeps a stale count from ever reaching
    /// the *"apply N deletions"* affordance.
    #[serde(default)]
    pub deletes_held: u64,
    /// How many deletes the delete rail **withheld because the path could not
    /// be read** on this set's last reconcile pass (`delete-propagation.md`
    /// § Unreadable is not absent) — `0` for every set whose tree reads.
    ///
    /// Non-zero means part of the folder is unreadable (lost permissions, a
    /// flapping mount): the engine refused to read the failure as a delete, so
    /// **nothing was changed anywhere**, and that subtree has stopped syncing
    /// both ways. Unlike [`Self::deletes_held`] there is nothing for the user to
    /// confirm — the app renders a status line with no action, the remedy being
    /// outside it.
    ///
    /// `u64` + `#[serde(default)]` for the hold's reasons: an agent that
    /// omits it and a healthy set both mean "do not warn". Derived,
    /// never stored — `ProgressEvent::DeletesSkippedUnreadable` →
    /// `SyncServiceState::deletes_skipped_unreadable`, with the engine's own
    /// lifetime.
    #[serde(default)]
    pub deletes_skipped_unreadable: u64,
}

/// The IPC-wire mirror of `fauna_sync_engine::succession_drain::ResealDrain` —
/// mapped at the same seam [`CorpusResealInfo`] is, so `fauna-ipc` stays free of
/// the engine dependency.
///
/// Two booleans rather than one so a cold reader can see *why* a device is not
/// licensed: waiting on its first fold and still owing re-seals are different
/// situations that resolve on different timescales.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResealDrainInfo {
    /// A full non-deferred fold has landed against the nest since this engine's
    /// DB was created, under the current owner root.
    pub folded: bool,
    /// The engine's pending-re-seal list was empty at answer time, under the
    /// current owner root.
    pub nothing_owed: bool,
    /// Every row holding bytes at rest was visible to that list — it is empty
    /// because nothing is owed, not because nothing was looked at.
    pub all_at_rest_classified: bool,
}

/// The IPC-wire mirror of `fauna_sync_engine::succession_progress::CorpusResealPass`
/// — the agent maps that record onto this serde type at the seam, so `fauna-ipc`
/// stays free of the engine dependency (the same split
/// [`BackupDestinationStatus`] follows).
///
/// The **projection** that turns this into a line of user-facing copy is
/// `fauna_client_sync::agent::CorpusResealProgress`, shared by every app, so no
/// client matches on these arms directly. Three arms because "done" and "still
/// owed" are the same settled pass with different remainders
/// (`succession-aftermath.md` § Re-key scope).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CorpusResealInfo {
    /// A pass is under way, or was interrupted before it settled.
    Running,
    /// A pass finished. `owed` counts entries it examined and could not move.
    Settled { resealed: u64, owed: u64 },
    /// The pass could not run at all; carries the agent's display string.
    Failed { reason: String },
    /// A pass state a newer agent names that this build does not, carried
    /// whole (`transport.md` § Rule 3 in full). It renders nothing — the
    /// projection reads it as no recorded pass — and it never fails the
    /// `ListEngines` reply that also feeds the predecessor-key licence. Never
    /// written by this build.
    #[serde(untagged)]
    Unknown(fauna_cbor::CarriedValue),
}

/// One row of [`ResponsePayload::BackupStatus`] — the IPC-wire shape of a
/// per-destination backup status row (a serde type of its own so fauna-ipc
/// stays free of the engine dependency). Same three fields and meaning as the
/// nest's `fauna.backup.status` rows (`behavior/backup-destinations.md`
/// § State & data shape → status read; `backup-destination-status-row`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupDestinationStatus {
    /// The configured destination this row reports on
    /// (`fauna_core::data::BackupDestination::destination_id`).
    pub destination_id: String,
    /// Unix seconds of the last successful manifest-mirror upload to this
    /// destination, or `None` until the first manifest has been mirrored.
    /// → `backup-destination-last-upload-time`.
    pub last_upload_time: Option<u64>,
    /// Source segments still queued for upload to this destination.
    /// → `backup-destination-backlog-count` ("N queued").
    pub backlog_count: u32,
}

#[cfg(test)]
mod signed_out_reconcile_tests {
    use super::*;

    const ACTOR_A: [u8; 32] = [0xAA; 32];
    const ACTOR_B: [u8; 32] = [0xBB; 32];

    fn cap_for(actor: [u8; 32], provisioned_at_ms: u64) -> SyncCapability {
        SyncCapability::new(
            vec![1u8; 32],
            actor.to_vec(),
            "https://nest.example".into(),
            "dev-1".into(),
            BearerToken::new("tok".into(), 1),
        )
        .with_provisioned_at_ms(provisioned_at_ms)
    }

    /// The case the whole reconcile exists for: the un-provision message was
    /// lost, so the capability that outlived the sign-out is revoked.
    #[test]
    fn a_capability_provisioned_before_the_sign_out_is_revoked() {
        let cap = cap_for(ACTOR_A, 1_000);
        let marker = SignedOutMarker::new(ACTOR_A.to_vec(), 2_000);
        assert!(capability_is_signed_out(&cap, &marker));
    }

    /// Signing back in outranks the marker — without this the reconcile would
    /// silently stop sync for a user who is signed in, inverting the very
    /// breach the no-expiry renewal grant exists to prevent.
    #[test]
    fn a_re_provision_after_the_sign_out_survives() {
        let cap = cap_for(ACTOR_A, 3_000);
        let marker = SignedOutMarker::new(ACTOR_A.to_vec(), 2_000);
        assert!(!capability_is_signed_out(&cap, &marker));
    }

    /// The agent's slot is single, so a sibling app sitting at account A's
    /// onboarding screen must not tear down the sync of account B, which is
    /// what actually provisioned.
    #[test]
    fn a_marker_for_another_account_revokes_nothing() {
        let cap = cap_for(ACTOR_B, 1_000);
        let marker = SignedOutMarker::new(ACTOR_A.to_vec(), 2_000);
        assert!(!capability_is_signed_out(&cap, &marker));
    }

    /// A capability `new` built and nothing stamped reads as time 0, older
    /// than any marker — the fail-safe direction.
    #[test]
    fn an_unstamped_capability_loses_to_any_marker() {
        let cap = SyncCapability::new(
            vec![1u8; 32],
            ACTOR_A.to_vec(),
            "https://nest.example".into(),
            "dev-1".into(),
            BearerToken::new("tok".into(), 1),
        );
        assert_eq!(cap.provisioned_at_ms, 0);
        assert!(capability_is_signed_out(
            &cap,
            &SignedOutMarker::new(ACTOR_A.to_vec(), 1)
        ));
    }

    /// A same-millisecond tie revokes: a live app re-provisions with a later
    /// stamp on its next tick, so the cost is bounded, while serving on a tie
    /// is not.
    #[test]
    fn a_tie_revokes() {
        let cap = cap_for(ACTOR_A, 2_000);
        let marker = SignedOutMarker::new(ACTOR_A.to_vec(), 2_000);
        assert!(capability_is_signed_out(&cap, &marker));
    }

    /// A record that names no account must not revoke anyone's — the marker is
    /// only ever allowed to remove authority.
    #[test]
    fn a_malformed_actor_id_revokes_nothing() {
        let cap = cap_for(ACTOR_A, 1_000);
        let marker = SignedOutMarker::new(vec![0xAA; 8], 2_000);
        assert!(!capability_is_signed_out(&cap, &marker));
    }

    /// The at-rest framing both processes share; a corrupt record decodes to
    /// `None` so the readers can treat it as "no marker".
    #[test]
    fn the_record_round_trips_and_corruption_reads_as_absent() {
        let marker = SignedOutMarker::new(ACTOR_A.to_vec(), 12_345);
        let record = marker.encode_record().expect("encodes");
        let back = SignedOutMarker::decode_record(&record).expect("decodes");
        assert_eq!(back.actor_id_array(), Some(ACTOR_A));
        assert_eq!(back.signed_out_at_ms, 12_345);
        assert!(SignedOutMarker::decode_record("not-hex-at-all!").is_none());
        assert!(SignedOutMarker::decode_record(&hex::encode([0u8; 4])).is_none());
    }

    /// The stamp is required: it round-trips, and a record without it is
    /// refused rather than read as some default.
    #[test]
    fn the_stamp_is_required_at_rest() {
        let cap = cap_for(ACTOR_A, 4_242);
        let bytes = fauna_cbor::encode_canonical(&cap).expect("encodes");
        let back: SyncCapability = fauna_cbor::decode_strict(&bytes).expect("decodes");
        assert_eq!(back.provisioned_at_ms, 4_242);

        let mut value: fauna_cbor::Value = fauna_cbor::decode_strict(&bytes).expect("a map");
        let fauna_cbor::Value::Map(fields) = &mut value else {
            panic!("a capability encodes as a map");
        };
        assert!(fields.remove("provisioned_at_ms").is_some());
        let stripped = fauna_cbor::encode_canonical(&value).expect("encodes");
        assert!(fauna_cbor::decode_strict::<SyncCapability>(&stripped).is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_payload, encode_frame};

    /// The store reply's regression list round-trips.
    #[test]
    fn a_store_reply_round_trips_its_source_regressions() {
        let info = CustodianStoreInfo {
            generations: 3,
            files: 5,
            bytes: 4096,
            source_regressions: vec![CustodianSourceRegression {
                ledger: "manifest.mail".into(),
                held: 40,
                served: 0,
                observed_at: 1_800_000_000,
            }],
        };
        let bytes = fauna_cbor::encode_canonical(&info).expect("encodes");
        let back: CustodianStoreInfo = fauna_cbor::decode_strict(&bytes).expect("decodes");
        assert_eq!(back, info);
    }

    #[test]
    fn round_trip_sync_request() {
        let req = Request {
            id: 1,
            method: RequestMethod::GetSyncStatus,
        };
        let frame = encode_frame(&req).unwrap();
        let decoded: Request = decode_payload(&frame[4..]).unwrap();
        assert_eq!(decoded.id, 1);
    }

    #[test]
    fn round_trip_sync_response() {
        let resp = Response {
            id: 1,
            result: ResponseResult::Ok(ResponsePayload::SyncStatus(SyncStatusInfo {
                connected: true,
                syncing: false,
                files_pending: 0,
                bytes_pending: 0,
                last_sync: Some(1711234567),
            })),
        };
        let frame = encode_frame(&resp).unwrap();
        let decoded: Response = decode_payload(&frame[4..]).unwrap();
        assert_eq!(decoded.id, 1);
    }

    /// The Explorer Share hand-off's reply: a file target and a folder-root
    /// target both survive the wire, the root's `path` staying `None`.
    #[test]
    fn round_trip_share_target_response() {
        for path in [Some("photos/holiday.jpg".to_string()), None] {
            let resp = Response {
                id: 9,
                result: ResponseResult::Ok(ResponsePayload::ShareTarget(ShareTargetInfo {
                    folder_id: 42,
                    path: path.clone(),
                })),
            };
            let frame = encode_frame(&resp).unwrap();
            let decoded: Response = decode_payload(&frame[4..]).unwrap();
            match decoded.result {
                ResponseResult::Ok(ResponsePayload::ShareTarget(t)) => {
                    assert_eq!(
                        t,
                        ShareTargetInfo {
                            folder_id: 42,
                            path
                        }
                    );
                }
                other => panic!("expected ShareTarget, got {other:?}"),
            }
        }
    }

    #[test]
    fn round_trip_list_file_versions_request() {
        let req = Request {
            id: 7,
            method: RequestMethod::ListFileVersions {
                path: r"C:\Sync\docs\report.txt".to_string(),
            },
        };
        let frame = encode_frame(&req).unwrap();
        let decoded: Request = decode_payload(&frame[4..]).unwrap();
        assert_eq!(decoded.id, 7);
        match decoded.method {
            RequestMethod::ListFileVersions { path } => {
                assert_eq!(path, r"C:\Sync\docs\report.txt");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn round_trip_file_version_list_response() {
        let resp = Response {
            id: 9,
            result: ResponseResult::Ok(ResponsePayload::FileVersionList(FileVersionListInfo {
                path: r"C:\Sync\docs\report.txt".to_string(),
                folder: "docs".to_string(),
                versions: vec![
                    FileVersionEntry {
                        version_num: 4,
                        size_bytes: 100,
                        created_at: 1_711_234_567,
                    },
                    FileVersionEntry {
                        version_num: 9,
                        size_bytes: 250,
                        created_at: 1_711_240_000,
                    },
                ],
            })),
        };
        let frame = encode_frame(&resp).unwrap();
        let decoded: Response = decode_payload(&frame[4..]).unwrap();
        match decoded.result {
            ResponseResult::Ok(ResponsePayload::FileVersionList(info)) => {
                assert_eq!(info.folder, "docs");
                assert_eq!(info.versions.len(), 2);
                // `version_num` is the nest's `sync_changes` seq — sparse, never a
                // dense 1..N ordinal (file-sync.md § File Versions).
                assert_eq!(info.versions[0].version_num, 4);
                assert_eq!(info.versions[1].version_num, 9);
                assert_eq!(info.versions[1].size_bytes, 250);
            }
            other => panic!("wrong payload: {other:?}"),
        }
    }

    #[test]
    fn round_trip_restore_file_version_request() {
        let req = Request {
            id: 11,
            method: RequestMethod::RestoreFileVersion {
                path: r"C:\Sync\docs\report.txt".to_string(),
                version_num: 42,
            },
        };
        let frame = encode_frame(&req).unwrap();
        let decoded: Request = decode_payload(&frame[4..]).unwrap();
        assert_eq!(decoded.id, 11);
        match decoded.method {
            RequestMethod::RestoreFileVersion { path, version_num } => {
                assert_eq!(path, r"C:\Sync\docs\report.txt");
                // Carried verbatim as the nest's `seq` — never renumbered.
                assert_eq!(version_num, 42);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    /// [`RequestMethod::name`] must **be** the wire key, for every verb — not a parallel
    /// list of strings that can quietly drift from it. Proven the only way that means
    /// anything: encode each variant and assert its `name()` appears verbatim in the frame.
    ///
    /// This is what makes the pipe server's per-request log line trustworthy (it logs
    /// `name()`), and it is why `name()` is a match rather than a `Debug` format — two
    /// variants carry key material and must never be logged whole.
    #[test]
    fn every_method_name_is_its_wire_key() {
        let cap = SyncCapability::new(
            vec![7u8; 32],
            vec![0x11u8; 32],
            "https://nest.example".into(),
            "dev".into(),
            BearerToken::new("tok".into(), 9_999),
        );
        let methods = vec![
            RequestMethod::GetSyncStatus,
            RequestMethod::AddLocation { path: "p".into() },
            RequestMethod::RemoveLocation { path: "p".into() },
            RequestMethod::ListLocations,
            RequestMethod::GetFileStatus { path: "p".into() },
            RequestMethod::PinFile { path: "p".into() },
            RequestMethod::UnpinFile { path: "p".into() },
            RequestMethod::FreeSpace { path: "p".into() },
            RequestMethod::SetLocationSyncMode {
                path: "p".into(),
                mode: "on-demand".into(),
            },
            RequestMethod::ShareFile { path: "p".into() },
            RequestMethod::GetFileDevices { path: "p".into() },
            RequestMethod::GetFileVersions { path: "p".into() },
            RequestMethod::GetServiceStatus,
            RequestMethod::Configure { nest_url: None },
            RequestMethod::Shutdown,
            RequestMethod::ProvisionCapability(cap),
            RequestMethod::RefreshBearer(BearerToken::new("t".into(), 4_000_000_000)),
            RequestMethod::SetLocationFolder {
                path: "p".into(),
                folder: "f".into(),
                folder_id: "local:1".into(),
            },
            RequestMethod::ListFileVersions { path: "p".into() },
            RequestMethod::RestoreFileVersion {
                path: "p".into(),
                version_num: 1,
            },
            RequestMethod::ListEngines,
            RequestMethod::GetBackupStatus,
            RequestMethod::Pause,
            RequestMethod::Resume,
            RequestMethod::AttachApp {
                app: None,
                notification_identity: None,
            },
        ];

        for method in methods {
            let name = method.name();
            let frame = encode_frame(&Request { id: 1, method }).expect("encode");
            assert!(
                frame.windows(name.len()).any(|w| w == name.as_bytes()),
                "RequestMethod::name() returned {name:?}, which is NOT the wire key an \
                 agent decodes — name() has drifted from the variant name"
            );
        }
    }

    /// The wire key for an enum variant is its **name** (serde's externally-tagged
    /// repr, never the integer discriminant). So *renaming* a variant is the
    /// breaking change, not reordering one. Pin the names an agent of another
    /// release decodes.
    #[test]
    fn version_variant_names_are_the_wire_keys() {
        let req = Request {
            id: 1,
            method: RequestMethod::ListFileVersions {
                path: "p".to_string(),
            },
        };
        let frame = encode_frame(&req).unwrap();
        assert!(
            frame
                .windows(b"ListFileVersions".len())
                .any(|w| w == b"ListFileVersions"),
            "variant name must appear verbatim as the wire key"
        );

        let restore = Request {
            id: 2,
            method: RequestMethod::RestoreFileVersion {
                path: "p".to_string(),
                version_num: 1,
            },
        };
        let frame = encode_frame(&restore).unwrap();
        assert!(
            frame
                .windows(b"RestoreFileVersion".len())
                .any(|w| w == b"RestoreFileVersion"),
            "variant name must appear verbatim as the wire key"
        );

        let resp = Response {
            id: 1,
            result: ResponseResult::Ok(ResponsePayload::FileVersionList(FileVersionListInfo {
                path: "p".to_string(),
                folder: "docs".to_string(),
                versions: Vec::new(),
            })),
        };
        let frame = encode_frame(&resp).unwrap();
        assert!(
            frame
                .windows(b"FileVersionList".len())
                .any(|w| w == b"FileVersionList"),
            "variant name must appear verbatim as the wire key"
        );
    }

    #[test]
    fn round_trip_sync_event() {
        let evt = Event {
            event: EventKind::FileStatusChanged {
                path: r"C:\Users\test\file.txt".into(),
                status: FileStatus::Synced,
            },
        };
        let frame = encode_frame(&evt).unwrap();
        let decoded: Event = decode_payload(&frame[4..]).unwrap();
        match decoded.event {
            EventKind::FileStatusChanged { path, status } => {
                assert_eq!(path, r"C:\Users\test\file.txt");
                assert_eq!(status, FileStatus::Synced);
            }
            _ => panic!("wrong event kind"),
        }
    }

    #[test]
    fn round_trip_file_status_request() {
        let req = Request {
            id: 10,
            method: RequestMethod::GetFileStatus {
                path: r"C:\test.txt".into(),
            },
        };
        let frame = encode_frame(&req).unwrap();
        let decoded: Request = decode_payload(&frame[4..]).unwrap();
        assert_eq!(decoded.id, 10);
        match decoded.method {
            RequestMethod::GetFileStatus { path } => assert_eq!(path, r"C:\test.txt"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn round_trip_service_status() {
        let resp = Response {
            id: 20,
            result: ResponseResult::Ok(ResponsePayload::ServiceStatus(ServiceStatusInfo {
                version: "0.1.0".into(),
                uptime_secs: 3600,
                connection: ConnectionState::Connected,
                sync: SyncStatusInfo {
                    connected: true,
                    syncing: false,
                    ..Default::default()
                },
                ..Default::default()
            })),
        };
        let frame = encode_frame(&resp).unwrap();
        let decoded: Response = decode_payload(&frame[4..]).unwrap();
        assert_eq!(decoded.id, 20);
    }

    /// The on-demand availability pair is additive in both directions: it
    /// round-trips when an agent reports it, and a reply without it
    /// (not probed yet) decodes to "cannot tell", never to "unavailable".
    #[test]
    fn the_on_demand_availability_is_additive_both_ways() {
        let status_of = |info: ServiceStatusInfo| -> ServiceStatusInfo {
            let frame = encode_frame(&Response {
                id: 23,
                result: ResponseResult::Ok(ResponsePayload::ServiceStatus(info)),
            })
            .unwrap();
            let decoded: Response = decode_payload(&frame[4..]).unwrap();
            match decoded.result {
                ResponseResult::Ok(ResponsePayload::ServiceStatus(s)) => s,
                other => panic!("wrong payload: {other:?}"),
            }
        };

        let reported = status_of(ServiceStatusInfo {
            on_demand_available: Some(false),
            on_demand_unavailable_reason: Some(ON_DEMAND_REASON_NO_FUSE_DEVICE.into()),
            ..Default::default()
        });
        assert_eq!(reported.on_demand_available, Some(false));
        assert_eq!(
            reported.on_demand_unavailable_reason.as_deref(),
            Some(ON_DEMAND_REASON_NO_FUSE_DEVICE)
        );

        let silent = status_of(ServiceStatusInfo::default());
        assert_eq!(silent.on_demand_available, None);
        assert_eq!(silent.on_demand_unavailable_reason, None);
    }

    /// The principal-support advertisement is additive in both directions
    /// (`version-compatibility.md` I4 applied to the local IPC seam), and the
    /// direction that matters is the ABSENT one: a reply carries
    /// no such field, and a newer app must read that as "does not advertise" —
    /// `store_principal_actor` only licenses the signed-out onboarding
    /// reconcile (`sync-agent.md` § Credential model, decision 4).
    #[test]
    fn the_principal_support_advertisement_is_additive_both_ways() {
        let actor_hex = "ab".repeat(32);
        let advertising = ServiceStatusInfo {
            version: "0.1.0".into(),
            store_principal_actor: Some(actor_hex.clone()),
            ..Default::default()
        };
        let frame = encode_frame(&Response {
            id: 21,
            result: ResponseResult::Ok(ResponsePayload::ServiceStatus(advertising)),
        })
        .unwrap();
        let decoded: Response = decode_payload(&frame[4..]).unwrap();
        match decoded.result {
            ResponseResult::Ok(ResponsePayload::ServiceStatus(s)) => {
                assert_eq!(s.store_principal_actor.as_deref(), Some(actor_hex.as_str()));
            }
            other => panic!("wrong payload: {other:?}"),
        }

        // An agent that does not advertise: the field is simply not on the wire. It must decode,
        // and it must decode to `None` rather than erroring — a strict decode
        // failure here would take out the whole status call an app's agent
        // health surface depends on.
        let old = ServiceStatusInfo {
            version: "0.0.1".into(),
            ..Default::default()
        };
        assert!(
            old.store_principal_actor.is_none(),
            "the default must be the non-advertising reading"
        );
        let frame = encode_frame(&Response {
            id: 22,
            result: ResponseResult::Ok(ResponsePayload::ServiceStatus(old)),
        })
        .unwrap();
        let decoded: Response = decode_payload(&frame[4..]).unwrap();
        match decoded.result {
            ResponseResult::Ok(ResponsePayload::ServiceStatus(s)) => {
                assert_eq!(s.store_principal_actor, None);
            }
            other => panic!("wrong payload: {other:?}"),
        }
    }

    #[test]
    fn round_trip_provision_capability() {
        let cap = SyncCapability::new(
            vec![7u8; 32],
            [0x11u8; 32].to_vec(),
            "https://example.com".into(),
            "dev-123".into(),
            BearerToken::new("tok".into(), 123),
        );
        let req = Request {
            id: 7,
            method: RequestMethod::ProvisionCapability(cap),
        };
        let frame = encode_frame(&req).unwrap();
        let decoded: Request = decode_payload(&frame[4..]).unwrap();
        assert_eq!(decoded.id, 7);
        match decoded.method {
            RequestMethod::ProvisionCapability(c) => {
                assert_eq!(c.backup_key_array(), Some([7u8; 32]));
                assert_eq!(c.actor_id_array(), Some([0x11u8; 32]));
                assert_eq!(c.bearer.token, "tok");
                assert_eq!(c.bearer.expires_at, 123);
            }
            _ => panic!("wrong variant"),
        }
    }

    /// The retired `content_key_bindings` key still decodes — ignored, never an
    /// error — so a capability carrying the key
    /// (`credentials.rs` decodes with the same
    /// strict decoder) provisions the agent unchanged
    /// (unknown keys are tolerated: additive both directions). A capability that
    /// failed to decode would strand the agent unprovisioned until the next
    /// sign-in.
    #[test]
    fn a_capability_carrying_the_retired_content_key_blob_still_decodes() {
        let cap = SyncCapability::new(
            vec![7u8; 32],
            [0x11u8; 32].to_vec(),
            "https://example.com".into(),
            "dev-123".into(),
            BearerToken::new("tok".into(), 123),
        );
        let bytes = fauna_cbor::encode_canonical(&cap).unwrap();
        let mut map = match fauna_cbor::decode_strict::<fauna_cbor::Value>(&bytes).unwrap() {
            fauna_cbor::Value::Map(map) => map,
            other => panic!("a capability encodes as a map, got {other:?}"),
        };
        map.insert(
            "content_key_bindings".into(),
            fauna_cbor::Value::Bytes(vec![0x81, 0xa0]),
        );
        let retired = fauna_cbor::encode_canonical(&fauna_cbor::Value::Map(map)).unwrap();
        let decoded: SyncCapability =
            fauna_cbor::decode_strict(&retired).expect("the retired key is ignored, not refused");
        assert_eq!(decoded.backup_key_array(), Some([7u8; 32]));
        assert_eq!(decoded.bearer.token, "tok");
    }

    #[test]
    fn provision_capability_actor_id_is_cbor_byte_string() {
        // The owner's public actor_id rides the same name-tagged CBOR map as the
        // backup_key, encoded as a 32-byte CBOR byte string (major type 2, 0x58 0x20
        // header). Use a distinct fill byte from backup_key so we don't false-match.
        let cap = SyncCapability::new(
            vec![7u8; 32],
            [0x9cu8; 32].to_vec(),
            "https://example.com".into(),
            "dev-123".into(),
            BearerToken::new("tok".into(), 123),
        );
        let req = Request {
            id: 7,
            method: RequestMethod::ProvisionCapability(cap),
        };
        let frame = encode_frame(&req).unwrap();
        let payload = &frame[4..];
        // Find a 0x58 0x20 header immediately followed by 32 bytes of 0x9c.
        let needle = [0x58u8, 0x20u8];
        let found = payload.windows(needle.len()).enumerate().any(|(i, w)| {
            w == needle && payload.len() >= i + 2 + 32 && payload[i + 2..i + 2 + 32] == [0x9cu8; 32]
        });
        assert!(
            found,
            "actor_id 32-byte CBOR byte string (0x58 0x20 + 32×0x9c) not found in frame"
        );
    }

    #[test]
    fn actor_id_array_rejects_non_32_lengths() {
        for len in [0usize, 1, 16, 31, 33, 64] {
            let cap = SyncCapability::new(
                vec![0u8; 32],
                vec![0u8; len],
                "https://example.com".into(),
                "dev-123".into(),
                BearerToken::new(String::new(), 4_000_000_000),
            );
            assert_eq!(cap.actor_id_array().is_some(), len == 32, "len {len}");
        }
    }

    #[test]
    fn provision_capability_key_is_cbor_byte_string() {
        // Distinct fill for actor_id so we match the backup_key's 32×0x07 run
        // specifically (CTAP2 canonical map ordering may emit either byte-string
        // field first, so search for the run, not just the first 0x58 0x20 header).
        let cap = SyncCapability::new(
            vec![7u8; 32],
            vec![0xa3u8; 32],
            "https://example.com".into(),
            "dev-123".into(),
            BearerToken::new("tok".into(), 123),
        );
        let req = Request {
            id: 7,
            method: RequestMethod::ProvisionCapability(cap),
        };
        let frame = encode_frame(&req).unwrap();
        let payload = &frame[4..];
        // CBOR byte string header for 32 bytes: major type 2 (0x40..0x57 for len<=23,
        // or 0x58 followed by one-byte length for len 24..255). 32 bytes → 0x58, 0x20.
        let needle = [0x58u8, 0x20u8];
        let found = payload.windows(needle.len()).enumerate().any(|(i, w)| {
            w == needle && payload.len() >= i + 2 + 32 && payload[i + 2..i + 2 + 32] == [7u8; 32]
        });
        assert!(
            found,
            "backup_key 32-byte CBOR byte string (0x58 0x20 + 32×0x07) not found in frame"
        );
    }

    #[test]
    fn round_trip_set_location_folder() {
        let req = Request {
            id: 9,
            method: RequestMethod::SetLocationFolder {
                path: r"C:\Users\test\Docs".into(),
                folder: "documents".into(),
                folder_id: "local:7".into(),
            },
        };
        let frame = encode_frame(&req).unwrap();
        let decoded: Request = decode_payload(&frame[4..]).unwrap();
        assert_eq!(decoded.id, 9);
        match decoded.method {
            RequestMethod::SetLocationFolder {
                path,
                folder,
                folder_id,
            } => {
                assert_eq!(path, r"C:\Users\test\Docs");
                assert_eq!(folder, "documents");
                assert_eq!(folder_id, "local:7");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn round_trip_refresh_bearer() {
        let req = Request {
            id: 8,
            method: RequestMethod::RefreshBearer(BearerToken::new("newtok".into(), 4_000_000_000)),
        };
        let frame = encode_frame(&req).unwrap();
        let decoded: Request = decode_payload(&frame[4..]).unwrap();
        assert_eq!(decoded.id, 8);
        match decoded.method {
            RequestMethod::RefreshBearer(b) => {
                assert_eq!(b.token, "newtok");
                assert_eq!(b.expires_at, 4_000_000_000);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn round_trip_unprovision_capability() {
        let req = Request {
            id: 11,
            method: RequestMethod::UnprovisionCapability,
        };
        let frame = encode_frame(&req).unwrap();
        let decoded: Request = decode_payload(&frame[4..]).unwrap();
        assert_eq!(decoded.id, 11);
        assert!(matches!(
            decoded.method,
            RequestMethod::UnprovisionCapability
        ));
    }

    /// The capability's wire fields, pinned: it carries **no renewal
    /// credential** — the store principal is the machine's only one, read by
    /// the agent load-only from the shared per-user slot
    /// (`sync-agent-credentials.md` § Credential model → the RULED 2026-09-28
    /// block, decision 1). A field added here is a deliberate act: a second
    /// key on the wire would be a second credential nothing revokes with the
    /// user's one delete gesture.
    #[test]
    fn a_provisioned_capability_carries_exactly_its_pinned_fields() {
        let cap = SyncCapability::new(
            vec![7u8; 32],
            vec![8u8; 32],
            "https://example.com".into(),
            "dev-123".into(),
            BearerToken::new("tok".into(), 123),
        );
        let payload = fauna_cbor::encode_canonical(&cap).unwrap();
        let fields: std::collections::BTreeMap<String, serde::de::IgnoredAny> =
            fauna_cbor::decode_strict(&payload).unwrap();
        assert_eq!(
            fields.keys().map(String::as_str).collect::<Vec<_>>(),
            CAPABILITY_WIRE_FIELDS,
            "the capability's wire fields changed"
        );
    }

    /// Every field a provisioned [`SyncCapability`] puts on the wire, in
    /// key order — no renewal credential among them.
    const CAPABILITY_WIRE_FIELDS: [&str; 8] = [
        "actor_id",
        "backup_key",
        "bearer",
        "device_id",
        "nest_url",
        "predecessor_actor_ids",
        "predecessor_backup_keys",
        "provisioned_at_ms",
    ];

    /// The retired owner keys survive an IPC round trip un-mangled, and the
    /// un-flattening is order-preserving — the walk is *nearest hop first*, and
    /// a reordered list would try the wrong root first on every open (correct,
    /// but it turns the common case into the slow one).
    #[test]
    fn capability_round_trips_the_predecessor_backup_keys_in_order() {
        let cap = SyncCapability::new(
            vec![0xABu8; 32],
            vec![0xCDu8; 32],
            "https://example.com".into(),
            "dev-123".into(),
            BearerToken::new("tok".into(), 1),
        )
        .with_predecessor_backup_keys(&[[0x11u8; 32], [0x22u8; 32]]);

        let req = Request {
            id: 7,
            method: RequestMethod::ProvisionCapability(cap),
        };
        let frame = encode_frame(&req).unwrap();
        let decoded: Request = decode_payload(&frame[4..]).unwrap();
        let RequestMethod::ProvisionCapability(back) = decoded.method else {
            panic!("wrong variant")
        };
        assert_eq!(
            back.predecessor_backup_keys(),
            vec![[0x11u8; 32], [0x22u8; 32]],
            "a successor's read fallback must survive the pipe intact and in order"
        );
    }

    /// The paired keys survive the pipe as pairs, in chain order — the
    /// per-signer bound (ruling (8)(c)) offers a predecessor's row only its
    /// own root and the later-in-chain ones, so a reorder or a re-pairing
    /// would hand one identity another's root. A capability without them
    /// decodes empty (additive), and a malformed blob yields none at all.
    #[test]
    fn capability_round_trips_the_paired_predecessor_keys_in_order() {
        let pairs = [([0xA1u8; 32], [0x11u8; 32]), ([0xA2u8; 32], [0x22u8; 32])];
        let cap = SyncCapability::new(
            vec![0xABu8; 32],
            vec![0xCDu8; 32],
            "https://example.com".into(),
            "dev-123".into(),
            BearerToken::new("tok".into(), 1),
        )
        .with_predecessor_keys_by_actor(&pairs);
        let req = Request {
            id: 7,
            method: RequestMethod::ProvisionCapability(cap),
        };
        let frame = encode_frame(&req).unwrap();
        let decoded: Request = decode_payload(&frame[4..]).unwrap();
        let RequestMethod::ProvisionCapability(mut back) = decoded.method else {
            panic!("wrong variant")
        };
        assert_eq!(back.predecessor_keys_by_actor(), pairs.to_vec());
        assert!(
            !format!("{back:?}").contains(&hex::encode([0x11u8; 32])),
            "the keys are redacted in Debug"
        );
        back.predecessor_keys_by_actor = vec![0u8; 96];
        assert!(back.predecessor_keys_by_actor().is_empty());
    }

    /// **Additive both directions** (`version-compatibility.md`): a capability
    /// without the field decodes with an empty list rather
    /// than failing, and the agent then behaves as it does with no retired keys — which
    /// is fail-CLOSED (a predecessor-sealed corpus stays unopenable), never a
    /// wrong-root open.
    #[test]
    fn a_capability_without_predecessor_keys_decodes_empty() {
        let old = SyncCapability::new(
            vec![0xABu8; 32],
            vec![0xCDu8; 32],
            "https://example.com".into(),
            "dev-123".into(),
            BearerToken::new("tok".into(), 1),
        );
        let req = Request {
            id: 7,
            method: RequestMethod::ProvisionCapability(old),
        };
        let frame = encode_frame(&req).unwrap();
        let decoded: Request = decode_payload(&frame[4..]).unwrap();
        let RequestMethod::ProvisionCapability(back) = decoded.method else {
            panic!("wrong variant")
        };
        assert!(back.predecessor_backup_keys().is_empty());
    }

    /// A malformed blob yields **empty**, not a truncated list. A partial list
    /// presents downstream as "this key does not open the corpus" — which is
    /// indistinguishable from corruption, the exact confusion this whole leg
    /// exists to remove.
    #[test]
    fn a_malformed_predecessor_blob_yields_no_keys_rather_than_a_partial_list() {
        for len in [1usize, 31, 33, 63, 65] {
            let mut cap = SyncCapability::new(
                vec![0xABu8; 32],
                vec![0xCDu8; 32],
                "https://example.com".into(),
                "dev-123".into(),
                BearerToken::new("tok".into(), 1),
            );
            cap.predecessor_backup_keys = vec![0x5Au8; len];
            assert!(
                cap.predecessor_backup_keys().is_empty(),
                "len {len} must yield no keys at all"
            );
        }
        // Positive control: the exact multiples DO decode, so the guard above is
        // about the length check and not about the accessor being broken.
        let mut cap = SyncCapability::new(
            vec![0xABu8; 32],
            vec![0xCDu8; 32],
            "https://example.com".into(),
            "dev-123".into(),
            BearerToken::new("tok".into(), 1),
        );
        cap.predecessor_backup_keys = vec![0x5Au8; 64];
        assert_eq!(cap.predecessor_backup_keys().len(), 2);
    }

    /// Item 3: the redaction discipline must travel with the value —
    /// a rule stated on `backup_key` alone silently fails to cover its sibling.
    #[test]
    fn capability_debug_redacts_predecessor_backup_keys() {
        let cap = SyncCapability::new(
            vec![0xABu8; 32],
            vec![0xCDu8; 32],
            "https://example.com".into(),
            "dev-123".into(),
            BearerToken::new("tok".into(), 1),
        )
        .with_predecessor_backup_keys(&[[0x7Eu8; 32]]);
        let s = format!("{cap:?}");
        assert!(
            s.contains("predecessor_backup_keys"),
            "the field must be visible as redacted, not omitted: {s}"
        );
        assert!(
            !s.contains("126") && !s.contains("0x7E") && !s.contains("7e, 7e"),
            "retired key bytes leaked in Debug: {s}"
        );
    }

    #[test]
    fn capability_debug_redacts_secrets() {
        let cap = SyncCapability::new(
            vec![0xABu8; 32],
            vec![0xCDu8; 32],
            "https://example.com".into(),
            "dev-123".into(),
            BearerToken::new("supersecret".into(), 1),
        );
        let s = format!("{cap:?}");
        assert!(
            !s.contains("supersecret"),
            "bearer token leaked in Debug: {s}"
        );
        assert!(
            s.contains("redacted"),
            "expected 'redacted' in Debug output: {s}"
        );
        assert!(
            s.contains("32 bytes redacted"),
            "expected '32 bytes redacted': {s}"
        );
        assert!(
            s.contains("11 chars redacted"),
            "expected '11 chars redacted': {s}"
        );
    }

    #[test]
    fn enum_debug_does_not_leak() {
        // The redacting Debug must hold transitively through the derived Debug on RequestMethod.
        let method = RequestMethod::ProvisionCapability(SyncCapability::new(
            vec![0xABu8; 32],
            vec![0xCDu8; 32],
            "https://example.com".into(),
            "dev-123".into(),
            BearerToken::new("supersecret".into(), 1),
        ));
        let s = format!("{method:?}");
        assert!(
            !s.contains("supersecret"),
            "bearer leaked through enum Debug: {s}"
        );
        assert!(
            s.contains("redacted"),
            "expected redaction marker, got: {s}"
        );
    }

    #[test]
    fn pipe_name_for_sid_formats() {
        assert_eq!(
            pipe_name_for_sid("S-1-5-21-1-2-3-1001"),
            r"\\.\pipe\fauna-sync.S-1-5-21-1-2-3-1001"
        );
    }

    #[test]
    fn mutex_name_for_pipe_formats() {
        assert_eq!(
            mutex_name_for_pipe(r"\\.\pipe\fauna-sync.S-1-5-21-1-2-3-1001"),
            r"Local\FaunaSyncAgent.S-1-5-21-1-2-3-1001"
        );
    }

    /// RED (pre-fix): a pipe name with no dot in its leaf — every non-production
    /// e2e/test `--pipe-name` (`fauna-sync-test-a`, `fauna-sync-e2e-<pid>`, …) is
    /// shaped this way — used to make `rsplit('.')` walk past the `\\.\` prefix's
    /// OWN dot and return a suffix containing embedded backslashes
    /// (`\pipe\fauna-sync-e2e-8`). `CreateMutexW` rejects that as a nested
    /// object-manager path with `ERROR_PATH_NOT_FOUND` (0x80070003) — the exact
    /// failure `test_per_user_sync_agent.py` and the `isolated_sync_agent` e2e
    /// harness hit: the agent logs "sync agent starting" then dies instantly.
    #[test]
    fn mutex_name_for_pipe_handles_dotless_leaf() {
        assert_eq!(
            mutex_name_for_pipe(r"\\.\pipe\fauna-sync-e2e-8"),
            r"Local\FaunaSyncAgent.fauna-sync-e2e-8"
        );
        assert_eq!(
            mutex_name_for_pipe(r"\\.\pipe\fauna-sync-test-a"),
            r"Local\FaunaSyncAgent.fauna-sync-test-a"
        );
    }

    #[cfg(windows)]
    #[test]
    fn current_user_pipe_name_ok() {
        let name = super::current_user_pipe_name().expect("current_user_pipe_name should succeed");
        assert!(
            name.starts_with(r"\\.\pipe\fauna-sync.S-"),
            "expected pipe name starting with r\"\\\\.\\ pipe\\fauna-sync.S-\", got: {name}"
        );
    }

    #[test]
    fn backup_key_array_rejects_non_32_lengths() {
        for len in [0usize, 1, 16, 31, 33, 64] {
            let cap = SyncCapability::new(
                vec![0u8; len],
                vec![0u8; 32],
                "https://example.com".into(),
                "dev-123".into(),
                BearerToken::new(String::new(), 4_000_000_000),
            );
            assert_eq!(cap.backup_key_array().is_some(), len == 32, "len {len}");
        }
    }

    #[test]
    fn round_trip_list_engines_request_and_response() {
        let req = Request {
            id: 70,
            method: RequestMethod::ListEngines,
        };
        let frame = encode_frame(&req).unwrap();
        let decoded: Request = decode_payload(&frame[4..]).unwrap();
        assert_eq!(decoded.id, 70);
        assert!(matches!(decoded.method, RequestMethod::ListEngines));
        assert_eq!(decoded.method.name(), "ListEngines");

        let resp = Response {
            id: 71,
            result: ResponseResult::Ok(ResponsePayload::Engines(vec![EngineInfo {
                folder: "docs".into(),
                mode: "always-resident".into(),
                serving: true,
                files_pending: 3,
                bytes_pending: 4096,
                ..Default::default()
            }])),
        };
        let frame = encode_frame(&resp).unwrap();
        let decoded: Response = decode_payload(&frame[4..]).unwrap();
        match decoded.result {
            ResponseResult::Ok(ResponsePayload::Engines(e)) => {
                assert_eq!(e.len(), 1);
                assert_eq!(e[0].folder, "docs");
                assert_eq!(e[0].mode, "always-resident");
                assert!(e[0].serving);
                assert_eq!(e[0].files_pending, 3);
                assert_eq!(e[0].bytes_pending, 4096);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn round_trip_get_backup_status_request_and_response() {
        let req = Request {
            id: 72,
            method: RequestMethod::GetBackupStatus,
        };
        let frame = encode_frame(&req).unwrap();
        let decoded: Request = decode_payload(&frame[4..]).unwrap();
        assert!(matches!(decoded.method, RequestMethod::GetBackupStatus));
        assert_eq!(decoded.method.name(), "GetBackupStatus");

        let resp = Response {
            id: 73,
            result: ResponseResult::Ok(ResponsePayload::BackupStatus(vec![
                BackupDestinationStatus {
                    destination_id: "dest-1".into(),
                    last_upload_time: Some(1_711_400_000),
                    backlog_count: 5,
                },
                BackupDestinationStatus {
                    destination_id: "dest-2".into(),
                    last_upload_time: None,
                    backlog_count: 0,
                },
            ])),
        };
        let frame = encode_frame(&resp).unwrap();
        let decoded: Response = decode_payload(&frame[4..]).unwrap();
        match decoded.result {
            ResponseResult::Ok(ResponsePayload::BackupStatus(rows)) => {
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0].destination_id, "dest-1");
                assert_eq!(rows[0].last_upload_time, Some(1_711_400_000));
                assert_eq!(rows[0].backlog_count, 5);
                assert!(rows[1].last_upload_time.is_none());
                assert_eq!(rows[1].backlog_count, 0);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn round_trip_pause_resume_requests() {
        for (method, name) in [
            (RequestMethod::Pause, "Pause"),
            (RequestMethod::Resume, "Resume"),
        ] {
            assert_eq!(method.name(), name);
            let frame = encode_frame(&Request { id: 1, method }).unwrap();
            let decoded: Request = decode_payload(&frame[4..]).unwrap();
            assert_eq!(decoded.method.name(), name);
        }
    }

    /// The full producer message and the prefix the matcher (the shared Rust
    /// convergence loop) keys on must not drift apart — a reword of either would
    /// silently break the re-provision trigger.
    #[test]
    fn no_capability_message_begins_with_prefix() {
        assert!(NO_CAPABILITY_ERROR_MESSAGE.starts_with(NO_CAPABILITY_ERROR_PREFIX));
        assert_eq!(NO_CAPABILITY_ERROR_PREFIX, "no capability provisioned");
    }

    #[test]
    fn round_trip_provision_capability_nest_url_and_device_id() {
        // RED: this test is written first; it will fail to compile until
        // SyncCapability::new gains nest_url + device_id parameters.
        let cap = SyncCapability::new(
            vec![7u8; 32],
            [0x11u8; 32].to_vec(),
            "https://example.com".into(),
            "dev-123".into(),
            BearerToken::new("tok".into(), 123),
        );
        let req = Request {
            id: 7,
            method: RequestMethod::ProvisionCapability(cap),
        };
        let frame = encode_frame(&req).unwrap();
        let decoded: Request = decode_payload(&frame[4..]).unwrap();
        assert_eq!(decoded.id, 7);
        match decoded.method {
            RequestMethod::ProvisionCapability(c) => {
                assert_eq!(c.nest_url, "https://example.com");
                assert_eq!(c.device_id, "dev-123");
                assert_eq!(c.backup_key_array(), Some([7u8; 32]));
                assert_eq!(c.actor_id_array(), Some([0x11u8; 32]));
                assert_eq!(c.bearer.token, "tok");
            }
            _ => panic!("wrong variant"),
        }
    }

    /// Wire-format print of `ProvisionCapability` / `RefreshBearer` (mirrors the
    /// `print_wire_format*` emitters in `lib.rs`). The C# conformance pins that
    /// consumed it died with the C# pipe codec (deleted 2026-08-05; only the
    /// `BackupKey::derive` vector survives, as `BackupKeyDeriveFfiTests`), so it
    /// is a readable record of the shape, not a cross-language contract:
    ///   cargo test -p fauna-ipc print_wire_format_for_provision_capability -- --nocapture
    ///
    /// The `backup_key` fixture is the Task-3.1 pinned vector
    /// `BackupKey::derive(&[0x01; 32])` = f39582d2…383a14. `backup_key`/`actor_id`
    /// are `serde_bytes` byte strings (major type 2), NOT arrays of integers.
    #[test]
    fn print_wire_format_for_provision_capability() {
        // backup_key = BackupKey::derive(&[0x01; 32]) (the Task-3.1 pinned vector).
        #[rustfmt::skip]
        let backup_key: Vec<u8> = vec![
            0xf3, 0x95, 0x82, 0xd2, 0x47, 0xfa, 0x3b, 0xb8,
            0x4a, 0x45, 0x22, 0x49, 0x43, 0xd9, 0xf0, 0x58,
            0xb8, 0x65, 0x0b, 0xed, 0x6d, 0x66, 0x40, 0xe3,
            0xa6, 0x91, 0x65, 0xa1, 0x47, 0x38, 0x3a, 0x14,
        ];
        let cap = SyncCapability::new(
            backup_key,
            vec![0x11u8; 32], // actor_id
            "https://example.com".into(),
            "dev-123".into(),
            BearerToken::new("tok".into(), 123),
        );
        let req = Request {
            id: 7,
            method: RequestMethod::ProvisionCapability(cap),
        };
        let payload = fauna_cbor::encode_canonical(&req).unwrap();
        eprintln!("ProvisionCapability wire: {}", hex::encode(&payload));

        // RefreshBearer(BearerToken::new("newtok", 4_000_000_000)) at id=8.
        let req2 = Request {
            id: 8,
            method: RequestMethod::RefreshBearer(BearerToken::new("newtok".into(), 4_000_000_000)),
        };
        let payload2 = fauna_cbor::encode_canonical(&req2).unwrap();
        eprintln!("RefreshBearer wire: {}", hex::encode(&payload2));
    }

    /// Cross-language wire-format emitter for the **device-global control-plane ops**
    /// (`sync-agent.md` § Control plane split #4): the three unit-variant requests
    /// `ListEngines` / `Pause` / `Resume` and the `Engines` response they and the
    /// desktop agent-status views ride on. The C# pins that consumed these hexes
    /// died with the C# pipe codec (deleted 2026-08-05); print them with:
    ///   cargo test -p fauna-ipc print_wire_format_for_control_plane_ops -- --nocapture
    ///
    /// Unit variants cross the wire as a bare text string (the variant NAME), never a
    /// single-key map — the same shape `Shutdown`/`UnprovisionCapability` already use.
    /// The `Engines` response is a newtype variant wrapping a LIST of `EngineInfo`
    /// maps, so the pin also guards the row's own field spelling + canonical key order.
    #[test]
    fn print_wire_format_for_control_plane_ops() {
        for (id, method) in [
            (11u64, RequestMethod::ListEngines),
            (12, RequestMethod::Pause),
            (13, RequestMethod::Resume),
            // The remote-change nudge (`file-sync.md` § Remote-change nudge). A
            // struct variant with one string field — same shape as
            // `ListSnapshots` — printed here because a silent encode mismatch
            // would look exactly like "the nudge is best-effort and got
            // dropped".
            (
                14,
                RequestMethod::PullFolderNow {
                    folder: "docs".to_string(),
                    folder_hash: None,
                },
            ),
        ] {
            let name = method.name();
            let payload = fauna_cbor::encode_canonical(&Request { id, method }).unwrap();
            eprintln!("{name}(id={id}) wire: {}", hex::encode(&payload));
        }

        // Two rows so the pin proves list framing, and so both `serving` states and a
        // non-zero backlog cross the wire (a single all-default row would let a
        // field-order or bool-encoding regression hide).
        let resp = Response {
            id: 11,
            result: ResponseResult::Ok(ResponsePayload::Engines(vec![
                EngineInfo {
                    folder: "photos".into(),
                    folder_id: "local:3".into(),
                    mode: "on-demand".into(),
                    serving: true,
                    files_pending: 3,
                    bytes_pending: 4096,
                    // The additive post-succession field, carried on one row and
                    // absent on the other: this pin is the wire's shape, and an
                    // `Option` that only ever crossed as `None` would pin half of it.
                    corpus_reseal: Some(CorpusResealInfo::Settled {
                        resealed: 7,
                        owed: 2,
                    }),
                    // Same reasoning for the drain observable: carried here,
                    // absent on the row below. Note it disagrees with the report
                    // above on purpose — a settled pass that still owes 2 is
                    // exactly the case where the license must NOT be granted, and
                    // the two fields are independent on the wire.
                    reseal_drain: Some(ResealDrainInfo {
                        folded: true,
                        nothing_owed: false,
                        all_at_rest_classified: true,
                    }),
                    // Same reasoning again for the mass-delete hold: non-zero
                    // here, defaulted on the row below. A `u64` that only ever
                    // crossed as `0` would pin half its shape exactly as a
                    // perpetually-`None` `Option` would.
                    deletes_held: 5,
                    // Same again for the unreadable-path count.
                    deletes_skipped_unreadable: 2,
                },
                EngineInfo {
                    folder: "docs".into(),
                    mode: "always".into(),
                    serving: false,
                    files_pending: 0,
                    bytes_pending: 0,
                    ..Default::default()
                },
            ])),
        };
        let payload = fauna_cbor::encode_canonical(&resp).unwrap();
        eprintln!("Engines response wire: {}", hex::encode(&payload));
    }
}

/// The unknown arms of the reply enums an older app already asks for
/// (`transport.md` § Rule 3 in full). The newer agent is modelled as a
/// test-only twin carrying one variant this build does not name, spliced into
/// a real reply at the enum's position: (1) the whole reply still decodes,
/// (2) the arm takes the restrictive reading, and (3) a carrying arm
/// re-encodes byte-identically while a collapsing one refuses to encode.
/// The readings an app derives from the arms are pinned beside their
/// projections in `fauna-client-sync` (`reseed_wire`, `agent`).
#[cfg(test)]
mod unknown_arm_tests {
    use super::*;
    use crate::decode_payload;
    use fauna_cbor::Value;

    /// The newer agent's data variant — every carrying enum here has data
    /// variants, so its unknown arm keeps the whole value.
    #[derive(Serialize)]
    enum NewerData {
        AddedInANewerAgent { n: u32, note: String },
    }

    /// The newer agent's unit variant, for the all-unit enums.
    #[derive(Serialize)]
    enum NewerUnit {
        AddedInANewerAgent,
    }

    fn to_value<T: Serialize>(t: &T) -> Value {
        decode_payload(&fauna_cbor::encode_canonical(t).unwrap()).unwrap()
    }

    /// `reply` encoded with the value at `path` replaced by `newer`'s
    /// encoding — the bytes a newer agent sends.
    fn newer_reply_bytes<T: Serialize>(reply: &Response, path: &[&str], newer: &T) -> Vec<u8> {
        let mut root = to_value(reply);
        let mut at = &mut root;
        for step in path {
            at = match at {
                Value::Map(m) => m.get_mut(*step).expect("path names a field"),
                Value::List(l) => &mut l[step.parse::<usize>().expect("an index")],
                other => panic!("path runs through a leaf: {other:?}"),
            };
        }
        *at = to_value(newer);
        fauna_cbor::encode_canonical(&root).unwrap()
    }

    fn ok(payload: ResponsePayload) -> Response {
        Response {
            id: 9,
            result: ResponseResult::Ok(payload),
        }
    }

    fn payload(bytes: &[u8]) -> ResponsePayload {
        match decode_payload::<Response>(bytes)
            .expect("the whole reply decodes")
            .result
        {
            ResponseResult::Ok(p) => p,
            other => panic!("not an Ok reply: {other:?}"),
        }
    }

    fn newer_data() -> NewerData {
        NewerData::AddedInANewerAgent {
            n: 3,
            note: "x".into(),
        }
    }

    /// A carrying arm hands back the newer writer's exact bytes.
    fn assert_reencodes_identically(bytes: &[u8]) {
        let reply: Response = decode_payload(bytes).unwrap();
        assert_eq!(
            fauna_cbor::encode_canonical(&reply).unwrap(),
            bytes,
            "a carried value re-encodes byte-identically"
        );
    }

    #[test]
    fn an_unknown_reseed_state_decodes_and_is_never_running() {
        let bytes = newer_reply_bytes(
            &ok(ResponsePayload::CustodianReseed(CustodianReseedState::Idle)),
            &["result", "Ok", "CustodianReseed"],
            &newer_data(),
        );
        match payload(&bytes) {
            ResponsePayload::CustodianReseed(CustodianReseedState::Unknown(_)) => {}
            other => panic!("expected the unknown arm, got {other:?}"),
        }
        assert_reencodes_identically(&bytes);
    }

    #[test]
    fn an_unknown_set_outcome_decodes_inside_a_finished_report() {
        let report = CustodianReseedReport {
            sets: vec![CustodianReseedSet {
                set_name: "s".into(),
                folder: true,
                folder_display_name: None,
                outcome: CustodianReseedSetOutcome::AlreadyLive,
            }],
            ..Default::default()
        };
        let bytes = newer_reply_bytes(
            &ok(ResponsePayload::CustodianReseed(
                CustodianReseedState::Finished(report),
            )),
            &[
                "result",
                "Ok",
                "CustodianReseed",
                "Finished",
                "sets",
                "0",
                "outcome",
            ],
            &newer_data(),
        );
        match payload(&bytes) {
            ResponsePayload::CustodianReseed(CustodianReseedState::Finished(r)) => assert!(
                matches!(r.sets[0].outcome, CustodianReseedSetOutcome::Unknown(_)),
                "got {:?}",
                r.sets[0].outcome
            ),
            other => panic!("expected a finished report, got {other:?}"),
        }
        assert_reencodes_identically(&bytes);
    }

    #[test]
    fn an_unknown_location_status_decodes_and_is_never_synced() {
        let bytes = newer_reply_bytes(
            &ok(ResponsePayload::Locations(vec![LocationInfo::default()])),
            &["result", "Ok", "Locations", "0", "status"],
            &newer_data(),
        );
        match payload(&bytes) {
            ResponsePayload::Locations(rows) => assert!(
                matches!(rows[0].status, LocationStatus::Unknown(_)),
                "never the Synced default: {:?}",
                rows[0].status
            ),
            other => panic!("expected locations, got {other:?}"),
        }
        assert_reencodes_identically(&bytes);
    }

    #[test]
    fn an_unknown_file_status_collapses_and_is_never_written_back() {
        let bytes = newer_reply_bytes(
            &ok(ResponsePayload::FileStatus(FileStatusInfo {
                path: "a".into(),
                status: FileStatus::Synced,
                size_bytes: 1,
                is_pinned: false,
            })),
            &["result", "Ok", "FileStatus", "status"],
            &NewerUnit::AddedInANewerAgent,
        );
        let ResponsePayload::FileStatus(info) = payload(&bytes) else {
            panic!("expected a file status");
        };
        assert_eq!(info.status, FileStatus::Unknown);
        assert!(
            fauna_cbor::encode_canonical(&info).is_err(),
            "a collapsed status refuses to encode"
        );
    }

    #[test]
    fn an_unknown_connection_state_collapses_and_is_never_written_back() {
        let bytes = newer_reply_bytes(
            &ok(ResponsePayload::ServiceStatus(ServiceStatusInfo {
                connection: ConnectionState::Connected,
                ..Default::default()
            })),
            &["result", "Ok", "ServiceStatus", "connection"],
            &NewerUnit::AddedInANewerAgent,
        );
        let ResponsePayload::ServiceStatus(info) = payload(&bytes) else {
            panic!("expected a service status");
        };
        assert_eq!(info.connection, ConnectionState::Unknown);
        assert!(
            fauna_cbor::encode_canonical(&info).is_err(),
            "a collapsed state refuses to encode"
        );
    }

    #[test]
    fn an_unknown_corpus_reseal_never_fails_the_engines_reply() {
        let bytes = newer_reply_bytes(
            &ok(ResponsePayload::Engines(vec![EngineInfo {
                folder: "f".into(),
                corpus_reseal: Some(CorpusResealInfo::Running),
                reseal_drain: Some(ResealDrainInfo {
                    folded: true,
                    nothing_owed: true,
                    all_at_rest_classified: true,
                }),
                ..Default::default()
            }])),
            &["result", "Ok", "Engines", "0", "corpus_reseal"],
            &newer_data(),
        );
        match payload(&bytes) {
            ResponsePayload::Engines(rows) => {
                assert!(matches!(
                    rows[0].corpus_reseal,
                    Some(CorpusResealInfo::Unknown(_))
                ));
                assert!(rows[0].reseal_drain.is_some(), "the rest of the row reads");
            }
            other => panic!("expected engines, got {other:?}"),
        }
        assert_reencodes_identically(&bytes);
    }
}
