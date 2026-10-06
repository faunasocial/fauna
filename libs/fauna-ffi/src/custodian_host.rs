//! Hosting a client-device custodian from a **mobile** shell — slice 3d's
//! mobile arm (`docs/goal/behavior/backup-destinations.md` § Third destination kind →
//! *Durability + labeling*; `docs/goal/behavior/backup-restore.md`
//! § Background Tasks).
//!
//! The desktop arm hosts in the per-user **sync agent**, which is a native Rust
//! process and needs no boundary. Mobile has no agent: iOS and android host in
//! the app itself, from its OS-scheduled background task, so the host has to
//! cross UniFFI. That is all this module is — the boundary, plus the two drives
//! a scheduler-owned platform needs.
//!
//! Every moving part stays shared: assembly is
//! [`fauna_sync_engine::custodian_host::CustodianHost`], the pull is
//! `custodian_pull`, the store is `custodian_store`, and *which row is mine* is
//! `fauna_core::data::custodian_assignment_for` reached through
//! [`BackupClient::custodian_assignment`]. Nothing here decides anything a
//! desktop host decides differently.
//!
//! # Two drives, never both
//!
//! `backup-restore.md` § Background Tasks settles the split for platforms whose
//! cadence an OS scheduler owns: the scheduled task calls
//! [`FfiCustodianHost::run_all_kinds`] once per wake, and the foreground holder
//! runs [`FfiCustodianHost::start_push_debounce`] — the **push-only** loop —
//! for as long as the app is foregrounded. Running `run_forever` here instead
//! would double the period `WorkManager`/`BGProcessingTask` already drives,
//! which is why no `start_forever` is exported on this type at all.
//!
//! # The FFI exclusion enum lives in `crate::cloud_backup`
//!
//! [`FfiCloudBackupExclusion`] (two arms — no `NotApplicable`, deliberately;
//! that module's docs carry why) and its excluder trait are shared with the
//! account runtime since 2026-08-26, the second store to state a posture.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use fauna_client_backup::BackupClient;
use fauna_client_backup::custodian::CapState;
use fauna_sync_engine::custodian_host::{CustodianHost, CustodianHostParams};
use fauna_sync_engine::custodian_pull::PullReport;
use fauna_sync_engine::custodian_store::{
    CloudBackupExclusion, CustodianStore, custodian_store_root,
};
use tokio_util::sync::CancellationToken;

use crate::cloud_backup::FfiCloudBackupExclusion;
use crate::crypto::{device32, secret32};
use crate::{FfiError, FfiNestClient};

/// A live custodian host. `Ok(None)` from [`build_custodian_host`] means this
/// device is not an enrolled custodian, so there is no host to hold.
#[derive(uniffi::Object)]
pub struct FfiCustodianHost {
    host: CustodianHost,
}

/// What one [`FfiCustodianHost::run_all_kinds`] pass did, summed over every
/// backed-up kind — enough for a scheduled task to log a line and decide
/// success-vs-retry, and deliberately not the whole per-kind `PullReport`
/// (a shell has nothing to decide per kind).
#[derive(uniffi::Record)]
pub struct FfiCustodianPullSummary {
    /// Segments fetched, sealed and stored this pass.
    pub stored_segments: u32,
    /// Segments the source stopped listing, tombstoned locally this pass.
    pub tombstoned_segments: u32,
    /// Bytes held after the pass — what the check-in reported.
    pub held_bytes: u64,
    /// `true` iff **any** kind ended at its capacity cap.
    ///
    /// Read from the pass's own `cap_state`, never inferred from
    /// `held_bytes >= cap`: a pass that stops at its cap ends *below* it, so
    /// the inference renders a stalled backup as healthy-with-room
    /// (`behavior/backup-destinations.md` § Third destination kind → *Status projection*).
    pub cap_reached: bool,
    /// Whether the pass's check-in reached the source nest — the same field,
    /// and the same rule, as the sync agent's pass report
    /// (`fauna_ipc::sync::CustodianPassReport::checked_in`), so an in-app
    /// host's report answers what an agent-hosted one does.
    pub checked_in: bool,
}

/// Cancel-handle for a running [`FfiCustodianHost::start_push_debounce`] loop.
///
/// The foreground holder keeps it while foregrounded and cancels (or drops —
/// `Drop` also cancels) when backgrounding. Cancelling is **required**, not
/// hygiene: the push-only loop has no periodic tick, so nothing else would ever
/// wake it to notice the source went away.
#[derive(uniffi::Object)]
pub struct FfiCustodianPushHandle {
    cancel: CancellationToken,
}

#[uniffi::export]
impl FfiCustodianPushHandle {
    /// Stop the push-debounce loop. Idempotent and cheap.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

impl Drop for FfiCustodianPushHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[fauna_uniffi_async::export]
impl FfiCustodianHost {
    /// One pull pass over every backed-up kind — the scheduled-task entry point
    /// (android `WorkManager`, iOS `BGProcessingTask`).
    ///
    /// Never fails as a whole: a per-kind failure is logged and the pass
    /// continues, because one kind's bad pass is not a reason to skip the
    /// others, and the next wake retries anyway. The scheduler learns what
    /// happened from the summary.
    pub async fn run_all_kinds(&self) -> FfiCustodianPullSummary {
        // Held for the whole pass so a concurrent `reclaim_custodian_store`
        // waits for it rather than deleting blobs this pass has written but not
        // yet recorded.
        let _active = ActivityGuard::pass();
        summarize(&self.host.run_all_kinds(now_secs()).await)
    }

    /// Start the **push-only** loop for a foregrounded app. Returns immediately
    /// with the cancel handle; see [`FfiCustodianPushHandle`] for why cancelling
    /// is required rather than optional.
    ///
    /// Call at most once per live host — the push subscriptions are not
    /// re-entrant.
    pub async fn start_push_debounce(self: Arc<Self>) -> Arc<FfiCustodianPushHandle> {
        let cancel = CancellationToken::new();
        let host = Arc::clone(&self);
        let loop_cancel = cancel.clone();
        tokio::spawn(async move {
            // Publishes the loop's cancel token beside the counter, so a
            // reclaim can stop it — the loop has no periodic tick, so a reclaim
            // that merely waited would wait forever. Dropped (and the slot
            // cleared) however the loop ends: cancelled, errored, or returned.
            let _active = ActivityGuard::push_loop(loop_cancel.clone());
            if let Err(e) = host.host.run_push_debounce(loop_cancel).await {
                tracing::warn!(error = %e, "custodian: foreground push loop ended with an error");
            }
        });
        Arc::new(FfiCustodianPushHandle { cancel })
    }

    /// The `destination_id` this host drives — the registry row a status row and
    /// a check-in are keyed on. For logging and for a shell that wants to show
    /// *which* row this device is serving.
    pub fn destination_id(&self) -> String {
        self.host.destination_id().to_string()
    }
}

/// Build this device's custodian host, or `Ok(None)` when this device is **not**
/// an enrolled custodian.
///
/// `None` is the ordinary answer on most devices and is not an error: the
/// scheduled task treats it as a clean no-op pass, exactly as the upload
/// coordinator's builder treats zero configured destinations. Discovery is the
/// nest's destination registry — never the at-rest config and never local IPC —
/// for the reason `docs/goal/architecture/apps/sync-agent.md` § Control plane
/// split gives: a cap the owner raises on another device must reach this one.
///
/// `device_id` is the device's **stable sync device id** — the same id
/// enrollment recorded in the registry row, and what the row is matched on.
/// `data_dir` is this shell's unscoped per-user base directory; the
/// actor-scoped store location under it is derived by shared Rust
/// ([`custodian_store_root`]), so the layout and the account scoping cannot
/// drift per platform.
///
/// # Errors
///
/// - `FfiError::General` if `owner_secret` / `device_id` are not 32 bytes.
/// - `FfiError::General` carrying the registry-read error chain (the device may
///   be offline — the next scheduled wake retries).
/// - `FfiError::General` if the store root cannot be created **or its
///   cloud-backup exclusion fails** — the build aborts rather than handing back
///   a host that would write into an unexcluded store.
#[fauna_uniffi_async::export]
pub async fn build_custodian_host(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    device_id: Vec<u8>,
    data_dir: String,
    exclusion: FfiCloudBackupExclusion,
) -> Result<Option<Arc<FfiCustodianHost>>, FfiError> {
    let secret = secret32(&owner_secret)?;
    let device_id_bytes = device32(&device_id)?;
    let device_id_hex = hex::encode(device_id_bytes);

    let ws = nest.nest_arc();
    let ws_url = ws.nest_url().to_string();
    let Some(assignment) = BackupClient::new(Arc::clone(&ws))
        .custodian_assignment(&device_id_hex)
        .await
        .map_err(|e| FfiError::General {
            msg: format!("read custodian assignment: {e}"),
        })?
    else {
        return Ok(None);
    };

    let actor_hex = hex::encode(
        fauna_core::identity::ActorKeypair::from_secret(secret)
            .actor_id()
            .0,
    );
    let root =
        custodian_store_root(Path::new(&data_dir), &actor_hex).map_err(|e| FfiError::General {
            msg: format!("resolve custodian store root: {e}"),
        })?;

    // The shared crossing (`crate::cloud_backup`): the shell excluder becomes
    // the enum's owned shell arm, the manifest declaration crosses verbatim.
    let exclusion: CloudBackupExclusion = exclusion.into();

    let store = CustodianStore::ensure_root(root, exclusion)
        .await
        .map_err(|e| FfiError::General {
            msg: format!("open custodian store: {e}"),
        })?;

    Ok(Some(Arc::new(FfiCustodianHost {
        host: CustodianHost::build(CustodianHostParams {
            source_ws_client: ws,
            source_nest_url: ws_url,
            secret,
            device_id: device_id_bytes,
            store,
            assignment,
        }),
    })))
}

// ---------------------------------------------------------------------------
// The two store verbs — the mobile in-app twin of the desktop agent's
// `GetCustodianStore` / `ReclaimCustodianStore`.
// ---------------------------------------------------------------------------

/// How long a reclaim waits for this process's own custodian work to stop
/// before refusing. The desktop agent's twin
/// (`bins/fauna-sync-agent/src/pipe_server.rs`) uses the same pair, and for the
/// same reason: a named, generous budget with a deadline poll rather than a
/// settle-sleep (`testing.md` convention 14) — the wait ends the instant the
/// last pass drops its guard, and the ceiling exists only so a wedged pull
/// turns into an honest refusal instead of an unbounded call.
const RECLAIM_TEARDOWN_BUDGET: Duration = Duration::from_secs(30);
const RECLAIM_TEARDOWN_POLL: Duration = Duration::from_millis(50);

/// This process's in-flight custodian work.
///
/// The desktop agent can promise a reclaim never deletes bytes out from under
/// its own writer because it *is* the writer's process and publishes the stint
/// beside the host (`SyncServiceState::hosted_custodian`). Mobile hosts in the
/// app, so the same promise has to be made here or not at all — and "not at
/// all" is the failure the desktop guarantee exists to prevent: a pull pass
/// writes blobs and then records them, so a sweep racing it deletes freshly
/// written blobs whose index row lands a moment later, leaving a store that
/// reports healthy and fails its next audit.
///
/// Process-global rather than threaded through the shells because a shell
/// cannot supply it: the scheduled pass ([`FfiCustodianHost::run_all_kinds`],
/// android `WorkManager` / iOS `BGProcessingTask`) and the foreground loop
/// ([`FfiCustodianHost::start_push_debounce`]) are started from two different
/// places that never meet, and the reclaim is called from a third.
///
/// Deliberately **not** keyed by actor. A mobile shell holds at most one live
/// custodian host — the current account's — so keying would buy nothing in
/// practice; and where it could differ, waiting for *any* in-flight pass is the
/// conservative direction, which is the direction this whole affordance takes.
/// Erring the other way would mean deleting during a pass because it was
/// attributed to someone else.
static CUSTODIAN_ACTIVITY: LazyLock<Mutex<CustodianActivity>> =
    LazyLock::new(|| Mutex::new(CustodianActivity::default()));

#[derive(Default)]
struct CustodianActivity {
    /// Passes currently touching the store — a scheduled `run_all_kinds`, a
    /// live push-debounce loop, or both.
    in_flight: usize,
    /// The live push-debounce loop's cancel token, when one is running. Held so
    /// a reclaim can *stop* the loop rather than wait out a loop that has no
    /// periodic tick and would therefore never end on its own.
    push_cancel: Option<CancellationToken>,
}

/// Lock the activity record, ignoring poisoning.
///
/// A pull pass that panicked must not leave every later reclaim permanently
/// refusing: the counter is plain data, and the guard's `Drop` runs during the
/// panic unwind regardless, so the value behind a poisoned lock is still true.
fn activity() -> std::sync::MutexGuard<'static, CustodianActivity> {
    CUSTODIAN_ACTIVITY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Marks custodian work in flight for as long as it lives.
struct ActivityGuard {
    /// Whether this guard owns the push-loop cancel slot as well as the counter.
    clears_push: bool,
}

impl ActivityGuard {
    /// A scheduled pass — counter only.
    fn pass() -> Self {
        activity().in_flight += 1;
        Self { clears_push: false }
    }

    /// The foreground push loop — counter plus the cancel slot a reclaim stops.
    fn push_loop(cancel: CancellationToken) -> Self {
        let mut a = activity();
        a.in_flight += 1;
        a.push_cancel = Some(cancel);
        Self { clears_push: true }
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        let mut a = activity();
        a.in_flight = a.in_flight.saturating_sub(1);
        if self.clears_push {
            a.push_cancel = None;
        }
    }
}

/// Stop a live push loop and wait for every in-flight pass to drop its guard.
///
/// `true` iff the store went quiet inside `budget`. Split out of the exported
/// verb so a test can pass a short budget: the production ceiling is 30s, and a
/// test that actually waits it out is a test nobody runs.
async fn custodian_work_stopped_within(budget: Duration) -> bool {
    // Cancel outside the lock, and *take* the token rather than borrow it: the
    // loop's own guard clears the slot as it unwinds and would otherwise
    // contend with this wait for the same mutex.
    let live = activity().push_cancel.take();
    if let Some(cancel) = live {
        tracing::info!("custodian: stopping this device's replica for a reclaim");
        cancel.cancel();
    }
    let deadline = Instant::now() + budget;
    loop {
        if activity().in_flight == 0 {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(RECLAIM_TEARDOWN_POLL).await;
    }
}

/// Where this device's sealed custodian store lives, for this owner.
///
/// The actor scoping is derived here from `owner_secret` exactly as
/// [`build_custodian_host`] derives it, so one owner's corpus can never be
/// measured — or reclaimed — under another's scope on a shared device.
fn store_root_for(owner_secret: &[u8], data_dir: &str) -> Result<PathBuf, FfiError> {
    let secret = secret32(owner_secret)?;
    let actor_hex = hex::encode(
        fauna_core::identity::ActorKeypair::from_secret(secret)
            .actor_id()
            .0,
    );
    custodian_store_root(Path::new(data_dir), &actor_hex).map_err(|e| FfiError::General {
        msg: format!("resolve custodian store root: {e}"),
    })
}

/// This device's sealed custodian store, measured — the mobile twin of the
/// agent's `CustodianStoreInfo`.
#[derive(uniffi::Record)]
pub struct FfiCustodianStoreInfo {
    /// Generations the index calls held. **Index truth** — zero here with
    /// non-zero `bytes` is the interrupted-`put` case, not a contradiction.
    pub generations: u64,
    /// Blob + manifest files on disk, recorded or orphaned alike.
    pub files: u64,
    /// Bytes those files occupy — the `store_holds_bytes` input to
    /// `fauna_core::data::custodian_store_is_orphaned`.
    pub bytes: u64,
    /// The source regressions standing on the store's audit record — each a
    /// pull pass refused because the source served a saved counter below the
    /// ledger the store holds. The shell hands them back, with this device's
    /// sync id, as [`crate::backup_audit_run_pass`]'s `own_custodian`, which
    /// folds them into this device's own destination row.
    #[uniffi(default = [])]
    pub source_regressions: Vec<FfiCustodianSourceRegression>,
}

/// One refused pull on [`FfiCustodianStoreInfo`] — the twin of the agent's
/// `CustodianSourceRegression`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCustodianSourceRegression {
    /// The held ledger's own path in the store — one per set and family.
    pub ledger: String,
    /// The generation of the ledger the store holds.
    pub held: u32,
    /// The lower counter the source served.
    pub served: u32,
    /// Unix seconds of the first pass that found the source there.
    pub observed_at: i64,
}

/// This device's own custodian store as a shell hands it to
/// [`crate::backup_audit_run_pass`] — the twin of
/// `fauna_client_backup::audit::OwnCustodianStore`.
#[derive(uniffi::Record)]
pub struct FfiOwnCustodianStore {
    /// This device's stable sync device id, hex — the one
    /// [`crate::custodian_store_is_orphaned`] takes.
    pub device_id: String,
    /// [`FfiCustodianStoreInfo::source_regressions`], as the store read
    /// returned it.
    pub source_regressions: Vec<FfiCustodianSourceRegression>,
}

/// What a reclaim did — the mobile twin of the agent's
/// `CustodianReclaimOutcome`.
#[derive(uniffi::Record)]
pub struct FfiCustodianReclaimOutcome {
    /// `true` iff the reclaim **refused** because this process's own custodian
    /// work would not stop in time. Nothing was deleted. A reported outcome,
    /// never an error: the store is intact, and the page's own orphaned row is
    /// both the honest state and the way to retry.
    pub still_hosting: bool,
    /// Blob + manifest files deleted, orphans from an interrupted `put`
    /// included.
    pub freed_files: u64,
    /// Bytes those files occupied.
    pub freed_bytes: u64,
}

/// Measure this device's sealed custodian store — the read behind
/// `backup-orphaned-store-row` on a mobile shell.
///
/// # Why a free fn and not a method on [`FfiCustodianHost`]
///
/// Because the host does not exist in the state this read exists to serve.
/// [`build_custodian_host`] answers `Ok(None)` whenever the source nest's
/// registry has no row naming this device — and an **orphaned** store is
/// precisely that state: bytes held, no destination row claiming them
/// (`fauna_core::data::custodian_store_is_orphaned`). A host-bound method would
/// therefore be unreachable exactly after the removal that strands the bytes,
/// and on every launch afterwards. The desktop seam has the same shape for the
/// same reason: `GetCustodianStore` is answered from the agent's own paths, not
/// from a live stint. The free fn also serves both mobile shells at once
/// (land it once, in whichever leg lands first) — and
/// is uniform with the `backup_destination_*` family that is already this
/// page's native seam.
///
/// Read-only and policy-free: it reports this device's disk. Whether that store
/// is *orphaned* is the app's call, because the destination rows that decide it
/// are the page's, not this boundary's.
///
/// A store that does not exist is an **empty** store, not an error — the honest
/// answer for a device that never enrolled. The root is opened with
/// `CustodianStore::at`, never `ensure_root`: a read must not create the
/// directory, and must not re-assert a cloud-backup exclusion over a store that
/// is not there.
#[fauna_uniffi_async::export]
pub async fn custodian_store_footprint(
    owner_secret: Vec<u8>,
    data_dir: String,
) -> Result<FfiCustodianStoreInfo, FfiError> {
    let root = store_root_for(&owner_secret, &data_dir)?;
    let store = CustodianStore::at(root);
    let footprint = store.footprint().await.map_err(|e| FfiError::General {
        msg: format!("measure custodian store: {e}"),
    })?;
    // The audit record through the same read-only door: a missing record is
    // one with no regressions.
    let source_regressions = store
        .audit_record()
        .await
        .source_regressions
        .into_iter()
        .map(|(ledger, r)| FfiCustodianSourceRegression {
            ledger,
            held: r.held,
            served: r.served,
            observed_at: r.observed_at,
        })
        .collect();
    Ok(FfiCustodianStoreInfo {
        generations: footprint.generations as u64,
        files: footprint.files as u64,
        bytes: footprint.bytes,
        source_regressions,
    })
}

/// Free this device's whole sealed custodian store, after stopping this
/// process's own custodian work — the `backup-destination-reclaim-button`
/// action, and the `backup-destination-remove-reclaim-checkbox` opt-in.
///
/// A free fn for the reason [`custodian_store_footprint`] is: the affordance is
/// offered for a store no destination row claims, which is exactly when no host
/// can be built.
///
/// # The one guarantee this adds over `reclaim_all`
///
/// The app decides *whether* to reclaim; what only this process can promise is
/// that the bytes are not deleted out from under its own writer. A live
/// foreground push loop is **stopped first** (it has no periodic tick, so
/// waiting it out is waiting forever), and the reclaim proceeds once every
/// in-flight pass has dropped its guard. Work that does not stop inside
/// [`RECLAIM_TEARDOWN_BUDGET`] answers `still_hosting` and **deletes nothing**,
/// rather than racing it.
///
/// In the ordinary orphaned flow there is nothing to stop: no row names this
/// device, so neither drive built a host. The window this guards is the
/// remove-with-opt-in path, where the row existed moments ago and a foreground
/// loop built while it did may still be live.
///
/// Expressed as `reclaim_all` — `reclaim` over every held row rather than a
/// directory delete — so it inherits the same reference-set sweep, orphans from
/// an interrupted `put` included, and **the root survives**: the platform
/// cloud-backup exclusion is an attribute of that directory, and re-creating it
/// on re-enrollment is a window in which the vendor cloud can start replicating
/// a freshly sealed corpus.
#[fauna_uniffi_async::export]
pub async fn reclaim_custodian_store(
    owner_secret: Vec<u8>,
    data_dir: String,
) -> Result<FfiCustodianReclaimOutcome, FfiError> {
    let root = store_root_for(&owner_secret, &data_dir)?;
    reclaim_store_at(root, RECLAIM_TEARDOWN_BUDGET).await
}

/// [`reclaim_custodian_store`]'s body, with the teardown budget as an argument
/// so a test can name a short one.
async fn reclaim_store_at(
    root: PathBuf,
    budget: Duration,
) -> Result<FfiCustodianReclaimOutcome, FfiError> {
    if !custodian_work_stopped_within(budget).await {
        tracing::warn!(
            "custodian: reclaim refused — this device's replica did not stop within \
             {budget:?}; nothing was deleted"
        );
        return Ok(FfiCustodianReclaimOutcome {
            still_hosting: true,
            freed_files: 0,
            freed_bytes: 0,
        });
    }
    let report = CustodianStore::at(root)
        .reclaim_all()
        .await
        .map_err(|e| FfiError::General {
            msg: format!("reclaim custodian store: {e}"),
        })?;
    tracing::info!(
        files = report.files,
        bytes = report.bytes,
        "custodian: reclaimed this device's sealed store"
    );
    Ok(FfiCustodianReclaimOutcome {
        still_hosting: false,
        freed_files: report.files as u64,
        freed_bytes: report.bytes,
    })
}

/// Restore the signed-in nest from this device's own sealed store, in this
/// process — the confirmed `backup-destination-reseed-confirm-button` action on
/// a phone (`backup-destinations.md` § Re-seed → *Where the ceremony runs*: a
/// seed-holding app that hosts its own store runs the ceremony itself).
///
/// A free fn for the reason [`custodian_store_footprint`] is: the gesture's
/// first home is the orphaned-store row, which is exactly when no host exists.
///
/// Nothing about the ceremony's order is decided here. The driver is the shared
/// `fauna_client_backup::reseed::run_reseed` over the shared
/// `fauna_sync_engine::reseed::ReseedDelivery` — the pair the desktop agent's
/// job runs — and the tail is [`crate::reseed`]'s `finish`, the pair every
/// shell's post-ceremony duty goes through. What this adds is only what the
/// in-process host alone can promise, the reclaim's own guarantee in reverse:
/// this process's custodian work is stopped first and a pass guard is held for
/// the whole ceremony, so neither a pull pass nor a reclaim writes or deletes
/// under the corpus while it is being delivered. Work that will not stop inside
/// [`RECLAIM_TEARDOWN_BUDGET`] answers `stopped` and sends nothing.
///
/// `nest` is the owner's keyed connection to the nest being seeded: the grant,
/// the custody records and the materialize ride it, and a bearer byte plane is
/// built from the seed for the chunks, as [`build_custodian_host`] builds its
/// own. `device_id` is this device's stable sync id (32 bytes), registered
/// write-capable on the target. `data_dir` is the unscoped per-user base, as
/// for [`reclaim_custodian_store`]. The folder custody rows are signed with this
/// connection's identity when this build carries the resolver
/// (`folders-author`), as the app's own sync client signs them.
///
/// A stop comes back as [`crate::FfiReseedResult::stopped`], never as an error:
/// nothing was made live, and re-running resumes every phase.
#[fauna_uniffi_async::export]
pub async fn reseed_custodian_store(
    nest: Arc<FfiNestClient>,
    owner_secret: Vec<u8>,
    device_id: Vec<u8>,
    data_dir: String,
) -> Result<crate::FfiReseedResult, FfiError> {
    use fauna_client_backup::reseed::run_reseed;
    use fauna_core::crypto::{BackupKey, NestBackupKey, OwnerSealKey};
    use fauna_core::identity::ActorKeypair;
    use fauna_sync_engine::reseed::{ReseedDelivery, SyncClientSink};

    let secret = secret32(&owner_secret)?;
    let device_id_bytes = device32(&device_id)?;
    let device_hex = hex::encode(device_id_bytes);
    let root = store_root_for(&owner_secret, &data_dir)?;

    if !custodian_work_stopped_within(RECLAIM_TEARDOWN_BUDGET).await {
        return Ok(crate::FfiReseedResult::stopped(
            "this device's backup would not pause; nothing was sent",
        ));
    }
    let _active = ActivityGuard::pass();

    // `at`, never `ensure_root`: a re-seed only reads the store, and must not
    // create one where this device never held a copy.
    let store = CustodianStore::at(root);
    let ws = nest.nest_arc();
    let actor_id = ActorKeypair::from_secret(secret).actor_id().0;
    let auth = Arc::new(fauna_client::AuthClient::new(
        ws.nest_url().to_string(),
        ActorKeypair::from_secret(secret),
    ));
    let sync_client = fauna_sync_engine::nest_client::SyncClient::new(auth, &device_id_bytes);
    let sink = SyncClientSink {
        client: &sync_client,
    };
    let nest_key = NestBackupKey::derive(&secret);
    #[allow(unused_mut)]
    let mut leg = ReseedDelivery::new(
        &store,
        &sink,
        ws.as_ref(),
        actor_id,
        device_hex.clone(),
        OwnerSealKey::Client(BackupKey::derive(&secret)),
        &nest_key,
    );
    #[cfg(feature = "folders-author")]
    {
        // The seed-holding process prepares each restored folder's target set
        // before the ceremony (`writer-signed-change-records.md` ruling
        // (7)(a)(i)): custody first, nonce minted — the leg then signs every
        // re-homed row under it, and the nest never creates the folder.
        let custody = crate::account_runtime::folder_key_store();
        let names: Vec<String> = match store.folder_names().await {
            Ok(names) => names.into_values().collect(),
            Err(e) => {
                tracing::warn!("re-seed: reading the store's folder names: {e:#}");
                Vec::new()
            }
        };
        let files = fauna_client_folders::FoldersClient::new(Arc::clone(&ws));
        fauna_client_folders::prepare_reseed_targets_logged(&files, &*custody, &names).await;
        leg = leg.with_record_signing(fauna_client_folders::record_signing(
            Arc::clone(&ws),
            &ActorKeypair::from_secret(secret),
            custody,
        ));
    }

    let outcome = run_reseed(
        &BackupClient::new(Arc::clone(&ws)),
        nest_key.to_bytes().to_vec(),
        &leg,
    )
    .await;
    drop(nest_key);
    Ok(match outcome {
        Ok(outcome) => crate::reseed::finish(&nest, outcome, &device_hex).await,
        Err(e) => crate::FfiReseedResult::stopped(e.to_string()),
    })
}

/// Fold one pass's per-kind reports into the shell's summary.
///
/// Separate from [`FfiCustodianHost::run_all_kinds`] because its two
/// interesting rules are pure and both are silent when wrong — a wrong
/// `held_bytes` overstates the store by a factor of the kind count, and a
/// wrong `cap_reached` renders a stopped backup as healthy.
fn summarize(reports: &[PullReport]) -> FfiCustodianPullSummary {
    FfiCustodianPullSummary {
        stored_segments: reports.iter().map(|r| r.stored_segments.len() as u32).sum(),
        tombstoned_segments: reports
            .iter()
            .map(|r| r.tombstoned_segments.len() as u32)
            .sum(),
        // Each kind's report carries the store's TOTAL held bytes as of its own
        // check-in, so the last pass's figure is the current one. Summing would
        // multiply the store by the number of kinds.
        held_bytes: reports.last().map(|r| r.held_bytes).unwrap_or(0),
        // Read from the pass's own verdict, never inferred from
        // `held_bytes >= cap`: a pass that stops at its cap ends *below* it.
        cap_reached: reports.iter().any(|r| r.cap_state == CapState::Reached),
        // The check-in is written inside `run_once`, and a failed check-in makes
        // that call return `Err` — so a kind that reached `reports` at all
        // definitively checked in (the agent's rule, verbatim).
        checked_in: !reports.is_empty(),
    }
}

/// Unix seconds. The scheduled task supplies no clock — a production pull's
/// notion of "now" is the wall clock, exactly as the desktop driver's loop uses
/// it; the *tests* that need determinism drive `CustodianPull::run_once`
/// directly with an explicit `now`, which is why that parameter exists.
fn now_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs_or_zero()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 32-byte owner secret that is not all-zero (the actor derivation is
    /// real, so the bytes must be a usable scalar seed).
    fn secret_bytes(tag: u8) -> Vec<u8> {
        let mut s = vec![7u8; 32];
        s[0] = tag;
        s
    }

    /// Bytes on disk under the store root, so `footprint` has something to
    /// find.
    ///
    /// Written in the store's **own fanned layout** — `blobs/<2-hex>/<64-hex>`,
    /// as `CustodianStore::fanned` lays it down — because both the walk
    /// (`measure_dir`) and the sweep (`sweep_dir`) iterate bucket directories
    /// and skip loose files at the top. A flat fixture measures as zero and
    /// would make every assertion below pass vacuously.
    ///
    /// No index row is written, which is deliberate: blobs on disk that the
    /// index does not name is exactly the interrupted-`put` case `footprint`
    /// walks the disk to see, and the one `reclaim_all`'s sweep exists to
    /// collect.
    fn seed_store_bytes(root: &Path, n: usize) {
        for i in 0..n {
            let hex = format!("{i:064x}");
            let bucket = root.join("blobs").join(&hex[..2]);
            std::fs::create_dir_all(&bucket).expect("create store bucket");
            std::fs::write(bucket.join(&hex), vec![0xABu8; 1024]).expect("write blob");
        }
    }

    /// Reset the process-global activity between tests — they share it, and a
    /// guard leaked by a failing test would make its neighbours lie.
    fn reset_activity() {
        let mut a = activity();
        a.in_flight = 0;
        a.push_cancel = None;
    }

    /// The activity record is process-global, so the tests that touch it must
    /// not run concurrently with each other. Serializing them here rather than
    /// demanding `--test-threads=1` keeps the rest of the crate's suite
    /// parallel.
    /// Async-aware on purpose: every test that takes it holds it across awaits
    /// (that is the point — the reclaim under test *is* the await), and a
    /// `std::sync::Mutex` held that way is `clippy::await_holding_lock`.
    static ACTIVITY_TESTS: LazyLock<tokio::sync::Mutex<()>> =
        LazyLock::new(|| tokio::sync::Mutex::new(()));

    async fn activity_test_lock() -> tokio::sync::MutexGuard<'static, ()> {
        let guard = ACTIVITY_TESTS.lock().await;
        reset_activity();
        guard
    }

    /// A read must never create the store root.
    ///
    /// This is the whole shape of the mobile leg: the affordance exists for a
    /// device whose destination row is gone, and the page reads the footprint
    /// on every mount — including on the overwhelming majority of devices that
    /// never enrolled at all. `ensure_root` here would mkdir a
    /// `backup-custodian` directory on every such device, and (worse) re-assert
    /// a cloud-backup exclusion over a store that does not exist.
    ///
    /// Mutation check: swapping `CustodianStore::at` for `ensure_root` in
    /// `custodian_store_footprint` reds the `!root.exists()` assertion.
    #[tokio::test]
    async fn the_footprint_read_never_creates_the_store_root() {
        let base = tempfile::tempdir().expect("tempdir");
        let secret = secret_bytes(1);
        let root = store_root_for(&secret, base.path().to_str().unwrap()).expect("root");
        assert!(!root.exists(), "precondition: nothing enrolled yet");

        let info = custodian_store_footprint(secret, base.path().to_str().unwrap().to_string())
            .await
            .expect("a device that never enrolled reads as an empty store, not an error");

        assert_eq!(info.bytes, 0, "an absent store holds nothing");
        assert!(
            !root.exists(),
            "a read must not create the store root — it would also re-assert a \
             cloud-backup exclusion over a store that is not there"
        );
    }

    /// The ordinary orphaned flow: bytes held, no host buildable, reclaim frees
    /// them and the root survives.
    ///
    /// The seeded blobs carry no index row, which is deliberate — that is the
    /// interrupted-`put` case `footprint` walks the disk to see and
    /// `reclaim_all`'s sweep exists to collect.
    #[tokio::test]
    async fn a_reclaim_frees_the_bytes_and_leaves_the_root_standing() {
        let _serial = activity_test_lock().await;
        let base = tempfile::tempdir().expect("tempdir");
        let secret = secret_bytes(2);
        let dir = base.path().to_str().unwrap().to_string();
        let root = store_root_for(&secret, &dir).expect("root");
        seed_store_bytes(&root, 3);

        let before = custodian_store_footprint(secret.clone(), dir.clone())
            .await
            .expect("measure");
        assert_eq!(
            before.bytes,
            3 * 1024,
            "precondition: the store holds bytes"
        );

        let outcome = reclaim_custodian_store(secret.clone(), dir.clone())
            .await
            .expect("reclaim");
        assert!(!outcome.still_hosting, "nothing was hosting");
        assert_eq!(outcome.freed_bytes, 3 * 1024);
        assert_eq!(outcome.freed_files, 3);

        let after = custodian_store_footprint(secret, dir)
            .await
            .expect("measure");
        assert_eq!(after.bytes, 0, "the bytes are actually gone");
        assert!(
            root.exists(),
            "the ROOT survives — the platform cloud-backup exclusion is an \
             attribute of that directory, and re-creating it on re-enrollment is \
             a window in which the vendor cloud can replicate a sealed corpus"
        );
    }

    /// The guarantee the desktop agent makes, made here: a reclaim racing this
    /// process's own custodian work refuses and deletes NOTHING.
    ///
    /// Mutation check: dropping the `custodian_work_stopped_within` call from
    /// `reclaim_store_at` reds both the `still_hosting` assertion AND the
    /// surviving-bytes one — the second is the load-bearing half, because a
    /// refusal that still deleted would be the exact silent corruption the
    /// guard exists to prevent (blobs written but not yet recorded).
    #[tokio::test]
    async fn a_reclaim_refuses_while_this_process_is_still_hosting() {
        let _serial = activity_test_lock().await;
        let base = tempfile::tempdir().expect("tempdir");
        let secret = secret_bytes(3);
        let dir = base.path().to_str().unwrap().to_string();
        let root = store_root_for(&secret, &dir).expect("root");
        seed_store_bytes(&root, 2);

        // A pass in flight — exactly what `run_all_kinds` holds for its
        // duration.
        let in_flight = ActivityGuard::pass();

        let outcome = reclaim_store_at(root.clone(), Duration::from_millis(150))
            .await
            .expect("a refusal is an outcome, never an error");

        assert!(
            outcome.still_hosting,
            "a reclaim that cannot stop this device's own writer must refuse"
        );
        assert_eq!(outcome.freed_bytes, 0, "a refusal frees nothing");
        assert_eq!(
            CustodianStore::at(root)
                .footprint()
                .await
                .expect("measure")
                .bytes,
            2 * 1024,
            "the bytes are still there — a refusal must not have deleted under \
             the in-flight pass"
        );

        drop(in_flight);
    }

    /// A reclaim STOPS the foreground push loop rather than waiting it out.
    ///
    /// The loop has no periodic tick (`start_push_debounce` is push-only,
    /// because WorkManager already drives the period), so a reclaim that merely
    /// waited for it would wait the whole budget and then refuse, every time,
    /// on any foregrounded app. Cancelling is what makes the ordinary path
    /// succeed.
    #[tokio::test]
    async fn a_reclaim_cancels_the_foreground_push_loop_it_is_waiting_on() {
        let _serial = activity_test_lock().await;
        let cancel = CancellationToken::new();
        let guard = ActivityGuard::push_loop(cancel.clone());

        // The loop's own teardown: it drops its guard once cancelled, exactly
        // as the spawned task does when `run_push_debounce` returns.
        let loop_side = tokio::spawn({
            let cancel = cancel.clone();
            async move {
                cancel.cancelled().await;
                drop(guard);
            }
        });

        assert!(
            custodian_work_stopped_within(Duration::from_secs(5)).await,
            "the wait must end by cancelling the loop, not by outlasting it"
        );
        assert!(cancel.is_cancelled(), "the loop was told to stop");
        loop_side.await.expect("loop side");
    }

    /// Two owners on one device never share a store root.
    ///
    /// The verbs take an owner secret and derive the actor scoping from it for
    /// this reason alone: these are a **destructive** verb and the read that
    /// arms it, and a shared-device account switch must not let one owner
    /// measure — or free — the other's sealed corpus.
    #[test]
    fn two_owners_never_share_a_store_root() {
        let base = tempfile::tempdir().expect("tempdir");
        let dir = base.path().to_str().unwrap();
        let a = store_root_for(&secret_bytes(4), dir).expect("root a");
        let b = store_root_for(&secret_bytes(5), dir).expect("root b");
        assert_ne!(a, b, "one owner must never reclaim another's corpus");
    }

    /// A secret that is not 32 bytes is refused before any path is resolved.
    #[test]
    fn a_malformed_owner_secret_never_reaches_the_disk() {
        let base = tempfile::tempdir().expect("tempdir");
        assert!(
            store_root_for(&[0u8; 8], base.path().to_str().unwrap()).is_err(),
            "a short secret must not resolve to some path anyway"
        );
    }

    fn report(held: u64, cap: CapState, stored: &[u32], tombstoned: &[u32]) -> PullReport {
        PullReport {
            stored_segments: stored.to_vec(),
            tombstoned_segments: tombstoned.to_vec(),
            held_bytes: held,
            cap_state: cap,
            ..PullReport::default()
        }
    }

    /// `held_bytes` is the store's total, reported afresh by every kind — so the
    /// fold takes the LAST one, never the sum.
    ///
    /// Mutation check: `.sum()` here returns 300 for this input, i.e. it would
    /// tell the user a 100-byte store holds 300 bytes, growing with every kind
    /// added to `BACKED_UP_KINDS`.
    #[test]
    fn held_bytes_is_the_last_report_not_the_sum() {
        let s = summarize(&[
            report(100, CapState::Ok, &[], &[]),
            report(100, CapState::Ok, &[], &[]),
            report(100, CapState::Ok, &[], &[]),
        ]);
        assert_eq!(
            s.held_bytes, 100,
            "held bytes is a total, not a per-kind delta"
        );
    }

    /// Cap-reached comes from the pass's own verdict. The inference this refuses
    /// (`held_bytes >= cap`) is FALSE for exactly this input: a pass that stops
    /// at its cap ends *below* it, so an inferring fold reports healthy-with-room
    /// for a backup that has stopped.
    #[test]
    fn cap_reached_reads_the_verdict_and_not_the_byte_count() {
        let s = summarize(&[
            report(10, CapState::Ok, &[], &[]),
            report(10, CapState::Reached, &[], &[]),
        ]);
        assert!(
            s.cap_reached,
            "a kind that stopped at its cap must surface, even holding far fewer bytes than the cap"
        );

        let healthy = summarize(&[report(u64::MAX, CapState::Ok, &[], &[])]);
        assert!(
            !healthy.cap_reached,
            "byte count alone must never raise cap-reached"
        );
    }

    /// Counts sum across kinds (unlike `held_bytes`) — they are per-pass
    /// deltas, not totals.
    #[test]
    fn segment_counts_sum_across_kinds() {
        let s = summarize(&[
            report(0, CapState::Ok, &[1, 2], &[9]),
            report(0, CapState::Ok, &[3], &[]),
        ]);
        assert_eq!(s.stored_segments, 3);
        assert_eq!(s.tombstoned_segments, 1);
    }

    /// A device with nothing to do reports zeroes rather than panicking on an
    /// empty report vector — the ordinary state of a custodian whose source has
    /// no new segments.
    #[test]
    fn an_empty_pass_is_all_zeroes() {
        let s = summarize(&[]);
        assert_eq!(s.stored_segments, 0);
        assert_eq!(s.held_bytes, 0);
        assert!(!s.cap_reached);
        assert!(
            !s.checked_in,
            "no kind reached `reports`, so no check-in happened"
        );
    }

    /// `checked_in` is the agent's rule (`pipe_server.rs`'s report): the check-in
    /// is written inside `run_once` and a failed one makes it return `Err`, so a
    /// kind that reached the report vector at all definitively checked in.
    #[test]
    fn any_report_means_the_pass_checked_in() {
        assert!(summarize(&[report(0, CapState::Ok, &[], &[])]).checked_in);
    }
}
