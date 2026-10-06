//! The `archive-import` wizard's state machine: the client-side steps
//! (Source → Archive → Scope → Confirm), the durable commit at `Start`, and
//! the folder-resident resume `hydrate` picks up
//! (`docs/goal/behavior/archive-import.md` § The wizard and its machine).
//!
//! Mirrors `MailImportMachine` exactly — Snapshot / Action / Machine /
//! `dispatch` / `hydrate`, client-side steps first, the durable commit at
//! `Start` — while depending on no mail crate: the two pieces that machine
//! reaches for through `fauna_client_mail_settings::state`
//! (`set_snapshot_error` and the `dispatch_capturing_error!` tail) are written
//! out inline here.
//!
//! Nothing here talks to a nest or a filesystem directly: every nest-ward call
//! is one [`ArchiveNest`] method and the archive itself is read through the
//! platform's [`ArchiveOpener`] (§ Architectural rules — the app parses and
//! signs; the nest only ever stores the sealed folder).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use fauna_archive::{ArchiveSummary, Category, ExportFormat, Platform};
use fauna_core::data::Timestamp;

use crate::nest::{ArchiveNest, ArchiveNestError, ArchiveOpener, SharedSource};
use crate::run::{category_of_kind, read_all, skip_line};
use crate::snapshot::{
    ArchiveImportAction, ArchiveImportSnapshot, ArchiveImportStatus, ArchiveImportStep,
    ArchiveSourceKind, ArchiveSummaryView, CategoryRow, RunState,
};
use crate::state::{ArchiveMarker, ImportState, STATE_PATH, SUMMARY_PATH};

/// Everything a dispatch can go wrong with. `Nest` is a seam failure,
/// `Archive` a refusal by the parser layer (an unrecognized zip, the HTML
/// export, a malformed member), `InvalidState` an action the current step
/// cannot take.
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error(transparent)]
    Nest(#[from] ArchiveNestError),
    #[error("archive: {0}")]
    Archive(String),
    #[error("{0}")]
    InvalidState(String),
    /// The import's folder state (or marker) holds a value only a newer
    /// version of Fauna can read: it is not resumable by this build, and this
    /// build never rewrites it (`state::ImportState::holds_unknown`).
    #[error("this import was started by a newer version of Fauna — update the app to continue it")]
    NewerImport,
}

/// Record a producer-side error for the page's reactive `error-message`
/// banner (§ Element IDs — `archive-import-error-log` is the run's skip log,
/// a different element): log it once, at the transition that sets it, **and**
/// store it for the next render. The per-app views paint `snapshot().error` on every
/// observer tick, so they cannot log it themselves without re-logging on
/// every repaint (`observability.md` § Log on the *event*, not the *paint*).
/// The message is the same user-facing string the banner shows, so it is
/// redaction-safe. The `fauna_client_mail_settings::state::set_snapshot_error`
/// shape, inlined — this crate deliberately depends on no mail crate.
pub(crate) fn set_snapshot_error(error: &mut Option<String>, message: String) {
    tracing::warn!(target: "fauna_archive_import", "{message}");
    *error = Some(message);
}

/// The run in progress: the archive folder's name, its marker, and the
/// checkpointed `state/import.cbor` a resume continues from (§ Storage — the
/// folder is the session).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunHandle {
    pub folder: String,
    pub state: ImportState,
    pub marker: ArchiveMarker,
}

/// Cooperative stop signalling: the import loop reads this between batches so
/// `Pause` drains what is in flight and `Cancel` keeps what already landed
/// (§ The wizard and its machine, step 5). The `MailImportMachine` shape.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum StopRequest {
    #[default]
    None,
    Pause,
    Cancel,
}

/// One instance per user client. Holds the rendered snapshot and drives both
/// seams; the UI is a read-only observer of [`Self::snapshot`].
///
/// Every field is `pub(crate)`: the run ([`crate::run`]) is a sibling module
/// and drives all of them — the seams, the snapshot, the opened archive and
/// the cooperative stop flag.
pub struct ArchiveImportMachine {
    pub(crate) nest: Arc<dyn ArchiveNest>,
    pub(crate) opener: Arc<dyn ArchiveOpener>,
    /// The owner's identity keypair — every imported post is signed by the
    /// app of the person it belongs to (§ Architectural rules).
    pub(crate) keypair: fauna_core::identity::ActorKeypair,
    pub(crate) inner: Mutex<ArchiveImportSnapshot>,
    /// The opened archive (set by `OpenArchive`, or by a resume whose folder
    /// can serve range reads over its `raw/` copy).
    pub(crate) source: Mutex<Option<SharedSource>>,
    /// The parser's index over [`Self::source`] — what Scope, Confirm and the
    /// run read.
    pub(crate) summary: Mutex<Option<ArchiveSummary>>,
    /// The folder + at-rest state of the run in progress.
    pub(crate) run: Mutex<Option<RunHandle>>,
    /// Polled by the import run between records.
    pub(crate) stop: Mutex<StopRequest>,
    /// Single-flight over the folder: set for as long as one `run_import`
    /// loop is in flight, so a second one is refused rather than left to
    /// author and checkpoint against its own `RunHandle` clone.
    pub(crate) running: AtomicBool,
    /// A one-shot test anchor: pause through the ordinary `Pause` arm once
    /// this many records have settled in the current run (convention 14 —
    /// the e2e restart-resume journey's causal anchor). `None` in production
    /// by construction: the only setter is compiled into test-capable builds.
    pub(crate) test_pause_after: Mutex<Option<u64>>,
}

/// The claim [`index_archive`] is written to protect, pinned at compile time:
/// natively the machine is `Send + Sync`, so the import run can be spawned.
#[cfg(not(target_arch = "wasm32"))]
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ArchiveImportMachine>();
};

impl ArchiveImportMachine {
    pub fn new(
        nest: Arc<dyn ArchiveNest>,
        opener: Arc<dyn ArchiveOpener>,
        keypair: fauna_core::identity::ActorKeypair,
    ) -> Self {
        Self {
            nest,
            opener,
            keypair,
            inner: Mutex::new(ArchiveImportSnapshot::empty()),
            source: Mutex::new(None),
            summary: Mutex::new(None),
            run: Mutex::new(None),
            stop: Mutex::new(StopRequest::None),
            running: AtomicBool::new(false),
            test_pause_after: Mutex::new(None),
        }
    }

    pub fn snapshot(&self) -> ArchiveImportSnapshot {
        fauna_core::clone_locked(&self.inner, |s| s)
    }

    /// Arm the one-shot pause-after-N-records anchor (field doc). Test-capable
    /// builds only — the tui automation surface's `archive_import_pause_after`
    /// command is its one caller.
    ///
    /// N greater than or equal to the records the run still has to settle
    /// never pauses — the run completes and the arm is spent; N = 0 behaves
    /// as 1 (the check runs after a record settles).
    #[cfg(any(debug_assertions, feature = "test-helpers"))]
    pub fn set_test_pause_after_records(&self, records: u64) {
        *self.test_pause_after.lock().expect("test pause mutex") = Some(records);
    }

    /// `true` exactly once, when the armed count is reached; spends the arm.
    pub(crate) fn test_pause_due(&self, settled: u64) -> bool {
        let mut slot = self.test_pause_after.lock().expect("test pause mutex");
        match *slot {
            Some(n) if settled >= n => {
                *slot = None;
                true
            }
            _ => false,
        }
    }

    /// Initial page load — and the `Refresh` action, which is the same thing.
    /// Goes through the same capture tail as [`Self::dispatch`]: a nest that
    /// refuses the very first call must leave a banner behind, not a page
    /// stuck on `Loading` with no explanation.
    pub async fn hydrate(&self) -> Result<(), DispatchError> {
        let result = self.refresh().await;
        self.capture(result)
    }

    pub async fn dispatch(&self, action: ArchiveImportAction) -> Result<(), DispatchError> {
        self.inner.lock().expect("snapshot mutex").error = None;
        if self.apply_client_action(&action) {
            return Ok(());
        }
        let result = match action {
            ArchiveImportAction::Refresh => self.refresh().await,
            ArchiveImportAction::OpenArchive => self.open_archive(),
            ArchiveImportAction::Next => self.next(),
            // The run (`run.rs`). `Start` is the durable commit; the app
            // spawns `run_import` after it, and `Pause`/`Cancel` are read by
            // that loop between records.
            ArchiveImportAction::Start => self.start().await,
            ArchiveImportAction::Pause => self.pause(),
            ArchiveImportAction::Resume => self.resume().await,
            ArchiveImportAction::Cancel => self.cancel().await,
            // The client-side variants were handled above.
            _ => Ok(()),
        };
        self.capture(result)
    }

    /// The `dispatch_capturing_error!` tail, inlined: on a failure record the
    /// message on the shared `error-message` banner and drop status back to
    /// idle, so no failure can leave the page spinning.
    fn capture(&self, result: Result<(), DispatchError>) -> Result<(), DispatchError> {
        if let Err(ref e) = result {
            let mut snap = self.inner.lock().expect("snapshot mutex");
            set_snapshot_error(&mut snap.error, e.to_string());
            snap.status = ArchiveImportStatus::Idle;
        }
        result
    }

    fn set_status(&self, status: ArchiveImportStatus) {
        self.inner.lock().expect("snapshot mutex").status = status;
    }

    /// Page load / resume. Asks the nest whether it can gate at all
    /// (§ Audience mapping — "Gating needs a live tier"), then looks for an
    /// unfinished import in an existing archive folder: the folder *is* the
    /// session, so a restart — on this device or another — resumes from
    /// `state/import.cbor` alone. Nothing unfinished ⇒ the wizard starts at
    /// step 1.
    async fn refresh(&self) -> Result<(), DispatchError> {
        // A run in flight owns the page: it paints progress from its own
        // handle, so a folder scan here would replace the live counts with
        // the last checkpoint's and flip a running import to Paused (with a
        // Resume button whose second loop the run then refuses). Only the
        // nest capability is re-read.
        if self.running.load(Ordering::Acquire) {
            let supports_hidden_tiers = self.nest.supports_hidden_tiers().await?;
            self.inner
                .lock()
                .expect("snapshot mutex")
                .nest_supports_hidden_tiers = Some(supports_hidden_tiers);
            return Ok(());
        }
        self.set_status(ArchiveImportStatus::Loading);
        let supports_hidden_tiers = self.nest.supports_hidden_tiers().await?;
        let folders = self.nest.list_archive_folders().await?;

        let mut resumable = None;
        for folder in folders {
            let Some(bytes) = self.nest.read_file(&folder.folder, STATE_PATH).await? else {
                continue;
            };
            // A folder whose state file does not decode is not a resume
            // candidate; the parser contract's "never fail the whole import
            // on one bad record" applies to the folder list too.
            let Ok(state) = fauna_cbor::decode_strict::<ImportState>(&bytes) else {
                continue;
            };
            if state.is_finished() {
                continue;
            }
            // A newer build's import — a value this build cannot read in the
            // state or the marker — is not resumable here: it never becomes
            // the `RunHandle`, so no resume, cancel or run of this build can
            // rewrite it. Its imported map still feeds dedup
            // (`run::dedup_ids`).
            if state.holds_unknown() || folder.marker.holds_unknown() {
                tracing::warn!(
                    target: "fauna_archive_import",
                    folder = %folder.folder,
                    "an unfinished import written by a newer build is not resumable by this one"
                );
                continue;
            }
            resumable = Some((folder, state));
            break;
        }

        // The Progress screen renders from the parser's own index, which the
        // run wrote into the folder at the end of its model phase: a resume
        // has no in-memory summary to draw its totals and category rows from
        // (§ Storage — the folder is the session).
        let stored_summary = match &resumable {
            Some((folder, _)) => self
                .nest
                .read_file(&folder.folder, SUMMARY_PATH)
                .await?
                .and_then(|bytes| fauna_cbor::decode_strict::<ArchiveSummary>(&bytes).ok()),
            None => None,
        };

        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.nest_supports_hidden_tiers = Some(supports_hidden_tiers);
        match resumable {
            Some((folder, state)) => {
                snap.resume_available = true;
                snap.folder_name = Some(folder.folder.clone());
                // Display only: the archive itself must be handed to the
                // machine again unless the folder can serve range reads.
                snap.archive_path = folder.marker.raw_file_name.clone();
                snap.imported = state.imported.len() as u64;
                snap.skipped = state.skipped.len() as u64;
                // The skip log is rebuilt from the folder, not left empty
                // beside a non-zero count: "12 skipped" over an empty
                // `archive-import-error-log` reads as a bug in the page.
                snap.skip_log = state.skipped.iter().map(skip_line).collect();
                if let Some(summary) = &stored_summary {
                    snap.total = state
                        .scope
                        .categories
                        .iter()
                        .map(|c| summary.counts.get(c))
                        .sum();
                    snap.categories = Category::ALL
                        .iter()
                        .map(|category| CategoryRow {
                            token: category.token().to_string(),
                            count: summary.counts.get(category),
                            importable: is_importable(category),
                            selected: state.scope.categories.contains(category),
                            imported: state
                                .imported
                                .iter()
                                .filter(|r| {
                                    category_of_kind(&r.external_id.kind).as_ref() == Some(category)
                                })
                                .count() as u64,
                            skipped: state
                                .skipped
                                .iter()
                                .filter(|s| &s.category == category)
                                .count() as u64,
                        })
                        .collect();
                }
                snap.step = ArchiveImportStep::Progress;
                snap.run_state = Some(RunState::Paused);
                *self.run.lock().expect("run mutex") = Some(RunHandle {
                    folder: folder.folder,
                    state,
                    marker: folder.marker,
                });
            }
            None => {
                // Nothing resumable ⇒ nothing to point at. A previous
                // hydrate's folder, run state and counts are cleared with the
                // `RunHandle` itself, or a `Refresh` after a run finished
                // would land the wizard on step 1 while the machine still
                // pointed at the finished folder. (The Done screen belongs to
                // the run that just finished, in this process; a fresh hydrate
                // over a finished folder is a fresh wizard.)
                *self.run.lock().expect("run mutex") = None;
                clear_run_fields(&mut snap);
                snap.step = ArchiveImportStep::Source;
            }
        }
        snap.status = ArchiveImportStatus::Idle;
        Ok(())
    }

    /// `Back` from a Progress or Done screen whose run has concluded — a
    /// cancelled or completed import: the folder is an end state on every
    /// device, so the page starts a fresh wizard exactly as a hydrate over a
    /// finished folder does. A paused or errored run stays where it is (it is
    /// resumable), and a running one has no `Back` at all.
    fn reset_wizard(&self) {
        *self.run.lock().expect("run mutex") = None;
        let mut snap = self.inner.lock().expect("snapshot mutex");
        clear_run_fields(&mut snap);
        snap.step = ArchiveImportStep::Source;
        drop(snap);
        self.forget_archive();
    }

    /// Step 2 (§ The wizard and its machine, step 2): open the entered path
    /// through the platform seam, read the central directory and the profile,
    /// and show the summary. A zip no parser recognizes, or an HTML-format
    /// export, is refused here with the message naming what to request
    /// instead. Synchronous: the opener seam is, and no nest call is made.
    fn open_archive(&self) -> Result<(), DispatchError> {
        // Every failure arm below forgets whatever was open before returning:
        // an archive that failed to open must never leave the machine holding
        // the *previous* one behind a snapshot that names the new path.
        let result = self.open_archive_inner();
        if result.is_err() {
            self.forget_archive();
        }
        result
    }

    fn open_archive_inner(&self) -> Result<(), DispatchError> {
        let path = self
            .inner
            .lock()
            .expect("snapshot mutex")
            .archive_path
            .clone();
        if path.is_empty() {
            return Err(DispatchError::InvalidState(
                "choose an archive first".into(),
            ));
        }
        self.set_status(ArchiveImportStatus::Loading);
        let source = self.opener.open(&path)?;
        let archive_bytes = source.len();
        // Handing the archive back to a folder-resident resume: it must be
        // the export the import started from, and the marker says which.
        let resuming_marker = if self.inner.lock().expect("snapshot mutex").resume_available {
            self.run
                .lock()
                .expect("run mutex")
                .as_ref()
                .map(|r| r.marker.clone())
        } else {
            None
        };
        if let Some(marker) = &resuming_marker {
            verify_handed_back(&source, marker)?;
        }
        let (platform, summary) = index_archive(&source)?;

        let mut snap = self.inner.lock().expect("snapshot mutex");
        // The detected platform wins over whatever step 1's picker said: the
        // archive itself is the authority on which platform produced it.
        // (Every parser of this build detects a platform it names; one it
        // could not name would leave the picker as it was.)
        if let Some(kind) = ArchiveSourceKind::from_platform(&platform) {
            snap.source_kind = kind;
        }
        // Handing the archive back to a folder-resident resume is not a walk
        // of the wizard: the scope, the totals and the progress rows are the
        // folder's, already rendered by `refresh`, and the only thing missing
        // was the zip itself. So the page goes straight back to Progress
        // (still paused, awaiting `Resume`) rather than to step 2's summary.
        let resuming = resuming_marker.is_some();
        if resuming {
            snap.step = ArchiveImportStep::Progress;
            snap.run_state = Some(RunState::Paused);
        } else {
            snap.categories = Category::ALL
                .iter()
                .map(|category| {
                    let count = summary.counts.get(category);
                    let importable = is_importable(category);
                    CategoryRow {
                        token: category.token().to_string(),
                        count,
                        importable,
                        selected: importable && count > 0,
                        imported: 0,
                        skipped: 0,
                    }
                })
                .collect();
            snap.summary = Some(summary_view(&summary, archive_bytes));
            snap.step = ArchiveImportStep::Archive;
        }
        snap.status = ArchiveImportStatus::Idle;
        drop(snap);

        *self.source.lock().expect("source mutex") = Some(source);
        *self.summary.lock().expect("summary mutex") = Some(summary);
        Ok(())
    }

    /// `Next` — Source → Archive → Scope → Confirm. Archive advances only
    /// once an archive is indexed; Scope validates the optional date range and
    /// recomputes the Confirm totals from the current scope.
    fn next(&self) -> Result<(), DispatchError> {
        let mut snap = self.inner.lock().expect("snapshot mutex");
        match snap.step {
            ArchiveImportStep::Source => {
                snap.step = ArchiveImportStep::Archive;
            }
            ArchiveImportStep::Archive => {
                if snap.summary.is_none() {
                    return Err(DispatchError::InvalidState(
                        "open an archive before choosing what to import".into(),
                    ));
                }
                snap.step = ArchiveImportStep::Scope;
            }
            ArchiveImportStep::Scope => {
                resolve_date_range(&snap.date_from, &snap.date_to)?;
                let records: u64 = snap
                    .categories
                    .iter()
                    .filter(|c| c.importable && c.selected)
                    .map(|c| c.count)
                    .sum();
                // Everything that leaves this device: the raw zip, kept
                // forever in `raw/`, plus the media re-uploaded through the
                // media pipeline (§ Storage).
                let bytes = snap
                    .summary
                    .as_ref()
                    .map(|s| s.archive_bytes + s.media_bytes)
                    .unwrap_or(0);
                snap.confirm_records = records;
                snap.confirm_bytes = bytes;
                snap.step = ArchiveImportStep::Confirm;
            }
            // Confirm advances via `Start`; Progress and Done have no `Next`.
            _ => {}
        }
        Ok(())
    }

    /// The wizard mutations that need no seam round-trip. Returns `true` when
    /// the action was handled here, so `dispatch` skips the fallible path.
    fn apply_client_action(&self, action: &ArchiveImportAction) -> bool {
        // Handled first, and outside the guard below: it takes two more locks.
        if let ArchiveImportAction::SetArchivePath { value } = action {
            self.set_archive_path(value);
            return true;
        }
        let mut snap = self.inner.lock().expect("snapshot mutex");
        match action {
            ArchiveImportAction::SelectSource { kind } => snap.source_kind = *kind,
            ArchiveImportAction::ToggleCategory { token, selected } => {
                if let Some(row) = snap.categories.iter_mut().find(|c| &c.token == token) {
                    // Model-only categories render unselectable (§ What each
                    // category becomes), so a toggle on one cannot select it.
                    row.selected = *selected && row.importable;
                }
            }
            ArchiveImportAction::SetAudienceMode { mode } => snap.audience_mode = *mode,
            ArchiveImportAction::SetDateFrom { value } => snap.date_from = value.clone(),
            ArchiveImportAction::SetDateTo { value } => snap.date_to = value.clone(),
            ArchiveImportAction::Back => {
                if matches!(
                    snap.step,
                    ArchiveImportStep::Progress | ArchiveImportStep::Done
                ) && matches!(
                    snap.run_state,
                    Some(RunState::Cancelled | RunState::Completed)
                ) {
                    drop(snap);
                    self.reset_wizard();
                    return true;
                }
                let step = prev_step(snap.step);
                snap.step = step;
            }
            _ => return false,
        }
        true
    }

    /// `archive-import-archive-path`. **A path change invalidates the opened
    /// archive**: the machine holds the source and the parser's index, the
    /// snapshot renders their counts, and the run derives `raw_file_name` from
    /// this field — so a path that no longer names what is open would have the
    /// marker say B while `raw/` held A, and Scope/Confirm show A's counts
    /// under B's name. The archive must therefore be re-opened, from step 2.
    ///
    /// The one exception is a folder-resident resume (§ Storage — the folder
    /// is the session): there `archive_path` is display only, carrying the
    /// marker's `raw_file_name` until the user hands the archive back, so
    /// there is nothing open to invalidate.
    fn set_archive_path(&self, value: &str) {
        let mut snap = self.inner.lock().expect("snapshot mutex");
        if snap.archive_path == value {
            return;
        }
        snap.archive_path = value.to_string();
        if snap.resume_available && self.run.lock().expect("run mutex").is_some() {
            return;
        }
        snap.step = ArchiveImportStep::Source;
        drop(snap);
        self.forget_archive();
    }

    /// Drops the opened archive, the parser's index over it, and every
    /// snapshot field derived from them. The one place any of the five is
    /// cleared, so they can never disagree with each other.
    fn forget_archive(&self) {
        let mut snap = self.inner.lock().expect("snapshot mutex");
        snap.summary = None;
        snap.categories.clear();
        snap.confirm_records = 0;
        snap.confirm_bytes = 0;
        drop(snap);
        *self.source.lock().expect("source mutex") = None;
        *self.summary.lock().expect("summary mutex") = None;
    }
}

/// Which categories become Fauna content in phase one (§ What each category
/// becomes): own posts, album media not attached to a post, and own events.
/// The rest are model-only — kept in the folder for phase two's linking, and
/// unselectable in the Scope step.
fn is_importable(category: &Category) -> bool {
    matches!(
        category,
        Category::Posts | Category::Albums | Category::Events
    )
}

/// Every snapshot field that describes a run — cleared together with the
/// `RunHandle`, so the page never names a folder the machine no longer
/// points at.
fn clear_run_fields(snap: &mut ArchiveImportSnapshot) {
    snap.resume_available = false;
    snap.folder_name = None;
    snap.run_state = None;
    snap.total = 0;
    snap.imported = 0;
    snap.skipped = 0;
    snap.errored = 0;
    snap.current_category = None;
    snap.skip_log.clear();
}

/// A resume's handed-back archive against the marker the import wrote: the
/// folder's model files came from ONE export, and media is read from the zip
/// by member path, so a different export with the same paths would attach the
/// wrong bytes to the right posts. Length first (free), then the hash — a
/// whole read of the local file, the price of a resume, paid once. A marker
/// from before either field existed checks nothing.
fn verify_handed_back(source: &SharedSource, marker: &ArchiveMarker) -> Result<(), DispatchError> {
    let mismatch = || {
        DispatchError::Archive(format!(
            "this is not the archive the import started from — hand back {}{}",
            marker.raw_file_name,
            marker
                .raw_len
                .map(|len| format!(" ({len} bytes)"))
                .unwrap_or_default()
        ))
    };
    if let Some(len) = marker.raw_len
        && source.len() != len
    {
        return Err(mismatch());
    }
    if let Some(expected) = &marker.raw_blake3 {
        let bytes = read_all(source)?;
        if blake3::hash(&bytes).to_hex().as_str() != expected {
            return Err(mismatch());
        }
    }
    Ok(())
}

/// `Back` reverses the client-side steps. A running, paused or errored
/// Progress screen has no `Back` (once `Start` has created the folder the
/// commit is durable, and the run is resumable); a concluded one is handled
/// before this is consulted (`ArchiveImportMachine::reset_wizard`).
fn prev_step(step: ArchiveImportStep) -> ArchiveImportStep {
    match step {
        ArchiveImportStep::Confirm => ArchiveImportStep::Scope,
        ArchiveImportStep::Scope => ArchiveImportStep::Archive,
        ArchiveImportStep::Archive => ArchiveImportStep::Source,
        other => other,
    }
}

/// Reads the central directory, detects platform and format, and runs the
/// parser's index. Deliberately its own synchronous function so the borrowing,
/// `!Send` [`fauna_archive::ArchiveReader`] and boxed parser never live across
/// an `await` in [`ArchiveImportMachine::dispatch`] — the machine has to stay
/// `Send` for the run's `tokio::spawn`.
pub(crate) fn index_archive(
    source: &SharedSource,
) -> Result<(Platform, ArchiveSummary), DispatchError> {
    let readable: &dyn fauna_archive::ArchiveSource = &**source;
    let mut reader = fauna_archive::ArchiveReader::open(readable)
        .map_err(|e| DispatchError::Archive(e.to_string()))?;
    let (parser, detected) = fauna_archive::detect(reader.directory())
        .ok_or_else(|| DispatchError::Archive("not a Facebook or Instagram export".into()))?;
    if detected.format == ExportFormat::Html {
        return Err(DispatchError::Archive(format!(
            "this is the HTML export — request the JSON format from {}",
            detected.platform.label()
        )));
    }
    let summary = parser
        .index(&mut reader)
        .map_err(|e| DispatchError::Archive(e.to_string()))?;
    Ok((detected.platform, summary))
}

/// The Archive step's summary card from the parser's index.
pub(crate) fn summary_view(summary: &ArchiveSummary, archive_bytes: u64) -> ArchiveSummaryView {
    ArchiveSummaryView {
        platform_label: summary.platform.label().to_string(),
        owner_display_name: summary.owner.display_name.clone(),
        first_at: summary.date_range.map(|r| r.first.0).unwrap_or(0),
        last_at: summary.date_range.map(|r| r.last.0).unwrap_or(0),
        media_bytes: summary.media_bytes,
        archive_bytes,
        known_audience: summary.audiences.known(),
        unknown_audience: summary.audiences.unknown,
    }
}

/// `YYYY-MM-DD` → midnight UTC of that day, in epoch micros. Strict and
/// zero-padded: the Scope step's date range is a coarse filter over the
/// archive's own timestamps (§ The wizard and its machine, step 3), not a
/// moment, so there is deliberately no time zone, no partial date and no
/// locale. Pre-epoch dates are `None` — `Timestamp` is unsigned.
pub(crate) fn parse_ymd(value: &str) -> Option<Timestamp> {
    // The workspace's one bare-date parser (`caltime`, priority #2) —
    // integer-only, so identical on every target.
    let days = fauna_core::caltime::days_from_ymd(value)?;
    if days < 0 {
        return None;
    }
    Some(Timestamp(days as u64 * 86_400 * 1_000_000))
}

/// The Scope step's `since`/`until` strings → the instants the run compares
/// against: `(inclusive start, EXCLUSIVE end)`. A bare date names a whole UTC
/// day and both ends include theirs, so the end is the midnight after the
/// `until` day (`mail-export.md` § UX shape step 2 owns the rule, which
/// `archive-import.md` step 3 adopts). Empty is unbounded; a malformed or
/// inverted range is an error, never "no range".
pub(crate) fn resolve_date_range(
    since: &str,
    until: &str,
) -> Result<(Option<Timestamp>, Option<Timestamp>), DispatchError> {
    let day = |value: &str| -> Result<Option<Timestamp>, DispatchError> {
        if value.is_empty() {
            return Ok(None);
        }
        parse_ymd(value)
            .map(Some)
            .ok_or_else(|| DispatchError::InvalidState("date must be YYYY-MM-DD".into()))
    };
    let (from, until) = (day(since)?, day(until)?);
    if let (Some(f), Some(u)) = (from, until)
        && f > u
    {
        return Err(DispatchError::InvalidState(
            "the since date is after the until date".into(),
        ));
    }
    Ok((from, until.map(|u| Timestamp(u.0 + 86_400 * 1_000_000))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nest::fakes::{FakeNest, VecOpener};
    use crate::snapshot::AudienceMode;
    use fauna_archive::testing::fixture_zip;

    fn machine_with(zip: Vec<u8>) -> (ArchiveImportMachine, Arc<FakeNest>) {
        let nest = Arc::new(FakeNest::new());
        let mut opener = VecOpener::default();
        opener.insert("/exports/facebook.zip", zip);
        let keypair = fauna_core::identity::ActorKeypair::from_secret([7u8; 32]);
        (
            ArchiveImportMachine::new(nest.clone(), Arc::new(opener), keypair),
            nest,
        )
    }

    #[tokio::test]
    async fn opening_the_fixture_archive_indexes_it_and_lands_on_the_archive_step() {
        let (m, _) = machine_with(fixture_zip("facebook-json"));
        m.hydrate().await.unwrap();
        assert_eq!(m.snapshot().step, ArchiveImportStep::Source);
        m.dispatch(ArchiveImportAction::SetArchivePath {
            value: "/exports/facebook.zip".into(),
        })
        .await
        .unwrap();
        m.dispatch(ArchiveImportAction::OpenArchive).await.unwrap();
        let s = m.snapshot();
        assert_eq!(s.step, ArchiveImportStep::Archive);
        assert_eq!(s.source_kind, ArchiveSourceKind::Facebook);
        let summary = s.summary.expect("summary");
        assert_eq!(summary.owner_display_name, "Test Owner");
        assert_eq!(summary.media_bytes, 62);
        assert_eq!((summary.known_audience, summary.unknown_audience), (3, 2));
        let posts = s.categories.iter().find(|c| c.token == "posts").unwrap();
        assert_eq!(
            (posts.count, posts.importable, posts.selected),
            (4, true, true)
        );
        let friends = s.categories.iter().find(|c| c.token == "friends").unwrap();
        assert_eq!(
            (friends.count, friends.importable, friends.selected),
            (2, false, false)
        );
        assert_eq!(s.error, None);
    }

    #[tokio::test]
    async fn an_html_export_is_refused_naming_the_format_to_request() {
        let (m, _) = machine_with(fixture_zip("facebook-html"));
        m.dispatch(ArchiveImportAction::SetArchivePath {
            value: "/exports/facebook.zip".into(),
        })
        .await
        .unwrap();
        let err = m
            .dispatch(ArchiveImportAction::OpenArchive)
            .await
            .expect_err("HTML is refused");
        assert!(err.to_string().contains("JSON"), "{err}");
        let s = m.snapshot();
        assert_eq!(s.step, ArchiveImportStep::Source);
        assert!(s.error.as_deref().unwrap_or("").contains("JSON"));
    }

    #[tokio::test]
    async fn a_zip_no_parser_recognizes_is_refused() {
        let (m, _) = machine_with(fauna_archive::testing::zip_of(&[("notes/todo.json", b"[]")]).0);
        m.dispatch(ArchiveImportAction::SetArchivePath {
            value: "/exports/facebook.zip".into(),
        })
        .await
        .unwrap();
        assert!(m.dispatch(ArchiveImportAction::OpenArchive).await.is_err());
    }

    #[tokio::test]
    async fn next_and_back_walk_the_client_side_steps_and_confirm_totals_follow_the_scope() {
        let (m, _) = machine_with(fixture_zip("facebook-json"));
        m.dispatch(ArchiveImportAction::SetArchivePath {
            value: "/exports/facebook.zip".into(),
        })
        .await
        .unwrap();
        m.dispatch(ArchiveImportAction::OpenArchive).await.unwrap();
        m.dispatch(ArchiveImportAction::Next).await.unwrap();
        assert_eq!(m.snapshot().step, ArchiveImportStep::Scope);
        m.dispatch(ArchiveImportAction::ToggleCategory {
            token: "events".into(),
            selected: false,
        })
        .await
        .unwrap();
        // A model-only row cannot be selected into the scope at all.
        m.dispatch(ArchiveImportAction::ToggleCategory {
            token: "friends".into(),
            selected: true,
        })
        .await
        .unwrap();
        assert!(
            !m.snapshot()
                .categories
                .iter()
                .find(|c| c.token == "friends")
                .unwrap()
                .selected
        );
        m.dispatch(ArchiveImportAction::SetAudienceMode {
            mode: AudienceMode::OnlyMe,
        })
        .await
        .unwrap();
        m.dispatch(ArchiveImportAction::SetDateFrom {
            value: "2020-01-01".into(),
        })
        .await
        .unwrap();
        m.dispatch(ArchiveImportAction::Next).await.unwrap();
        let s = m.snapshot();
        assert_eq!(s.step, ArchiveImportStep::Confirm);
        assert_eq!(
            s.confirm_records,
            4 + 1,
            "posts + albums; events deselected"
        );
        assert_eq!(
            s.confirm_bytes,
            s.summary.as_ref().unwrap().archive_bytes + 62
        );
        m.dispatch(ArchiveImportAction::Back).await.unwrap();
        assert_eq!(m.snapshot().step, ArchiveImportStep::Scope);
        m.dispatch(ArchiveImportAction::SetDateTo {
            value: "not a date".into(),
        })
        .await
        .unwrap();
        assert!(m.dispatch(ArchiveImportAction::Next).await.is_err());
        assert_eq!(m.snapshot().step, ArchiveImportStep::Scope);

        // An inverted range is refused, never read as "no range".
        m.dispatch(ArchiveImportAction::SetDateTo {
            value: "2019-12-31".into(),
        })
        .await
        .unwrap();
        let err = m.dispatch(ArchiveImportAction::Next).await.unwrap_err();
        assert!(err.to_string().contains("after"), "{err}");
        assert_eq!(m.snapshot().step, ArchiveImportStep::Scope);
        // The same day at both ends is a one-day range, not an inversion.
        m.dispatch(ArchiveImportAction::SetDateTo {
            value: "2020-01-01".into(),
        })
        .await
        .unwrap();
        m.dispatch(ArchiveImportAction::Next).await.unwrap();
        assert_eq!(m.snapshot().step, ArchiveImportStep::Confirm);
    }

    #[test]
    fn a_date_range_admits_both_named_days_whole_and_nothing_beyond() {
        const SEC: u64 = 1_000_000;
        let (from, until) = resolve_date_range("2019-06-01", "2019-06-30").unwrap();
        let scope = crate::state::ImportScope {
            categories: vec![],
            audience_mode: crate::state::StoredAudienceMode::Original,
            date_from: from,
            date_until_exclusive: until,
            extra: Default::default(),
        };
        let june_1 = parse_ymd("2019-06-01").unwrap().0;
        let july_1 = parse_ymd("2019-07-01").unwrap().0;
        assert!(
            !scope.admits_date(Timestamp(june_1 - SEC)),
            "31 May 23:59:59"
        );
        assert!(scope.admits_date(Timestamp(june_1)), "1 June 00:00:00");
        assert!(
            scope.admits_date(Timestamp(july_1 - SEC)),
            "30 June 23:59:59"
        );
        assert!(!scope.admits_date(Timestamp(july_1)), "1 July 00:00:00");

        assert_eq!(resolve_date_range("", "").unwrap(), (None, None));
        assert!(resolve_date_range("2019-06-30", "2019-06-01").is_err());
        assert!(resolve_date_range("2019-6-01", "").is_err());
        assert!(resolve_date_range("", "2019-06-31").is_err());
    }

    #[tokio::test]
    async fn hydrate_surfaces_the_nests_hidden_tier_support_and_a_resumable_run() {
        let (m, nest) = machine_with(fixture_zip("facebook-json"));
        nest.set_supports_hidden_tiers(false);
        m.hydrate().await.unwrap();
        assert_eq!(m.snapshot().nest_supports_hidden_tiers, Some(false));
        // A folder with an unfinished state is offered for resume.
        nest.seed_folder(
            "Facebook archive 2026-09-06",
            &crate::state::ArchiveMarker {
                platform: fauna_archive::Platform::Facebook,
                owner: fauna_archive::ExternalActorRef::new(
                    fauna_archive::Platform::Facebook,
                    Some("test.owner.fixture".into()),
                    "Test Owner",
                ),
                import_id: "00".repeat(32),
                parser_version: fauna_archive::PARSER_VERSION,
                raw_file_name: "facebook.zip".into(),
                created_at: fauna_core::data::Timestamp(1),
                raw_len: None,
                raw_blake3: None,
            },
            &crate::state::ImportState {
                import_id: "00".repeat(32),
                scope: crate::state::ImportScope {
                    categories: vec![fauna_archive::Category::Posts],
                    audience_mode: crate::state::StoredAudienceMode::Original,
                    date_from: None,
                    date_until_exclusive: None,
                    extra: Default::default(),
                },
                phase: crate::state::ImportPhase::Authoring {
                    category: fauna_archive::Category::Posts,
                    next_index: 1,
                },
                imported: vec![],
                skipped: vec![],
                updated_at: fauna_core::data::Timestamp(1),
                in_flight: None,
                extra: Default::default(),
            },
        );
        m.hydrate().await.unwrap();
        let s = m.snapshot();
        assert!(s.resume_available);
        assert_eq!(s.step, ArchiveImportStep::Progress);
        assert_eq!(s.run_state, Some(RunState::Paused));
        assert_eq!(
            s.folder_name.as_deref(),
            Some("Facebook archive 2026-09-06")
        );
    }

    fn fixture_marker() -> ArchiveMarker {
        ArchiveMarker {
            platform: fauna_archive::Platform::Facebook,
            owner: fauna_archive::ExternalActorRef::new(
                fauna_archive::Platform::Facebook,
                Some("test.owner.fixture".into()),
                "Test Owner",
            ),
            import_id: "00".repeat(32),
            parser_version: fauna_archive::PARSER_VERSION,
            raw_file_name: "facebook.zip".into(),
            created_at: fauna_core::data::Timestamp(1),
            raw_len: None,
            raw_blake3: None,
        }
    }

    fn fixture_state(phase: crate::state::ImportPhase) -> ImportState {
        ImportState {
            import_id: "00".repeat(32),
            scope: crate::state::ImportScope {
                categories: vec![Category::Posts],
                audience_mode: crate::state::StoredAudienceMode::Original,
                date_from: None,
                date_until_exclusive: None,
                extra: Default::default(),
            },
            phase,
            imported: vec![],
            skipped: vec![],
            updated_at: fauna_core::data::Timestamp(1),
            in_flight: None,
            extra: Default::default(),
        }
    }

    /// The snapshot must never name one archive while the machine holds
    /// another: everything derived from the open archive goes when the path
    /// changes, and the wizard drops back to step 2's entry.
    #[tokio::test]
    async fn changing_the_archive_path_forgets_the_opened_archive() {
        let (m, _) = machine_with(fixture_zip("facebook-json"));
        m.dispatch(ArchiveImportAction::SetArchivePath {
            value: "/exports/facebook.zip".into(),
        })
        .await
        .unwrap();
        m.dispatch(ArchiveImportAction::OpenArchive).await.unwrap();
        m.dispatch(ArchiveImportAction::Next).await.unwrap();
        assert_eq!(m.snapshot().step, ArchiveImportStep::Scope);

        m.dispatch(ArchiveImportAction::SetArchivePath {
            value: "/exports/other.zip".into(),
        })
        .await
        .unwrap();
        let s = m.snapshot();
        assert_eq!(s.summary, None);
        assert!(s.categories.is_empty());
        assert_eq!((s.confirm_records, s.confirm_bytes), (0, 0));
        assert_eq!(s.step, ArchiveImportStep::Source);

        // Step 1 → 2 still walks, but with no archive indexed the wizard
        // cannot advance past step 2.
        m.dispatch(ArchiveImportAction::Next).await.unwrap();
        assert_eq!(m.snapshot().step, ArchiveImportStep::Archive);
        assert!(m.dispatch(ArchiveImportAction::Next).await.is_err());
        assert_eq!(m.snapshot().step, ArchiveImportStep::Archive);
    }

    #[tokio::test]
    async fn a_failed_open_leaves_no_stale_archive_behind() {
        let (m, _) = machine_with(fixture_zip("facebook-json"));
        m.dispatch(ArchiveImportAction::SetArchivePath {
            value: "/exports/facebook.zip".into(),
        })
        .await
        .unwrap();
        m.dispatch(ArchiveImportAction::OpenArchive).await.unwrap();
        assert!(m.snapshot().summary.is_some());

        m.dispatch(ArchiveImportAction::SetArchivePath {
            value: "/exports/not-here.zip".into(),
        })
        .await
        .unwrap();
        let err = m
            .dispatch(ArchiveImportAction::OpenArchive)
            .await
            .expect_err("the opener has no such path");
        assert!(err.to_string().contains("not-here.zip"), "{err}");
        let s = m.snapshot();
        assert_eq!(s.summary, None);
        assert!(s.categories.is_empty());
        assert!(s.error.is_some());
    }

    /// The resume fields describe a folder that exists right now. A finished
    /// run, or a folder that has gone away, must clear every one of them —
    /// otherwise `Refresh` lands the wizard on step 1 while the machine still
    /// points at the old folder.
    #[tokio::test]
    async fn hydrate_clears_the_resume_fields_when_nothing_is_resumable() {
        let (m, nest) = machine_with(fixture_zip("facebook-json"));
        nest.seed_folder(
            "Facebook archive finished",
            &fixture_marker(),
            &fixture_state(crate::state::ImportPhase::Finished),
        );
        m.hydrate().await.unwrap();
        let s = m.snapshot();
        assert!(!s.resume_available);
        assert_eq!(s.folder_name, None);
        assert_eq!(s.run_state, None);
        assert_eq!(s.step, ArchiveImportStep::Source);

        nest.seed_folder(
            "Facebook archive running",
            &fixture_marker(),
            &fixture_state(crate::state::ImportPhase::Authoring {
                category: Category::Posts,
                next_index: 1,
            }),
        );
        m.hydrate().await.unwrap();
        let s = m.snapshot();
        assert!(s.resume_available);
        assert_eq!(s.folder_name.as_deref(), Some("Facebook archive running"));
        assert_eq!(s.run_state, Some(RunState::Paused));

        nest.clear_folders();
        m.hydrate().await.unwrap();
        let s = m.snapshot();
        assert!(!s.resume_available);
        assert_eq!(s.folder_name, None);
        assert_eq!(s.run_state, None);
        assert_eq!((s.total, s.imported, s.skipped, s.errored), (0, 0, 0, 0));
        assert_eq!(s.current_category, None);
        assert!(s.skip_log.is_empty());
        assert_eq!(s.step, ArchiveImportStep::Source);
    }

    #[test]
    fn parse_ymd_accepts_dates_and_refuses_everything_else() {
        assert_eq!(
            parse_ymd("1970-01-02"),
            Some(fauna_core::data::Timestamp(86_400_000_000))
        );
        assert_eq!(parse_ymd("2020-13-01"), None);
        assert_eq!(parse_ymd("2020-1-1"), None);
        assert_eq!(parse_ymd(""), None);
    }
}
