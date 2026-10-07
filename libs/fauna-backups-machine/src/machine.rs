//! The page-level Backups state machine — the snapshot half.
//!
//! Uses `std::sync::Mutex` (not tokio's) so getters and sync gestures work from
//! any client thread without an async context, mirroring
//! `fauna_devices_machine::DevicesMachine`.
//!
//! Shape owner: `docs/goal/ui/backups.md` § Snapshot-list shape.

use std::sync::{Arc, Mutex};

use fauna_core::localized::LocalizedText;
use fauna_core::sync::is_reserved_folder_name;
use fauna_protocol::filesync::{
    SnapshotCheckReply, SnapshotGetReply, SnapshotPruneSetPolicyReply, SnapshotSummaryRow,
};
use fauna_protocol::folders::FolderSummary as WireFolderSummary;

use crate::nest_api::BackupsNestApi;
use crate::observer::BackupsObserver;
use crate::snapshots::{
    BackupFolderRow, BackupOp, BackupsSnapshot, CheckOutcome, PolicyState, PruneCandidate,
    PrunePreview, RowIntegrity, SnapshotDetail, SnapshotFileRow, SnapshotRow, SnapshotState,
};

/// i18n key for a page-load failure.
const REFRESH_ERROR_KEY: &str = "backups.error_refresh";
/// i18n key for a snapshot-create failure.
const CREATE_ERROR_KEY: &str = "backups.error_create_snapshot";
/// i18n key for a snapshot-delete failure.
const DELETE_ERROR_KEY: &str = "backups.error_delete_snapshot";
/// i18n key for an immediate-delete failure.
const DELETE_IMMEDIATE_ERROR_KEY: &str = "backups.error_delete_snapshot_immediate";
/// i18n key for an undelete failure.
const UNDELETE_ERROR_KEY: &str = "backups.error_undelete_snapshot";
/// i18n key for a prune failure (preview or execute).
const PRUNE_ERROR_KEY: &str = "backups.error_prune";
/// i18n key for an integrity-check failure. ⚠ A check that *completes* with
/// findings is NOT this — it is a [`CheckOutcome`] with `is_ok == false`
/// (§ Architectural rules, rule 6). This key is only for a check that could not
/// run at all.
const CHECK_ERROR_KEY: &str = "backups.error_check";
/// i18n key for a snapshot-detail (file list) read failure.
const DETAIL_ERROR_KEY: &str = "backups.error_detail";

fn error_text(key: &str, detail: &str) -> LocalizedText {
    LocalizedText::key_arg(key, "message", detail.to_string())
}

/// Internal page state. In-memory only; clients read snapshots via the getter.
struct State {
    folders: Vec<BackupFolderRow>,
    selected_folder: Option<String>,
    snapshots: Vec<SnapshotRow>,
    in_progress_op: Option<BackupOp>,
    check_result: Option<CheckOutcome>,
    prune_preview: Option<PrunePreview>,
    detail: Option<SnapshotDetail>,
    error: Option<LocalizedText>,
    /// A folder was picked while an op held the slot; that op loads it before
    /// releasing the slot ([`BackupsMachine::finish_op`]).
    reselect_pending: bool,
    /// A refresh was asked for while an op held the slot; that op re-reads the
    /// page before releasing the slot ([`BackupsMachine::finish_op`]). A plain
    /// re-read, unlike a pick: it keeps the op's own verdicts.
    reload_pending: bool,
}

impl State {
    fn new() -> Self {
        Self {
            folders: Vec::new(),
            selected_folder: None,
            snapshots: Vec::new(),
            in_progress_op: None,
            check_result: None,
            prune_preview: None,
            detail: None,
            error: None,
            reselect_pending: false,
            reload_pending: false,
        }
    }

    /// The `last-backed-up` element: the selected set's newest snapshot
    /// `created_at`, `None` when it has none (renders "never").
    ///
    /// Derived on read rather than stored, so it cannot go stale after a create
    /// or a delete — which is precisely windows' live bug (a hand-refreshed
    /// label) and linux's (a carried-over value on an emptied set).
    ///
    /// Taken over the *whole* list rather than `first()`: identical under the
    /// wire's newest-first order, but it does not silently invert if a future
    /// caller ever hands the machine rows in another order.
    fn last_backed_up(&self) -> Option<i64> {
        self.snapshots.iter().map(|s| s.created_at).max()
    }
}

#[cfg_attr(feature = "uniffi", derive(uniffi::Object))]
pub struct BackupsMachine {
    state: Mutex<State>,
    observer: Arc<dyn BackupsObserver>,
    nest_api: Arc<dyn BackupsNestApi>,
    /// The shell's stable sync device id, for snapshot row provenance
    /// (§ Snapshot-list shape, *Create* ruling). Wired post-construction by the
    /// client's build glue via [`Self::set_device_id`], the same pattern
    /// `DevicesMachine::set_mls_query` uses — the concrete id lives on the
    /// shell, not on the builder. `None` until then ⇒ unattributed snapshots,
    /// which is exactly what the wire's `Option` means.
    device_id: Mutex<Option<Vec<u8>>>,
}

impl BackupsMachine {
    /// Construct the page machine over an injected [`BackupsNestApi`] seam.
    /// State starts empty; the client calls `refresh()` to populate it.
    ///
    /// Not a `#[uniffi::constructor]` — the seam (`Arc<dyn …>`) has no FFI ABI.
    /// Clients construct via `nest_api::build_backups_machine` (native
    /// `fauna-ffi` / linux / tui, wasm web), which binds the session's connected
    /// requester and the reader's label custody; tests pass a
    /// `FakeBackupsNestApi`.
    pub fn new(observer: Arc<dyn BackupsObserver>, nest_api: Arc<dyn BackupsNestApi>) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::new()),
            observer,
            nest_api,
            device_id: Mutex::new(None),
        })
    }

    /// Wire the shell's stable sync device id (see [`Self::device_id`]). Called
    /// by the client's build glue after construction, **never over UniFFI** in
    /// the raw-bytes form — the FFI face takes hex.
    pub fn set_device_id(&self, device_id: Option<Vec<u8>>) {
        *self.device_id.lock().unwrap() = device_id;
    }
}

// ── Reads ───────────────────────────────────────────────────────

#[cfg_attr(feature = "uniffi", uniffi::export)]
impl BackupsMachine {
    /// The whole renderable page state. Clients call this on every observer
    /// tick and render off the result; they never see the internal state.
    pub fn snapshot(&self) -> BackupsSnapshot {
        let state = self.state.lock().unwrap();
        BackupsSnapshot {
            folders: state.folders.clone(),
            selected_folder: state.selected_folder.clone(),
            snapshots: state.snapshots.clone(),
            last_backed_up: state.last_backed_up(),
            in_progress_op: state.in_progress_op,
            check_result: state.check_result.clone(),
            prune_preview: state.prune_preview.clone(),
            detail: state.detail.clone(),
            error: state.error.clone(),
        }
    }

    /// Clear the `error-message` element.
    pub fn clear_error(&self) {
        self.state.lock().unwrap().error = None;
        self.observer.on_changed();
    }

    /// `immediate-delete-confirm-button`'s enabled flag — the shared
    /// `fauna_client_snapshots::immediate_delete_button_enabled` predicate with
    /// the machine's **real** in-flight flag threaded in.
    ///
    /// Exposed here rather than left to each app because the `deleting` half is
    /// the one every app got wrong: linux hard-codes `false` today, and five
    /// apps hand-rolled the whole four-way `&&` before the shared predicate
    /// existed. An app calls this and binds the result; it never re-derives.
    pub fn immediate_delete_enabled(
        &self,
        confirm_id: String,
        target_id: String,
        acknowledge: String,
    ) -> bool {
        let deleting = self.state.lock().unwrap().in_progress_op.is_some();
        immediate_delete_button_enabled(deleting, &confirm_id, &target_id, &acknowledge)
    }
}

/// The immediate-delete friction-bar predicate, inlined from
/// `fauna_client_snapshots::immediate_delete_button_enabled`.
///
/// ⚠ Deliberately a local copy of a **four-line** predicate rather than a
/// dependency: `fauna-client-snapshots` is a `rpc-glue`-only dep here (it pulls
/// the transport), and this machine's `default` build — the one tier_1 tests and
/// the UniFFI face compile — must stay transport-free. The acknowledge constant
/// itself still comes from `fauna-protocol`, so the load-bearing half cannot
/// drift; a `debug_assert`-style pin lives in the crate's tests.
fn immediate_delete_button_enabled(
    deleting: bool,
    confirm_id: &str,
    target_id: &str,
    acknowledge_typed: &str,
) -> bool {
    !deleting
        && !target_id.is_empty()
        && confirm_id == target_id
        && acknowledge_typed == fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT
}

// ── Gestures ────────────────────────────────────────────────────

#[cfg_attr(feature = "uniffi", fauna_uniffi_async::export)]
impl BackupsMachine {
    /// Load the folder selector and the selected set's snapshots — the page's
    /// mount read, and the re-read every successful mutation ends with.
    ///
    /// Selection is **page-lifetime with a deterministic default**
    /// (§ Snapshot-list shape, *Selection* ruling): the first row of the
    /// name-ordered list once loaded, and a selected set that has disappeared
    /// falls back to that default rather than sticking to a name the nest no
    /// longer serves.
    ///
    /// A refresh asked for while another op holds the slot is **not dropped**:
    /// it is recorded and that op re-reads before releasing the slot. The op in
    /// flight may have read the folder list before the change this refresh was
    /// asked to pick up, so dropping it can leave the page stale until the next
    /// visit (§ Snapshot-list shape, *Selection* ruling).
    pub async fn refresh(&self) {
        if !self.begin_op_or_queue_reload() {
            return;
        }
        let outcome = self.load().await;
        self.finish_op(outcome.err()).await;
    }

    /// Select a folder — the `backup-folder-selector` gesture.
    ///
    /// Clears the session-local check result and any pending prune preview:
    /// both are scoped to the set they were produced for, and carrying either
    /// across a selection change would render one set's verdict against
    /// another's rows.
    ///
    /// A pick is the user's gesture, not an op, so the single-flight gate never
    /// refuses it (§ Snapshot-list shape, *Selection* ruling). It is recorded at
    /// once; when another op is in flight, that op keeps its slot and loads the
    /// pick as it ends ([`Self::finish_op`]).
    pub async fn select_folder(&self, name: String) {
        let claimed = {
            let mut state = self.state.lock().unwrap();
            state.selected_folder = Some(name);
            state.check_result = None;
            state.prune_preview = None;
            // An open detail belongs to the outgoing set's snapshot. Carrying it
            // across would paint one set's file list under another set's rows —
            // the same scoping reason the two verdicts above are cleared.
            state.detail = None;
            state.snapshots.clear();
            // Recorded and claimed under one lock, so no op can begin between
            // the two and leave the pick neither loaded nor pending.
            if state.in_progress_op.is_some() {
                state.reselect_pending = true;
                false
            } else {
                state.in_progress_op = Some(BackupOp::Refresh);
                state.error = None;
                true
            }
        };
        self.observer.on_changed();
        if !claimed {
            return;
        }
        let outcome = self.load().await;
        self.finish_op(outcome.err()).await;
    }

    /// `snapshot-create-button` — take a manual snapshot of the selected set.
    ///
    /// Untagged, per the *Create* ruling (a tag is a retention shield).
    pub async fn create_snapshot(&self) {
        let Some(folder) = self.selected() else {
            return;
        };
        if !self.begin_op(BackupOp::Create) {
            return;
        }
        let device_id = self.device_id.lock().unwrap().clone();
        let outcome = match self.nest_api.create_snapshot(&folder, device_id).await {
            Ok(()) => self.load().await,
            Err(e) => Err(error_text(CREATE_ERROR_KEY, e.detail())),
        };
        self.finish_op(outcome.err()).await;
    }

    /// `snapshot-delete-button[i]` — queue the 48 h soft delete. The row then
    /// renders `DeletionPending`; cancelling it is the pending-actions
    /// surface's job, not this page's.
    ///
    /// The confirm is client glue (per-app), not machine state.
    pub async fn delete_snapshot(&self, snapshot_id: i64) {
        if !self.begin_op(BackupOp::Delete) {
            return;
        }
        let outcome = match self.nest_api.delete_snapshot(snapshot_id).await {
            Ok(()) => self.load().await,
            Err(e) => Err(error_text(DELETE_ERROR_KEY, e.detail())),
        };
        self.finish_op(outcome.err()).await;
    }

    /// `immediate-delete-confirm-button` — the modal-gated hard delete.
    ///
    /// `confirm_id` / `acknowledge` are what the user typed into
    /// `immediate-delete-confirm-input` / `immediate-delete-acknowledge-input`.
    /// The enable predicate is [`Self::immediate_delete_enabled`]; this call
    /// re-checks it rather than trusting the caller, so a client that wires the
    /// button wrong cannot skip the friction bar (§ Architectural rules, rule 4
    /// is a behavioural invariant, not styling).
    pub async fn delete_snapshot_immediate(
        &self,
        snapshot_id: i64,
        confirm_id: String,
        acknowledge: String,
    ) {
        if !self.immediate_delete_enabled(
            confirm_id.clone(),
            snapshot_id.to_string(),
            acknowledge.clone(),
        ) {
            return;
        }
        if !self.begin_op(BackupOp::ImmediateDelete) {
            return;
        }
        let outcome = match self
            .nest_api
            .delete_snapshot_immediate(snapshot_id, &confirm_id, &acknowledge)
            .await
        {
            Ok(()) => self.load().await,
            Err(e) => Err(error_text(DELETE_IMMEDIATE_ERROR_KEY, e.detail())),
        };
        self.finish_op(outcome.err()).await;
    }

    /// `snapshot-undelete-button[i]` — recover a soft-deleted snapshot before
    /// its `purge_after` (§ Snapshot-list shape, *Soft-deleted rows* ruling; id
    /// user-approved 2026-08-14).
    ///
    /// Refused unless the row is currently `SoftDeleted`: the affordance
    /// renders only there, and the machine enforces the rule structurally
    /// rather than trusting each app's render — the same shape as
    /// [`Self::prune_execute`]'s no-preview no-op.
    pub async fn undelete_snapshot(&self, snapshot_id: i64) {
        let is_soft_deleted = self.state.lock().unwrap().snapshots.iter().any(|row| {
            row.id == snapshot_id && matches!(row.state, SnapshotState::SoftDeleted { .. })
        });
        if !is_soft_deleted {
            return;
        }
        if !self.begin_op(BackupOp::Undelete) {
            return;
        }
        let outcome = match self.nest_api.undelete_snapshot(snapshot_id).await {
            Ok(()) => self.load().await,
            Err(e) => Err(error_text(UNDELETE_ERROR_KEY, e.detail())),
        };
        self.finish_op(outcome.err()).await;
    }

    /// `snapshot-prune-button` — the **preview** half: a dry run of the set's
    /// own resting retention policy.
    ///
    /// Never carries a client-chosen policy (§ Architectural rules, rule 5).
    pub async fn prune_preview(&self) {
        let Some(folder) = self.selected() else {
            return;
        };
        if !self.begin_op(BackupOp::Prune) {
            return;
        }
        let outcome = match self.nest_api.prune_set_policy(&folder, true).await {
            Ok(reply) => {
                self.state.lock().unwrap().prune_preview = Some(transcribe_preview(&reply));
                Ok(())
            }
            Err(e) => Err(error_text(PRUNE_ERROR_KEY, e.detail())),
        };
        self.finish_op(outcome.err()).await;
    }

    /// Execute the previewed prune. **Offered only from a preview** — a call
    /// with no `prune_preview` standing is a no-op, which is what makes the
    /// preview-first flow structural rather than a per-app UI convention.
    pub async fn prune_execute(&self) {
        let Some(folder) = self.selected() else {
            return;
        };
        if self.state.lock().unwrap().prune_preview.is_none() {
            return;
        }
        if !self.begin_op(BackupOp::Prune) {
            return;
        }
        let outcome = match self.nest_api.prune_set_policy(&folder, false).await {
            Ok(_) => {
                self.state.lock().unwrap().prune_preview = None;
                self.load().await
            }
            Err(e) => Err(error_text(PRUNE_ERROR_KEY, e.detail())),
        };
        self.finish_op(outcome.err()).await;
    }

    /// Dismiss a standing prune preview without executing it.
    pub fn cancel_prune_preview(&self) {
        self.state.lock().unwrap().prune_preview = None;
        self.observer.on_changed();
    }

    /// `snapshot-check-button` — run the integrity check directly. One click
    /// runs it; there is no second confirmation step (apple's sheet is
    /// reconciled away).
    ///
    /// A check that **completes with findings** is a result, not an error: it
    /// lands in `check_result` and implicates its rows, and `error-message`
    /// stays clear (§ Architectural rules, rule 6).
    pub async fn check(&self) {
        let Some(folder) = self.selected() else {
            return;
        };
        if !self.begin_op(BackupOp::Check) {
            return;
        }
        let outcome = match self.nest_api.check(&folder, false).await {
            Ok(reply) => {
                let outcome = transcribe_check(&reply);
                let mut state = self.state.lock().unwrap();
                apply_integrity(&mut state.snapshots, &outcome);
                state.check_result = Some(outcome);
                Ok(())
            }
            Err(e) => Err(error_text(CHECK_ERROR_KEY, e.detail())),
        };
        self.finish_op(outcome.err()).await;
    }

    /// Click `snapshot-item[i]` — open that snapshot's file list
    /// (`snapshot-detail-files`).
    ///
    /// The read is the seam's **custody-wired** `SnapshotsClient::get`, so a
    /// sealed set's paths arrive opened without the app wiring custody a second
    /// time (§ User actions puts this on "the machine's custody-wired
    /// `SnapshotsClient::get`"). Single-flight like every other gesture: opening
    /// a detail while a create is running would paint a list read against a
    /// pre-create population.
    pub async fn open_snapshot(&self, snapshot_id: i64) {
        if !self.begin_op(BackupOp::Detail) {
            return;
        }
        let outcome = match self.nest_api.get_snapshot(snapshot_id).await {
            Ok(reply) => {
                self.state.lock().unwrap().detail = Some(transcribe_detail(snapshot_id, &reply));
                Ok(())
            }
            Err(e) => Err(error_text(DETAIL_ERROR_KEY, e.detail())),
        };
        self.finish_op(outcome.err()).await;
    }

    /// Close the open file list. Local state only — no round trip.
    pub fn close_snapshot_detail(&self) {
        self.state.lock().unwrap().detail = None;
        self.observer.on_changed();
    }
}

// ── Internals ───────────────────────────────────────────────────

impl BackupsMachine {
    fn selected(&self) -> Option<String> {
        self.state.lock().unwrap().selected_folder.clone()
    }

    /// Claim the single-flight slot. `false` ⇒ another op is in flight and the
    /// caller must return without touching state — the one rule that retires
    /// the six apps' ad-hoc partial busy flags.
    fn begin_op(&self, op: BackupOp) -> bool {
        self.try_begin_op(op, false)
    }

    /// [`Self::begin_op`] for a refresh: when the slot is held, record the
    /// re-read for the holder to run instead of dropping it. Checked and
    /// recorded under one lock, so the holder cannot release the slot between
    /// the two and leave the refresh neither run nor pending.
    fn begin_op_or_queue_reload(&self) -> bool {
        self.try_begin_op(BackupOp::Refresh, true)
    }

    fn try_begin_op(&self, op: BackupOp, queue_reload: bool) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.in_progress_op.is_some() {
            if queue_reload {
                state.reload_pending = true;
            }
            return false;
        }
        state.in_progress_op = Some(op);
        // A new gesture clears the previous failure — the banner describes the
        // last completed attempt, never a stale one.
        state.error = None;
        drop(state);
        self.observer.on_changed();
        true
    }

    /// Release the single-flight slot, recording `error` when the op failed.
    /// Logs the failure once here at the producer — the per-app views paint
    /// `snapshot().error` reactively on every tick, so logging there would
    /// re-fire on every repaint (observability.md § Log on the *event*, not the
    /// *paint*).
    fn end_op(state: &mut State, error: Option<LocalizedText>) {
        state.in_progress_op = None;
        if let Some(err) = error {
            tracing::warn!(target: "fauna_backups", "{}", err.log_line());
            state.error = Some(err);
        }
    }

    /// End an op, first loading any folder picked while it held the slot. A pick
    /// is never dropped (§ Snapshot-list shape, *Selection* ruling), and every
    /// op ends here, so a pick made during a check, a detail read or a create is
    /// loaded just as one made during a refresh is. A refresh asked for while
    /// it held the slot is re-read here the same way, without the pick's
    /// clearing of the op's own verdicts.
    async fn finish_op(&self, mut error: Option<LocalizedText>) {
        loop {
            {
                let mut state = self.state.lock().unwrap();
                let reload = std::mem::take(&mut state.reload_pending);
                let reselect = std::mem::take(&mut state.reselect_pending);
                if !reload && !reselect {
                    // Released under the same lock that found nothing pending,
                    // so a pick or refresh recorded after this check finds the
                    // slot free and runs itself.
                    Self::end_op(&mut state, error);
                    break;
                }
                if reselect {
                    // What the op produced described the set it began on; the
                    // pick already cleared these once, and the op may have
                    // written them again since.
                    state.check_result = None;
                    state.prune_preview = None;
                    state.detail = None;
                }
            }
            if let Err(e) = self.load().await {
                error = error.or(Some(e));
            }
        }
        self.observer.on_changed();
    }

    /// The shared read both `refresh` and every successful mutation end with:
    /// the folder list, the selection reconciliation, then the selected set's
    /// snapshots.
    async fn load(&self) -> Result<(), LocalizedText> {
        let wire_sets = self
            .nest_api
            .list_folders()
            .await
            .map_err(|e| error_text(REFRESH_ERROR_KEY, e.detail()))?;
        let folders = transcribe_folders(wire_sets);

        let selected = {
            let mut state = self.state.lock().unwrap();
            // A selection that no longer exists falls back to the default
            // rather than sticking to a name the nest no longer serves.
            let still_present = state
                .selected_folder
                .as_ref()
                .is_some_and(|name| folders.iter().any(|row| &row.name == name));
            if !still_present {
                state.selected_folder = folders.first().map(|row| row.name.clone());
                // The previous set's verdicts do not describe the new one.
                state.check_result = None;
                state.prune_preview = None;
                state.detail = None;
            }
            state.folders = folders;
            state.selected_folder.clone()
        };

        let Some(folder) = selected else {
            // No sets at all — nothing to list, and `last_backed_up` correctly
            // renders "never".
            let mut state = self.state.lock().unwrap();
            state.snapshots.clear();
            return Ok(());
        };

        let wire_rows = self
            .nest_api
            .list_snapshots(&folder)
            .await
            .map_err(|e| error_text(REFRESH_ERROR_KEY, e.detail()))?;

        let mut state = self.state.lock().unwrap();
        if state.selected_folder.as_deref() != Some(folder.as_str()) {
            // A pick landed while these rows were in flight. They describe the
            // outgoing set, and the pick's own load (`finish_op`) fetches the
            // new one — committing them would paint one set's rows under
            // another's name.
            return Ok(());
        }
        state.snapshots = wire_rows.iter().map(transcribe_row).collect();
        // Re-apply this session's check verdict to the freshly-read rows, so a
        // create or delete does not silently reset every row to `Unknown`.
        if let Some(outcome) = state.check_result.clone() {
            apply_integrity(&mut state.snapshots, &outcome);
        }
        // An immediate delete of the OPEN snapshot leaves a detail describing a
        // row that no longer exists — and its per-file download buttons would
        // then act on manifests the nest has dropped. Close it rather than
        // render it: the id, not an index, is what makes this checkable.
        let open_id = state.detail.as_ref().map(|d| d.snapshot_id);
        if open_id.is_some_and(|id| !state.snapshots.iter().any(|row| row.id == id)) {
            state.detail = None;
        }
        Ok(())
    }
}

// ── Transcribes + derivations ───────────────────────────────────

/// The opened snapshot's file list.
///
/// `snapshot_id` is taken from the **request**, not from `reply.id`: the detail
/// is keyed to the row the user clicked, and a reply that disagreed would
/// otherwise silently re-point the open pane at another snapshot. The paths are
/// whatever the custody-wired seam produced — this transcribe never unseals, and
/// must not, or the consumer-wiring rule would have a second implementation.
fn transcribe_detail(snapshot_id: i64, reply: &SnapshotGetReply) -> SnapshotDetail {
    SnapshotDetail {
        snapshot_id,
        files: reply
            .files
            .iter()
            .map(|f| SnapshotFileRow {
                path: f.path.clone(),
                size_bytes: f.size_bytes,
                file_type: f.file_type.clone(),
                manifest_hash: hex::encode(f.manifest_hash.as_ref()),
            })
            .collect(),
    }
}

/// Selector rows: owner-scoped, reserved (`__`) names excluded, name-ordered.
///
/// The filter and the ordering live here rather than on the seam so a fake can
/// feed the machine an unfiltered, unordered list and prove both rulings.
fn transcribe_folders(wire: Vec<WireFolderSummary>) -> Vec<BackupFolderRow> {
    let mut rows: Vec<BackupFolderRow> = wire
        .into_iter()
        .filter(|fs| !is_reserved_folder_name(&fs.name))
        .map(|fs| BackupFolderRow {
            name: fs.name,
            snapshot_count: fs.cached_snapshot_count,
            last_snapshot_at: fs.cached_last_snapshot_at,
        })
        .collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

fn transcribe_row(wire: &SnapshotSummaryRow) -> SnapshotRow {
    SnapshotRow {
        id: wire.id,
        created_at: wire.created_at,
        file_count: wire.file_count,
        total_bytes: wire.total_bytes,
        device_id: wire.device_id.as_ref().map(|b| hex::encode(b.as_ref())),
        tags: wire.tags.clone().unwrap_or_default(),
        state: row_state(wire),
        // Set by `apply_integrity` from the session's check verdict, if any.
        integrity: RowIntegrity::Unknown,
    }
}

/// `soft_deleted` wins over `deletion_pending`: the soft delete has *already
/// happened*, so the cancellable window it may also carry is spent. Reading
/// them the other way round would offer a cancel for a deletion that is done.
fn row_state(wire: &SnapshotSummaryRow) -> SnapshotState {
    if wire.soft_deleted {
        SnapshotState::SoftDeleted {
            purge_after: wire.purge_after,
        }
    } else if wire.deletion_pending {
        SnapshotState::DeletionPending {
            execute_after: wire.execute_after,
        }
    } else {
        SnapshotState::Active
    }
}

fn transcribe_check(reply: &SnapshotCheckReply) -> CheckOutcome {
    let mut implicated: Vec<i64> = reply
        .structured_errors
        .iter()
        .filter_map(|e| e.snapshot_id)
        .collect();
    implicated.sort_unstable();
    implicated.dedup();
    CheckOutcome {
        // The shared predicate, called — never re-derived.
        is_ok: reply.is_ok(),
        snapshots_checked: reply.snapshots_checked,
        files_checked: reply.files_checked,
        manifests_checked: reply.manifests_checked,
        chunks_checked: reply.chunks_checked,
        missing_manifests: reply.missing_manifests,
        missing_chunks: reply.missing_chunks,
        corrupt_manifests: reply.corrupt_manifests,
        implicated,
    }
}

fn transcribe_preview(reply: &SnapshotPruneSetPolicyReply) -> PrunePreview {
    PrunePreview {
        would_prune: reply.pruned,
        remaining: reply.remaining,
        candidates: reply
            .snapshots
            .iter()
            .map(|s| PruneCandidate {
                id: s.id,
                created_at: s.created_at,
                tags: s.tags.clone(),
            })
            .collect(),
        policy_state: PolicyState::from_wire(&reply.policy_state),
    }
}

/// Stamp each row with this session's check verdict.
///
/// A finding with no `snapshot_id` (a set-level fault) implicates no row — it is
/// still visible in [`CheckOutcome`], which is where a set-level problem
/// belongs. Rows a check did not implicate become `CheckedOk`, not `Unknown`:
/// the check covered them and found nothing.
fn apply_integrity(rows: &mut [SnapshotRow], outcome: &CheckOutcome) {
    for row in rows.iter_mut() {
        row.integrity = if outcome.implicated.contains(&row.id) {
            RowIntegrity::Implicated
        } else {
            RowIntegrity::CheckedOk
        };
    }
}
