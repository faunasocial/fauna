//! The **Backups** page — both of its ratified halves: backup-destination
//! management (`docs/goal/ui/backups.md` § Manage backup destinations +
//! § State & data shape → Create / Edit / Remove protocol + § Per-destination
//! status read) and, below it, the **restore** surfaces (§ Restore from backup
//! destination + § Restore history + § Restore divergence).
//!
//! **Why not the third.** `ui/backups.md:4` is `Status: partially-specified`:
//! the destination half and the restore/divergence surfaces are ratified, while
//! the snapshot-list half — the folder selector, the snapshot list,
//! create/delete/prune/check, the immediate-delete modal and the per-file
//! download inside it — stays **draft**, resolved by a snapshot-shape design
//! pass. Building against a draft section is the ask-gate hard stop, so this
//! module takes the two ratified halves exactly and `ui-actual-tui.yaml`
//! declares the snapshot ids absent. When that design pass lands, the snapshot
//! surface drops in between these two sections on the same page.
//!
//! **Where the logic lives: entirely shared, and this is the second direct-Rust
//! consumer.** linux reaches the same three functions with no FFI hop; every
//! other app goes through the native FFI or the wasm twin. This module holds
//! **no** enroll/persist logic (priority #2) — it resolves the candidate and
//! sequences shared calls:
//!
//!   * [`fauna_sync_engine::segment_backup::resolve_and_enroll_destination`]
//!     — add's whole sequence in one call: connects to the typed URL as the
//!     owner's stable cross-nest actor, so the `fauna.auth.handshake` *is*
//!     the reachability + authorization proof, reads `fauna.nest.info`, then
//!     runs the shared enroll (grant → source `nest.info` → destination
//!     `writer_grant.register` → source `destination.register` → the
//!     `fauna.state.backup` write, under this box's id).
//!   * [`fauna_sync_engine::segment_backup::edit_destination`] — edit's whole
//!     sequence in one call too: load, re-verify identity only on a
//!     URL-pointing-at-a-different-nest change (via the dropping wrapper
//!     `resolve_destination` internally — it must not repeat add's
//!     grant/register side effects), then the `edit_backup_destination`
//!     mutate helper through the one `fauna.state.backup` write door.
//!     [`fauna_client_config::deregister_backup_destination`] is remove's
//!     symmetric two-step.
//!   * [`fauna_client_config::read_backup_status`] — the **nest's**
//!     `fauna.backup.status` projection, the one status read all 7 apps call
//!     (`backups.md` § Per-destination status read). tui never had the retired
//!     (now deleted) source-side `destination_status()` model, so it starts
//!     on the repointed shape with nothing to migrate.
//!   * [`SnapshotsClient`] (`fauna-client-snapshots`) — the restore half's whole
//!     call surface: `list` for the local-snapshot picker,
//!     `list_restore_history` + `list_restore_divergence` for the history rows
//!     and their banners, and `restore_message_kind` for the action.
//!     `backups.md:310` puts exactly these four in that crate and keeps only the
//!     render rules — the "local snapshot" fallback, the `(unknown)` MUA, the
//!     "~N writes lost" phrasing, banner-only-when-≥1, and the friction-bar
//!     enable — as per-app glue, which is all this module adds.
//!
//! **The restore half's shapes worth knowing.** The history section paints
//! **flat** — section, list and items each at the top level, with only the
//! divergence banner registered `.within(ids::RESTORE_HISTORY_ITEM, i)`. The shared
//! suite reads the banner with the single-step scope `restore-history-item[i]`,
//! which [`crate::automation::Registry::matches`] resolves wherever that
//! container sits in the banner's ancestor path. ⚠ Until the 2026-08-14
//! descendant ruling (e2e-conventions.md § convention 1) the match was anchored
//! at the root, so an item nested under `restore-history-list` would have pushed
//! that container to the front of the banner's path and every scoped read would
//! have resolved to nothing while the page painted perfectly — this module's
//! flatness was forced by that, and is now merely conventional. The forensic
//! modal is the same inline reveal the two
//! destination dialogs use, and is **cancel-only** (`backups.md:76`) — ui.yaml
//! scopes no id to its close control, so that button paints untagged.
//!
//! **Non-optimistic by construction.** Every mutation re-reads the destination
//! list *and* the status projection before painting (the Bridges-page posture),
//! so a painted row always reflects what the nest persisted, never the
//! keystroke. That is also why [`Outcome`] carries no "added"/"removed" variant:
//! there is exactly one success shape, a fresh `Loaded`.
//!
//! **Row containment.** Each destination's four member elements register
//! `.within(ids::BACKUP_DESTINATION_STATUS_ROW, i)` so a scoped
//! `is_visible("backup-destination-last-upload-time",
//! scope="backup-destination-status-row[0]")` reads exactly that row — a nested
//! indexed list that skips the containment declaration paints fine and reads
//! empty in every scoped query (the A6 nesting lesson; `sync-agent.md:222`).
//! The row element itself is pushed first, carrying the shared display label,
//! because the suite also drives the flat `get_text("backup-destination-status-row",
//! index=i)` and `click("backup-destination-edit-button", index=i)`.
//!
//! **The two dialogs are inline reveals**, not overlays — the same shape linux
//! uses for its `backup-destination-add-modal` / `-remove-confirm-modal` (a
//! `Box` toggled visible rather than an `adw` modal) and the shape a terminal
//! wants anyway. They are painted only while open, which is exactly what
//! ui.yaml's `optional_elements` scope for them means ("present only while
//! open").
//!
//! **Errors bridge onto the page `error-message`** (`backups.md:284` — "Errors
//! (unreachable nest, identity refusal) surface in the page `error-message`"),
//! i.e. `App::errors[Page::Backups]`, and the dialog stays open on failure so
//! the user can fix the URL. linux keeps a section-local error label as well;
//! tui has one error line per page, which is the goal doc's own wording.
//!
//! The async split is the page-module contract's (`apps/tui.md` § The
//! page-module contract): [`apply_local`] lands the synchronous half and hands
//! back an [`Op`]; the agent's click path awaits the op and folds its
//! [`Outcome`] before replying, while the keyboard path spawns it.

use fauna_ui_ids as ids;
use std::collections::BTreeMap;
use std::sync::Arc;

use fauna_backups_machine::{
    BackupOp, BackupsMachine, BackupsObserver, BackupsSnapshot, PolicyState, SnapshotState,
};
use fauna_client::NestClient;
use fauna_client_backup::audit::{DestinationAuditRecord, run_audit_pass};
use fauna_client_config::{
    BackupStateStore, CustodianEnrollment, deregister_backup_destination, enroll_client_custodian,
    keep_backup_destination_at_rest, load_backup_state, load_backup_state_refiled,
    read_backup_status, reenroll_custodian_after_reseed,
};
use fauna_client_snapshots::SnapshotsClient;
use fauna_client_snapshots::filesync::{
    RestoreDivergenceRow, RestoreHistoryRow, SnapshotSummaryRow,
};
use fauna_core::data::{BackupDestination, DestinationUnattestedMark};
use fauna_core::identity::ActorKeypair;
use fauna_core::secret::SecretArray32;
use fauna_i18n::strings::backups as t;
use fauna_protocol::backup::BackupDestinationStatusItem;

use crate::app::App;
use crate::element::{Element, Field, Gesture, SelectTarget};
use crate::pages::Page;

/// What every destination-list op needs: the source nest, the owner secret
/// and the account store's backup-state door.
type DestSession = (Arc<NestClient>, [u8; 32], Arc<dyn BackupStateStore>);

/// The `restore-kind-checkbox` set in paint order — `backups.md:61` ("two
/// checkboxes, `mail`, `calendar`"). The token is the wire kind, the second
/// field the localized label; `conv` is deliberately absent until the
/// conversations rollout's Plan 9 lands (`backups.md:68`), and widening the set
/// is a ui.yaml change made then, with approval.
const RESTORE_KINDS: [(&str, &str); 2] = [
    ("mail", t::RESTORE_KINDS_MAIL),
    ("calendar", t::RESTORE_KINDS_CALENDAR),
];

// ── State ────────────────────────────────────────────────────────────────────

/// Which dialog the add/edit form is currently serving. `None` on the state
/// means the form is closed, so the three `backup-destination-add-modal`
/// elements do not paint at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormMode {
    /// `backup-destination-add-button` opened it — confirm runs the enroll
    /// sequence.
    Add,
    /// `backup-destination-edit-button[i]` opened it, prefilled from that row —
    /// confirm renames / re-points the destination with this `destination_id`.
    Edit(String),
}

/// [`BackupsObserver`] is a construction requirement only. `BackupsMachine`
/// notifies it synchronously after every gesture, but this page re-reads
/// `machine.snapshot()` right after each awaited [`Op`] instead — the same
/// awaited-hydrate shape the destination half above and the Settings sub-pages
/// use, rather than linux's push-observer render loop.
struct NoopObserver;

impl BackupsObserver for NoopObserver {
    fn on_changed(&self) {}
}

/// Which destination kind the add dialog is composing — the two implemented
/// arms of `backup-destination-kind-select` ("Another nest" / "This device").
///
/// The S3 kind is ratified but deferred to its own design pass
/// (`backups.md` § Second destination kind), so it is deliberately absent rather
/// than present-and-disabled: an option that cannot be chosen teaches the user
/// nothing and would need its own "not yet" copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DestinationKindChoice {
    /// A peer nest the owner administers — the v1 kind, and the default because
    /// it is the one that actually satisfies "off-site".
    #[default]
    Nest,
    /// This device, as a client custodian pulling a sealed replica.
    ClientDevice,
}

impl DestinationKindChoice {
    /// The wire discriminator, from shared Rust — never a local string literal.
    fn wire(self) -> &'static str {
        match self {
            Self::Nest => fauna_core::data::DESTINATION_KIND_NEST,
            Self::ClientDevice => fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE,
        }
    }

    /// The option text the select paints — the same shared badge label the
    /// status rows use, so the option a user picks and the badge they get back
    /// cannot drift apart.
    fn label(self) -> String {
        crate::format::backup_destination_kind(self.wire())
    }

    /// Both options, in paint order.
    fn all() -> [Self; 2] {
        [Self::Nest, Self::ClientDevice]
    }

    /// Resolve a picked option label back to the choice. `select(id, value)`
    /// carries the painted text, so this is the inverse of [`Self::label`].
    fn from_label(label: &str) -> Option<Self> {
        Self::all().into_iter().find(|c| c.label() == label)
    }

    /// Resolve a `select(id, value)` target — **the wire discriminator**, which
    /// is what the other apps take and what the shared action layer sends.
    ///
    /// This select used to be keyed on the painted LABEL, and that was a real
    /// cross-app divergence rather than a tui detail: every other app resolves
    /// `"client-device"`, so `actions/backups.py::select_destination_kind`
    /// (whose docstring already says "the wire value, never the localized
    /// label") could not drive tui at all. It went unseen because the test that
    /// exercises it is marked macos/ios/windows only — stale since all 7 apps
    /// landed the UI leg — so the default `[tui]` app set never ran it. Found
    /// 2026-08-21 by the first run of the client-custodian tier_3 proof, which
    /// failed on its very first gesture.
    ///
    /// The label arm is kept as a fallback because tui's own keyboard path
    /// cycles the painted options, and because a label that happens to be
    /// unambiguous costs nothing to accept — but the wire value is the contract,
    /// and it is what survives translation.
    fn from_select_value(value: &str) -> Option<Self> {
        Self::all()
            .into_iter()
            .find(|c| c.wire() == value)
            .or_else(|| Self::from_label(value))
    }
}

/// Which `restore-kind-checkbox` boxes are ticked, parallel to [`RESTORE_KINDS`].
///
/// A newtype rather than a bare `[bool; 2]` so "both checked" (`backups.md:61`)
/// is the type's own [`Default`] and [`BackupsState`] keeps its derive — the
/// default lives in one place instead of being re-asserted at every
/// construction site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreKinds([bool; RESTORE_KINDS.len()]);

impl Default for RestoreKinds {
    fn default() -> Self {
        Self([true; RESTORE_KINDS.len()])
    }
}

impl RestoreKinds {
    fn checked(self, index: usize) -> bool {
        self.0.get(index).copied().unwrap_or(false)
    }

    fn toggle(&mut self, index: usize) {
        if let Some(slot) = self.0.get_mut(index) {
            *slot = !*slot;
        }
    }
}

/// The `restore-progress` step (`backups.md:64`).
///
/// The doc's finer-grained chunk/manifest steps are a destination-restore
/// concern; the local path this page drives is one awaited round-trip, so it has
/// exactly these three states and each maps to an existing shared string.
/// The re-seed gesture (`ui/backups.md` § Restore after losing the nest).
///
/// `Running` paints `backup-destination-reseed-result` with the running line
/// and disarms the button, so a second press cannot land while the agent is
/// still working; `Done` keeps the last result on screen until the next run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ReseedView {
    #[default]
    Closed,
    /// `backup-destination-reseed-confirm-modal` is open.
    Confirming,
    Running,
    Done(fauna_client_backup::reseed::ReseedOutcome),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RestoreProgress {
    #[default]
    Idle,
    Running,
    Done,
}

impl RestoreProgress {
    fn text(self) -> &'static str {
        match self {
            RestoreProgress::Idle => t::RESTORE_PROGRESS_IDLE,
            RestoreProgress::Running => t::RESTORE_PROGRESS_RUNNING,
            RestoreProgress::Done => t::RESTORE_PROGRESS_DONE,
        }
    }
}

/// What a completed restore reported that the page must keep — the reply is
/// the only carrier of its `config_present` advisory (`ui/backups.md`
/// § Restore from backup destination, `restore-warning`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Restored {
    pub config_present: bool,
}

/// The restore half's three reads, landed together by one [`Op::Refresh`].
///
/// Grouped rather than spread across [`Outcome::Loaded`] so the destination half
/// and the restore half each stay one field of the one success shape.
#[derive(Debug, Default)]
pub struct RestoreData {
    /// `restore-snapshot-select` options, newest first.
    pub snapshots: Vec<SnapshotSummaryRow>,
    /// `restore-history-item` rows, newest first.
    pub history: Vec<RestoreHistoryRow>,
    /// Divergence rows per `snapshot_id`.
    pub divergence: BTreeMap<i64, Vec<RestoreDivergenceRow>>,
}

/// The Backups page's state, hung off [`App`].
///
/// Deliberately mirrors this box's `fauna.state.backup` destination list plus
/// the nest's status projection rather than inventing a page-local model: the destination row IS
/// the shared [`BackupDestination`] (`backups.md` § Destination data model), the
/// status row IS the wire [`BackupDestinationStatusItem`], and the restore rows
/// ARE the wire [`RestoreHistoryRow`] / [`RestoreDivergenceRow`].
#[derive(Default)]
pub struct BackupsState {
    /// The live WS-RPC channel to the **source** nest, installed at the post-auth
    /// hook. `None` pre-auth — every reader degrades gracefully.
    pub nest: Option<Arc<NestClient>>,
    /// The owner's identity seed. Both the `ActorKeypair` the config seal needs
    /// and the `NestBackupKey` the grant step derives come from it, so the page
    /// carries it in a zeroizing newtype rather than as a bare array.
    pub secret: Option<SecretArray32>,
    /// The account plane's `fauna.state.backup` door
    /// (`settings::nests::backup_door`) — where the destination list and its
    /// review marks live, keyed per source box. `None` pre-auth, like
    /// [`Self::nest`]; every destination op carries a clone of it.
    pub store: Option<Arc<dyn BackupStateStore>>,
    /// The configured destinations, freshest-read-wins. Row index `i` here is
    /// the `backup-destination-status-row[i]` occurrence index.
    pub destinations: Vec<BackupDestination>,
    /// The post-succession review marks for those destinations, read from the
    /// same `fauna.state.backup` row in the same pass — `BackupState::marks`.
    /// Carried beside the rows rather than read off them because the verdict
    /// lives on its own merge plane (that field's docs say why); the page asks
    /// `DestinationUnattestedMark::row_is_raised` and never the row alone.
    pub unattested_destination_marks: Vec<DestinationUnattestedMark>,
    /// The nest's per-destination projection, keyed by `destination_id`. A
    /// destination with no entry renders the not-yet-backed-up baseline
    /// ("never" / "0 queued") rather than nothing.
    pub statuses: BTreeMap<String, BackupDestinationStatusItem>,
    /// This client's own audit verdicts, ordered like [`Self::destinations`]
    /// (the shared `merge_outcomes` guarantees that), driving
    /// `backup-destination-last-audit-time` and the `backup-audit-alert`
    /// banners.
    ///
    /// **The MERGED picture, never one pass's raw outcomes.** A destination
    /// inside its 24 h debounce contributes nothing to a pass, so rendering
    /// `audit_all`'s return would blank a standing alert on most passes —
    /// precisely for the destination that is broken. `run_audit_pass` hands back
    /// the merge, and this field holds exactly what it returned.
    pub audit: Vec<DestinationAuditRecord>,
    /// Which dialog the `backup-destination-add-modal` is serving, or `None`
    /// when it is closed.
    pub form: Option<FormMode>,
    /// The `destination_id` the `backup-destination-remove-confirm-modal` is
    /// armed for, or `None` when it is closed.
    pub removing: Option<String>,
    /// `backup-destination-url-input` buffer.
    pub url_input: String,
    /// `backup-destination-name-input` buffer.
    pub name_input: String,
    /// Which kind the open add dialog is composing — `backup-destination-kind-select`.
    ///
    /// Add-only: [`FormMode::Edit`] paints the select disabled at the row's own
    /// kind, because the kind is not an editable property. Re-pointing a nest row
    /// at "this device" would keep a `destination_id` whose registry row, status
    /// projection and (for a nest) writer grant all describe the other kind —
    /// `backups.md` § Create / edit / remove restricts edit to the display name
    /// and URL for exactly that reason.
    pub kind_input: DestinationKindChoice,
    /// `backup-destination-capacity-input` buffer — the user-typed cap, read
    /// through the shared `parse_byte_size` at confirm. Client-device kind only.
    pub capacity_input: String,

    /// Bytes this device is holding in a sealed custodian store that **no**
    /// destination row claims — `Some` exactly when `backup-orphaned-store-row`
    /// paints (`backups.md` § Manage backup destinations → *Reclaim this
    /// device's copy*). Refreshed with every whole-page read.
    ///
    /// The **verdict**, not the measurement, and reached in the op rather than
    /// here: it needs this device's sync id, which is a SQLite read no render
    /// path may do (the same reason `Op::EnrollCustodian` reads it inside its
    /// runner), and it is the shared
    /// `fauna_core::data::custodian_store_is_orphaned` that decides it — a rule
    /// this page must not re-derive, because the case it exists for (two rows
    /// naming this device) reads as "no assignment" to the obvious
    /// re-derivation and would offer to delete a live custody copy.
    ///
    /// `None` therefore covers three different truths — not orphaned, nothing
    /// held, and no agent to ask — which paint identically. Nothing downstream
    /// may read it as "measured zero".
    pub orphaned_store: Option<u64>,
    /// Whether `backup-reclaim-confirm-modal` is open.
    pub reclaiming: bool,
    /// The `backup-destination-remove-reclaim-checkbox` tick inside the open
    /// remove-confirm dialog: *also* free this device's copy now.
    ///
    /// Deliberately reset by [`Action::OpenRemoveConfirm`] rather than carried:
    /// the opt-in is an intent about *this* removal, and a tick that survived
    /// from a dialog the user cancelled would delete an offline copy nobody
    /// asked about in this gesture.
    pub remove_reclaim: bool,
    /// The re-seed gesture's state (`ui/backups.md` § Restore after losing the
    /// nest): the confirm modal, the running job, and the last result.
    pub reseed: ReseedView,

    // ── the restore half ─────────────────────────────────────────────────────
    /// The local snapshots `restore-snapshot-select` offers — the owner-implicit
    /// message-kind mode of `fauna.filesync.snapshot.list` (`backups.md:310`,
    /// "the local-snapshot picker is populated from
    /// `fauna.filesync.snapshot.list`").
    pub snapshots: Vec<SnapshotSummaryRow>,
    /// Which [`Self::snapshots`] row the picker has selected. An index rather
    /// than an id so the empty list needs no "selected nothing" sentinel; every
    /// reader goes through [`Self::selected_snapshot_id`].
    pub selected_snapshot: usize,
    /// Which [`Self::destinations`] row `restore-source-select` has selected.
    pub selected_source: usize,
    /// `restore-confirm-input` buffer — the re-typed snapshot id.
    pub restore_confirm: String,
    /// The `restore-kind-checkbox` set.
    pub restore_kinds: RestoreKinds,
    /// `restore-history-item` rows, newest first.
    pub restore_history: Vec<RestoreHistoryRow>,
    /// Divergence rows per `snapshot_id`. A snapshot with an empty entry paints
    /// no banner at all (`backups.md:75` — "only renders on rows where ≥1
    /// `bridge_restore_divergence` row exists").
    pub divergence: BTreeMap<i64, Vec<RestoreDivergenceRow>>,
    /// Which [`Self::restore_history`] row's `restore-divergence-details-modal`
    /// is open, or `None` when closed.
    pub divergence_modal: Option<usize>,
    /// The `restore-progress` step.
    pub restore_progress: RestoreProgress,
    /// Whether `restore-warning` paints: the last restore's reply said
    /// `config_present == false`. The reply is the advisory's only carrier, so
    /// this is the one copy — cleared by the next read that is not a restore.
    pub restore_config_absent: bool,

    // ── the snapshot half (`ui/backups.md` § Snapshot-list shape) ────────────
    /// The shared page machine. `None` pre-login. Built once at the post-auth
    /// hook like the Settings sub-pages' machines: construction is sync and
    /// cheap, and holding it as an `Arc` is what lets an [`Op`] carry it across
    /// a `tokio::spawn`.
    pub machine: Option<Arc<BackupsMachine>>,
    /// The account's folder-key custody, installed at the post-auth hook. The
    /// re-seed's target pre-create mints each restored folder set's nonce into
    /// it (`writer-signed-change-records.md` ruling (7)(a)(i)).
    pub folder_keys: Option<Arc<dyn fauna_client_folders::FolderKeyStore>>,
    /// The last machine snapshot this page painted. `None` until the nav-edge
    /// hydrate folds one — the snapshot half then renders its empty states
    /// rather than stale rows.
    ///
    /// ⚠ This is a *cache of the render*, never a second source of truth: every
    /// gesture re-reads `machine.snapshot()` and replaces it wholesale. Nothing
    /// on this page edits it in place.
    pub snap: Option<BackupsSnapshot>,
    /// Which snapshot the `immediate-delete-confirm-modal` is armed for, or
    /// `None` when it is closed. The id, not a row index: the list underneath
    /// can be re-read while the modal is open.
    pub immediate_delete: Option<i64>,
    /// `immediate-delete-confirm-input` buffer — the re-typed snapshot id.
    pub immediate_confirm: String,
    /// `immediate-delete-acknowledge-input` buffer — the exact ack phrase.
    pub immediate_ack: String,
}

impl BackupsState {
    /// The `(nest, secret)` pair every op needs, or `None` pre-auth.
    fn session(&self) -> Option<(Arc<NestClient>, [u8; 32])> {
        Some((self.nest.clone()?, self.secret.as_ref()?.to_array()))
    }

    /// [`Self::session`] plus the backup-state door — what every op that reads
    /// or writes the destination list needs. `None` pre-auth.
    fn dest_session(&self) -> Option<DestSession> {
        let (nest, secret) = self.session()?;
        Some((nest, secret, self.store.clone()?))
    }

    /// The machine, or `None` pre-auth. Every snapshot-half gesture goes
    /// through this — a page with no machine renders its empty states and
    /// dispatches nothing, which is the honest pre-login shape.
    fn machine(&self) -> Option<Arc<BackupsMachine>> {
        self.machine.clone()
    }

    /// The machine snapshot to render, or the empty default before the first
    /// hydrate. Never `Option`-branched at every paint site.
    fn snap(&self) -> BackupsSnapshot {
        self.snap.clone().unwrap_or_default()
    }

    /// The snapshot id at list index `i`, for the indexed row gestures.
    fn snapshot_row_id(&self, index: usize) -> Option<i64> {
        self.snap.as_ref()?.snapshots.get(index).map(|row| row.id)
    }

    fn destination(&self, index: usize) -> Option<&BackupDestination> {
        self.destinations.get(index)
    }

    /// Is the row the remove-confirm dialog is armed for a **client device**? —
    /// the `backup-destination-remove-reclaim-checkbox` render rule
    /// (`backups.md` § Remove: the opt-in is client-device kind only).
    ///
    /// Answered through the shared `row_is_a_client_device` rather than a local
    /// `kind == "client-device"`, so an `Inert` row — a `client-device` row with
    /// no device id, which nothing can drive and which therefore owns no local
    /// store — does not offer to free a copy that does not exist.
    fn removing_a_client_device(&self) -> bool {
        let Some(id) = self.removing.as_deref() else {
            return false;
        };
        self.destinations
            .iter()
            .find(|d| d.destination_id == id)
            .is_some_and(|d| fauna_core::data::row_is_a_client_device(d.custodian_row()))
    }

    /// The selected snapshot's id, stringified — both the friction bar's match
    /// target and the restore call's `confirm_id`.
    fn selected_snapshot_id(&self) -> Option<String> {
        self.snapshots
            .get(self.selected_snapshot)
            .map(|row| row.id.to_string())
    }

    /// Whether `restore-confirm-button` is armed: the typed text must equal the
    /// **selected** snapshot's id, and no restore may already be in flight —
    /// both halves of `backups.md:63`.
    fn restore_armed(&self) -> bool {
        self.restore_progress != RestoreProgress::Running
            && self
                .selected_snapshot_id()
                .is_some_and(|id| id == self.restore_confirm)
    }
}

/// Build the page state at the post-auth hook. Deliberately does **not** kick a
/// fetch: entering the tab is the trigger ([`nav_enter_op`], awaited on the nav
/// edge), so a login never pays for a page the user may not open — the Nostr /
/// Bridges / Media posture.
pub fn init(
    nest: Arc<NestClient>,
    secret: [u8; 32],
    folder_keys: std::sync::Arc<dyn fauna_client_folders::FolderKeyStore>,
    store: Arc<dyn BackupStateStore>,
    succession_predecessors: &[fauna_core::crypto::BackupKey],
) -> BackupsState {
    let machine = build_machine(
        Arc::clone(&nest),
        secret,
        Arc::clone(&folder_keys),
        succession_predecessors,
    );
    BackupsState {
        nest: Some(nest),
        secret: Some(SecretArray32::from(secret)),
        store: Some(store),
        machine: Some(machine),
        folder_keys: Some(folder_keys),
        ..BackupsState::default()
    }
}

/// Build the shared snapshot-half machine over `nest`'s authenticated
/// connection, with the reader's **label custody** wired in.
///
/// Custody is a construction input, not an optional extra: `snapshot-detail-files`
/// is a sealed-plane read, so a keyless machine renders a sealed set's file list
/// empty (`behavior/path-sealing.md` § THE CONSUMER-WIRING RULE). The seam holds
/// it — this page never sees a key.
///
/// `succession_predecessors` are read candidates for rows a succession re-pointed
/// but did not re-seal; never a seal root (`LabelCustody::with_predecessors`),
/// which is the same split the Devices sub-page's custody uses.
fn build_machine(
    nest: Arc<NestClient>,
    secret: [u8; 32],
    folder_keys: std::sync::Arc<dyn fauna_client_folders::FolderKeyStore>,
    succession_predecessors: &[fauna_core::crypto::BackupKey],
) -> Arc<BackupsMachine> {
    let resolver: Arc<dyn fauna_core::folder_keys::FolderKeyResolver> = Arc::new(
        fauna_client_folders::NestFolderKeyResolver::new(Arc::clone(&nest), folder_keys),
    );
    let custody = fauna_core::label_custody::LabelCustody::new(
        Some(resolver),
        Some(fauna_core::crypto::BackupKey::derive(&secret)),
    )
    .with_predecessors(succession_predecessors.to_vec());
    let observer: Arc<dyn BackupsObserver> = Arc::new(NoopObserver);
    fauna_backups_machine::build_backups_machine(nest, observer, custody)
}

/// The refetch entering this tab implies — the page's leg of the one nav-edge
/// hook (`crate::app::on_nav_enter`). Returns the op; the caller runs it (the
/// agent awaits, the keyboard spawns).
pub fn nav_enter_op(
    state: &BackupsState,
    custodian: crate::sync_agent::CustodianStoreHandle,
) -> Option<Op> {
    let (nest, secret, store) = state.dest_session()?;
    Some(Op::Refresh {
        nest,
        secret,
        store,
        // One op for the WHOLE page — both halves in one nav edge, rather than
        // racing two independent fetches (the module's stated posture).
        machine: state.machine(),
        custodian,
    })
}

/// One audit pass on its own — the op behind the `backup_audit_run_now` agent
/// command. `None` pre-auth, which is the one honest reason the command cannot
/// be honoured (testing.md convention 11: the caller then fails loudly on
/// `error-message` rather than acking a no-op).
pub fn audit_op(
    state: &BackupsState,
    custodian: crate::sync_agent::CustodianStoreHandle,
) -> Option<Op> {
    let (nest, secret, store) = state.dest_session()?;
    Some(Op::Audit {
        nest,
        secret,
        store,
        custodian,
    })
}

// ── Field access ─────────────────────────────────────────────────────────────

/// A Backups-page editable field. Both are local dialog buffers committed by
/// `backup-destination-add-confirm-button`, never per keystroke.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BackupsField {
    /// `backup-destination-url-input`.
    Url,
    /// `backup-destination-name-input`.
    Name,
    /// `backup-destination-capacity-input` — the client-device kind's only knob.
    Capacity,
    /// `restore-confirm-input` — the re-typed snapshot id. Unlike the two dialog
    /// buffers this one is read on **every** keystroke, because the friction bar
    /// it arms is repainted from it (`backups.md:62`).
    RestoreConfirm,
    /// `immediate-delete-confirm-input` — the re-typed snapshot id. Read every
    /// keystroke, like [`Self::RestoreConfirm`]: the friction bar it arms is
    /// repainted from it (§ Architectural rules, rule 4).
    ImmediateConfirm,
    /// `immediate-delete-acknowledge-input` — the exact ack phrase, same
    /// per-keystroke reason.
    ImmediateAck,
}

pub fn field(state: &BackupsState, field: &BackupsField) -> String {
    match field {
        BackupsField::Url => state.url_input.clone(),
        BackupsField::Name => state.name_input.clone(),
        BackupsField::Capacity => state.capacity_input.clone(),
        BackupsField::RestoreConfirm => state.restore_confirm.clone(),
        BackupsField::ImmediateConfirm => state.immediate_confirm.clone(),
        BackupsField::ImmediateAck => state.immediate_ack.clone(),
    }
}

pub fn set_field(state: &mut BackupsState, field: BackupsField, value: String) {
    match field {
        BackupsField::Url => state.url_input = value,
        BackupsField::Name => state.name_input = value,
        BackupsField::Capacity => state.capacity_input = value,
        BackupsField::RestoreConfirm => state.restore_confirm = value,
        BackupsField::ImmediateConfirm => state.immediate_confirm = value,
        BackupsField::ImmediateAck => state.immediate_ack = value,
    }
}

// ── Gestures ─────────────────────────────────────────────────────────────────

/// Which ceremony `backup-destination-add-confirm-button` runs — the
/// discriminant [`Action::SubmitForm`] carries.
///
/// One dialog paints all three, and they are not variations of one call: adding
/// a peer nest runs the shared five-step enroll, adding this device runs the
/// three-step custodian enroll, and editing rewrites one row of the owner's own
/// `fauna.state.backup` plane. The paint knows which, so the gesture says which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitTarget {
    /// The add dialog with the `Nest` kind selected.
    AddNest,
    /// The add dialog with the `This device` kind selected.
    AddCustodian,
    /// The edit dialog, reopened on an existing row.
    Edit,
}

/// A gesture on the Backups page. Each is either a local dialog transition or
/// one shared-Rust sequence — never a navigation decision of its own.
#[derive(Debug, Clone)]
pub enum Action {
    /// `backup-destination-add-button` — open the empty add dialog.
    OpenAddForm,
    /// `backup-destination-edit-button[i]` — open the dialog prefilled from that
    /// row (`backups.md` § Edit: "reopens the dialog to change the display name
    /// or URL").
    OpenEditForm { index: usize },
    /// `backup-destination-add-cancel-button` — close without side effect.
    CancelForm,
    /// `backup-destination-add-confirm-button` — run add or edit.
    ///
    /// Which of the three ceremonies it runs is **carried**, read off the same
    /// paint that produced the button, rather than re-derived from
    /// `BackupsState::form` at press time. The reason is the offline gate: the
    /// three end in different wire kinds of different *classes* — adding needs
    /// a nest, editing is a `fauna.state.backup` write that does not — so a single
    /// undiscriminated variant could only answer `None`, leaving a control live
    /// that cannot finish (`admin::Action::IssueDnsCert`'s `single_issue`, the
    /// same shape for the same reason).
    SubmitForm(SubmitTarget),
    /// `backup-destination-remove-button[i]` — arm the remove-confirm dialog.
    OpenRemoveConfirm { index: usize },
    /// `backup-destination-keep-button[i]` — the owner adjudicates a row the
    /// post-succession aftermath carried across (`succession-aftermath.md`
    /// § Adjudicating what the aftermath carries across). Deliberately **no**
    /// confirm dialog, unlike `OpenRemoveConfirm`: Keep is non-destructive and
    /// re-decidable (the row stays removable forever after), so a confirm would
    /// be friction with no payoff — the same rule the unattested-member roster
    /// follows.
    KeepDestination { index: usize },
    /// `backup-destination-remove-cancel-button` — close without side effect.
    CancelRemove,
    /// `backup-destination-remove-confirm-button` — run the shared deregister.
    ConfirmRemove,
    /// `backup-destination-kind-select` — pick which kind the add dialog is
    /// composing. Carries the painted option label, which is what
    /// `select(id, value)` takes.
    SelectDestinationKind(String),

    /// `backup-destination-remove-reclaim-checkbox` — tick the opt-in inside
    /// the open remove-confirm dialog (client-device rows only).
    ToggleRemoveReclaim,
    /// `backup-destination-reclaim-button[i]` — arm the reclaim-confirm dialog
    /// for this device's orphaned sealed store.
    OpenReclaimConfirm,
    /// `backup-reclaim-cancel-button` — close it without side effect.
    CancelReclaim,
    /// `backup-reclaim-confirm-button` — free this device's whole sealed store.
    ConfirmReclaim,

    /// `backup-destination-reseed-button[i]` — arm the re-seed confirm. Every
    /// placement runs the same leg (this device's own store), so the press
    /// carries no row.
    OpenReseedConfirm,
    /// `backup-destination-reseed-cancel-button` — close it without side effect.
    CancelReseed,
    /// `backup-destination-reseed-confirm-button` — run the re-seed ceremony.
    ConfirmReseed,

    // ── the restore half ─────────────────────────────────────────────────────
    /// `restore-source-select` — pick which configured destination holds the
    /// chunks. Carries the shared destination *label*, which is what the picker
    /// paints and what `select(id, value)` takes.
    SelectRestoreSource(String),
    /// `restore-snapshot-select` — pick which local snapshot to restore.
    SelectSnapshot(String),

    // ── the snapshot half (`ui/backups.md` § Snapshot-list shape) ────────────
    /// `backup-folder-selector` — pick whose snapshots the list shows. Carries
    /// the set NAME (the raw-value picker contract).
    SelectFolder(String),
    /// `snapshot-create-button` — take a manual, untagged snapshot.
    CreateSnapshot,
    /// `snapshot-delete-button[i]` — queue the soft delete for that row.
    DeleteSnapshot { index: usize },
    /// `snapshot-undelete-button[i]` — recover that row out of `SoftDeleted`
    /// before its `purge_after`. Painted only on a soft-deleted row; the machine
    /// refuses any other state, so the render is an affordance rule and not the
    /// enforcement (§ *Soft-deleted rows* ruling).
    UndeleteSnapshot { index: usize },
    /// `snapshot-immediate-delete-button[i]` — open the friction-bar modal. Never
    /// a one-click delete (§ Architectural rules, rule 4).
    OpenImmediateDelete { index: usize },
    /// `immediate-delete-cancel-button` — close the modal without side effect.
    CancelImmediateDelete,
    /// `immediate-delete-confirm-button` — issue the hard delete.
    ConfirmImmediateDelete,
    /// `snapshot-prune-button` — the preview half (a dry run of the set's own
    /// resting policy). Never carries a client-chosen policy (rule 5).
    PrunePreview,
    /// Execute the standing preview.
    PruneExecute,
    /// Dismiss the standing preview.
    PruneCancel,
    /// `snapshot-check-button` — run the integrity check directly. One click runs
    /// it; there is no second confirmation step.
    CheckIntegrity,
    /// Click `snapshot-item[i]` — open that snapshot's file list.
    OpenSnapshot { index: usize },
    /// Close the open file list.
    CloseSnapshotDetail,
    /// `snapshot-file-download-button[i]` — save one file's bytes out of the open
    /// snapshot (single-file restore).
    DownloadFile { index: usize },
    /// `restore-kind-checkbox[i]` — tick or untick one kind
    /// (`backups.md:61`: "user may uncheck one").
    ToggleRestoreKind { index: usize },
    /// `restore-confirm-button` — restore the selected snapshot.
    SubmitRestore,
    /// `restore-divergence-banner` on `restore-history-item[i]` — open that
    /// row's forensic details modal.
    OpenDivergenceModal { index: usize },
    /// The forensic modal's untagged close control — cancel-only, no side
    /// effect (`backups.md:76`).
    CloseDivergenceModal,
}

impl Action {
    /// The wire kind this gesture issues — the offline gate's input
    /// (`crate::element::Gesture::wire_kind`), exhaustive with no fallback arm
    /// so a new variant cannot skip the question.
    ///
    /// **The page splits by who arbitrates, not by how weighty the button
    /// looks.** Its most destructive-sounding gestures are the ones that stay
    /// live: an ordinary snapshot delete is the 48-hour *soft* delete
    /// (`OfflineQueued`), and taking a manual snapshot is a replayable intent —
    /// both class 2, exactly what the outbox exists to carry. What genuinely
    /// needs a nest is the enrollment plane (a destination has to be resolved,
    /// granted and registered before it exists at all) plus the three
    /// irreversible snapshot verbs the nest alone can perform: the hard delete,
    /// the prune, and the restore.
    ///
    /// Two `None`s here are worth naming because they are *not* "unswept":
    /// [`Self::DownloadFile`] is network-shaped with no kind at all — its bytes
    /// ride the bulk-binary `/api/v1/{manifests,chunks}` carve-out, the media
    /// page's finding from the same direction — and [`Self::SelectFolder`]
    /// fires a whole read set (`list_folders` + `list_snapshots`), so there
    /// is no single kind to name and desensitizing a selector would strand the
    /// user on whichever set was showing when the link dropped.
    pub fn wire_kind(&self) -> Option<&'static str> {
        match self {
            // ── Enrolling a destination: the nest arbitrates ────────────────
            // Both add ceremonies are composite, and each declares the kind
            // that BINDS it (the settings ruling): the peer-nest enroll's first
            // mandatory call is the key grant — the config write that ends it
            // is `OfflineSafe`, and declaring *that* would leave a control live
            // that cannot finish — while the custodian enroll has no resolve
            // step and binds on its registry write.
            Action::SubmitForm(SubmitTarget::AddNest) => Some("fauna.backup.nest_key.grant"),
            Action::SubmitForm(SubmitTarget::AddCustodian) => {
                Some("fauna.backup.destination.register")
            }
            // Editing is a rename / re-point of one row of the owner's OWN
            // config document, so it stays live — the same `fauna.account.state.put`
            // answer the admin DNS slice and the mail revoke pair reach.
            Action::SubmitForm(SubmitTarget::Edit) => Some("fauna.account.state.put"),
            Action::ConfirmRemove => Some("fauna.backup.destination.remove"),
            // ⚠ **Keep** is the counterweight to Remove and must NOT follow it:
            // the post-succession verdict is recorded at rest and nothing about
            // the backup plane changes, so it is a succession-ledger write and stays
            // live (`succession-aftermath.md` § Adjudicating what the aftermath
            // carries across — the row stays removable forever after).
            Action::KeepDestination { .. } => Some("fauna.account.state.put"),

            // ── Snapshots: the replayable half stays live ───────────────────
            Action::CreateSnapshot => Some("fauna.filesync.snapshot.create_folder"),
            Action::DeleteSnapshot { .. } => Some("fauna.filesync.snapshot.delete"),
            // The inverse of the soft delete, and `OfflineSafe` on the shared
            // registry: recovering a row the nest still holds is idempotent, so
            // it stays live for exactly the reason the delete beside it does.
            Action::UndeleteSnapshot { .. } => Some("fauna.filesync.snapshot.undelete"),
            // ── Snapshots: the three the nest alone can perform ─────────────
            Action::ConfirmImmediateDelete => Some("fauna.filesync.snapshot.delete_immediate"),
            // Preview and execute are one kind with a `dry_run` flag, so both
            // arms answer it — and the preview is `OnlineOnly` for the honest
            // reason that a dry run offline would have nothing to dry-run.
            Action::PrunePreview | Action::PruneExecute => {
                Some("fauna.filesync.snapshot.prune_set_policy")
            }
            Action::SubmitRestore => Some("fauna.filesync.snapshot.restore_message_kind"),
            // Two reads, declared rather than left `None` (the admin
            // `OpenSeedRotateConfirm` precedent): a `Read` gates nothing, but
            // recording it means a later reclassification reaches this page for
            // free. The check is a read even though it is the page's heaviest
            // call — it produces a verdict, it changes nothing.
            Action::CheckIntegrity => Some("fauna.filesync.snapshot.check"),
            Action::OpenSnapshot { .. } => Some("fauna.filesync.snapshot.get"),

            // ── No kind to name (see the method doc) ────────────────────────
            Action::SelectFolder(_) | Action::DownloadFile { .. } => None,

            // ── Reaches the sync agent, not the nest ───────────────────────
            // The reclaim deletes bytes on THIS disk over the local IPC seam.
            // There is no wire kind to name and, more to the point, nothing for
            // the offline gate to protect: the whole premise of the kind is that
            // this copy is readable with no nest alive anywhere
            // (`backup-destinations.md` § Standalone restore), so a gesture that
            // frees it must stay live exactly when the network is gone.
            Action::ConfirmReclaim => None,
            // The re-seed's calls are the agent's, over its own connection to
            // the nest being seeded: the app issues no wire kind of its own
            // here, and the nest the ceremony targets is the one this app is
            // signed in to, so it is reachable exactly when the gesture is.
            Action::ConfirmReseed => None,

            // ── Local by construction ──────────────────────────────────────
            // Dialog transitions, selection state and the restore-kind ticks:
            // every one is a buffer write whose call, if any, is on the confirm
            // gesture beside it.
            Action::ToggleRemoveReclaim
            | Action::OpenReclaimConfirm
            | Action::CancelReclaim
            | Action::OpenReseedConfirm
            | Action::CancelReseed
            | Action::OpenAddForm
            | Action::OpenEditForm { .. }
            | Action::CancelForm
            | Action::SelectDestinationKind(_)
            | Action::OpenRemoveConfirm { .. }
            | Action::CancelRemove
            | Action::OpenImmediateDelete { .. }
            | Action::CancelImmediateDelete
            // Dismissing a standing preview is local: the preview it drops was
            // already fetched, and dropping it issues nothing.
            | Action::PruneCancel
            | Action::CloseSnapshotDetail
            | Action::SelectRestoreSource(_)
            | Action::SelectSnapshot(_)
            | Action::ToggleRestoreKind { .. }
            | Action::OpenDivergenceModal { .. }
            | Action::CloseDivergenceModal => None,
        }
    }
}

pub fn apply_local(app: &mut App, action: Action) -> Option<Op> {
    // Taken before the page borrow, because it reads a *different* field of the
    // same `App` and the ops that carry it are built while `st` is live.
    let custodian_handle = app.sync_agent.custodian_store();
    let st = &mut app.backups;
    match action {
        Action::OpenAddForm => {
            st.form = Some(FormMode::Add);
            st.removing = None;
            st.url_input.clear();
            st.name_input.clear();
            st.capacity_input.clear();
            st.kind_input = DestinationKindChoice::default();
            app.errors.remove(&Page::Backups);
            None
        }
        Action::OpenEditForm { index } => {
            let dest = st.destination(index)?;
            let id = dest.destination_id.clone();
            let url = dest.destination_nest_url.clone();
            let name = dest.display_name.clone().unwrap_or_default();
            // Prefilled from the row so the disabled select paints this row's own
            // kind rather than the add-dialog default. An unrecognised kind (a
            // newer client wrote it) shows as `Nest` in a control the edit path
            // paints disabled and never reads back, so it cannot rewrite the row.
            let kind = if dest.kind == fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE {
                DestinationKindChoice::ClientDevice
            } else {
                DestinationKindChoice::Nest
            };
            let capacity = dest
                .capacity_cap_bytes
                .map(|b| crate::format::byte_size(b as i64))
                .unwrap_or_default();
            st.form = Some(FormMode::Edit(id));
            st.removing = None;
            st.url_input = url;
            st.name_input = name;
            st.kind_input = kind;
            st.capacity_input = capacity;
            app.errors.remove(&Page::Backups);
            None
        }
        Action::CancelForm => {
            st.form = None;
            st.url_input.clear();
            st.name_input.clear();
            st.capacity_input.clear();
            st.kind_input = DestinationKindChoice::default();
            app.errors.remove(&Page::Backups);
            None
        }
        Action::SelectDestinationKind(value) => {
            // Add-only: the select is painted disabled in edit mode, so a
            // keyboard actuation there must not re-point an existing row at a
            // kind whose registry row and grants describe the other one.
            if matches!(st.form, Some(FormMode::Add))
                && let Some(choice) = DestinationKindChoice::from_select_value(&value)
            {
                st.kind_input = choice;
            }
            None
        }
        Action::SubmitForm(target) => {
            let (nest, secret, store) = st.dest_session()?;
            let url = st.url_input.trim().to_string();
            let name = st.name_input.trim().to_string();
            app.errors.remove(&Page::Backups);
            // The open form is still the guard (only the dialog paints this
            // gesture) and still the only holder of the edit target's row id;
            // `target` is the paint's own statement of which ceremony that
            // dialog was running. A disagreement between the two — the dialog
            // changed under a slow press — drops the gesture rather than
            // running the other ceremony's call, the same posture a stale row
            // index takes on this page.
            let form = st.form.clone()?;
            match target {
                SubmitTarget::AddCustodian if matches!(form, FormMode::Add) => {
                    // The cap is the kind's only knob, and a blank one is a real
                    // choice: `CustodianEnrollment::capacity_cap_bytes: None` is
                    // documented as uncapped. A non-blank one that cannot be read
                    // is a refusal the user sees, never a substituted default —
                    // guessing a cap is how a device's disk fills.
                    let typed = st.capacity_input.trim();
                    let cap = if typed.is_empty() {
                        None
                    } else {
                        match fauna_core::format::parse_byte_size(typed) {
                            Some(bytes) => Some(bytes),
                            None => {
                                app.errors.insert(
                                    Page::Backups,
                                    t::BACKUP_DESTINATION_CAPACITY_INVALID.to_string(),
                                );
                                return None;
                            }
                        }
                    };
                    Some(Op::EnrollCustodian {
                        nest,
                        secret,
                        store,
                        name,
                        capacity_cap_bytes: cap,
                        custodian: custodian_handle,
                    })
                }
                // A blank URL is not an error to surface — the confirm button is
                // painted disabled in that state, so this is only reachable via a
                // keyboard actuation of a disabled control.
                SubmitTarget::AddNest if matches!(form, FormMode::Add) => {
                    if url.is_empty() {
                        return None;
                    }
                    Some(Op::Add {
                        nest,
                        secret,
                        store,
                        url,
                        name,
                    })
                }
                SubmitTarget::Edit => {
                    let FormMode::Edit(id) = form else {
                        return None;
                    };
                    if url.is_empty() {
                        return None;
                    }
                    Some(Op::Edit {
                        nest,
                        secret,
                        store,
                        id,
                        url,
                        name,
                    })
                }
                // The dialog moved out from under the press.
                SubmitTarget::AddNest | SubmitTarget::AddCustodian => None,
            }
        }
        Action::OpenRemoveConfirm { index } => {
            let id = st.destination(index)?.destination_id.clone();
            st.removing = Some(id);
            st.form = None;
            // The opt-in is an intent about THIS removal: a tick left over from
            // a dialog the user cancelled would delete an offline copy nobody
            // asked about in this gesture.
            st.remove_reclaim = false;
            app.errors.remove(&Page::Backups);
            None
        }
        Action::CancelRemove => {
            st.removing = None;
            st.remove_reclaim = false;
            app.errors.remove(&Page::Backups);
            None
        }
        Action::ConfirmRemove => {
            let (nest, secret, store) = st.dest_session()?;
            let id = st.removing.clone()?;
            let reclaim = st.remove_reclaim;
            app.errors.remove(&Page::Backups);
            Some(Op::Remove {
                nest,
                secret,
                store,
                id,
                reclaim,
                custodian: custodian_handle,
            })
        }
        Action::KeepDestination { index } => {
            let (nest, secret, store) = st.dest_session()?;
            let id = st.destination(index)?.destination_id.clone();
            app.errors.remove(&Page::Backups);
            Some(Op::Keep {
                nest,
                secret,
                store,
                id,
            })
        }
        Action::ToggleRemoveReclaim => {
            // Only meaningful while the dialog it lives in is open; a stale
            // actuation must not leave a tick armed for the *next* removal.
            st.removing.as_ref()?;
            st.remove_reclaim = !st.remove_reclaim;
            None
        }
        Action::OpenReclaimConfirm => {
            // Armed only while the row that carries the button is painted. The
            // keyboard-actuation backstop behind the render guard, and the one
            // that matters most on this page: every other stale actuation here
            // costs a no-op, this one would delete the owner's only offline copy.
            st.orphaned_store?;
            st.reclaiming = true;
            st.form = None;
            st.removing = None;
            app.errors.remove(&Page::Backups);
            None
        }
        Action::CancelReclaim => {
            st.reclaiming = false;
            app.errors.remove(&Page::Backups);
            None
        }
        Action::ConfirmReclaim => {
            // Re-checked at press time, not trusted from the paint: the modal
            // can outlive the verdict that opened it (a refresh landing between
            // the two re-enrolls this device, say).
            st.orphaned_store?;
            let destinations = st.destinations.clone();
            app.errors.remove(&Page::Backups);
            Some(Op::ReclaimStore {
                custodian: custodian_handle,
                destinations,
            })
        }
        Action::OpenReseedConfirm => {
            // Armed only while a row carrying the button is painted — the same
            // keyboard-actuation backstop as the reclaim's — and never while a
            // run is in flight.
            if st.reseed == ReseedView::Running
                || reseed_rows(st, custodian_handle.device_id()).is_empty()
            {
                return None;
            }
            st.reseed = ReseedView::Confirming;
            st.form = None;
            st.removing = None;
            st.reclaiming = false;
            app.errors.remove(&Page::Backups);
            None
        }
        Action::CancelReseed => {
            if st.reseed == ReseedView::Confirming {
                st.reseed = ReseedView::Closed;
            }
            app.errors.remove(&Page::Backups);
            None
        }
        Action::ConfirmReseed => {
            if st.reseed != ReseedView::Confirming {
                return None;
            }
            let (nest, secret, store) = st.dest_session()?;
            let folder_keys = st.folder_keys.clone()?;
            st.reseed = ReseedView::Running;
            app.errors.remove(&Page::Backups);
            Some(Op::Reseed {
                nest,
                secret,
                store,
                folder_keys,
                custodian: custodian_handle,
            })
        }

        // ── the restore half ─────────────────────────────────────────────────
        Action::SelectRestoreSource(value) => {
            // Matched on the painted label rather than an index, because that is
            // all a select carries. An unknown value leaves the selection alone.
            st.selected_source = st
                .destinations
                .iter()
                .position(|dest| destination_label(dest) == value)?;
            None
        }
        Action::SelectSnapshot(value) => {
            st.selected_snapshot = st
                .snapshots
                .iter()
                .position(|row| snapshot_label(row) == value)?;
            None
        }
        Action::ToggleRestoreKind { index } => {
            st.restore_kinds.toggle(index);
            None
        }
        Action::SubmitRestore => {
            let (nest, secret, store) = st.dest_session()?;
            // The button is painted disabled unless armed; this is the
            // keyboard-actuation backstop behind that guard (the `SubmitForm`
            // posture). The nest re-checks the same bar regardless — a mismatch
            // is `fauna.filesync.snapshot.confirm_mismatch`, refused without
            // mutating state.
            if !st.restore_armed() {
                return None;
            }
            let snapshot_id = st.snapshots.get(st.selected_snapshot)?.id;
            let confirm_id = st.restore_confirm.clone();
            st.restore_progress = RestoreProgress::Running;
            app.errors.remove(&Page::Backups);
            Some(Op::Restore {
                nest,
                secret,
                store,
                snapshot_id,
                confirm_id,
            })
        }
        Action::OpenDivergenceModal { index } => {
            // Armed only for a row that actually has divergence rows — the
            // banner paints only then, so anything else is a stale actuation.
            let row = st.restore_history.get(index)?;
            if st
                .divergence
                .get(&row.snapshot_id)
                .is_none_or(Vec::is_empty)
            {
                return None;
            }
            st.divergence_modal = Some(index);
            None
        }
        Action::CloseDivergenceModal => {
            st.divergence_modal = None;
            None
        }

        // ── the snapshot half ───────────────────────────────────────────────
        //
        // Every arm here is a thin dispatch: the machine owns single-flight,
        // the preview-first rule and the friction bar, so this page never
        // re-checks them locally (§ Where logic lives — an app "renders
        // `backups_snapshot()` and dispatches gestures; it holds no page
        // logic"). The one thing that IS local is which modal is open.
        Action::SelectFolder(name) => Some(Op::Snapshots {
            machine: st.machine()?,
            gesture: SnapshotGesture::Select(name),
        }),
        Action::CreateSnapshot => Some(Op::Snapshots {
            machine: st.machine()?,
            gesture: SnapshotGesture::Create,
        }),
        Action::DeleteSnapshot { index } => {
            let id = st.snapshot_row_id(index)?;
            Some(Op::Snapshots {
                machine: st.machine()?,
                gesture: SnapshotGesture::Delete(id),
            })
        }
        Action::UndeleteSnapshot { index } => {
            let id = st.snapshot_row_id(index)?;
            Some(Op::Snapshots {
                machine: st.machine()?,
                gesture: SnapshotGesture::Undelete(id),
            })
        }
        Action::OpenImmediateDelete { index } => {
            // Opening the modal is the whole gesture — it never deletes
            // (rule 4). Both buffers start empty so the bar starts disarmed.
            st.immediate_delete = Some(st.snapshot_row_id(index)?);
            st.immediate_confirm.clear();
            st.immediate_ack.clear();
            app.errors.remove(&Page::Backups);
            None
        }
        Action::CancelImmediateDelete => {
            close_immediate_delete(st);
            None
        }
        Action::ConfirmImmediateDelete => {
            let id = st.immediate_delete?;
            let machine = st.machine()?;
            let confirm = st.immediate_confirm.clone();
            let acknowledge = st.immediate_ack.clone();
            // The button is painted disabled unless the machine's own predicate
            // says armed; this is the keyboard-actuation backstop behind that
            // guard, and the machine re-checks it a third time before the call
            // (the `SubmitRestore` posture).
            if !machine.immediate_delete_enabled(
                confirm.clone(),
                id.to_string(),
                acknowledge.clone(),
            ) {
                return None;
            }
            // The modal closes on the row LEAVING the machine's list
            // (`fold_snapshots_page`) — never on this dispatch. A
            // `hard_floor_breach` refusal must leave the modal and the typed
            // inputs standing (linux's `close_immediate_delete_modal_if_landed`
            // / windows' `CloseImmediateDeleteIfLanded`, the same pattern both
            // already carry — this call used to close eagerly here, which hid
            // every rejection behind an already-dismissed modal).
            Some(Op::Snapshots {
                machine,
                gesture: SnapshotGesture::ImmediateDelete {
                    id,
                    confirm,
                    acknowledge,
                },
            })
        }
        Action::PrunePreview => Some(Op::Snapshots {
            machine: st.machine()?,
            gesture: SnapshotGesture::PrunePreview,
        }),
        Action::PruneExecute => Some(Op::Snapshots {
            machine: st.machine()?,
            gesture: SnapshotGesture::PruneExecute,
        }),
        Action::PruneCancel => {
            st.machine()?.cancel_prune_preview();
            // Local, but the render reads the machine, so re-fold its snapshot
            // rather than leaving the page painting a preview it just dismissed.
            st.snap = Some(st.machine()?.snapshot());
            None
        }
        Action::CheckIntegrity => Some(Op::Snapshots {
            machine: st.machine()?,
            gesture: SnapshotGesture::Check,
        }),
        Action::OpenSnapshot { index } => {
            let id = st.snapshot_row_id(index)?;
            Some(Op::Snapshots {
                machine: st.machine()?,
                gesture: SnapshotGesture::OpenDetail(id),
            })
        }
        Action::CloseSnapshotDetail => {
            st.machine()?.close_snapshot_detail();
            st.snap = Some(st.machine()?.snapshot());
            None
        }
        Action::DownloadFile { index } => {
            let (nest, secret) = st.session()?;
            let file = st.snap.as_ref()?.detail.as_ref()?.files.get(index)?;
            // Only a regular file has bytes to walk; a dir/symlink row paints the
            // button-less shape, and this is the actuation backstop behind that.
            if file.file_type != "regular" {
                return None;
            }
            let manifest_hash = file.manifest_hash.clone();
            let path = file.path.clone();
            app.errors.remove(&Page::Backups);
            Some(Op::DownloadFile {
                nest,
                secret,
                manifest_hash,
                path,
            })
        }
    }
}

/// Which [`BackupsMachine`] gesture an [`Op::Snapshots`] runs. A plain data enum
/// so the op stays `Debug`-free-of-secrets and `Send`.
#[derive(Debug, Clone)]
pub enum SnapshotGesture {
    Select(String),
    Create,
    Delete(i64),
    Undelete(i64),
    ImmediateDelete {
        id: i64,
        confirm: String,
        acknowledge: String,
    },
    PrunePreview,
    PruneExecute,
    Check,
    OpenDetail(i64),
}

// ── Ops ──────────────────────────────────────────────────────────────────────

/// One awaited round-trip. Each carries its own `nest` + `secret` so the future
/// is `Send` and owns everything it needs (the page state is never captured).
///
/// No `Debug` derive: `NestClient` implements none (like every other page's
/// `Op`), and a secret-carrying variant is exactly the wrong thing to make
/// printable anyway.
pub enum Op {
    /// Load the destination list + the nest's status projection + the whole
    /// restore half. One op for the whole page, so one nav edge paints a
    /// complete surface instead of racing two independent fetches.
    Refresh {
        nest: Arc<NestClient>,
        secret: [u8; 32],
        /// The `fauna.state.backup` door the destination list is read and
        /// written through (`BackupsState::store`).
        store: Arc<dyn BackupStateStore>,
        /// The snapshot half's machine, refreshed in the same op. `None`
        /// pre-auth — the destination half still loads.
        machine: Option<Arc<BackupsMachine>>,
        /// The sync-agent handle the `backup-orphaned-store-row` verdict is
        /// measured through. Inert off-desktop, which reads as "nothing held".
        custodian: crate::sync_agent::CustodianStoreHandle,
    },
    /// One [`BackupsMachine`] gesture. Carries the machine rather than the
    /// session, because every snapshot-half call goes through the machine's own
    /// custody-wired seam — this page holds no second `SnapshotsClient`.
    Snapshots {
        machine: Arc<BackupsMachine>,
        gesture: SnapshotGesture,
    },
    /// Save one file's bytes out of the open snapshot — the only snapshot-half
    /// gesture that is NOT a machine call, because the machine's job ends at the
    /// file list: the bytes ride the shared client-side walk
    /// (`fauna_core::file_download` via `SyncEngine::download_file_bytes_by_manifest`)
    /// and only the *save* step is platform glue
    /// (`backups.md` § Where logic lives → *Single-file byte download*).
    DownloadFile {
        nest: Arc<NestClient>,
        secret: [u8; 32],
        manifest_hash: String,
        path: String,
    },
    /// Restore the selected local snapshot — the `restore-confirm-button`
    /// action. `confirm_id` is the user's typed text passed through verbatim,
    /// so the nest applies the same friction bar the button painted.
    Restore {
        nest: Arc<NestClient>,
        secret: [u8; 32],
        /// The `fauna.state.backup` door the destination list is read and
        /// written through (`BackupsState::store`).
        store: Arc<dyn BackupStateStore>,
        snapshot_id: i64,
        confirm_id: String,
    },
    /// Resolve the candidate, then run the shared five-step enroll.
    Add {
        nest: Arc<NestClient>,
        secret: [u8; 32],
        /// The `fauna.state.backup` door the destination list is read and
        /// written through (`BackupsState::store`).
        store: Arc<dyn BackupStateStore>,
        url: String,
        name: String,
    },
    /// Rename and/or re-point an existing destination.
    Edit {
        nest: Arc<NestClient>,
        secret: [u8; 32],
        /// The `fauna.state.backup` door the destination list is read and
        /// written through (`BackupsState::store`).
        store: Arc<dyn BackupStateStore>,
        id: String,
        url: String,
        name: String,
    },
    /// Clear the post-succession review mark on one destination — the owner
    /// pressed **Keep**. A `fauna.state.backup` mark write and nothing else:
    /// the row was already re-registered by the aftermath, so nothing about the
    /// backup plane changes, only whether the row still asks to be looked at.
    Keep {
        nest: Arc<NestClient>,
        secret: [u8; 32],
        /// The `fauna.state.backup` door the destination list is read and
        /// written through (`BackupsState::store`).
        store: Arc<dyn BackupStateStore>,
        id: String,
    },
    /// Enroll **this device** as a client custodian — the shared three-step
    /// sequence (`fauna_client_config::enroll_client_custodian`).
    ///
    /// Carries no URL and no resolve step: a custodian has no address, which is
    /// exactly why the kind pulls instead of being pushed to
    /// (`backups.md` § Custodian contract, question 2). The device id is read
    /// inside the runner rather than passed in, because it is a SQLite read this
    /// gesture must not do on the UI path.
    EnrollCustodian {
        nest: Arc<NestClient>,
        secret: [u8; 32],
        /// The `fauna.state.backup` door the destination list is read and
        /// written through (`BackupsState::store`).
        store: Arc<dyn BackupStateStore>,
        name: String,
        capacity_cap_bytes: Option<u64>,
        /// So the repaint re-derives the orphaned-store verdict: enrolling is
        /// what makes a previously orphaned store claimed again.
        custodian: crate::sync_agent::CustodianStoreHandle,
    },
    /// Run the shared two-step deregister.
    Remove {
        nest: Arc<NestClient>,
        secret: [u8; 32],
        /// The `fauna.state.backup` door the destination list is read and
        /// written through (`BackupsState::store`).
        store: Arc<dyn BackupStateStore>,
        id: String,
        /// The `backup-destination-remove-reclaim-checkbox` opt-in: also free
        /// this device's sealed copy, in the same gesture.
        ///
        /// Carried rather than re-read at run time because it is an intent
        /// about *this* removal (`backups.md` § Remove — "the moment the intent
        /// forms"), and because the removal is what makes the store orphaned:
        /// after it lands there is no row left to ask.
        reclaim: bool,
        /// The agent handle the opt-in needs. Inert off-desktop, where there is
        /// no local store to free.
        custodian: crate::sync_agent::CustodianStoreHandle,
    },
    /// Free this device's whole sealed custodian store — the confirmed
    /// `backup-destination-reclaim-button` action.
    ///
    /// ⚠ **Carries no nest session, deliberately: this is the one page gesture
    /// that must work with no nest alive anywhere.** That is the whole premise
    /// of the kind (`backup-destinations.md` § Third destination kind →
    /// *Standalone restore*), and routing the repaint through the page's
    /// ordinary whole-page read would have made freeing local disk space depend
    /// on a reachable nest — the reclaim would land and the page would then
    /// paint an error over a stale row. Nothing about the destination list
    /// changes here anyway; only this device's own store does.
    ///
    /// It stays non-optimistic all the same: the verdict is **re-measured** from
    /// the agent afterwards, never assumed from the fact that the call
    /// succeeded. `destinations` is the list the page already holds, carried in
    /// because the shared rule needs both halves and this op has no `App`.
    ReclaimStore {
        custodian: crate::sync_agent::CustodianStoreHandle,
        destinations: Vec<BackupDestination>,
    },
    /// Seed the signed-in nest from this device's copy — the confirmed
    /// `backup-destination-reseed-button` action (`backup-destinations.md`
    /// § Re-seed → *Where the ceremony runs*): the sync agent runs the shared
    /// driver over the store it owns, this op hands it the owner's
    /// `NestBackupKey`, waits for the job to end, and then carries out the
    /// post-ceremony duty (re-enrolling this device as the custodian) when,
    /// and only when, the whole corpus is live.
    Reseed {
        nest: Arc<NestClient>,
        secret: [u8; 32],
        /// The `fauna.state.backup` door. The re-enrollment reads this box's
        /// list through it after the ceremony — not the page's painted copy — so
        /// it keeps this device's own row (id, name, cap) when the list still
        /// has one.
        store: Arc<dyn BackupStateStore>,
        /// The account's folder-key custody — the target pre-create mints into
        /// it before the agent's job starts.
        folder_keys: Arc<dyn fauna_client_folders::FolderKeyStore>,
        custodian: crate::sync_agent::CustodianStoreHandle,
    },
    /// One audit pass on its own, folding onto [`Outcome::Audited`] — the
    /// targeted re-run the `backup_audit_run_now` agent command drives, so an
    /// e2e proof exercises the same [`audit_pass`] a user's page visit does
    /// without also re-reading the restore half.
    Audit {
        nest: Arc<NestClient>,
        secret: [u8; 32],
        /// The `fauna.state.backup` door the destination list is read and
        /// written through (`BackupsState::store`).
        store: Arc<dyn BackupStateStore>,
        /// This device's own custodian store, whose standing source
        /// regressions the pass folds into its row.
        custodian: crate::sync_agent::CustodianStoreHandle,
    },
}

/// What an op resolved to.
#[derive(Debug)]
pub enum Outcome {
    /// A fresh whole-page read. Every successful op ends here — the page is
    /// non-optimistic by construction (module docs).
    Loaded {
        destinations: Vec<BackupDestination>,
        /// The review marks for those rows, from the same config read — the
        /// page's `backup-destination-unattested-mark` is a function of these,
        /// not of the row's legacy stamp.
        unattested_destination_marks: Vec<DestinationUnattestedMark>,
        statuses: BTreeMap<String, BackupDestinationStatusItem>,
        restore: RestoreData,
        /// This client's own audit picture for the destinations above — see
        /// [`audit_pass`] for why it rides the same op rather than a second one,
        /// and why `None` (no pass completed) is distinct from `Some(vec![])`.
        audit: Option<Vec<DestinationAuditRecord>>,
        /// Whether a restore is what produced this read. The one bit that
        /// distinguishes the shapes — it drives `restore-progress` to
        /// [`RestoreProgress::Done`] and clears the friction bar, without
        /// splitting the single success variant the module's posture rests on.
        restored: Option<Restored>,
        /// The snapshot half's fresh render, when the op carried a machine.
        /// `None` pre-auth leaves whatever the page already painted.
        ///
        /// Boxed: `BackupsSnapshot` is a wide record, and every variant of the
        /// outer `PageOutcome` pays for the largest one carried inline.
        snapshots_page: Option<Box<BackupsSnapshot>>,
        /// The re-derived `backup-orphaned-store-row` verdict — the doubly
        /// optional shape [`Self::Loaded::audit`] already uses, and for the same
        /// reason: **`None` is "not measured, keep what is painted"**, while
        /// `Some(None)` is "measured, nothing orphaned" and clears the row.
        ///
        /// Only the ops that can change the answer carry a handle to measure
        /// with (the whole-page read, the custodian enroll, the removal, the
        /// reclaim itself); an edit or a snapshot gesture cannot, so it does not
        /// pay for an IPC round trip to re-learn what it did not touch.
        orphaned_store: Option<Option<u64>>,
    },
    /// The sealed custodian store was freed, and this is what the agent
    /// reports about it **afterwards** — `None` once nothing is orphaned any
    /// more, which is what retires `backup-orphaned-store-row`.
    ///
    /// A variant of its own rather than a [`Self::Loaded`], because the reclaim
    /// touches nothing the destination list describes and must not depend on a
    /// reachable nest to repaint (see [`Op::ReclaimStore`]).
    Reclaimed(Option<u64>),
    /// The re-seed ceremony reached its end. `page` is the whole-page read
    /// taken afterwards (a [`Self::Loaded`], or the [`Self::Failed`] that read
    /// became), so the rows, the orphaned-store verdict and the result land in
    /// one fold.
    Reseeded {
        result: fauna_client_backup::reseed::ReseedOutcome,
        page: Box<Outcome>,
    },
    /// A file's bytes were saved. Nothing on the page changes — the gesture's
    /// whole effect is on disk — so this carries no state; it exists so a
    /// failure is distinguishable from a silent no-op (convention 11: a command
    /// is honoured or fails loudly, never dropped).
    Downloaded,
    /// A snapshot-half gesture completed — the machine's fresh render, with the
    /// destination half untouched.
    ///
    /// There is no failure variant: the machine puts its own failures on
    /// `snapshot().error`, which is the page `error-message` this page renders.
    /// Folding them onto [`Self::Failed`] too would paint the banner twice.
    SnapshotsLoaded(Box<BackupsSnapshot>),
    /// One audit pass's merged picture, with the rest of the page untouched.
    /// `None` carries the same "no pass completed, keep what is rendered"
    /// meaning as [`Self::Loaded`]'s field.
    Audited(Option<Vec<DestinationAuditRecord>>),
    /// A resolve / enroll / config / restore failure — lands on `error-message`,
    /// and the open dialog stays open so the input can be corrected.
    Failed(String),
}

impl Op {
    pub async fn run(self) -> Outcome {
        match self {
            Op::Refresh {
                nest,
                secret,
                store,
                machine,
                custodian,
            } => refresh(nest, secret, store, None, machine, Some(custodian)).await,
            Op::Add {
                nest,
                secret,
                store,
                url,
                name,
            } => {
                match add_destination(Arc::clone(&nest), store.as_ref(), secret, url, name).await {
                    Ok(()) => refresh(nest, secret, store, None, None, None).await,
                    Err(e) => Outcome::Failed(e),
                }
            }
            Op::Edit {
                nest,
                secret,
                store,
                id,
                url,
                name,
            } => match edit_destination(&nest, store.as_ref(), secret, id, url, name).await {
                Ok(()) => refresh(nest, secret, store, None, None, None).await,
                Err(e) => Outcome::Failed(e),
            },
            Op::EnrollCustodian {
                nest,
                secret,
                store,
                name,
                capacity_cap_bytes,
                custodian,
            } => {
                match enroll_custodian(
                    Arc::clone(&nest),
                    store.as_ref(),
                    secret,
                    name,
                    capacity_cap_bytes,
                )
                .await
                {
                    // Carries a handle because enrolling is the other half of
                    // the verdict: a store this device just re-claimed must stop
                    // offering to delete itself in the same repaint.
                    Ok(()) => refresh(nest, secret, store, None, None, Some(custodian)).await,
                    Err(e) => Outcome::Failed(e),
                }
            }
            Op::Remove {
                nest,
                secret,
                store,
                id,
                reclaim,
                custodian,
            } => {
                let removed = match bound_source_nest(&nest).await {
                    Ok(source_nest) => deregister_backup_destination(
                        Arc::clone(&nest),
                        store.as_ref(),
                        source_nest,
                        &id,
                    )
                    .await
                    .map_err(|e| e.to_string()),
                    Err(e) => Err(e),
                };
                match removed {
                    // The opt-in runs AFTER the deregister, never before: the
                    // removal is what makes the store orphaned, and freeing it
                    // first would leave a live custody row pointing at bytes
                    // that are already gone if the deregister then failed.
                    Ok(_) => {
                        if reclaim && let Err(e) = custodian.reclaim().await {
                            // The removal DID land, so the message says so:
                            // reporting a bare reclaim failure over a list that
                            // still paints the removed row is the one reading a
                            // user cannot recover from. The list corrects on the
                            // next page entry, and the surviving store then
                            // paints its own orphaned-store row — which is both
                            // the honest state and the way to retry.
                            return Outcome::Failed(format!(
                                "the destination was removed, but this device's copy could not                                  be freed: {e}"
                            ));
                        }
                        refresh(nest, secret, store, None, None, Some(custodian)).await
                    }
                    Err(e) => Outcome::Failed(e),
                }
            }
            Op::ReclaimStore {
                custodian,
                destinations,
            } => match custodian.reclaim().await {
                // A refusal is a reported outcome, not an error (the agent's own
                // replica had not finished stopping, so nothing was deleted).
                // The page says so, and the row stays — because the store does.
                Ok(Some(outcome)) if outcome.still_hosting => {
                    Outcome::Failed(t::BACKUP_RECLAIM_STILL_HOSTING.to_string())
                }
                // `None` is "no agent on this platform" — convention 11: a
                // command this app cannot honour fails loudly, never silently.
                Ok(None) => Outcome::Failed(
                    "reclaim: this device drives no sync agent, so it holds no backup copy"
                        .to_string(),
                ),
                // Re-measured, not assumed: the store the agent now reports is
                // what the row re-derives from.
                Ok(Some(_)) => {
                    Outcome::Reclaimed(orphaned_store_bytes(&custodian, &destinations).await)
                }
                Err(e) => Outcome::Failed(e),
            },
            Op::Reseed {
                nest,
                secret,
                store,
                folder_keys,
                custodian,
            } => reseed(nest, secret, store, folder_keys, custodian).await,
            Op::Keep {
                nest,
                secret,
                store,
                id,
            } => match keep_destination(&nest, store.as_ref(), &id).await {
                Ok(()) => refresh(nest, secret, store, None, None, None).await,
                Err(e) => Outcome::Failed(e),
            },
            Op::Restore {
                nest,
                secret,
                store,
                snapshot_id,
                confirm_id,
            } => {
                let client = SnapshotsClient::new(Arc::clone(&nest));
                match client.restore_message_kind(snapshot_id, confirm_id).await {
                    // The restore wrote a `restore_history` row; the refresh is
                    // what surfaces it, so the section never needs a second poke.
                    Ok(reply) => {
                        let restored = Restored {
                            config_present: reply.config_present,
                        };
                        refresh(nest, secret, store, Some(restored), None, None).await
                    }
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::Snapshots { machine, gesture } => {
                match gesture {
                    SnapshotGesture::Select(name) => machine.select_folder(name).await,
                    SnapshotGesture::Create => machine.create_snapshot().await,
                    SnapshotGesture::Delete(id) => machine.delete_snapshot(id).await,
                    SnapshotGesture::Undelete(id) => machine.undelete_snapshot(id).await,
                    SnapshotGesture::ImmediateDelete {
                        id,
                        confirm,
                        acknowledge,
                    } => {
                        machine
                            .delete_snapshot_immediate(id, confirm, acknowledge)
                            .await
                    }
                    SnapshotGesture::PrunePreview => machine.prune_preview().await,
                    SnapshotGesture::PruneExecute => machine.prune_execute().await,
                    SnapshotGesture::Check => machine.check().await,
                    SnapshotGesture::OpenDetail(id) => machine.open_snapshot(id).await,
                }
                Outcome::SnapshotsLoaded(Box::new(machine.snapshot()))
            }
            Op::DownloadFile {
                nest,
                secret,
                manifest_hash,
                path,
            } => download_snapshot_file(nest, secret, &manifest_hash, &path).await,
            Op::Audit {
                nest,
                secret,
                store,
                custodian,
            } => {
                let source_nest_url = nest.nest_url();
                match load_destinations(&nest, store.as_ref()).await {
                    Ok((source_nest, destinations, _marks)) => Outcome::Audited(
                        audit_pass(
                            &nest,
                            secret,
                            &destinations,
                            &source_nest_url,
                            source_nest,
                            Some(&custodian),
                        )
                        .await,
                    ),
                    Err(e) => Outcome::Failed(e),
                }
            }
        }
    }
}

/// Save one file's bytes out of the open snapshot.
///
/// The bytes come from the **shared** client-side walk — `fauna_core::file_download`
/// via `SyncEngine::download_file_bytes_by_manifest` — which owns the two
/// content-address GETs, the seal discriminator, key precedence, decompression
/// and the whole-file verify. This function adds only the two things that are
/// genuinely per-platform: where the file lands, and the error text
/// (`backups.md` § Where logic lives → *Single-file byte download*: "only the
/// save step is platform glue").
///
/// **Where it lands, on a terminal.** tui has no file-save dialog to raise —
/// `tui.md` § Declared platform absences already rules that out for uploads —
/// so the destination is the user's downloads dir, and the saved name is the
/// file's own basename. Under e2e automation the harness dir wins, which is the
/// same `FAUNA_E2E_DOWNLOAD_DIR` bypass linux, windows and apple all take.
async fn download_snapshot_file(
    nest: Arc<NestClient>,
    secret: [u8; 32],
    manifest_hash_hex: &str,
    relative_path: &str,
) -> Outcome {
    let Some(raw) = fauna_core::hex32::decode(manifest_hash_hex).ok() else {
        return Outcome::Failed(format!("{relative_path}: manifest hash is not 32 bytes"));
    };
    let manifest_hash = fauna_core::data::ContentHash::from_digest_raw(raw);

    // A SQLite read — deliberately here, inside the op, never on the UI path
    // (the `EnrollCustodian` posture).
    let Some(device_hex) = crate::media::device_id_hex_for_secret(secret) else {
        return Outcome::Failed("this device has no sync device id yet".to_string());
    };
    let Some(device_id) = fauna_core::hex32::decode(&device_hex).ok() else {
        return Outcome::Failed("this device's sync device id is unreadable".to_string());
    };

    let Some(save_dir) = download_dir() else {
        return Outcome::Failed("no downloads directory to save into".to_string());
    };
    let name = relative_path
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(relative_path);
    let save_path = save_dir.join(name);

    let auth = nest.auth().clone();
    let secret_hex = fauna_core::hex32::encode(&secret);
    let path = relative_path.to_string();
    // The throwaway `SyncEngine` owns an in-memory rusqlite connection — `Send`
    // but `!Sync`, so its future cannot ride the multi-threaded runtime this op
    // runs on. Confine it to a dedicated thread with its own current-thread
    // runtime, exactly as linux does.
    let joined = tokio::task::spawn_blocking(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("build snapshot-download runtime: {e}"))?;
        let bytes = rt.block_on(async move {
            let engine = fauna_sync_engine::engine_lifecycle::build_restore_engine(
                auth,
                nest,
                device_id,
                &secret_hex,
            );
            engine
                .download_file_bytes_by_manifest(manifest_hash, None, &path)
                .await
                .map_err(|e| format!("download {path:?}: {e:#}"))
        })?;
        if let Some(parent) = save_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&save_path, &bytes)
            .map_err(|e| format!("write {}: {e}", save_path.display()))
    })
    .await;

    match joined {
        Ok(Ok(())) => Outcome::Downloaded,
        Ok(Err(e)) => Outcome::Failed(e),
        Err(e) => Outcome::Failed(format!("snapshot-download worker: {e}")),
    }
}

/// Where a downloaded file lands — snapshots here, and the account-data
/// export (`settings/account.rs`, no native save dialog on a terminal either)
/// reuses this rather than a second copy. The e2e harness dir when the run
/// provides one, else the user's downloads dir, else `$HOME`.
pub(crate) fn download_dir() -> Option<std::path::PathBuf> {
    // The harness override, and NOT part of the production resolution below.
    //
    // Two gates, both required (`crate::e2e_mode_enabled`, and `conversations::init`
    // for the same shape): the `#[cfg]` is convention 15's outer boundary, so a
    // release build compiles neither the read nor the literal, and the predicate is
    // the inner switch within a test-capable build. Ungated, this shipped
    // `FAUNA_E2E_DOWNLOAD_DIR` in the release binary as the FIRST preference of a
    // production path — so a real user's downloaded snapshot, exported account data
    // and exported mail archive all landed wherever whoever controlled the launch
    // environment said, which is the risk convention 15 opens by naming. Found by a
    // `strings` of the release artifact, not by a scanner: no scan then covered `apps/*`.
    #[cfg(any(test, debug_assertions, feature = "e2e-agent"))]
    if crate::e2e_mode_enabled()
        && let Some(dir) = std::env::var_os("FAUNA_E2E_DOWNLOAD_DIR").filter(|v| !v.is_empty())
    {
        return Some(std::path::PathBuf::from(dir));
    }
    if let Some(dir) = std::env::var_os("XDG_DOWNLOAD_DIR").filter(|v| !v.is_empty()) {
        return Some(std::path::PathBuf::from(dir));
    }
    let home = std::path::PathBuf::from(std::env::var_os("HOME")?);
    let downloads = home.join("Downloads");
    Some(if downloads.is_dir() { downloads } else { home })
}

/// How long one whole audit pass may take before the page gives up on it.
///
/// A named generous budget, not a latency assumption (testing.md convention 14):
/// a green pass against a live destination costs a connect + one
/// `custody.list` + `AUDIT_SAMPLE_K` blob reads, far inside this, and pays
/// nothing for the ceiling. It exists because the pass rides the **same awaited
/// op** as the page read (see [`audit_pass`]), so an unbounded connect to a
/// black-holed destination would hold the Backups page's nav edge open with no
/// way out — `NestClient::connect` carries no deadline of its own.
const AUDIT_BUDGET: std::time::Duration = std::time::Duration::from_secs(90);

/// Run one audit pass over `destinations` and hand back the merged picture.
///
/// **Why this rides the same op as the page read**, where linux deliberately
/// fires it as a second, independent round trip: tui's actuation contract is
/// one awaited op per nav edge or gesture (`crate::app::on_nav_enter` — "it
/// returns the op instead of firing it, so the caller keeps the await-vs-spawn
/// choice"), and the agent path awaits it to completion before its next read.
/// A backgrounded audit would therefore be exactly the fire-and-forget shape
/// that contract exists to prevent: the driver's next `count("backup-audit-alert")`
/// could legitimately read the pre-pass frame. Folding it in makes the surface
/// deterministic — when `add_destination` returns, the audit has already run and
/// rendered. The shared 24 h `AUDIT_MIN_INTERVAL` debounce is what keeps the
/// steady state honest: a repeat visit costs one store read and **no** round
/// trip, because `audit_all` skips every destination that is not due.
///
/// **`None` means "no pass completed", which is NOT the same as an empty
/// picture** — and conflating them is the exact failure the shared
/// `merge_outcomes` doc warns about. `Some(vec![])` is a real finding (no
/// destination is configured, so every banner must clear — rule 2: an alarm for
/// a backup the user deliberately stopped is noise they cannot dismiss);
/// `None` is the timeout below, where the previously-rendered verdicts must
/// stand, or a slow network would silently switch off a standing alert.
async fn audit_pass(
    nest: &NestClient,
    secret: [u8; 32],
    destinations: &[BackupDestination],
    source_nest_url: &str,
    source_nest: [u8; 32],
    custodian: Option<&crate::sync_agent::CustodianStoreHandle>,
) -> Option<Vec<DestinationAuditRecord>> {
    if destinations.is_empty() {
        return Some(Vec::new());
    }
    // This device's own custodian store, read from the agent that hosts it:
    // its standing source regressions fold into the row that assigns this
    // device. An op that carried no handle, or an agent that did not answer,
    // is "no store was read" — the row's record stands as it was.
    let own_custodian = match custodian {
        Some(handle) => handle.own_custodian().await,
        None => None,
    };
    let actor_id_hex = ActorKeypair::from_secret(secret).actor_id_hex();
    let store = crate::backup_audit::store(&actor_id_hex);
    // The audited set is the client's OWN pinned destination list, and the two
    // seams below are the only things a shell supplies: shared Rust does the
    // freshness comparison, the inclusion sampling, the verdicts and the merge
    // (`ui/backups.md` § Audit-alert surface — "a shell renders that vector; it
    // implements no merge, no debounce, and no verdict logic").
    let connector = fauna_client_pair::native_backup_destination_connector(secret);
    // The covered-folder mirror plane's population anchor is this device's
    // synced replica of each covered folder: the sync engine's
    // `ReplicaFolderIndex` over the state dir the local sync agent keeps this
    // actor's `fsid-<ref>.db` files in (tui runs no engine of its own; the
    // agent's resident engines are what hold the replica) and the bound source
    // nest, read once for the pass.
    let folder_index = fauna_sync_engine::segment_backup::bound_replica_folder_index(
        nest,
        Some(fauna_sync_engine::segment_backup::local_agent_state_dir(
            &actor_id_hex,
        )),
    )
    .await;
    let inclusion = fauna_client_pair::native_backup_inclusion_source(secret, folder_index);
    // `source_nest_url` is the owner's own nest, dialled by the pass only to
    // settle a ledger regression a destination served (the generation pin,
    // `fauna_client_backup::audit::SourceLedgerVouch`) or to read its rotation
    // chain when a destination's seat names its predecessor; `source_nest` is
    // the identity the list rests under, the one a seat is carried to.
    let pass = run_audit_pass(
        connector.as_ref(),
        inclusion.as_ref(),
        &store,
        destinations,
        source_nest_url,
        &source_nest,
        own_custodian.as_ref(),
        crate::backup_audit::now_secs(),
    );
    match tokio::time::timeout(AUDIT_BUDGET, pass).await {
        Ok(pass) => {
            for degradation in &pass.degradations {
                tracing::warn!("backup audit: {degradation}");
            }
            Some(pass.records)
        }
        Err(_) => {
            // Nothing was persisted, so the next pass simply retries against
            // the same prior state.
            tracing::warn!(
                "backup audit: pass exceeded {}s, leaving the previous verdicts standing",
                AUDIT_BUDGET.as_secs()
            );
            None
        }
    }
}

/// Load the destination list and, when there is at least one, the nest's status
/// projection for it.
///
/// The zero-destination early return is not an optimization detail: the status
/// rows render only when ≥1 destination is configured (`backups.md`
/// § Per-destination status read), and the shared read declines to enroll an
/// owner who configured nothing — so the round-trip would be pure cost.
///
/// A status-read failure degrades to the not-yet-backed-up baseline rather than
/// erroring the page: the destinations themselves loaded fine, and "never / 0
/// queued" is exactly what a destination whose coordinator has not run yet
/// honestly reads.
async fn refresh(
    nest: Arc<NestClient>,
    secret: [u8; 32],
    store: Arc<dyn BackupStateStore>,
    restored: Option<Restored>,
    machine: Option<Arc<BackupsMachine>>,
    custodian: Option<crate::sync_agent::CustodianStoreHandle>,
) -> Outcome {
    let (source_nest, destinations, unattested_destination_marks) =
        match load_destinations(&nest, store.as_ref()).await {
            Ok(loaded) => loaded,
            Err(e) => return Outcome::Failed(e),
        };
    let statuses = if destinations.is_empty() {
        BTreeMap::new()
    } else {
        read_backup_status(Arc::clone(&nest), store.as_ref(), secret, source_nest)
            .await
            .map(|reply| {
                reply
                    .destinations
                    .into_iter()
                    .map(|row| (row.destination_id.clone(), row))
                    .collect()
            })
            .unwrap_or_default()
    };
    let source_nest_url = nest.nest_url();
    // The restore half is NOT gated on having a destination: its local path
    // restores a snapshot this nest already holds, which is exactly the flow a
    // user with zero destinations configured runs.
    let restore = match load_restore(Arc::clone(&nest)).await {
        Ok(r) => r,
        Err(e) => return Outcome::Failed(e),
    };
    // Last, and deliberately not fatal: the audit is this client's independent
    // check *on top of* the page, so a destination that will not answer must
    // still leave the rows and the restore half rendering. A failure there is
    // already a verdict (`Unreachable`), not an error to surface.
    let audit = audit_pass(
        &nest,
        secret,
        &destinations,
        &source_nest_url,
        source_nest,
        custodian.as_ref(),
    )
    .await;
    // The snapshot half, in the same op. Its failures live on the machine's own
    // `error` field (which this page renders as `error-message`), so a snapshot
    // read that fails must NOT take the destination half down with it — hence a
    // fold of `snapshot()`, never an early `Outcome::Failed`.
    let snapshots_page = match machine {
        Some(m) => {
            m.refresh().await;
            Some(Box::new(m.snapshot()))
        }
        None => None,
    };
    // The orphaned-store verdict, reached here for the same reason the audit is:
    // it needs a read (this device's sync id, and the agent's measurement of the
    // store) that must not happen on a render path. `None` means *not measured*
    // — the op carried no handle because nothing it did could change the answer
    // — and keeps whatever the page already paints.
    let orphaned_store = match custodian {
        Some(handle) => Some(orphaned_store_bytes(&handle, &destinations).await),
        None => None,
    };
    Outcome::Loaded {
        destinations,
        unattested_destination_marks,
        statuses,
        restore,
        audit,
        restored,
        snapshots_page,
        orphaned_store,
    }
}

/// Is this device holding a sealed store no destination row claims, and how
/// much? — the `backup-orphaned-store-row` verdict
/// (`backups.md` § Manage backup destinations → *Reclaim this device's copy*).
///
/// Two reads and one shared rule. The measurement comes from the sync agent,
/// which is the authority on where the store lives (`sync-agent.md` § Control
/// plane split), and so does the device id the rows are judged against — the
/// one that agent registers under, carried on the handle; the claim half is
/// `fauna_core::data::custodian_store_is_orphaned` over the rows this page just
/// loaded. Neither half is decided here.
///
/// Every failure answers `None` — no agent, no sync id, a refusing agent. That
/// is the conservative direction for a gesture that deletes the owner's only
/// offline copy: not knowing must never paint the button.
async fn orphaned_store_bytes(
    custodian: &crate::sync_agent::CustodianStoreHandle,
    destinations: &[BackupDestination],
) -> Option<u64> {
    let store = custodian.read().await.ok().flatten()?;
    let device_hex = custodian.device_id()?;
    fauna_core::data::custodian_store_is_orphaned(
        destinations.iter().map(|d| d.custodian_row()),
        device_hex,
        store.holds_bytes(),
    )
    .then_some(store.bytes)
}

/// Where `backup-destination-reseed-button` paints: `None` for the orphaned
/// store's row, `Some(id)` for a destination row. The shared
/// `reseed_sources` decides which rows are sources at all; this keeps the ones
/// whose delivery leg this build can run, so a nest-kind row joins by its leg
/// becoming built, never by a change here.
fn reseed_rows(st: &BackupsState, this_device_id: Option<&str>) -> Vec<Option<String>> {
    fauna_client_backup::reseed::reseed_sources(
        &st.destinations,
        this_device_id.unwrap_or_default(),
        st.orphaned_store.is_some(),
    )
    .into_iter()
    .filter(|s| s.leg.is_built())
    .map(|s| s.destination_id)
    .collect()
}

/// The re-seed: the target pre-create and the agent's job, waited on to its end
/// through the shared `fauna_client_sync::reseed_wire::await_agent_reseed`,
/// then the shared post-ceremony duty
/// (`fauna_client_config::reenroll_custodian_after_reseed`), then a whole-page
/// read. Nothing about the ceremony's order is decided here.
async fn reseed(
    nest: Arc<NestClient>,
    secret: [u8; 32],
    store: Arc<dyn BackupStateStore>,
    folder_keys: Arc<dyn fauna_client_folders::FolderKeyStore>,
    custodian: crate::sync_agent::CustodianStoreHandle,
) -> Outcome {
    // Held in its own zeroize-on-drop type; the one bare copy is the IPC frame's,
    // which zeroizes itself too.
    let key = fauna_core::crypto::NestBackupKey::derive(&secret);
    let result = match custodian
        .reseed(Arc::clone(&nest), folder_keys, key.to_bytes())
        .await
    {
        Some(Ok(result)) => result,
        None => return Outcome::Failed(t::BACKUP_RESEED_NO_AGENT.to_string()),
        Some(Err(e)) => return Outcome::Failed(t::backup_reseed_failed(&e)),
    };
    drop(key);

    let device = custodian
        .device_id()
        .map(str::to_string)
        .or_else(|| crate::media::device_id_hex_for_secret(secret))
        .unwrap_or_default();
    // This box's list as it rests now, not the page's painted copy: the
    // re-enrollment keeps this device's own row (id, name, cap) when it has one.
    let reenrolled = match bound_source_nest(&nest).await {
        Ok(source_nest) => match load_backup_state(store.as_ref(), source_nest).await {
            Ok(state) => reenroll_custodian_after_reseed(
                Arc::clone(&nest),
                store.as_ref(),
                source_nest,
                &result,
                &device,
                &state.backup.destinations,
                || uuid::Uuid::new_v4().to_string(),
            )
            .await
            .map(|r| r.map(|_| ()).map_err(|e| e.to_string())),
            Err(e) => Some(Err(e.to_string())),
        },
        Err(e) => Some(Err(e)),
    };
    if let Some(Err(e)) = reenrolled {
        // The data IS back; say so, and say what did not happen.
        return Outcome::Reseeded {
            result,
            page: Box::new(Outcome::Failed(t::backup_reseed_reenroll_failed(&e))),
        };
    }
    let page = refresh(nest, secret, store, None, None, Some(custodian)).await;
    Outcome::Reseeded {
        result,
        page: Box::new(page),
    }
}

/// The restore half's three reads.
///
/// Unlike the status projection above these do **not** degrade to empty on
/// failure: an owner-scoped read that errors is a real fault worth showing,
/// whereas "no rows" is already the honest empty case and needs no error to
/// express it. Silently swallowing a failure here would also strip the one
/// diagnostic `test_backups_restore.py` prints when it cannot find a row.
///
/// The divergence fan-out is per-row by construction — `list_restore_divergence`
/// is snapshot-scoped, so there is no bulk form to prefer — and deduplicated,
/// because restoring the same snapshot twice writes two history rows keyed to
/// one snapshot. Doing it inside the op (linux fires it from its message loop
/// after the history lands) means the banners are already correct the first time
/// the section paints, with no second repaint for a driver to race.
async fn load_restore(nest: Arc<NestClient>) -> Result<RestoreData, String> {
    let client = SnapshotsClient::new(nest);
    let snapshots = client
        .list(None, None, 0)
        .await
        .map_err(|e| e.to_string())?
        .rows;
    let history = client
        .list_restore_history(0)
        .await
        .map_err(|e| e.to_string())?
        .rows;

    let mut divergence: BTreeMap<i64, Vec<RestoreDivergenceRow>> = BTreeMap::new();
    for row in &history {
        if divergence.contains_key(&row.snapshot_id) {
            continue;
        }
        let rows = client
            .list_restore_divergence(row.snapshot_id)
            .await
            .map_err(|e| e.to_string())?
            .rows;
        divergence.insert(row.snapshot_id, rows);
    }

    Ok(RestoreData {
        snapshots,
        history,
        divergence,
    })
}

/// The 32-byte id this connection is bound to — the source-box key every
/// `fauna.state.backup` read and write names (`fauna_client_pair::resolve_this_nest_id`).
/// Unprovable ⇒ an error, never a guess: the page read then shows a failure and
/// every write refuses — there is no fallback to another box's list.
async fn bound_source_nest(nest: &Arc<NestClient>) -> Result<[u8; 32], String> {
    fauna_client_pair::resolve_this_nest_id(nest)
        .await?
        .try_into()
        .map_err(|_| "this nest's id was not 32 bytes".to_string())
}

/// This box's destination rows **and** their review marks, from one
/// `fauna.state.backup` read (re-filing a list still keyed under an older box
/// id): the marks ride the same row, so reading the list without them would
/// render a raised destination clean. Returns the bound box id too — the status
/// read keys on it.
async fn load_destinations(
    nest: &Arc<NestClient>,
    store: &dyn BackupStateStore,
) -> Result<
    (
        [u8; 32],
        Vec<BackupDestination>,
        Vec<DestinationUnattestedMark>,
    ),
    String,
> {
    let source_nest = bound_source_nest(nest).await?;
    // A refused read says what it waits for — the nest, or another of the
    // account's devices — never the store's internal text.
    let state = load_backup_state_refiled(store, nest, source_nest)
        .await
        .map_err(|e| {
            e.not_ready_reason()
                .map_or_else(|| e.to_string(), str::to_string)
        })?;
    Ok((source_nest, state.backup.destinations, state.marks))
}

/// Resolve identity + verify reachability/authorization BEFORE recording,
/// keeping the authenticated connection open — the shared enroll sequence reuses
/// it to register the nest-writer grant at the destination.
async fn add_destination(
    nest: Arc<NestClient>,
    store: &dyn BackupStateStore,
    secret: [u8; 32],
    url: String,
    name: String,
) -> Result<(), String> {
    // The writer the destination authorizes is the id this connection proved,
    // never the nest's own claim — and the box whose list the new row lands in.
    let source_nest_id = bound_source_nest(&nest).await?;
    // Blank name ⇒ the destination's handle domain, per backups.md § State &
    // data shape → Create step 1; the fallback lives inside the shared enroll.
    fauna_sync_engine::segment_backup::resolve_and_enroll_destination(
        nest,
        store,
        secret,
        url,
        name,
        source_nest_id,
    )
    .await
    .map(|_| ())
}

/// Enroll this device as a client custodian — the shared three-step sequence.
///
/// **No resolve step and no connection to a second nest**, unlike
/// [`add_destination`]: a custodian has no address to resolve, which is the
/// property that makes the kind pull rather than be pushed to
/// (`backups.md` § Custodian contract, question 2). The whole sequence — the
/// registry write, the `fauna.state.backup` write, the crash-safety ordering, and the
/// deliberate absence of a `NestBackupKey` grant — is
/// [`enroll_client_custodian`]'s; this only supplies what the *shell* knows.
///
/// The device id is read here rather than on the gesture path because
/// `device_id_hex` opens a SQLite store, and the same id every one of this
/// device's file-sync engines presents is what the source nest keys the status
/// projection on — a page-local substitute would project a row nothing drives.
/// Its absence is surfaced, never defaulted: the shared enroll refuses a blank
/// id for exactly that reason, and this reaches the same refusal by the same
/// path rather than inventing a second message for it.
async fn enroll_custodian(
    nest: Arc<NestClient>,
    store: &dyn BackupStateStore,
    secret: [u8; 32],
    name: String,
    capacity_cap_bytes: Option<u64>,
) -> Result<(), String> {
    let custodian_device_id = crate::media::device_id_hex_for_secret(secret).unwrap_or_default();
    let source_nest = bound_source_nest(&nest).await?;
    enroll_client_custodian(
        nest,
        store,
        source_nest,
        CustodianEnrollment {
            destination_id: uuid::Uuid::new_v4().to_string(),
            custodian_device_id,
            // Blank name ⇒ the device id, so a row is never nameless; the
            // fallback lives inside the shared enroll.
            display_name: name,
            capacity_cap_bytes,
        },
    )
    .await
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Rename and/or re-point a destination.
///
/// A URL change must resolve to the **same** nest (`backups.md` § Edit): a
/// different `nest_id` is remove + re-add, refused inline. Re-resolving only
/// when the URL actually changed is what keeps renaming an *offline* destination
/// working.
/// Clear one destination's post-succession review mark — the **Keep** half of
/// the pair (`succession-aftermath.md` § Adjudicating what the aftermath carries
/// across). Remove has no twin here: `deregister_backup_destination` takes the
/// row and its mark together, which is why no second removal mechanism exists.
///
/// A no-op `keep` is not an error. The owner may press Keep on a row a
/// concurrent device already adjudicated, and the honest result is the same
/// either way — the row is no longer raised.
/// The write itself is shared (`fauna_client_config::keep_backup_destination_at_rest`)
/// rather than open-coded here: it must ride the one `fauna.state.backup` write
/// door (`mutate_backup`) so a Keep is not undone by a concurrent device. The
/// nest is read only to prove which box's list the mark sits on.
async fn keep_destination(
    nest: &Arc<NestClient>,
    store: &dyn BackupStateStore,
    id: &str,
) -> Result<(), String> {
    let source_nest = bound_source_nest(nest).await?;
    keep_backup_destination_at_rest(store, source_nest, id)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

async fn edit_destination(
    nest: &Arc<NestClient>,
    store: &dyn BackupStateStore,
    secret: [u8; 32],
    id: String,
    url: String,
    name: String,
) -> Result<(), String> {
    let source_nest = bound_source_nest(nest).await?;
    fauna_sync_engine::segment_backup::edit_destination(store, source_nest, secret, id, url, name)
        .await
        .map(|_| ())
}

/// Fold an op's result back into the page. One function for both dispatch
/// paths, so they cannot disagree about what an outcome means.
///
/// Success closes whichever dialog was open and clears the error; failure keeps
/// the dialog open with the message on `error-message` (module docs).
pub fn apply_outcome(app: &mut App, outcome: Outcome) {
    match outcome {
        Outcome::Loaded {
            destinations,
            unattested_destination_marks,
            statuses,
            restore,
            audit,
            restored,
            snapshots_page,
            orphaned_store,
        } => {
            // `None` = the op carried no machine (pre-auth); keep what is painted
            // rather than blanking the snapshot half.
            if let Some(page) = snapshots_page {
                fold_snapshots_page(app, *page);
            }
            // One row per enrolled destination for the page's own render
            // state — coverage rows (per-folder mirror-set rows sharing a
            // destination_id) are folded away here, AFTER `audit_pass` above
            // already ran against the raw `destinations` it needs to derive
            // `attached_mirror_sets` from every row (
            // `fauna_core::data::distinct_destinations`'s own doc).
            app.backups.destinations = fauna_core::data::distinct_destinations(&destinations);
            app.backups.unattested_destination_marks = unattested_destination_marks;
            app.backups.statuses = statuses;
            // `None` = no pass completed this op; the standing verdicts must
            // survive it (`audit_pass`).
            if let Some(records) = audit {
                app.backups.audit = records;
            }
            app.backups.snapshots = restore.snapshots;
            app.backups.restore_history = restore.history;
            app.backups.divergence = restore.divergence;
            // A refreshed list can be shorter than the one the pickers pointed
            // into (a snapshot pruned, a destination removed). Clamping keeps
            // both indices valid; leaving them past the end would silently
            // disarm the friction bar with no visible cause.
            clamp_index(
                &mut app.backups.selected_snapshot,
                app.backups.snapshots.len(),
            );
            clamp_index(
                &mut app.backups.selected_source,
                app.backups.destinations.len(),
            );
            // The rows behind an open modal have just been replaced, so the
            // index it holds no longer means what it did.
            app.backups.divergence_modal = None;
            app.backups.restore_progress = if restored.is_some() {
                RestoreProgress::Done
            } else {
                RestoreProgress::Idle
            };
            // Written in the same fold as the DONE state, so a reader that has
            // seen DONE has seen the warning's final verdict too.
            app.backups.restore_config_absent = restored.is_some_and(|r| !r.config_present);
            if restored.is_some() {
                app.backups.restore_confirm.clear();
            }
            // `None` = this op did nothing that could change the verdict, so
            // it did not pay for the measurement; `Some(None)` = measured, and
            // nothing is orphaned any more.
            if let Some(verdict) = orphaned_store {
                app.backups.orphaned_store = verdict;
            }
            app.backups.form = None;
            app.backups.removing = None;
            app.backups.remove_reclaim = false;
            app.backups.reclaiming = false;
            app.backups.url_input.clear();
            app.backups.name_input.clear();
            app.errors.remove(&Page::Backups);
        }
        Outcome::Audited(records) => {
            if let Some(records) = records {
                app.backups.audit = records;
            }
            // Deliberately clears no error and closes no dialog: an audit pass
            // is an observation about destinations, not a page mutation, so it
            // must not wipe a message the user still needs to read.
        }
        Outcome::Failed(msg) => {
            // A failed restore must not leave the progress line claiming one is
            // running: `restore_armed` reads it, so a stuck `Running` would
            // disarm the friction bar permanently with no way back.
            if app.backups.restore_progress == RestoreProgress::Running {
                app.backups.restore_progress = RestoreProgress::Idle;
            }
            // Same for the re-seed: a stuck `Running` would disarm its button.
            if app.backups.reseed == ReseedView::Running {
                app.backups.reseed = ReseedView::Closed;
            }
            app.errors.insert(Page::Backups, msg);
        }
        Outcome::Reseeded { result, page } => {
            // The page read first, so its fold cannot clear the result; the
            // result lands whether or not that read succeeded, because the
            // ceremony's effect on the nest already happened.
            apply_outcome(app, *page);
            app.backups.reseed = ReseedView::Done(result);
        }
        Outcome::Reclaimed(orphaned_store) => {
            app.backups.orphaned_store = orphaned_store;
            app.backups.reclaiming = false;
            app.errors.remove(&Page::Backups);
        }
        Outcome::SnapshotsLoaded(page) => fold_snapshots_page(app, *page),
        // Nothing to fold: the bytes are on disk and the page is unchanged.
        Outcome::Downloaded => {}
    }
}

/// Fold a fresh machine render into the page.
///
/// **The machine's `error` becomes the page `error-message`.** The snapshot half
/// holds no error state of its own: a failed gesture lands on
/// `BackupsSnapshot::error`, and this is the one place it crosses onto
/// `app.errors[Page::Backups]`. A *cleared* machine error clears the banner too —
/// otherwise a successful retry would leave the previous failure on screen, which
/// is the one thing the machine's own "a new gesture clears the previous failure"
/// rule exists to prevent.
///
/// ⚠ It never carries a **success**: a completed prune or check reports through
/// `prune_preview` / `check_result`, which is § Architectural rules rule 6, and
/// an e2e that reads `error_text()` as a failure witness depends on it.
fn fold_snapshots_page(app: &mut App, page: BackupsSnapshot) {
    let error = page
        .error
        .as_ref()
        .map(|text| text.clone().resolve(fauna_i18n::strings::lookup));
    // The modal's target may have just been deleted; close it rather than leave
    // a friction bar armed for a row that is gone.
    if app
        .backups
        .immediate_delete
        .is_some_and(|id| !page.snapshots.iter().any(|row| row.id == id))
    {
        close_immediate_delete(&mut app.backups);
    }
    app.backups.snap = Some(page);
    match error {
        Some(msg) => {
            app.errors.insert(Page::Backups, msg);
        }
        None => {
            app.errors.remove(&Page::Backups);
        }
    }
}

/// Close the immediate-delete modal and clear both friction-bar buffers.
///
/// Both buffers, always: a retained ack phrase would leave the NEXT snapshot's
/// modal one field away from armed, which is exactly the one-click delete
/// rule 4 forbids.
fn close_immediate_delete(st: &mut BackupsState) {
    st.immediate_delete = None;
    st.immediate_confirm.clear();
    st.immediate_ack.clear();
}

/// Pull `index` back inside a list that may have shrunk, leaving `0` for an
/// empty one (where every reader answers `None` anyway).
fn clamp_index(index: &mut usize, len: usize) {
    *index = (*index).min(len.saturating_sub(1));
}

// ── Paint ─────────────────────────────────────────────────────────────────────

/// The row's human label — the shared `display_name`-else-URL-host fallback, so
/// the seven apps never drift on a blank name (`backups.md` § Where logic
/// lives → Destination row label).
fn destination_label(dest: &BackupDestination) -> String {
    fauna_core::format::backup_destination_label(
        dest.display_name.as_deref(),
        &dest.destination_nest_url,
    )
}

/// `backup-destination-last-upload-time` text, via the shared
/// [`fauna_client_backup::row_text::destination_last_upload_text`] — the
/// field extraction and the resolve both moved there (2026-08-21) because
/// this body and linux's were byte-identical; this door only supplies "now".
fn last_upload_text(status: Option<&BackupDestinationStatusItem>) -> String {
    let now_ms = fauna_core::data::Timestamp::now_millis_or_zero() as i64;
    fauna_client_backup::row_text::destination_last_upload_text(status, now_ms)
}

/// `backup-destination-backlog-count` text, via the shared
/// [`fauna_client_backup::row_text::destination_backlog_text`].
fn backlog_text(status: Option<&BackupDestinationStatusItem>) -> String {
    fauna_client_backup::row_text::destination_backlog_text(status)
}

/// `backup-destination-last-audit-time` text for a row: when **this client's
/// own** audit last passed against the destination, via the shared
/// [`fauna_client_backup::row_text::destination_last_audit_text`]. `None` (no
/// pass yet) ⇒ "never".
///
/// Note what this is *not*: the row above it (`last_upload_time`) is the
/// source nest reporting on its own uploads, and this one is the only line on
/// the page that neither the source nor the destination gets to assert.
fn last_audit_text(record: Option<&DestinationAuditRecord>) -> String {
    // The same clock the pass was judged against, so a stamp this client wrote
    // can never read as being in the future.
    let now_ms = crate::backup_audit::now_secs().saturating_mul(1_000);
    fauna_client_backup::row_text::destination_last_audit_text(record, now_ms)
}

/// `backup-destination-last-audit-time` text for a **client-device custodian**
/// row: when that device's own self-audit last **passed**, via the shared
/// [`fauna_client_backup::row_text::destination_self_audit_text`].
///
/// The verdict itself is not rendered here — it is what raises (or does not
/// raise) the banner, through the same shared predicate. What this cell owes is
/// the honest stamp plus the honest provenance, and `None` reads as *not yet*,
/// never as a pass: a custodian shipped before the carrier landed reports
/// nothing at all.
fn self_audit_text(status: Option<&BackupDestinationStatusItem>) -> String {
    let now_ms = crate::backup_audit::now_secs().saturating_mul(1_000);
    fauna_client_backup::row_text::destination_self_audit_text(status, now_ms)
}

pub fn elements(app: &App) -> Vec<Element> {
    let st = &app.backups;
    let mut out = vec![Element::label(ids::PAGE_HEADING, t::TITLE)];

    // `backup-audit-alert` banners come FIRST, directly under the heading and
    // ahead of every section: "a warning that a backup is not keeping up must be
    // visible without scrolling past the snapshot list" (`backups.md`
    // § Audit-alert surface — linux mounts them above its page scroller for the
    // same reason; a terminal's equivalent of "outside the scroller" is "first
    // in the frame").
    audit_alert_elements(st, &mut out);

    // Section chrome — ui.yaml scopes no id to the destinations heading or its
    // explanatory line, so both paint untagged (the honest way to render real UI
    // that is not automatable; minting `backup-destinations-title` would be the
    // invented-ID anti-pattern).
    out.push(Element::chrome(t::BACKUP_DESTINATIONS_TITLE));
    out.push(Element::chrome(t::BACKUP_DESTINATIONS_DESC));

    // Always present — the entry point for the first destination
    // (`backups.md` § Element IDs: "always present").
    out.push(Element::gesture_button(
        ids::BACKUP_DESTINATION_ADD_BUTTON,
        t::BACKUP_DESTINATION_ADD_BUTTON,
        true,
        Gesture::Backups(Action::OpenAddForm),
    ));

    if let Some(mode) = &st.form {
        form_elements(st, mode, &mut out);
    }
    if st.removing.is_some() {
        remove_confirm_elements(st, &mut out);
    }
    // The orphaned store and its reclaim gesture sit with the destination list
    // they are about — above the rows, like the sole-client warning, because
    // what they describe is a destination that is no longer there.
    let reseed_rows = reseed_rows(st, app.sync_agent.custodian_store().device_id());
    orphaned_store_elements(st, reseed_rows.contains(&None), &mut out);
    if st.reclaiming {
        reclaim_confirm_elements(&mut out);
    }
    reseed_elements(st, &mut out);

    if st.destinations.is_empty() {
        out.push(Element::chrome(t::BACKUP_DESTINATIONS_EMPTY));
    } else {
        // The standing warning for "every destination I have is one of my own
        // devices" (`backups.md` § Durability + labeling). Painted above the
        // rows it is about, and only while true — the `backup-audit-alert`
        // idiom. Not an alert: nothing is failing, the durability story is just
        // weaker than the user may assume.
        if fauna_core::data::every_destination_is_a_client_device(&st.destinations) {
            out.push(Element::label(
                ids::BACKUP_SOLE_CLIENT_DESTINATION_WARNING,
                t::BACKUP_SOLE_CLIENT_DESTINATION_WARNING,
            ));
        }
        for (i, dest) in st.destinations.iter().enumerate() {
            let reseed_here = reseed_rows.contains(&Some(dest.destination_id.clone()));
            status_row(st, i, dest, reseed_here, &mut out);
        }
    }

    // The snapshot half sits between the two ratified halves that shipped first,
    // exactly where this module's docs said it would land.
    snapshot_elements(st, &mut out);

    // Deliberately NOT inside the `else`: the restore half's local path works
    // with zero destinations configured, which is the state all three
    // `test_backups_restore.py` cases run in.
    restore_elements(st, &mut out);
    out
}

// ── Paint: the snapshot half (`ui/backups.md` § Snapshot-list shape) ─────────

/// The whole snapshot half, painted off one [`BackupsSnapshot`].
///
/// Every value here is READ from the machine — no derivation happens in this
/// file. That is the point of the ratified section: `last-backed-up` is the
/// machine's `last_backed_up` (not a selector column, not `last_change_at`),
/// each row's state and integrity are the machine's, and the busy gate is its
/// single `in_progress_op` rather than the six ad-hoc partial flags the shipped
/// apps grew.
fn snapshot_elements(st: &BackupsState, out: &mut Vec<Element>) {
    let page = st.snap();
    // Single-flight: while an op runs, EVERY mutating control is disabled
    // (§ *Create* ruling). Painted as one predicate so no control can drift off it.
    let busy = page.in_progress_op.is_some();

    out.push(Element::chrome(t::SNAPSHOTS_TITLE));

    // The selector — a raw-value picker over the set NAME, which is what
    // `select("backup-folder-selector", name)` takes on every native app.
    let names: Vec<String> = page.folders.iter().map(|fs| fs.name.clone()).collect();
    let selected = page.selected_folder.clone().unwrap_or_default();
    let no_sets = names.is_empty();
    out.push(Element {
        enabled: !no_sets && !busy,
        ..Element::select(
            ids::BACKUP_FOLDER_SELECTOR,
            selected,
            SelectTarget::BackupFolder,
            names,
        )
        .labelled(t::FOLDER_LABEL)
    });
    // A disabled control the user can see states why (Copy comprehensibility
    // rule 5) — the same shape the restore pickers use.
    if no_sets {
        out.push(Element::chrome(t::NO_FOLDERS));
    }

    // ONE non-indexed element = the selected set's newest snapshot `created_at`,
    // "never" when it has none. The derivation is the machine's.
    out.push(Element::label(
        ids::LAST_BACKED_UP,
        match page.last_backed_up {
            Some(at) => t::last_backed_up_at(&crate::format::format_epoch_us(
                at.saturating_mul(1_000_000),
            )),
            None => t::LAST_BACKED_UP_NEVER.to_string(),
        },
    ));

    // The action row. Create/prune/check are set-scoped, so all three need a
    // selection as well as an idle machine.
    let armed = !busy && page.selected_folder.is_some();
    out.push(Element::gesture_button(
        ids::SNAPSHOT_CREATE_BUTTON,
        t::CREATE_SNAPSHOT,
        armed,
        Gesture::Backups(Action::CreateSnapshot),
    ));
    out.push(Element::gesture_button(
        ids::SNAPSHOT_PRUNE_BUTTON,
        t::PRUNE_BUTTON,
        armed,
        Gesture::Backups(Action::PrunePreview),
    ));
    out.push(Element::gesture_button(
        ids::SNAPSHOT_CHECK_BUTTON,
        t::CHECK_BUTTON,
        armed,
        Gesture::Backups(Action::CheckIntegrity),
    ));
    // The busy line names WHICH op is running, so the disabled row above is not
    // left unexplained.
    if let Some(op) = page.in_progress_op {
        out.push(Element::chrome(busy_text(op)));
    }

    check_result_elements(&page, out);
    prune_preview_elements(&page, busy, out);

    // The list container is always painted, so the id never vanishes mid-poll.
    out.push(Element::label(ids::SNAPSHOT_LIST, String::new()));
    if page.snapshots.is_empty() {
        out.push(Element::chrome(t::NO_SNAPSHOTS));
    }
    for (index, row) in page.snapshots.iter().enumerate() {
        snapshot_row_elements(index, row, busy, out);
    }

    if let Some(id) = st.immediate_delete {
        immediate_delete_elements(st, id, out);
    }
    detail_elements(&page, out);
}

/// The `in_progress_op` line's text — the shared
/// [`fauna_backups_machine::busy_text`] decision (tui↔linux twin harvest;
/// previously hand-rolled identically here and on
/// linux).
fn busy_text(op: BackupOp) -> String {
    fauna_backups_machine::busy_text(op).resolve(fauna_i18n::strings::lookup)
}

/// One `snapshot-item[i]` and its per-row lifecycle affordances: the two delete
/// controls always, plus `snapshot-undelete-button` on a soft-deleted row.
///
/// The row carries its snapshot id as a **test attr**, which is a cross-app
/// contract rather than a test nicety (§ *Row content contract*) — it is linux's
/// shape, and it is what lets the immediate-delete friction bar read back the id
/// it must re-type.
fn snapshot_row_elements(
    index: usize,
    row: &fauna_backups_machine::SnapshotRow,
    busy: bool,
    out: &mut Vec<Element>,
) {
    let when = crate::format::format_epoch_us(row.created_at.saturating_mul(1_000_000));
    let mut text = t::snapshot_row(
        &row.id.to_string(),
        &when,
        &t::file_count(&row.file_count.to_string()),
        &crate::format::byte_size(row.total_bytes),
    );
    // Both suffix rules are the shared `fauna_backups_machine` decisions (same
    // lift as `busy_text`); timestamp rendering stays the tui's own door.
    let deadline = row
        .state
        .deadline()
        .map(|at| crate::format::format_epoch_us(at.saturating_mul(1_000_000)));
    for suffix in [
        fauna_backups_machine::snapshot_state_text(&row.state, deadline.as_deref()),
        fauna_backups_machine::snapshot_integrity_text(row.integrity),
    ]
    .into_iter()
    .flatten()
    {
        text.push_str("  ");
        text.push_str(&suffix.resolve(fauna_i18n::strings::lookup));
    }
    // Registered `.within(ids::SNAPSHOT_ITEM, index)` — i.e. INSIDE ITSELF, which
    // looks odd and is load-bearing. `Registry::matches` is a strict path-PREFIX
    // test, so the shared suite's `get_attr("snapshot-item", "snapshot-id",
    // scope="snapshot-item[i]")` — how every app hands the friction bar the id it
    // must re-type — can only resolve if the row's own path STARTS with its own
    // scope step. With an empty path the row paints perfectly and every scoped
    // read of it returns `Null`, which surfaces far away as "snapshot N never
    // appeared in the list" while the diagnostic prints the row (2026-08-05).
    // Unscoped reads are unaffected: an empty scope resolves to the whole frame,
    // so `count`/`get_text` still see exactly one entry per row.
    out.push(
        Element::label(ids::SNAPSHOT_ITEM, text)
            .attr("snapshot-id", row.id.to_string())
            .clickable(Gesture::Backups(Action::OpenSnapshot { index }))
            .within(ids::SNAPSHOT_ITEM, index),
    );
    out.push(
        Element::gesture_button(
            ids::SNAPSHOT_DELETE_BUTTON,
            t::SNAPSHOT_DELETE_BUTTON,
            !busy,
            Gesture::Backups(Action::DeleteSnapshot { index }),
        )
        .within(ids::SNAPSHOT_ITEM, index),
    );
    out.push(
        Element::gesture_button(
            ids::SNAPSHOT_IMMEDIATE_DELETE_BUTTON,
            t::IMMEDIATE_DELETE_BUTTON,
            !busy,
            Gesture::Backups(Action::OpenImmediateDelete { index }),
        )
        .within(ids::SNAPSHOT_ITEM, index),
    );
    // Recovery is offered ONLY out of `SoftDeleted` (§ *Soft-deleted rows*), so
    // the button's PRESENCE is the row-state observable — the same "present only
    // while the state stands" shape as `snapshot-prune-execute-button`. A row in
    // any other state paints no such control, and the machine refuses the
    // gesture regardless, so a keyboard actuation past the render cannot get
    // ahead of the rule.
    if matches!(row.state, SnapshotState::SoftDeleted { .. }) {
        out.push(
            Element::gesture_button(
                ids::SNAPSHOT_UNDELETE_BUTTON,
                t::SNAPSHOT_UNDELETE_BUTTON,
                !busy,
                Gesture::Backups(Action::UndeleteSnapshot { index }),
            )
            .within(ids::SNAPSHOT_ITEM, index),
        );
    }
}

/// The check verdict — a RESULT surface, never `error-message` (rule 6).
fn check_result_elements(page: &BackupsSnapshot, out: &mut Vec<Element>) {
    let Some(result) = &page.check_result else {
        return;
    };
    // The shared `is_ok()` predicate, called — never re-derived from counts.
    let text = if result.is_ok {
        t::check_result_ok(
            &result.snapshots_checked.to_string(),
            &result.files_checked.to_string(),
            &result.chunks_checked.to_string(),
        )
    } else {
        t::check_result_errors(
            &result.missing_manifests.to_string(),
            &result.missing_chunks.to_string(),
            &result.corrupt_manifests.to_string(),
        )
    };
    out.push(Element::label(ids::SNAPSHOT_CHECK_RESULT, text));
}

/// The prune preview — execute is offered ONLY from here (§ *Prune* ruling), and
/// the two no-op policy states say why nothing would be pruned rather than
/// showing an empty success.
///
/// `snapshot-prune-preview` is a "present only while one stands" scope, exactly
/// like `snapshot-detail-files`: the early return above leaves it unregistered,
/// so its presence IS the standing-preview state and no test needs to read a
/// count to know one.
fn prune_preview_elements(page: &BackupsSnapshot, busy: bool, out: &mut Vec<Element>) {
    let Some(preview) = &page.prune_preview else {
        return;
    };
    // The VERDICT rides the id'd element's OWN text, beside the title. What this
    // surface promises is that it says *why* the count is what it is — "nothing
    // to prune" is a different answer from "no retention policy configured for
    // this set", and the two are indistinguishable from outside when the only
    // observables are the preview's presence and the execute button's absence
    // (§ Errors & edge cases — *Prune with no candidates*, which rules all three
    // states typed off the reply). Painting the sentence as unregistered
    // `chrome` under the id left exactly that hole: the page said it on screen
    // and nothing could read which of the two it had said.
    let verdict = match preview.policy_state {
        PolicyState::NotSet => t::PRUNE_POLICY_NOT_SET.to_string(),
        PolicyState::Unparseable => t::PRUNE_POLICY_UNPARSEABLE.to_string(),
        PolicyState::Applied if preview.candidates.is_empty() => {
            t::PRUNE_PREVIEW_NOTHING.to_string()
        }
        PolicyState::Applied => t::prune_preview_counts(
            &preview.would_prune.to_string(),
            &preview.remaining.to_string(),
        ),
    };
    out.push(Element::label(
        ids::SNAPSHOT_PRUNE_PREVIEW,
        format!("{}  {verdict}", t::PRUNE_PREVIEW_TITLE),
    ));
    // The candidate list stays chrome under the preview: ui.yaml scopes the rows
    // no id of their own, and the verdict line above already carries the counts.
    if matches!(preview.policy_state, PolicyState::Applied) {
        for candidate in &preview.candidates {
            out.push(
                Element::chrome(t::prune_preview_candidate(
                    &candidate.id.to_string(),
                    &crate::format::format_epoch_us(candidate.created_at.saturating_mul(1_000_000)),
                ))
                .within(ids::SNAPSHOT_PRUNE_PREVIEW, 0),
            );
        }
    }
    // Execute only when the preview actually names something to delete: an
    // armed button over zero candidates would promise an effect it cannot have.
    // Its PRESENCE is therefore the cross-app observable for "this preview found
    // a candidate" — which is what lets an e2e test tell a dry run from an
    // executed prune without reading a row count (the nest's list keeps
    // soft-deleted rows, so the count does not move).
    let executable = matches!(preview.policy_state, PolicyState::Applied)
        && !preview.candidates.is_empty()
        && !busy;
    if executable {
        out.push(Element::gesture_button(
            ids::SNAPSHOT_PRUNE_EXECUTE_BUTTON,
            t::PRUNE_EXECUTE_BUTTON,
            true,
            Gesture::Backups(Action::PruneExecute),
        ));
    }
    out.push(Element::gesture_button(
        ids::SNAPSHOT_PRUNE_CANCEL_BUTTON,
        t::PRUNE_CANCEL_BUTTON,
        true,
        Gesture::Backups(Action::PruneCancel),
    ));
}

/// The immediate-delete modal — the friction bar (§ Architectural rules, rule 4).
///
/// The confirm button's enabled flag is the MACHINE's predicate, not a local
/// re-derivation: `deleting` is the half every shipped app got wrong, and the
/// machine is the only thing that knows it.
fn immediate_delete_elements(st: &BackupsState, id: i64, out: &mut Vec<Element>) {
    out.push(Element::label(
        ids::IMMEDIATE_DELETE_CONFIRM_MODAL,
        t::immediate_delete_modal_title(&id.to_string()),
    ));
    out.push(
        Element::chrome(t::IMMEDIATE_DELETE_WARNING).within(ids::IMMEDIATE_DELETE_CONFIRM_MODAL, 0),
    );
    out.push(
        Element::input(
            ids::IMMEDIATE_DELETE_CONFIRM_INPUT,
            st.immediate_confirm.clone(),
            Field::Backups(BackupsField::ImmediateConfirm),
        )
        .labelled(t::IMMEDIATE_DELETE_CONFIRM_ID_PLACEHOLDER)
        .within(ids::IMMEDIATE_DELETE_CONFIRM_MODAL, 0),
    );
    out.push(
        Element::input(
            ids::IMMEDIATE_DELETE_ACKNOWLEDGE_INPUT,
            st.immediate_ack.clone(),
            Field::Backups(BackupsField::ImmediateAck),
        )
        .labelled(t::IMMEDIATE_DELETE_ACKNOWLEDGE_PLACEHOLDER)
        .within(ids::IMMEDIATE_DELETE_CONFIRM_MODAL, 0),
    );
    let armed = st.machine.as_ref().is_some_and(|m| {
        m.immediate_delete_enabled(
            st.immediate_confirm.clone(),
            id.to_string(),
            st.immediate_ack.clone(),
        )
    });
    out.push(
        Element::gesture_button(
            ids::IMMEDIATE_DELETE_CONFIRM_BUTTON,
            t::IMMEDIATE_DELETE_CONFIRM_BUTTON,
            armed,
            Gesture::Backups(Action::ConfirmImmediateDelete),
        )
        .within(ids::IMMEDIATE_DELETE_CONFIRM_MODAL, 0),
    );
    out.push(
        Element::gesture_button(
            ids::IMMEDIATE_DELETE_CANCEL_BUTTON,
            t::IMMEDIATE_DELETE_CANCEL_BUTTON,
            true,
            Gesture::Backups(Action::CancelImmediateDelete),
        )
        .within(ids::IMMEDIATE_DELETE_CONFIRM_MODAL, 0),
    );
}

/// The open snapshot's file list, with a per-file download button.
///
/// Painted only while a snapshot is open — `snapshot-detail-files` is a
/// "present only while open" scope, and the machine drops the detail whenever
/// its snapshot stops being listed, so this cannot outlive its rows.
fn detail_elements(page: &BackupsSnapshot, out: &mut Vec<Element>) {
    let Some(detail) = &page.detail else {
        return;
    };
    out.push(Element::label(
        ids::SNAPSHOT_DETAIL_FILES,
        t::snapshot(&detail.snapshot_id.to_string()),
    ));
    if detail.files.is_empty() {
        out.push(Element::chrome(t::NO_FILES_IN_SNAPSHOT).within(ids::SNAPSHOT_DETAIL_FILES, 0));
    }
    for (index, file) in detail.files.iter().enumerate() {
        out.push(
            Element::chrome(format!(
                "{}  {}",
                file.path,
                crate::format::byte_size(file.size_bytes)
            ))
            .within(ids::SNAPSHOT_DETAIL_FILES, 0),
        );
        // Only a regular file has bytes to fetch; a dir/symlink row paints
        // without the affordance rather than with a dead one.
        if file.file_type == "regular" {
            out.push(
                Element::gesture_button(
                    ids::SNAPSHOT_FILE_DOWNLOAD_BUTTON,
                    t::DOWNLOAD,
                    true,
                    Gesture::Backups(Action::DownloadFile { index }),
                )
                .within(ids::SNAPSHOT_DETAIL_FILES, 0),
            );
        }
    }
    // ui.yaml scopes no id to the close control, so it paints untagged — the
    // same call the divergence modal's close makes.
    out.push(Element::gesture_button(
        String::new(),
        t::RESTORE_DIVERGENCE_CLOSE,
        true,
        Gesture::Backups(Action::CloseSnapshotDetail),
    ));
}

/// The add/edit dialog — painted only while open, which is what ui.yaml's
/// "present only while open" scope for these five ids means.
fn form_elements(st: &BackupsState, mode: &FormMode, out: &mut Vec<Element>) {
    let title = match mode {
        FormMode::Add => t::BACKUP_DESTINATION_FORM_ADD_TITLE,
        FormMode::Edit(_) => t::BACKUP_DESTINATION_FORM_EDIT_TITLE,
    };
    out.push(Element::label(ids::BACKUP_DESTINATION_ADD_MODAL, title));
    let adding = matches!(mode, FormMode::Add);
    let custodian = st.kind_input == DestinationKindChoice::ClientDevice;

    // The kind select — add-only as an editable control. Painted (disabled) in
    // edit mode too, rather than vanishing, so the row's kind stays legible while
    // renaming it; the kind itself is not an editable property
    // (`backups.md` § Create / edit / remove).
    out.push(
        Element {
            enabled: adding,
            ..Element::select(
                ids::BACKUP_DESTINATION_KIND_SELECT,
                // A RAW-VALUE picker: `get_text`/`select` round-trip the wire
                // discriminator, and the human label rides `display_value`. The
                // options a driver may name are therefore the same two strings
                // on all 7 apps, and they survive translation — a label-keyed
                // select silently becomes undriveable the moment the copy is
                // reworded or localized.
                st.kind_input.wire(),
                SelectTarget::BackupDestinationKind,
                DestinationKindChoice::all()
                    .iter()
                    .map(|c| c.wire().to_string())
                    .collect(),
            )
        }
        .display_value(st.kind_input.label())
        .labelled(t::BACKUP_DESTINATION_KIND_SELECT_LABEL)
        .within(ids::BACKUP_DESTINATION_ADD_MODAL, 0),
    );

    // The URL is the nest kind's field — ui.yaml scopes it "nest kind only". A
    // custodian has no address at all, so painting an empty URL box for it would
    // invite the user to type one that nothing could ever use.
    if !custodian {
        out.push(
            Element::input(
                ids::BACKUP_DESTINATION_URL_INPUT,
                st.url_input.clone(),
                Field::Backups(BackupsField::Url),
            )
            .labelled(t::BACKUP_DESTINATION_URL_PLACEHOLDER)
            .within(ids::BACKUP_DESTINATION_ADD_MODAL, 0),
        );
    }
    out.push(
        Element::input(
            ids::BACKUP_DESTINATION_NAME_INPUT,
            st.name_input.clone(),
            Field::Backups(BackupsField::Name),
        )
        .labelled(t::BACKUP_DESTINATION_NAME_PLACEHOLDER)
        .within(ids::BACKUP_DESTINATION_ADD_MODAL, 0),
    );
    // The capacity cap — the client-device kind's only knob, so it paints only
    // for that kind (ui.yaml: "this-device kind only").
    if custodian {
        out.push(
            Element::input(
                ids::BACKUP_DESTINATION_CAPACITY_INPUT,
                st.capacity_input.clone(),
                Field::Backups(BackupsField::Capacity),
            )
            .labelled(t::BACKUP_DESTINATION_CAPACITY_PLACEHOLDER)
            .within(ids::BACKUP_DESTINATION_ADD_MODAL, 0),
        );
        // The honest statement `backups.md` § Threat model requires at opt-in:
        // a full offline corpus is a materially different exposure from an
        // ordinary logged-in device, and the user must see it here rather than
        // discover it. Untagged — it is glue copy, not a new automatable id.
        out.push(Element::chrome(t::BACKUP_DESTINATION_CUSTODIAN_EXPOSURE));
    }
    // Disabled on a blank URL for the nest kind: a destination with no URL has
    // nothing to resolve, and the enroll's first step would fail with a transport
    // error that reads like a network problem. The custodian kind has no URL to
    // require, so its confirm is always live — a blank cap is a real choice
    // (uncapped) and a malformed one is refused at submit with its own message.
    // Which ceremony this press runs is decided HERE, where both halves of the
    // discriminant are already in hand, and carried on the gesture — so the
    // offline gate can answer the exact kind (`SubmitTarget`).
    let target = match (adding, custodian) {
        (false, _) => SubmitTarget::Edit,
        (true, true) => SubmitTarget::AddCustodian,
        (true, false) => SubmitTarget::AddNest,
    };
    out.push(
        Element::gesture_button(
            ids::BACKUP_DESTINATION_ADD_CONFIRM_BUTTON,
            t::BACKUP_DESTINATION_ADD_CONFIRM,
            custodian || !st.url_input.trim().is_empty(),
            Gesture::Backups(Action::SubmitForm(target)),
        )
        .within(ids::BACKUP_DESTINATION_ADD_MODAL, 0),
    );
    out.push(
        Element::gesture_button(
            ids::BACKUP_DESTINATION_ADD_CANCEL_BUTTON,
            t::BACKUP_DESTINATION_ADD_CANCEL,
            true,
            Gesture::Backups(Action::CancelForm),
        )
        .within(ids::BACKUP_DESTINATION_ADD_MODAL, 0),
    );
}

/// The remove-confirm dialog — painted only while armed. Its warning text is the
/// shared string; the consequence it states (the coordinator reclaims the
/// offsite copy) is `backups.md` § Remove.
fn remove_confirm_elements(st: &BackupsState, out: &mut Vec<Element>) {
    out.push(Element::label(
        ids::BACKUP_DESTINATION_REMOVE_CONFIRM_MODAL,
        t::BACKUP_DESTINATION_REMOVE_CONFIRM_TITLE,
    ));
    // The opt-in, on client-device rows ONLY (`backups.md` § Remove): removal
    // keeps the local sealed store by design, and this is the affordance for
    // "and free it now — the moment the intent forms". A nest destination has
    // no local copy to offer, so painting it there would be an inert control.
    if st.removing_a_client_device() {
        out.push(
            Element::checkbox_gesture(
                ids::BACKUP_DESTINATION_REMOVE_RECLAIM_CHECKBOX,
                t::BACKUP_DESTINATION_REMOVE_RECLAIM_CHECKBOX,
                st.remove_reclaim,
                Gesture::Backups(Action::ToggleRemoveReclaim),
            )
            .within(ids::BACKUP_DESTINATION_REMOVE_CONFIRM_MODAL, 0),
        );
    }
    out.push(
        Element::gesture_button(
            ids::BACKUP_DESTINATION_REMOVE_CONFIRM_BUTTON,
            t::BACKUP_DESTINATION_REMOVE_CONFIRM_BUTTON,
            true,
            Gesture::Backups(Action::ConfirmRemove),
        )
        .within(ids::BACKUP_DESTINATION_REMOVE_CONFIRM_MODAL, 0),
    );
    out.push(
        Element::gesture_button(
            ids::BACKUP_DESTINATION_REMOVE_CANCEL_BUTTON,
            t::BACKUP_DESTINATION_REMOVE_CANCEL_BUTTON,
            true,
            Gesture::Backups(Action::CancelRemove),
        )
        .within(ids::BACKUP_DESTINATION_REMOVE_CONFIRM_MODAL, 0),
    );
}

/// The orphaned-store row and its reclaim button — painted only while this
/// device holds a sealed store no destination row claims
/// (`backups.md` § Manage backup destinations → *Reclaim this device's copy*).
///
/// **Why the gesture hangs here and not on a destination row.** Removing a
/// client-device destination deliberately keeps the local store — it is the
/// owner's only offline copy (3c-ii) — so by the time a user wants the space
/// back there is no destination row left to hang a button off, and without this
/// row the disk space is unrecoverable from the app.
///
/// The verdict is `st.orphaned_store`, reached in the op through the shared
/// `custodian_store_is_orphaned`; nothing is decided here. The row carries the
/// button `.within` it, the `backup-destination-status-row` containment idiom,
/// so a scoped read resolves exactly this row's control.
fn orphaned_store_elements(st: &BackupsState, reseed_here: bool, out: &mut Vec<Element>) {
    let Some(bytes) = st.orphaned_store else {
        return;
    };
    out.push(Element::label(
        ids::BACKUP_ORPHANED_STORE_ROW,
        crate::format::orphaned_store(bytes),
    ));
    // Restore first, reclaim second: on a rebuilt nest this row is where the
    // owner's only copy is offered back, and the destructive gesture beside it
    // must not be the first one met.
    if reseed_here {
        out.push(reseed_button(st).within(ids::BACKUP_ORPHANED_STORE_ROW, 0));
    }
    out.push(
        Element::gesture_button(
            ids::BACKUP_DESTINATION_RECLAIM_BUTTON,
            t::BACKUP_DESTINATION_RECLAIM_BUTTON,
            true,
            Gesture::Backups(Action::OpenReclaimConfirm),
        )
        .within(ids::BACKUP_ORPHANED_STORE_ROW, 0),
    );
}

/// `backup-destination-reseed-button`, disarmed while a run is in flight.
fn reseed_button(st: &BackupsState) -> Element {
    Element::gesture_button(
        ids::BACKUP_DESTINATION_RESEED_BUTTON,
        t::BACKUP_DESTINATION_RESEED_BUTTON,
        st.reseed != ReseedView::Running,
        Gesture::Backups(Action::OpenReseedConfirm),
    )
}

/// The re-seed confirm modal and the result view (`ui/backups.md` § Restore
/// after losing the nest). The result's lines are the shared
/// `fauna_client_backup::reseed::result_lines`, verdict first; nothing about
/// what "restored" means is decided here.
fn reseed_elements(st: &BackupsState, out: &mut Vec<Element>) {
    match &st.reseed {
        ReseedView::Closed => {}
        ReseedView::Confirming => {
            out.push(Element::label(
                ids::BACKUP_DESTINATION_RESEED_CONFIRM_MODAL,
                t::BACKUP_RESEED_CONFIRM_TITLE,
            ));
            // The consequence, stated where the decision is made (the reclaim
            // modal's idiom); ui.yaml scopes no id to it.
            out.push(Element::chrome(t::BACKUP_RESEED_CONFIRM_BODY));
            out.push(
                Element::gesture_button(
                    ids::BACKUP_DESTINATION_RESEED_CONFIRM_BUTTON,
                    t::BACKUP_RESEED_CONFIRM_BUTTON,
                    true,
                    Gesture::Backups(Action::ConfirmReseed),
                )
                .within(ids::BACKUP_DESTINATION_RESEED_CONFIRM_MODAL, 0),
            );
            out.push(
                Element::gesture_button(
                    ids::BACKUP_DESTINATION_RESEED_CANCEL_BUTTON,
                    t::BACKUP_RESEED_CANCEL_BUTTON,
                    true,
                    Gesture::Backups(Action::CancelReseed),
                )
                .within(ids::BACKUP_DESTINATION_RESEED_CONFIRM_MODAL, 0),
            );
        }
        ReseedView::Running => out.push(Element::label(
            ids::BACKUP_DESTINATION_RESEED_RESULT,
            t::BACKUP_RESEED_RUNNING,
        )),
        ReseedView::Done(result) => {
            let text = fauna_client_backup::reseed::result_lines(result)
                .iter()
                .map(|line| line.resolve_nested(fauna_i18n::strings::lookup))
                .collect::<Vec<_>>()
                .join("\n");
            out.push(Element::label(ids::BACKUP_DESTINATION_RESEED_RESULT, text));
        }
    }
}

/// The reclaim-confirm dialog — a **plain** confirm, no re-type.
///
/// The asymmetry with `snapshot-immediate-delete`'s friction bar is ratified and
/// deliberate: reclaiming destroys this device's standalone-restore property,
/// which is why it gets a modal at all, but the store itself is re-buildable
/// from a fresh pull whenever the device re-enrolls — so the bytes are not
/// irrecoverable and a re-type would be friction with no payoff.
fn reclaim_confirm_elements(out: &mut Vec<Element>) {
    out.push(Element::label(
        ids::BACKUP_RECLAIM_CONFIRM_MODAL,
        t::BACKUP_RECLAIM_CONFIRM_TITLE,
    ));
    // The consequence, stated where the decision is made rather than left to
    // the button label (Copy comprehensibility rule 5). ui.yaml scopes no id to
    // it, so it paints untagged.
    out.push(Element::chrome(t::BACKUP_RECLAIM_CONFIRM_BODY));
    out.push(
        Element::gesture_button(
            ids::BACKUP_RECLAIM_CONFIRM_BUTTON,
            t::BACKUP_RECLAIM_CONFIRM_BUTTON,
            true,
            Gesture::Backups(Action::ConfirmReclaim),
        )
        .within(ids::BACKUP_RECLAIM_CONFIRM_MODAL, 0),
    );
    out.push(
        Element::gesture_button(
            ids::BACKUP_RECLAIM_CANCEL_BUTTON,
            t::BACKUP_RECLAIM_CANCEL_BUTTON,
            true,
            Gesture::Backups(Action::CancelReclaim),
        )
        .within(ids::BACKUP_RECLAIM_CONFIRM_MODAL, 0),
    );
}

/// The indexed `backup-audit-alert` banners — one per destination in a failing
/// state, none at all when everything is healthy.
///
/// Painted **flat**, not `.within` anything: `test_backups.py` reads them with a
/// bare `count("backup-audit-alert")` / the shared suite never scopes them, and
/// declaring containment a reader does not use is what silently empties a scoped
/// query (the `restore-history-item` lesson, inverted — read the shared action
/// before choosing the shape).
///
/// Which verdicts are loud is **not** decided here: `DestinationAuditRecord::
/// alert_reasons(now)` is the single shared answer (built on `alert_reason()`,
/// through which `AuditVerdict::is_alerting` is defined), so this client cannot
/// drift into alerting on, say, a
/// transient `Unreachable` — the laptop-on-a-plane case the loop deliberately
/// keeps quiet.
fn audit_alert_elements(st: &BackupsState, out: &mut Vec<Element>) {
    // A client-device custodian reporting its OWN copy as failing — the only
    // failure signal that exists for a kind the owner-side loop can never
    // sample. Withholding it silences the row, and silence reads as the
    // sleeping-device case: the wrong alarm, thirty days late
    // (`backup-destinations.md` § Custodian contract, question 4). Which
    // reported states are loud is not decided here either —
    // `fauna_core::format::backup_self_audit_is_alerting` is the single answer,
    // and it keeps both silence and an unrecognised newer value QUIET.
    for dest in &st.destinations {
        let status = st.statuses.get(&dest.destination_id);
        if fauna_core::format::backup_self_audit_is_alerting(
            status.and_then(|s| s.audit_state.as_deref()),
        ) {
            out.push(Element::label(
                ids::BACKUP_AUDIT_ALERT,
                crate::format::backup_audit_alert(
                    fauna_core::format::BackupAuditAlertReason::SelfReported,
                    &destination_label(dest),
                ),
            ));
        }
    }
    // Every reason the shared door yields — the standing verdict's, then an
    // accepted source regression's self-expiring recovery notice while its
    // window is open (`backups.md` § Audit-alert surface, the fifth reason).
    let now = crate::backup_audit::now_secs();
    for record in &st.audit {
        let reasons = record.alert_reasons(now);
        if reasons.is_empty() {
            continue;
        }
        // Name the destination the way every other row does (shared
        // display-name-else-URL-host fallback), so a banner and its row agree.
        let dest_label = st
            .destinations
            .iter()
            .find(|d| d.destination_id == record.state.destination_id)
            .map(destination_label)
            .unwrap_or_else(|| record.state.destination_id.clone());
        for reason in reasons {
            out.push(Element::label(
                ids::BACKUP_AUDIT_ALERT,
                crate::format::backup_audit_alert(reason, &dest_label),
            ));
        }
    }
}

/// One `backup-destination-status-row` and its five members, each registered
/// `.within(ids::BACKUP_DESTINATION_STATUS_ROW, i)` (module docs — the scoped
/// reads the shared suite makes depend on it).
fn status_row(
    st: &BackupsState,
    i: usize,
    dest: &BackupDestination,
    reseed_here: bool,
    out: &mut Vec<Element>,
) {
    let status = st.statuses.get(&dest.destination_id);
    // The row's own text is the display label — `get_text("backup-destination-
    // status-row", index=i)` is how every app's suite reads which destination
    // a row is.
    out.push(Element::label(
        ids::BACKUP_DESTINATION_STATUS_ROW,
        destination_label(dest),
    ));
    // The kind badge — the visible half of "a client custodian never silently
    // satisfies *you have an off-site backup*" (`backups.md` § Durability +
    // labeling). Reads the row's own discriminator through the shared label, so
    // an unknown kind a newer client wrote renders as itself rather than
    // masquerading as a nest.
    out.push(
        Element::label(
            ids::BACKUP_DESTINATION_KIND_BADGE,
            crate::format::backup_destination_kind(&dest.kind),
        )
        .within(ids::BACKUP_DESTINATION_STATUS_ROW, i),
    );
    out.push(
        Element::label(
            ids::BACKUP_DESTINATION_LAST_UPLOAD_TIME,
            last_upload_text(status),
        )
        .within(ids::BACKUP_DESTINATION_STATUS_ROW, i),
    );
    // Usage — client-device rows only (ui.yaml). A nest row has no cap and no
    // held-bytes report, so the element is absent rather than empty.
    if matches!(
        dest.kind_view(),
        fauna_core::data::DestinationKind::ClientDevice { .. }
    ) {
        out.push(
            Element::label(
                ids::BACKUP_DESTINATION_USAGE,
                crate::format::backup_usage(
                    status.and_then(|s| s.held_bytes),
                    dest.capacity_cap_bytes,
                    status.and_then(|s| s.cap_state.as_deref()),
                ),
            )
            .within(ids::BACKUP_DESTINATION_STATUS_ROW, i),
        );
    }
    out.push(
        Element::label(ids::BACKUP_DESTINATION_BACKLOG_COUNT, backlog_text(status))
            .within(ids::BACKUP_DESTINATION_STATUS_ROW, i),
    );
    // last-audit-time — who audited depends on the KIND, and the wording says
    // which (`backups.md` § Audit-alert surface → *The client-device arm*).
    //
    // A nest row carries this client's own independent check, "never" until one
    // passes. A client-device custodian has no address for that loop to reach —
    // inclusion-sampling a sleeping device is structurally impossible — so its
    // cell carries the custodian's own self-audit, arriving on the status row
    // from the check-in. Rendering the owner-side wording over a self-report
    // would let it wear the words of an independent verification; rendering the
    // owner-side loop's permanent silence would read as "never checked" for a
    // device that is checking itself every pass. Both are wrong in the quiet
    // direction, which is the direction this page exists to refuse.
    //
    // Keyed by `destination_id` rather than by index: `merge_outcomes` orders
    // its result like the destination list, but a row must never inherit
    // another destination's verdict if that ever stops holding.
    let is_client_device = matches!(
        dest.kind_view(),
        fauna_core::data::DestinationKind::ClientDevice { .. }
    );
    out.push(
        Element::label(
            ids::BACKUP_DESTINATION_LAST_AUDIT_TIME,
            if is_client_device {
                self_audit_text(status)
            } else {
                last_audit_text(
                    st.audit
                        .iter()
                        .find(|r| r.state.destination_id == dest.destination_id),
                )
            },
        )
        .within(ids::BACKUP_DESTINATION_STATUS_ROW, i),
    );
    out.push(
        Element::gesture_button(
            ids::BACKUP_DESTINATION_EDIT_BUTTON,
            t::BACKUP_DESTINATION_EDIT_BUTTON,
            true,
            Gesture::Backups(Action::OpenEditForm { index: i }),
        )
        .within(ids::BACKUP_DESTINATION_STATUS_ROW, i),
    );
    out.push(
        Element::gesture_button(
            ids::BACKUP_DESTINATION_REMOVE_BUTTON,
            t::BACKUP_DESTINATION_REMOVE_BUTTON,
            true,
            Gesture::Backups(Action::OpenRemoveConfirm { index: i }),
        )
        .within(ids::BACKUP_DESTINATION_STATUS_ROW, i),
    );
    // "Restore my data to this nest" — only on the row naming a copy THIS
    // device holds (`ui/backups.md` § Restore after losing the nest).
    if reseed_here {
        out.push(reseed_button(st).within(ids::BACKUP_DESTINATION_STATUS_ROW, i));
    }
    // The post-succession review mark and its Keep half — present only while
    // this row is actually raised (`succession-aftermath.md` § Adjudicating what
    // the aftermath carries across). Absent rather than empty on an ordinary
    // row, which is what keeps the mark meaningful: in a healthy account every
    // destination is the owner's own, and a permanently-rendered element would
    // train the user straight past the one succession that matters.
    //
    // Remove is deliberately NOT re-rendered here — the row already carries
    // `backup-destination-remove-button` above, so Keep joins the affordance
    // that exists instead of minting a second removal path.
    if DestinationUnattestedMark::row_is_raised(&st.unattested_destination_marks, dest) {
        out.push(
            Element::label(
                ids::BACKUP_DESTINATION_UNATTESTED_MARK,
                t::BACKUP_DESTINATION_UNATTESTED_MARK,
            )
            .within(ids::BACKUP_DESTINATION_STATUS_ROW, i),
        );
        out.push(
            Element::gesture_button(
                ids::BACKUP_DESTINATION_KEEP_BUTTON,
                t::BACKUP_DESTINATION_KEEP_BUTTON,
                true,
                Gesture::Backups(Action::KeepDestination { index: i }),
            )
            .within(ids::BACKUP_DESTINATION_STATUS_ROW, i),
        );
    }
}

// ── Paint: the restore half ──────────────────────────────────────────────────

/// One `restore-snapshot-select` option, via the shared
/// [`fauna_client_snapshots::snapshot_restore_option_label`] — this body and
/// linux's were byte-identical, so both now defer to the one implementation
/// (android/apple/windows consume the same logic over UniFFI).
fn snapshot_label(row: &SnapshotSummaryRow) -> String {
    fauna_client_snapshots::snapshot_restore_option_label(row.message_kind.as_deref(), row.id)
}

/// The restore half: the local-restore action card, the restore-history
/// section, then the forensic modal when one is open. Painted below the
/// destination section on the same page.
fn restore_elements(st: &BackupsState, out: &mut Vec<Element>) {
    out.push(Element::chrome(t::RESTORE_LOCAL_TITLE));

    // `restore-source-select` — the configured destinations, disabled at zero
    // (`backups.md:59`), which is windows' shape for the same element.
    //
    // Selecting one is remembered and painted, but it does NOT re-point the
    // picker below: listing snapshots held *at a destination* is the
    // cross-location disaster-recovery pull, and that path is unbuilt — every
    // `source_member_id` is `None` today (`backups.md:163-164`) and the
    // destination-side listing lands with the segment-backup rollout. What the
    // confirm button restores from is therefore the LOCAL snapshot picker,
    // exactly as on linux, android and web.
    let sources: Vec<String> = st.destinations.iter().map(destination_label).collect();
    let selected_source = sources.get(st.selected_source).cloned().unwrap_or_default();
    let sources_empty = sources.is_empty();
    out.push(Element {
        enabled: !sources_empty,
        // Prompted, so the row says what it picks even with nothing to pick:
        // an empty unlabelled select paints as a bare `<  >`, which is pure
        // noise (`apps/tui.md` § Rendering — a Select paints `prompt: < value >`).
        ..Element::select(
            ids::RESTORE_SOURCE_SELECT,
            selected_source,
            SelectTarget::RestoreSource,
            sources,
        )
        .labelled(t::RESTORE_SOURCE_LABEL)
    });
    // …and, disabled, it states WHY — the `restore-snapshot-select` empty-line
    // shape below, which this picker was missing (Copy comprehensibility rule 5:
    // no user-reachable disabled control without a visible reason).
    if sources_empty {
        out.push(Element::chrome(t::RESTORE_NO_DESTINATIONS));
    }

    // `restore-snapshot-select` — always painted so the id never vanishes
    // mid-poll; disabled (and followed by the shared empty line) when the owner
    // has no snapshots to restore.
    let labels: Vec<String> = st.snapshots.iter().map(snapshot_label).collect();
    let selected_snapshot = labels
        .get(st.selected_snapshot)
        .cloned()
        .unwrap_or_default();
    out.push(Element {
        enabled: !labels.is_empty(),
        ..Element::select(
            ids::RESTORE_SNAPSHOT_SELECT,
            selected_snapshot,
            SelectTarget::RestoreSnapshot,
            labels,
        )
        .labelled(t::RESTORE_SNAPSHOT_LABEL)
    });
    if st.snapshots.is_empty() {
        out.push(Element::chrome(t::RESTORE_NO_SNAPSHOTS));
    }

    // The kinds set. ui.yaml types the container a `component` and gives it no
    // text of its own — it is a pure grouping node, so its two children carry
    // the labels and it registers with an empty string rather than an invented
    // heading.
    out.push(Element::label(ids::RESTORE_KINDS_CHECKBOXES, String::new()));
    for (index, (_kind, label)) in RESTORE_KINDS.iter().enumerate() {
        out.push(
            Element::checkbox_gesture(
                ids::RESTORE_KIND_CHECKBOX,
                *label,
                st.restore_kinds.checked(index),
                Gesture::Backups(Action::ToggleRestoreKind { index }),
            )
            .within(ids::RESTORE_KINDS_CHECKBOXES, 0),
        );
    }

    // The friction bar (`backups.md:62-63`) — the same exact-match gate tui
    // already uses for `settings-delete-confirm-field`.
    out.push(
        Element::input(
            ids::RESTORE_CONFIRM_INPUT,
            st.restore_confirm.clone(),
            Field::Backups(BackupsField::RestoreConfirm),
        )
        .labelled(t::RESTORE_CONFIRM_PLACEHOLDER),
    );
    out.push(Element::gesture_button(
        ids::RESTORE_CONFIRM_BUTTON,
        t::RESTORE_CONFIRM_BUTTON,
        st.restore_armed(),
        Gesture::Backups(Action::SubmitRestore),
    ));
    out.push(Element::label(
        ids::RESTORE_PROGRESS,
        st.restore_progress.text(),
    ));
    // The completed-with-caveat advisory: its own id beside the progress line,
    // never `error-message` — the restore succeeded.
    if st.restore_config_absent {
        out.push(Element::label(
            ids::RESTORE_WARNING,
            t::RESTORE_WARNING_CONFIG_ABSENT,
        ));
    }

    // The history section. Section, list and items all paint FLAT — see the
    // module docs. The banner must be registered INSIDE `restore-history-item[i]`
    // for the shared single-step scoped read to resolve; since the 2026-08-14
    // descendant ruling, nesting the items under the list container would no
    // longer break that, but flat is the shape this page already has.
    out.push(Element::label(
        ids::RESTORE_HISTORY_SECTION,
        t::RESTORE_SECTION_TITLE,
    ));
    out.push(Element::label(ids::RESTORE_HISTORY_LIST, String::new()));
    for (index, row) in st.restore_history.iter().enumerate() {
        history_row(st, index, row, out);
    }

    if let Some(index) = st.divergence_modal {
        divergence_modal_elements(st, index, out);
    }
}

/// One `restore-history-item` and, when that snapshot diverged, its banner.
fn history_row(st: &BackupsState, index: usize, row: &RestoreHistoryRow, out: &mut Vec<Element>) {
    // `source_member_id` None → the shared "local snapshot" label; Some → the
    // SHARED short hex, one source of truth so the seven apps never drift
    // (`backups.md:311`). Every row is `None` today, so an all-"local snapshot"
    // list is correct rather than a bug.
    let source = match &row.source_member_id {
        None => t::RESTORE_SOURCE_LOCAL.to_string(),
        Some(member) => fauna_core::format::hex_short(member),
    };
    // The shared relative-time bucketing, not linux's raw epoch dump (whose own
    // comment calls the precision a follow-up) — `completed_at` is a past event,
    // which is exactly what that bucketing is for.
    let when = crate::format::format_epoch_us(row.completed_at.saturating_mul(1_000_000));
    out.push(Element::label(
        ids::RESTORE_HISTORY_ITEM,
        t::restore_history_row(&row.kinds_restored, &source, &when),
    ));

    let diverged = st
        .divergence
        .get(&row.snapshot_id)
        .map_or(0, |rows| rows.len());
    if diverged > 0 {
        out.push(
            Element::gesture_button(
                ids::RESTORE_DIVERGENCE_BANNER,
                t::restore_divergence_banner(&diverged.to_string()),
                true,
                Gesture::Backups(Action::OpenDivergenceModal { index }),
            )
            .within(ids::RESTORE_HISTORY_ITEM, index),
        );
    }
}

/// The forensic details modal — one row per (collection, MUA), the lost-writes
/// footer, and a close control. No action buttons: the server's state won and
/// the list only records what the MUA lost (`backups.md:76`).
fn divergence_modal_elements(st: &BackupsState, index: usize, out: &mut Vec<Element>) {
    let Some(row) = st.restore_history.get(index) else {
        return;
    };
    let rows = st
        .divergence
        .get(&row.snapshot_id)
        .map(Vec::as_slice)
        .unwrap_or_default();

    out.push(Element::label(
        ids::RESTORE_DIVERGENCE_DETAILS_MODAL,
        t::RESTORE_DIVERGENCE_MODAL_TITLE,
    ));
    for entry in rows {
        let mua = entry
            .mua_id
            .clone()
            .unwrap_or_else(|| t::RESTORE_DIVERGENCE_UNKNOWN_MUA.to_string());
        out.push(
            Element::label(
                ids::RESTORE_DIVERGENCE_DETAILS_ITEM,
                t::restore_divergence_detail_row(
                    &entry.collection,
                    &mua,
                    &entry.client_modseq.to_string(),
                    &entry.server_modseq.to_string(),
                    &entry.lost_event_count.to_string(),
                ),
            )
            .within(ids::RESTORE_DIVERGENCE_DETAILS_MODAL, 0),
        );
    }
    out.push(Element::chrome(t::RESTORE_DIVERGENCE_FOOTER));
    // ui.yaml scopes no id to the close control, so it paints untagged:
    // keyboard-actuatable for a human, invisible to automation. Minting a
    // app-specific `restore-divergence-close-button` would be the invented-ID
    // anti-pattern (the same call the destinations heading makes above).
    out.push(Element::gesture_button(
        String::new(),
        t::RESTORE_DIVERGENCE_CLOSE,
        true,
        Gesture::Backups(Action::CloseDivergenceModal),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::authed_app;
    use fauna_backups_machine::RowIntegrity;
    use fauna_client_backup::audit::{AuditVerdict, DestinationAuditState};

    fn dummy_nest() -> Arc<NestClient> {
        NestClient::new(
            "http://127.0.0.1:9".to_string(),
            ActorKeypair::from_secret([7u8; 32]),
        )
    }

    fn backups_app() -> App {
        let mut app = authed_app();
        app.page = Page::Backups;
        app.backups = init(
            dummy_nest(),
            [7u8; 32],
            Arc::new(fauna_client_folders::MemoryFolderKeyStore::default()),
            Arc::new(fauna_client_config::NoLedgerStore),
            &[],
        );
        app
    }

    fn destination(id: &str, url: &str, name: Option<&str>) -> BackupDestination {
        BackupDestination {
            destination_id: id.to_string(),
            destination_nest_url: url.to_string(),
            destination_actor_pubkey: [9u8; 32],
            folder_name: "__mail".to_string(),
            added_at: 1_700_000_000,
            display_name: name.map(str::to_string),
            ..Default::default()
        }
    }

    fn count_id(app: &App, id: &str) -> usize {
        elements(app).iter().filter(|e| e.id == id).count()
    }

    fn text_of(app: &App, id: &str) -> String {
        elements(app)
            .into_iter()
            .find(|e| e.id == id)
            .map(|e| e.text)
            .unwrap_or_default()
    }

    /// Count occurrences of `id` scoped to one `backup-destination-status-row[i]`
    /// — the registry's own containment rule, asked through `Registry` itself,
    /// so this proves the scoped e2e query `is_visible("backup-destination-
    /// last-upload-time", scope="backup-destination-status-row[0]")` resolves.
    fn count_scoped(app: &App, id: &str, index: usize) -> usize {
        count_scoped_in(app, id, "backup-destination-status-row", index)
    }

    /// The same check for any container — the restore half needs it for
    /// `restore-history-item[i]`, whose banner containment is what makes the
    /// shared scoped read resolve at all.
    fn count_scoped_in(app: &App, id: &str, container: &str, index: usize) -> usize {
        crate::automation::Registry::of(elements(app))
            .count_scoped(id, &[(container.to_string(), index)])
    }

    fn custodian(id: &str, name: &str, cap: Option<u64>) -> BackupDestination {
        BackupDestination {
            destination_id: id.to_string(),
            kind: fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE.to_string(),
            custodian_device_id: Some("dead".repeat(16)),
            capacity_cap_bytes: cap,
            folder_name: "__mail".to_string(),
            added_at: 1_700_000_000,
            display_name: Some(name.to_string()),
            ..Default::default()
        }
    }

    // ── Reclaim this device's copy (`backups.md` § Manage backup destinations)

    /// The row paints only when the page holds the orphan verdict, and it
    /// carries its button `.within` itself — so the scoped read a shared action
    /// makes (`scope="backup-orphaned-store-row[0]"`) resolves.
    #[test]
    fn the_orphaned_store_row_paints_its_reclaim_button_inside_itself() {
        let mut app = backups_app();
        assert_eq!(count_id(&app, "backup-orphaned-store-row"), 0);
        assert_eq!(count_id(&app, "backup-destination-reclaim-button"), 0);

        app.backups.orphaned_store = Some(3 * 1024 * 1024 * 1024);
        assert_eq!(count_id(&app, "backup-orphaned-store-row"), 1);
        assert_eq!(
            count_scoped_in(
                &app,
                "backup-destination-reclaim-button",
                "backup-orphaned-store-row",
                0
            ),
            1
        );
        // The size is in the sentence: "free up space" with no number tells the
        // user nothing about whether it is worth doing.
        assert!(
            text_of(&app, "backup-orphaned-store-row").contains("3"),
            "row text must name what is held: {:?}",
            text_of(&app, "backup-orphaned-store-row")
        );
    }

    /// ⚠ The guard that matters most on this page. The confirm gesture is a
    /// keyboard-actuable control, and every other stale actuation here costs a
    /// no-op — this one would delete the owner's only offline copy. So both the
    /// open and the confirm re-check the verdict rather than trusting the paint.
    #[test]
    fn a_reclaim_gesture_with_no_orphaned_store_does_nothing() {
        let mut app = backups_app();
        assert!(apply_local(&mut app, Action::OpenReclaimConfirm).is_none());
        assert!(!app.backups.reclaiming);
        assert_eq!(count_id(&app, "backup-reclaim-confirm-modal"), 0);

        // Armed by a verdict, then the verdict goes away under the open modal —
        // a refresh landed that re-enrolled this device.
        app.backups.orphaned_store = Some(1);
        apply_local(&mut app, Action::OpenReclaimConfirm);
        assert!(app.backups.reclaiming);
        app.backups.orphaned_store = None;
        assert!(
            apply_local(&mut app, Action::ConfirmReclaim).is_none(),
            "the modal outlived its verdict; confirming must issue nothing"
        );
    }

    /// The modal is a plain confirm — no re-type, unlike the immediate-delete
    /// friction bar — because the store is re-buildable from a fresh pull on
    /// re-enrollment. Cancelling leaves the store and the row exactly as they
    /// were.
    #[test]
    fn the_reclaim_modal_is_a_plain_confirm_and_cancels_cleanly() {
        let mut app = backups_app();
        app.backups.orphaned_store = Some(4096);
        apply_local(&mut app, Action::OpenReclaimConfirm);

        assert_eq!(count_id(&app, "backup-reclaim-confirm-modal"), 1);
        assert_eq!(
            count_scoped_in(
                &app,
                "backup-reclaim-confirm-button",
                "backup-reclaim-confirm-modal",
                0
            ),
            1
        );
        assert_eq!(
            count_scoped_in(
                &app,
                "backup-reclaim-cancel-button",
                "backup-reclaim-confirm-modal",
                0
            ),
            1
        );
        // No friction bar: the modal's whole tagged surface is the two buttons,
        // with nothing to type into — unlike `snapshot-immediate-delete`, whose
        // consequence really is irrecoverable. (Asserted over the modal's own
        // scope: `restore-confirm-input` sits in the always-painted restore
        // half of this same page, so a bare page-wide count would prove
        // nothing.)
        let inside: Vec<String> = elements(&app)
            .into_iter()
            .filter(|e| {
                e.path
                    .iter()
                    .any(|(c, _)| c == "backup-reclaim-confirm-modal")
            })
            .map(|e| e.id)
            .collect();
        assert_eq!(
            inside,
            vec![
                "backup-reclaim-confirm-button".to_string(),
                "backup-reclaim-cancel-button".to_string(),
            ]
        );

        assert!(apply_local(&mut app, Action::CancelReclaim).is_none());
        assert!(!app.backups.reclaiming);
        assert_eq!(count_id(&app, "backup-reclaim-confirm-modal"), 0);
        assert_eq!(
            count_id(&app, "backup-orphaned-store-row"),
            1,
            "cancelling frees nothing, so the row stays"
        );
    }

    // ── Restore after losing the nest (`ui/backups.md`) ─────────────────────

    /// On a rebuilt nest the orphaned row is where the owner's copy is offered
    /// back: the re-seed button sits inside it, ahead of the reclaim.
    #[test]
    fn the_orphaned_store_row_offers_the_reseed_ahead_of_the_reclaim() {
        let mut app = backups_app();
        assert_eq!(count_id(&app, "backup-destination-reseed-button"), 0);
        app.backups.orphaned_store = Some(4096);
        assert_eq!(
            count_scoped_in(
                &app,
                "backup-destination-reseed-button",
                "backup-orphaned-store-row",
                0
            ),
            1
        );
        let order: Vec<String> = elements(&app)
            .into_iter()
            .map(|e| e.id)
            .filter(|id| id.ends_with("-reseed-button") || id.ends_with("-reclaim-button"))
            .collect();
        assert_eq!(
            order,
            vec![
                "backup-destination-reseed-button".to_string(),
                "backup-destination-reclaim-button".to_string(),
            ]
        );
    }

    /// The confirm opens only over a painted source and runs only from the open
    /// modal; cancelling closes it and issues nothing.
    #[test]
    fn the_reseed_confirm_arms_only_over_a_source_and_cancels_cleanly() {
        let mut app = backups_app();
        assert!(apply_local(&mut app, Action::OpenReseedConfirm).is_none());
        assert_eq!(app.backups.reseed, ReseedView::Closed);
        assert!(
            apply_local(&mut app, Action::ConfirmReseed).is_none(),
            "no modal, no run"
        );

        app.backups.orphaned_store = Some(4096);
        assert!(apply_local(&mut app, Action::OpenReseedConfirm).is_none());
        assert_eq!(app.backups.reseed, ReseedView::Confirming);
        for id in [
            "backup-destination-reseed-confirm-button",
            "backup-destination-reseed-cancel-button",
        ] {
            assert_eq!(
                count_scoped_in(&app, id, "backup-destination-reseed-confirm-modal", 0),
                1,
                "{id}"
            );
        }
        assert!(apply_local(&mut app, Action::CancelReseed).is_none());
        assert_eq!(app.backups.reseed, ReseedView::Closed);
        assert_eq!(count_id(&app, "backup-destination-reseed-confirm-modal"), 0);
    }

    /// Confirming starts the run and disarms the button until it ends, so a
    /// second press cannot land mid-run; the result view says it is running.
    #[test]
    fn a_confirmed_reseed_runs_once_and_says_so() {
        let mut app = backups_app();
        app.backups.orphaned_store = Some(4096);
        apply_local(&mut app, Action::OpenReseedConfirm);
        assert!(matches!(
            apply_local(&mut app, Action::ConfirmReseed),
            Some(Op::Reseed { .. })
        ));
        assert_eq!(app.backups.reseed, ReseedView::Running);
        assert_eq!(
            text_of(&app, "backup-destination-reseed-result"),
            t::BACKUP_RESEED_RUNNING
        );
        let button = elements(&app)
            .into_iter()
            .find(|e| e.id == "backup-destination-reseed-button")
            .expect("the button stays painted");
        assert!(!button.enabled, "a running re-seed disarms its button");
        assert!(apply_local(&mut app, Action::OpenReseedConfirm).is_none());
        assert_eq!(app.backups.reseed, ReseedView::Running);

        // A failed run hands the button back.
        apply_outcome(&mut app, Outcome::Failed("boom".into()));
        assert_eq!(app.backups.reseed, ReseedView::Closed);
    }

    /// The result view renders the shared lines, verdict first, and survives
    /// the whole-page read it arrives with.
    #[test]
    fn a_finished_reseed_paints_the_shared_result_lines() {
        use fauna_client_backup::reseed::{
            DeliveredCorpus, DeliveredSet, ReseedOutcome, SetAxis, SetOutcome, SetResult,
        };
        let mut app = backups_app();
        app.backups.reseed = ReseedView::Running;
        let mail = DeliveredSet {
            set_name: "__mail".into(),
            axis: SetAxis::Segment,
            folder_display_name: None,
            folder_label: None,
        };
        let result = ReseedOutcome {
            delivered: DeliveredCorpus {
                sets: vec![mail.clone()],
                ..Default::default()
            },
            sets: vec![SetResult {
                set: mail,
                outcome: SetOutcome::Materialized {
                    segments: vec![0],
                    records: 2,
                },
            }],
        };
        apply_outcome(
            &mut app,
            Outcome::Reseeded {
                result: result.clone(),
                page: Box::new(Outcome::Reclaimed(None)),
            },
        );
        assert_eq!(app.backups.reseed, ReseedView::Done(result));
        let text = text_of(&app, "backup-destination-reseed-result");
        assert!(text.starts_with(t::RESEED_RESULT_WHOLE), "{text:?}");
        assert!(text.contains('2'), "{text:?}");
    }

    /// ⚠ The reclaim op carries **no nest session**, and that is the property,
    /// not an omission: this is the one gesture on the page whose whole premise
    /// is that the copy is usable with no nest alive anywhere
    /// (`backup-destinations.md` § Third destination kind → *Standalone
    /// restore*). Routing its repaint through the page's whole-page read would
    /// make freeing local disk space depend on a reachable nest — the reclaim
    /// would land and the page would then paint an error over a stale row.
    #[test]
    fn reclaiming_needs_no_nest_and_asks_for_none() {
        let mut app = backups_app();
        app.backups.destinations = vec![custodian("d2", "Laptop", None)];
        app.backups.orphaned_store = Some(2048);
        apply_local(&mut app, Action::OpenReclaimConfirm);

        let op = apply_local(&mut app, Action::ConfirmReclaim).expect("reclaim op");
        assert!(matches!(op, Op::ReclaimStore { .. }));

        // …and it still works with the session gone entirely, which every other
        // mutating gesture on this page refuses.
        app.backups.nest = None;
        app.backups.secret = None;
        apply_local(&mut app, Action::OpenReclaimConfirm);
        assert!(
            apply_local(&mut app, Action::ConfirmReclaim).is_some(),
            "a device with no nest session must still be able to free its own disk"
        );
        apply_local(&mut app, Action::OpenRemoveConfirm { index: 0 });
        assert!(
            apply_local(&mut app, Action::ConfirmRemove).is_none(),
            "precondition: the nest-side gestures DO require a session"
        );
    }

    /// The reclaim's repaint is re-measured, never assumed: the outcome carries
    /// what the agent reported *after* the call, and folding it is what retires
    /// the row and closes the modal.
    #[test]
    fn a_reclaimed_store_retires_its_row_from_the_re_measured_verdict() {
        let mut app = backups_app();
        app.backups.orphaned_store = Some(2048);
        apply_local(&mut app, Action::OpenReclaimConfirm);
        assert_eq!(count_id(&app, "backup-reclaim-confirm-modal"), 1);

        apply_outcome(&mut app, Outcome::Reclaimed(None));
        assert_eq!(app.backups.orphaned_store, None);
        assert!(!app.backups.reclaiming);
        assert_eq!(count_id(&app, "backup-orphaned-store-row"), 0);
        assert_eq!(count_id(&app, "backup-reclaim-confirm-modal"), 0);

        // A store the agent still reports as orphaned keeps its row — the fold
        // takes the measurement, not the fact that a call succeeded.
        apply_outcome(&mut app, Outcome::Reclaimed(Some(512)));
        assert_eq!(count_id(&app, "backup-orphaned-store-row"), 1);
    }

    /// The remove dialog's opt-in is client-device kind only: a nest destination
    /// has no local copy to free, so painting the tick there would be an inert
    /// control on a destructive dialog.
    #[test]
    fn the_remove_reclaim_opt_in_paints_only_for_a_client_device_row() {
        let mut app = backups_app();
        app.backups.destinations = vec![
            destination("d1", "https://a.example", Some("Attic")),
            custodian("d2", "Laptop", None),
        ];

        apply_local(&mut app, Action::OpenRemoveConfirm { index: 0 });
        assert_eq!(count_id(&app, "backup-destination-remove-confirm-modal"), 1);
        assert_eq!(
            count_id(&app, "backup-destination-remove-reclaim-checkbox"),
            0,
            "a nest destination holds nothing on this device"
        );

        apply_local(&mut app, Action::OpenRemoveConfirm { index: 1 });
        assert_eq!(
            count_scoped_in(
                &app,
                "backup-destination-remove-reclaim-checkbox",
                "backup-destination-remove-confirm-modal",
                0,
            ),
            1
        );
    }

    /// ⚠ The opt-in is an intent about **this** removal. A tick left standing
    /// from a dialog the user cancelled would delete an offline copy nobody
    /// asked about in the next gesture.
    #[test]
    fn the_remove_reclaim_tick_never_survives_its_dialog() {
        let mut app = backups_app();
        app.backups.destinations = vec![
            custodian("d2", "Laptop", None),
            custodian("d3", "Nas", None),
        ];

        apply_local(&mut app, Action::OpenRemoveConfirm { index: 0 });
        apply_local(&mut app, Action::ToggleRemoveReclaim);
        assert!(app.backups.remove_reclaim);

        apply_local(&mut app, Action::CancelRemove);
        assert!(!app.backups.remove_reclaim);

        // …and re-arming on a different row starts clean too.
        apply_local(&mut app, Action::OpenRemoveConfirm { index: 0 });
        apply_local(&mut app, Action::ToggleRemoveReclaim);
        apply_local(&mut app, Action::OpenRemoveConfirm { index: 1 });
        assert!(!app.backups.remove_reclaim);
    }

    /// The tick rides the op it belongs to, so the runner cannot re-read a value
    /// that has since changed — and it is `false` unless the user set it, on a
    /// dialog whose default must never be "also delete my offline copy".
    #[test]
    fn the_remove_op_carries_the_opt_in_and_defaults_to_keeping_the_copy() {
        let mut app = backups_app();
        app.backups.destinations = vec![custodian("d2", "Laptop", None)];

        apply_local(&mut app, Action::OpenRemoveConfirm { index: 0 });
        let op = apply_local(&mut app, Action::ConfirmRemove).expect("remove op");
        assert!(
            matches!(op, Op::Remove { reclaim: false, .. }),
            "the default keeps the store — 3c-ii, it is the owner's only offline copy"
        );

        apply_local(&mut app, Action::OpenRemoveConfirm { index: 0 });
        apply_local(&mut app, Action::ToggleRemoveReclaim);
        let op = apply_local(&mut app, Action::ConfirmRemove).expect("remove op");
        assert!(matches!(op, Op::Remove { reclaim: true, .. }));
    }

    /// A tick arriving with no dialog open is a stale actuation, not a latent
    /// preference: it must not arm the next removal.
    #[test]
    fn a_tick_with_no_remove_dialog_open_is_ignored() {
        let mut app = backups_app();
        assert!(apply_local(&mut app, Action::ToggleRemoveReclaim).is_none());
        assert!(!app.backups.remove_reclaim);
    }

    /// Picking "This device" swaps the dialog's per-kind fields: a custodian has
    /// no address at all (`backups.md` § Custodian contract, question 1), so the
    /// URL box must not be offered, and the capacity cap — the kind's only knob —
    /// must be.
    #[test]
    fn the_kind_select_swaps_the_url_box_for_the_capacity_cap() {
        let mut app = backups_app();
        apply_local(&mut app, Action::OpenAddForm);
        // The nest kind is the default, because it is the one that actually
        // satisfies "off-site".
        assert_eq!(count_id(&app, "backup-destination-kind-select"), 1);
        assert_eq!(count_id(&app, "backup-destination-url-input"), 1);
        assert_eq!(count_id(&app, "backup-destination-capacity-input"), 0);

        let device = DestinationKindChoice::ClientDevice.label();
        apply_local(&mut app, Action::SelectDestinationKind(device));
        assert_eq!(count_id(&app, "backup-destination-url-input"), 0);
        assert_eq!(count_id(&app, "backup-destination-capacity-input"), 1);
        // The nest kind's blank-URL disable must not strand the custodian's
        // confirm: it has no URL to require.
        let confirm = elements(&app)
            .into_iter()
            .find(|e| e.id == "backup-destination-add-confirm-button")
            .expect("confirm paints");
        assert!(confirm.enabled, "a custodian add has no URL to wait for");
    }

    /// The kind is not an editable property (`backups.md` § Create / edit /
    /// remove restricts edit to the display name and URL). Re-pointing a live row
    /// at the other kind would keep a `destination_id` whose registry row, status
    /// projection and grants all describe the kind it used to be.
    #[test]
    fn edit_mode_cannot_re_point_a_row_at_another_kind() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://nas.example.com", Some("Off"))];
        apply_local(&mut app, Action::OpenEditForm { index: 0 });

        let select = elements(&app)
            .into_iter()
            .find(|e| e.id == "backup-destination-kind-select")
            .expect("the select still paints, so the row's kind stays legible");
        assert!(!select.enabled, "the kind is not editable");

        // A keyboard actuation of the disabled control must not take effect.
        let device = DestinationKindChoice::ClientDevice.label();
        apply_local(&mut app, Action::SelectDestinationKind(device));
        assert_eq!(app.backups.kind_input, DestinationKindChoice::Nest);
        assert_eq!(count_id(&app, "backup-destination-capacity-input"), 0);
    }

    /// **An empty picker must still say what it picks, and why it is dead.**
    /// With no destinations the source picker painted a bare `<  >` — no
    /// prompt, no value, no reason — which is pure noise on a terminal (the
    /// audit's empty-select finding). It now prompts, and is followed by the
    /// on-screen reason `restore-snapshot-select` already had (Copy
    /// comprehensibility rule 5).
    #[test]
    fn the_empty_restore_source_picker_is_prompted_and_states_why_it_is_disabled() {
        let app = backups_app();
        let els = elements(&app);
        let select = els
            .iter()
            .find(|e| e.id == "restore-source-select")
            .expect("the picker always paints, so its id never vanishes mid-poll");
        assert!(!select.enabled, "nothing to pick from");
        assert_eq!(
            select.label.as_deref(),
            Some(t::RESTORE_SOURCE_LABEL),
            "an empty picker still names what it picks"
        );
        assert!(
            select.text.is_empty(),
            "the prompt is paint-only — the registry value stays bare"
        );
        assert!(
            crate::ui::painted_line_texts(&els)
                .iter()
                .any(|l| l.contains(t::RESTORE_NO_DESTINATIONS)),
            "a disabled control the user can see states its reason"
        );
    }

    /// The other direction: with destinations configured the picker is live and
    /// the dead-end reason is gone — so the assertion above pins the empty
    /// state, not a line that is always painted.
    #[test]
    fn a_configured_destination_makes_the_source_picker_live_and_drops_the_reason() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://nas.example.com", Some("NAS"))];
        let els = elements(&app);
        let select = els
            .iter()
            .find(|e| e.id == "restore-source-select")
            .unwrap();
        assert!(select.enabled);
        assert!(
            !crate::ui::painted_line_texts(&els)
                .iter()
                .any(|l| l.contains(t::RESTORE_NO_DESTINATIONS)),
            "the reason line belongs to the empty state alone"
        );
    }

    /// A capacity the shell cannot read becomes a refusal the user sees — never a
    /// substituted default, and never an enroll carrying a guessed cap.
    #[test]
    fn an_unreadable_capacity_refuses_instead_of_guessing() {
        let mut app = backups_app();
        apply_local(&mut app, Action::OpenAddForm);
        apply_local(
            &mut app,
            Action::SelectDestinationKind(DestinationKindChoice::ClientDevice.label()),
        );
        set_field(&mut app.backups, BackupsField::Capacity, "loads".into());

        let op = apply_local(&mut app, Action::SubmitForm(SubmitTarget::AddCustodian));
        assert!(op.is_none(), "no enroll may be dispatched on a bad cap");
        assert!(
            app.errors.contains_key(&Page::Backups),
            "the refusal must be visible, not silent"
        );

        // A blank cap is a different thing entirely: uncapped is a real choice.
        set_field(&mut app.backups, BackupsField::Capacity, String::new());
        let op = apply_local(&mut app, Action::SubmitForm(SubmitTarget::AddCustodian));
        assert!(
            matches!(
                op,
                Some(Op::EnrollCustodian {
                    capacity_cap_bytes: None,
                    ..
                })
            ),
            "a blank cap enrolls uncapped, it does not refuse"
        );
    }

    /// The badge is the visible half of "a client custodian never silently
    /// satisfies *you have an off-site backup*", and usage is client-device only.
    #[test]
    fn a_custodian_row_is_badged_and_carries_usage_where_a_nest_row_does_not() {
        let mut app = backups_app();
        app.backups.destinations = vec![
            destination("d1", "https://nas.example.com", Some("Offsite")),
            custodian("d2", "This laptop", Some(50 * 1024 * 1024 * 1024)),
        ];
        // Both rows carry a badge, and the two badges differ.
        assert_eq!(count_scoped(&app, "backup-destination-kind-badge", 0), 1);
        assert_eq!(count_scoped(&app, "backup-destination-kind-badge", 1), 1);
        let badges: Vec<String> = elements(&app)
            .into_iter()
            .filter(|e| e.id == "backup-destination-kind-badge")
            .map(|e| e.text)
            .collect();
        assert_ne!(
            badges[0], badges[1],
            "a device must not render as an off-site nest"
        );
        // Usage is the custodian row's alone.
        assert_eq!(count_scoped(&app, "backup-destination-usage", 0), 0);
        assert_eq!(count_scoped(&app, "backup-destination-usage", 1), 1);
    }

    /// The standing warning renders only while every destination is one of the
    /// owner's own devices — and a single nest destination clears it.
    #[test]
    fn the_sole_client_warning_tracks_whether_any_off_site_copy_exists() {
        let mut app = backups_app();
        // Empty is not "sole client" — the empty state has its own copy.
        assert_eq!(count_id(&app, "backup-sole-client-destination-warning"), 0);

        app.backups.destinations = vec![custodian("d1", "This laptop", None)];
        assert_eq!(count_id(&app, "backup-sole-client-destination-warning"), 1);

        app.backups.destinations.push(destination(
            "d2",
            "https://nas.example.com",
            Some("Offsite"),
        ));
        assert_eq!(
            count_id(&app, "backup-sole-client-destination-warning"),
            0,
            "one real off-site copy is what the warning is asking for"
        );
    }

    /// The zero-destination landing: the add button is always present (it is the
    /// entry point for the *first* destination), and no status row or dialog
    /// paints. This is the state `test_backup_destination_crud` asserts before
    /// it adds anything.
    #[test]
    fn empty_state_paints_only_the_always_present_add_button() {
        let app = backups_app();
        assert_eq!(count_id(&app, "page-heading"), 1);
        assert_eq!(count_id(&app, "backup-destination-add-button"), 1);
        assert_eq!(count_id(&app, "backup-destination-status-row"), 0);
        // Both dialogs are "present only while open" (ui.yaml optional_elements).
        assert_eq!(count_id(&app, "backup-destination-add-modal"), 0);
        assert_eq!(count_id(&app, "backup-destination-remove-confirm-modal"), 0);
    }

    /// A configured destination paints one row whose text is the SHARED
    /// display-name-else-host label, with all four members registered *inside*
    /// that row — the containment the scoped e2e reads depend on.
    #[test]
    fn a_destination_paints_one_row_with_its_members_contained() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination(
            "d1",
            "https://nas.example.com",
            Some("Offsite"),
        )];

        assert_eq!(count_id(&app, "backup-destination-status-row"), 1);
        assert_eq!(text_of(&app, "backup-destination-status-row"), "Offsite");
        for id in [
            "backup-destination-last-upload-time",
            "backup-destination-backlog-count",
            "backup-destination-last-audit-time",
            "backup-destination-edit-button",
            "backup-destination-remove-button",
        ] {
            assert_eq!(count_scoped(&app, id, 0), 1, "{id} not inside row 0");
        }
    }

    // ── the post-succession review mark (succession-aftermath.md
    //    § Adjudicating what the aftermath carries across) ──────────────────

    /// One open mark on `id`, keyed to a raising event no other test shares.
    fn open_mark(id: &str) -> DestinationUnattestedMark {
        DestinationUnattestedMark {
            destination_id: id.to_string(),
            predecessor: ActorKeypair::from_secret([3u8; 32]).actor_id(),
            verdict: fauna_core::data::UnattestedVerdict::Open,
        }
    }

    /// An ordinary row paints neither the mark nor Keep. This is what keeps the
    /// mark meaningful: in a healthy account every destination is the owner's
    /// own, so an always-rendered element would train the user straight past
    /// the one succession that matters.
    #[test]
    fn an_unraised_destination_paints_no_review_mark_and_no_keep() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://nas.example.com", None)];

        assert_eq!(count_id(&app, "backup-destination-unattested-mark"), 0);
        assert_eq!(count_id(&app, "backup-destination-keep-button"), 0);
    }

    /// A row the aftermath carried across paints both, inside its own row —
    /// and paints them *beside* the Remove button that already exists, rather
    /// than minting a second removal path.
    #[test]
    fn a_carried_across_destination_paints_the_mark_and_keep_inside_its_row() {
        let mut app = backups_app();
        let raised = destination("d1", "https://nas.example.com", Some("Offsite"));
        app.backups.destinations =
            vec![destination("d0", "https://mine.example.com", None), raised];
        app.backups.unattested_destination_marks = vec![open_mark("d1")];

        for id in [
            "backup-destination-unattested-mark",
            "backup-destination-keep-button",
            "backup-destination-remove-button",
        ] {
            assert_eq!(
                count_scoped(&app, id, 1),
                1,
                "{id} not inside the raised row"
            );
        }
        // ...and only the raised row carries them.
        assert_eq!(
            count_scoped(&app, "backup-destination-unattested-mark", 0),
            0
        );
        assert_eq!(count_scoped(&app, "backup-destination-keep-button", 0), 0);
    }

    /// The copy is a review prompt, not an alarm. Asserted on the **rendered
    /// text** rather than the key, because the wording is the load-bearing part:
    /// almost every row on this list after a recovery is a friend of the user's
    /// own making, and copy that framed them as suspects would accuse the user's
    /// own devices after every successful recovery.
    #[test]
    fn the_review_copy_reads_as_a_review_not_as_damage() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://nas.example.com", None)];
        app.backups.unattested_destination_marks = vec![open_mark("d1")];

        let copy = text_of(&app, "backup-destination-unattested-mark").to_lowercase();
        assert!(
            copy.contains("still backing up"),
            "the copy must say the backups are working, not that something broke: {copy}"
        );
        assert!(
            !copy.contains("attack")
                && !copy.contains("compromise")
                && !copy.contains("suspicious")
                && !copy.contains("intruder"),
            "the copy must not accuse: {copy}"
        );
    }

    /// Keep emits a succession-ledger write for the row it was pressed on, and no
    /// remove-confirm dialog — Keep is non-destructive and re-decidable, so the
    /// friction bar Remove carries would be friction with no payoff.
    #[test]
    fn keep_writes_that_rows_adjudication_and_arms_no_confirm_dialog() {
        let mut app = backups_app();
        app.backups.destinations = vec![
            destination("d0", "https://mine.example.com", None),
            destination("d1", "https://nas.example.com", None),
        ];
        app.backups.unattested_destination_marks = vec![open_mark("d1")];

        let op = apply_local(&mut app, Action::KeepDestination { index: 1 });
        match op {
            Some(Op::Keep { id, .. }) => {
                assert_eq!(id, "d1", "Keep must name the row it was pressed on")
            }
            _ => panic!("expected a Keep op"),
        }
        assert!(
            app.backups.removing.is_none(),
            "Keep must not arm the remove-confirm dialog"
        );
    }

    /// **An answered destination stays answered.** A Keep recorded at rest on
    /// the mark plane lowers the row: the page never re-asks the owner a
    /// question they settled on their other device.
    #[test]
    fn a_kept_destination_stays_clean() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://nas.example.com", None)];
        app.backups.unattested_destination_marks = vec![DestinationUnattestedMark {
            verdict: fauna_core::data::UnattestedVerdict::Kept,
            ..open_mark("d1")
        }];

        assert_eq!(count_id(&app, "backup-destination-unattested-mark"), 0);
        assert_eq!(count_id(&app, "backup-destination-keep-button"), 0);
    }

    // ── the audit surface (`backups.md` § Audit-alert surface) ───────────────

    fn audit_record(
        destination_id: &str,
        last_passed_at: Option<i64>,
        verdict: Option<AuditVerdict>,
    ) -> DestinationAuditRecord {
        DestinationAuditRecord {
            state: DestinationAuditState {
                destination_id: destination_id.to_string(),
                last_passed_at,
                last_attempt_at: Some(1_700_000_000),
                verified_ledger_generations: Default::default(),
                accepted_regressions: Default::default(),
                seat_settled_under: None,
            },
            verdict,
        }
    }

    /// A destination this client has never audited reads "never" — and that is
    /// deliberately **not** an alert: a destination enrolled ten minutes ago has
    /// never passed and is perfectly healthy (escalating a long-standing "never"
    /// is `AUDIT_OVERDUE`'s job, not this label's).
    #[test]
    fn a_never_audited_destination_reads_never_and_raises_no_alert() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://a.example", Some("A"))];
        app.backups.audit = vec![DestinationAuditRecord::never("d1")];

        assert_eq!(
            text_of(&app, "backup-destination-last-audit-time"),
            // The exact string `test_backups.py::_NEVER_AUDIT_TEXT` compares
            // against — a client rendering anything else fails the e2e with a
            // diff no one can read.
            "Last checked: never"
        );
        assert_eq!(count_id(&app, "backup-audit-alert"), 0);
    }

    // ── the client-device arm of the same surface ────────────────────────
    //
    // A custodian has no address, so the owner-side loop can never sample it:
    // its audit answer arrives on the STATUS ROW as the device's own verdict
    // (`backup-destinations.md` § Custodian contract, question 4).

    fn status_row_with_audit(
        id: &str,
        audit_state: Option<&str>,
        last_audit_passed_at: Option<u64>,
    ) -> BackupDestinationStatusItem {
        BackupDestinationStatusItem {
            destination_id: id.to_string(),
            audit_state: audit_state.map(str::to_string),
            last_audit_passed_at,
            ..Default::default()
        }
    }

    /// **A custodian that has never self-audited renders ABSENCE, not a
    /// verdict** — the property the verify-back names explicitly. The
    /// nest refuses to invent a verdict for a row that has not reported, and the
    /// app must not invent one either: reading silence as a pass would render an
    /// unverified copy as verified, and reading it as a failure would raise a
    /// fleet-wide false data-loss alarm for every custodian that has not
    /// self-audited yet.
    #[test]
    fn a_custodian_that_never_self_audited_renders_absence_and_raises_no_alert() {
        let mut app = backups_app();
        app.backups.destinations = vec![custodian("d1", "This device", None)];
        app.backups
            .statuses
            .insert("d1".to_string(), status_row_with_audit("d1", None, None));

        assert_eq!(
            text_of(&app, "backup-destination-last-audit-time"),
            "Self-checked: not yet"
        );
        assert_eq!(count_id(&app, "backup-audit-alert"), 0);
    }

    /// The custodian's cell never borrows the owner-side wording. The owner-side
    /// loop's silence about a custodian is structural, not a finding — rendering
    /// its "Last checked: never" here would report a device that self-checks
    /// every pass as unchecked.
    #[test]
    fn a_custodian_row_never_wears_the_owner_side_audit_wording() {
        let mut app = backups_app();
        app.backups.destinations = vec![custodian("d1", "This device", None)];
        app.backups.statuses.insert(
            "d1".to_string(),
            status_row_with_audit(
                "d1",
                Some(fauna_core::data::AUDIT_STATE_OK),
                u64::try_from(crate::backup_audit::now_secs()).ok(),
            ),
        );

        let text = text_of(&app, "backup-destination-last-audit-time");
        assert!(
            text.starts_with("Self-checked:"),
            "a self-report must name its provenance, got {text:?}"
        );
        assert!(!text.contains("Last checked"), "got {text:?}");
        assert_eq!(count_id(&app, "backup-audit-alert"), 0);
    }

    /// A **reported failure is loud** — the only failure signal that exists for
    /// a kind the owner cannot sample. The banner names the destination, like
    /// every other one on this page.
    #[test]
    fn a_custodian_reporting_its_own_failure_paints_a_named_banner() {
        let mut app = backups_app();
        app.backups.destinations = vec![custodian("d1", "Laptop", None)];
        app.backups.statuses.insert(
            "d1".to_string(),
            status_row_with_audit(
                "d1",
                Some(fauna_core::data::AUDIT_STATE_FAILED),
                // Deliberately stale: the last time it PASSED. A failure never
                // advances that clock, so the row must not read as fresh.
                Some(1_700_000_000),
            ),
        );

        assert_eq!(count_id(&app, "backup-audit-alert"), 1);
        let banner = text_of(&app, "backup-audit-alert");
        assert!(banner.contains("Laptop"), "got {banner:?}");

        // The row must not read as fresh: it renders the stale PASSED time, not
        // "checked seconds ago" and not "not yet". Red-verified against
        // PROBE-71-A (flipping the fixture above to `now_secs()`).
        let audit_time = text_of(&app, "backup-destination-last-audit-time");
        assert!(
            audit_time.starts_with("Self-checked:"),
            "got {audit_time:?}"
        );
        assert!(audit_time.contains("2023"), "got {audit_time:?}");
        assert_ne!(audit_time, "Self-checked: not yet");
    }

    /// An unrecognised verdict a NEWER client wrote stays quiet. This client
    /// cannot know whether it names a failure, and inventing one is the same
    /// false alarm as reading silence as failure — the conservative direction is
    /// the shared predicate's, not each app's.
    #[test]
    fn an_unrecognised_reported_verdict_stays_quiet() {
        let mut app = backups_app();
        app.backups.destinations = vec![custodian("d1", "Laptop", None)];
        app.backups.statuses.insert(
            "d1".to_string(),
            status_row_with_audit("d1", Some("degraded-in-some-newer-way"), None),
        );

        assert_eq!(count_id(&app, "backup-audit-alert"), 0);
    }

    /// A nest row is untouched by all of the above: its cell still carries this
    /// client's own independent check, and a custodian's reported failure raises
    /// a banner for the CUSTODIAN, never for the nest beside it.
    #[test]
    fn a_nest_row_keeps_the_owner_side_audit_and_its_own_silence() {
        let mut app = backups_app();
        app.backups.destinations = vec![
            destination("d1", "https://a.example", Some("Attic")),
            custodian("d2", "Laptop", None),
        ];
        app.backups.audit = vec![DestinationAuditRecord::never("d1")];
        app.backups.statuses.insert(
            "d2".to_string(),
            status_row_with_audit("d2", Some(fauna_core::data::AUDIT_STATE_FAILED), None),
        );

        // Row 0 (the nest) keeps the owner-side wording...
        assert_eq!(
            elements(&app)
                .into_iter()
                .find(|e| e.id == "backup-destination-last-audit-time")
                .map(|e| e.text)
                .unwrap_or_default(),
            "Last checked: never"
        );
        // ...and exactly one banner is painted, for the custodian.
        assert_eq!(count_id(&app, "backup-audit-alert"), 1);
        assert!(text_of(&app, "backup-audit-alert").contains("Laptop"));
    }

    /// A passed audit advances the row off "never" — the single property
    /// `test_backup_audit_pass_advances_the_last_checked_row` asserts.
    #[test]
    fn a_passed_audit_advances_the_row_off_never() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://a.example", Some("A"))];
        app.backups.audit = vec![audit_record(
            "d1",
            Some(crate::backup_audit::now_secs()),
            Some(AuditVerdict::Passed),
        )];
        assert_ne!(
            text_of(&app, "backup-destination-last-audit-time"),
            "Last checked: never"
        );
        assert_eq!(count_id(&app, "backup-audit-alert"), 0);
    }

    /// An alerting verdict paints one banner naming the destination *and* the
    /// reason — indexed, so two failing destinations get two banners rather than
    /// one ambiguous "backup problem".
    #[test]
    fn each_alerting_destination_paints_its_own_named_banner() {
        let mut app = backups_app();
        app.backups.destinations = vec![
            destination("d1", "https://a.example", Some("Attic")),
            destination("d2", "https://b.example", Some("Basement")),
        ];
        app.backups.audit = vec![
            audit_record(
                "d1",
                None,
                Some(AuditVerdict::Overdue {
                    since_secs: 9 * 24 * 60 * 60,
                }),
            ),
            audit_record("d2", Some(1_700_000_000), Some(AuditVerdict::Passed)),
        ];

        assert_eq!(count_id(&app, "backup-audit-alert"), 1);
        let banner = text_of(&app, "backup-audit-alert");
        assert!(
            banner.contains("Attic"),
            "banner does not name it: {banner:?}"
        );
        assert!(
            !banner.contains("Basement"),
            "the healthy destination must not appear: {banner:?}"
        );
    }

    /// Which verdicts are loud is the SHARED `alert_reason()`, and the one that
    /// most matters is the verdict that must stay **quiet**: a transient
    /// `Unreachable` is the laptop-on-a-plane case, and a client that alerted on
    /// it would cry wolf every time a user closed their lid.
    #[test]
    fn a_transient_unreachable_stays_quiet() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://a.example", Some("A"))];
        app.backups.audit = vec![audit_record(
            "d1",
            Some(1_700_000_000),
            Some(AuditVerdict::Unreachable {
                error: "connection refused".into(),
            }),
        )];
        assert_eq!(count_id(&app, "backup-audit-alert"), 0);
    }

    // ── the fifth reason: an accepted source regression's recovery notice ──
    //
    // The owner's nest went backwards and the destination still holds what it
    // lost; the audit PASSES (the destination did nothing wrong), so the banner
    // comes from the shared `alert_reasons(now)` door, not the verdict.

    /// A passed record carrying one accepted regression whose window closes at
    /// `recoverable_until`.
    fn regressed_record(destination_id: &str, recoverable_until: i64) -> DestinationAuditRecord {
        let mut record = audit_record(
            destination_id,
            Some(crate::backup_audit::now_secs()),
            Some(AuditVerdict::Passed),
        );
        record.state.accepted_regressions.insert(
            "__mail/ledger".into(),
            fauna_client_backup::audit::AcceptedRegression {
                pinned: 40,
                served: 30,
                observed_at: crate::backup_audit::now_secs(),
                floored_at: None,
                recoverable_until: Some(recoverable_until),
            },
        );
        record
    }

    /// An open recovery window paints the fifth reason, naming the destination
    /// and the days left — on a destination whose audit PASSED.
    #[test]
    fn an_open_recovery_window_paints_a_named_banner() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://a.example", Some("Attic"))];
        app.backups.audit = vec![regressed_record(
            "d1",
            crate::backup_audit::now_secs() + 10 * 24 * 60 * 60 + 60,
        )];

        assert_eq!(count_id(&app, "backup-audit-alert"), 1);
        let banner = text_of(&app, "backup-audit-alert");
        assert!(banner.contains("Attic"), "got {banner:?}");
        assert!(banner.contains("10 more days"), "got {banner:?}");
    }

    /// A record with no deadline — the copy holds what the nest lost as live
    /// rows, which nothing reclaims — paints the banner without a day count.
    #[test]
    fn a_record_with_no_deadline_paints_until_recovered() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://a.example", Some("Attic"))];
        let mut record = regressed_record("d1", 0);
        for regression in record.state.accepted_regressions.values_mut() {
            regression.recoverable_until = None;
        }
        app.backups.audit = vec![record];

        assert_eq!(count_id(&app, "backup-audit-alert"), 1);
        let banner = text_of(&app, "backup-audit-alert");
        assert!(banner.contains("Attic"), "got {banner:?}");
        assert!(banner.contains("until it is recovered"), "got {banner:?}");
        assert!(!banner.contains("more days"), "got {banner:?}");
    }

    /// A device whose own custodian store holds a source regression paints the
    /// fifth reason on its own row, through the shared fold and with no
    /// element or string of its own: the store's record carries no deadline,
    /// so the banner says *until it is recovered*.
    #[test]
    fn a_regression_on_this_devices_store_paints_until_recovered_on_its_row() {
        let mut app = backups_app();
        app.backups.destinations = vec![BackupDestination {
            destination_id: "d1".into(),
            kind: fauna_core::data::DESTINATION_KIND_CLIENT_DEVICE.into(),
            custodian_device_id: Some("aa11".into()),
            folder_name: "__mail".into(),
            added_at: 1_700_000_000,
            display_name: Some("Laptop".into()),
            ..Default::default()
        }];
        let mut records = vec![DestinationAuditRecord::never("d1")];
        assert_eq!(count_id(&app, "backup-audit-alert"), 0);

        fauna_client_backup::audit::fold_own_custodian_regressions(
            &mut records,
            &app.backups.destinations,
            &fauna_client_backup::audit::OwnCustodianStore {
                device_id: "aa11".into(),
                source_regressions: vec![fauna_client_backup::audit::StoreSourceRegression {
                    ledger: "manifest.mail".into(),
                    held: 40,
                    served: 0,
                    observed_at: crate::backup_audit::now_secs(),
                }],
            },
        );
        app.backups.audit = records;

        assert_eq!(count_id(&app, "backup-audit-alert"), 1);
        let banner = text_of(&app, "backup-audit-alert");
        assert!(banner.contains("Laptop"), "got {banner:?}");
        assert!(banner.contains("until it is recovered"), "got {banner:?}");
    }

    /// A closed window paints nothing: the notice expires by itself.
    #[test]
    fn a_closed_recovery_window_paints_nothing() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://a.example", Some("Attic"))];
        app.backups.audit = vec![regressed_record("d1", crate::backup_audit::now_secs() - 1)];

        assert_eq!(count_id(&app, "backup-audit-alert"), 0);
    }

    /// A standing alerting verdict and an open window are two reasons: both
    /// paint, flat, under the same indexed id.
    #[test]
    fn a_standing_verdict_and_an_open_window_paint_two_banners() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://a.example", Some("Attic"))];
        let mut record = regressed_record("d1", crate::backup_audit::now_secs() + 3 * 24 * 60 * 60);
        record.verdict = Some(AuditVerdict::Overdue {
            since_secs: 9 * 24 * 60 * 60,
        });
        app.backups.audit = vec![record];

        assert_eq!(count_id(&app, "backup-audit-alert"), 2);
    }

    /// The banners paint **above** every section, so a warning that a backup is
    /// not keeping up is visible without scrolling past the destination and
    /// restore surfaces (`backups.md` § Audit-alert surface).
    #[test]
    fn alert_banners_paint_ahead_of_every_section() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://a.example", Some("A"))];
        app.backups.audit = vec![audit_record(
            "d1",
            None,
            Some(AuditVerdict::InclusionFailure {
                missing: 2,
                sampled: 16,
            }),
        )];
        let ids: Vec<String> = elements(&app).into_iter().map(|e| e.id).collect();
        let banner = ids.iter().position(|id| id == "backup-audit-alert");
        let first_row = ids
            .iter()
            .position(|id| id == "backup-destination-add-button");
        assert!(banner.is_some() && banner < first_row, "{ids:?}");
    }

    /// A row reads ITS OWN destination's verdict. `merge_outcomes` orders its
    /// result like the destination list today, so an index lookup would pass —
    /// this pins the keyed lookup that keeps a row from inheriting another
    /// destination's "last checked" if that ever stops holding.
    #[test]
    fn a_row_reads_its_own_destinations_verdict_not_its_neighbours() {
        let mut app = backups_app();
        app.backups.destinations = vec![
            destination("d1", "https://a.example", Some("A")),
            destination("d2", "https://b.example", Some("B")),
        ];
        // Deliberately in the OPPOSITE order to the destination list.
        app.backups.audit = vec![
            audit_record("d2", Some(crate::backup_audit::now_secs()), None),
            DestinationAuditRecord::never("d1"),
        ];
        let scoped_text = |index: usize| {
            elements(&app)
                .into_iter()
                .find(|e| {
                    e.id == "backup-destination-last-audit-time"
                        && e.path.first() == Some(&("backup-destination-status-row".into(), index))
                })
                .map(|e| e.text)
                .unwrap_or_default()
        };
        assert_eq!(scoped_text(0), "Last checked: never", "row 0 is d1 (never)");
        assert_ne!(
            scoped_text(1),
            "Last checked: never",
            "row 1 is d2 (passed)"
        );
    }

    /// An audit outcome must not wipe a standing error or close an open dialog:
    /// it is an observation about destinations, not a page mutation.
    #[test]
    fn an_audit_outcome_leaves_the_rest_of_the_page_alone() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://a.example", Some("A"))];
        app.backups.form = Some(FormMode::Add);
        app.errors.insert(Page::Backups, "resolve failed".into());

        apply_outcome(
            &mut app,
            Outcome::Audited(Some(vec![DestinationAuditRecord::never("d1")])),
        );

        assert_eq!(
            app.errors.get(&Page::Backups).map(String::as_str),
            Some("resolve failed")
        );
        assert!(app.backups.form.is_some(), "the open dialog was closed");
        assert_eq!(app.backups.audit.len(), 1);
    }

    /// **`None` is not an empty picture.** A pass that did not complete (the
    /// `AUDIT_BUDGET` timeout) must leave the standing verdicts on screen —
    /// blanking them would switch off a real alarm because the network was slow,
    /// which is the failure `merge_outcomes`' own docs warn about.
    #[test]
    fn an_incomplete_pass_leaves_the_standing_verdicts_alone() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://a.example", Some("A"))];
        app.backups.audit = vec![audit_record(
            "d1",
            None,
            Some(AuditVerdict::Overdue {
                since_secs: 9 * 24 * 60 * 60,
            }),
        )];
        assert_eq!(count_id(&app, "backup-audit-alert"), 1);

        apply_outcome(&mut app, Outcome::Audited(None));
        assert_eq!(
            count_id(&app, "backup-audit-alert"),
            1,
            "an incomplete pass cleared a standing alert"
        );

        // …whereas a real empty picture (no destinations left) DOES clear it —
        // `merge_outcomes` rule 2: an alarm for a backup the user deliberately
        // stopped is noise they cannot dismiss.
        apply_outcome(&mut app, Outcome::Audited(Some(Vec::new())));
        assert_eq!(count_id(&app, "backup-audit-alert"), 0);
    }

    /// A blank `display_name` falls back to the destination URL's host through
    /// the shared `backup_destination_label` — never to an empty row. Port and
    /// path are stripped by the shared `url_host`, which is what keeps the seven
    /// apps showing the same string for the same destination.
    #[test]
    fn a_nameless_destination_labels_itself_from_the_url_host() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://nas.example.com:8443", None)];
        assert_eq!(
            text_of(&app, "backup-destination-status-row"),
            "nas.example.com"
        );
    }

    /// Two destinations index independently: row 1's members are scoped to row 1,
    /// not to row 0. A nested indexed list that shares one scope reads the wrong
    /// destination while painting perfectly (the A6 nesting lesson).
    #[test]
    fn two_destinations_scope_their_members_to_their_own_row() {
        let mut app = backups_app();
        app.backups.destinations = vec![
            destination("d1", "https://a.example", Some("A")),
            destination("d2", "https://b.example", Some("B")),
        ];
        assert_eq!(count_id(&app, "backup-destination-status-row"), 2);
        assert_eq!(count_scoped(&app, "backup-destination-edit-button", 0), 1);
        assert_eq!(count_scoped(&app, "backup-destination-edit-button", 1), 1);
    }

    /// The add dialog opens empty, and its confirm is disabled until a URL is
    /// typed — the enroll's first step is a connect, so a blank URL would fail
    /// with a transport error that reads like a network problem.
    #[test]
    fn the_add_dialog_opens_empty_with_confirm_disabled_until_a_url_is_typed() {
        let mut app = backups_app();
        assert!(apply_local(&mut app, Action::OpenAddForm).is_none());

        assert_eq!(count_id(&app, "backup-destination-add-modal"), 1);
        assert_eq!(
            text_of(&app, "backup-destination-add-modal"),
            t::BACKUP_DESTINATION_FORM_ADD_TITLE
        );
        assert_eq!(text_of(&app, "backup-destination-url-input"), "");
        let confirm = elements(&app)
            .into_iter()
            .find(|e| e.id == "backup-destination-add-confirm-button")
            .expect("confirm button");
        assert!(!confirm.enabled, "blank URL must not arm the enroll");

        let _ = app.set_field(
            Field::Backups(BackupsField::Url),
            "https://nas.example.com".to_string(),
        );
        let confirm = elements(&app)
            .into_iter()
            .find(|e| e.id == "backup-destination-add-confirm-button")
            .expect("confirm button");
        assert!(confirm.enabled);
    }

    /// A blank-URL submit produces no op at all — the disabled confirm is the
    /// painted guard, and this is the keyboard-actuation backstop behind it.
    #[test]
    fn submitting_a_blank_url_produces_no_op() {
        let mut app = backups_app();
        apply_local(&mut app, Action::OpenAddForm);
        assert!(apply_local(&mut app, Action::SubmitForm(SubmitTarget::AddNest)).is_none());
    }

    /// Edit reopens the SAME dialog prefilled from the row it was opened on, in
    /// edit mode — `actions/backups.py::edit_destination` clicks the row's edit
    /// button then types into `backup-destination-name-input`, so the prefill is
    /// what makes a rename keep the existing URL.
    #[test]
    fn edit_prefills_the_dialog_from_its_own_row() {
        let mut app = backups_app();
        app.backups.destinations = vec![
            destination("d1", "https://a.example", Some("A")),
            destination("d2", "https://b.example", Some("B")),
        ];
        assert!(apply_local(&mut app, Action::OpenEditForm { index: 1 }).is_none());

        assert_eq!(app.backups.form, Some(FormMode::Edit("d2".to_string())));
        assert_eq!(
            text_of(&app, "backup-destination-url-input"),
            "https://b.example"
        );
        assert_eq!(text_of(&app, "backup-destination-name-input"), "B");
        assert_eq!(
            text_of(&app, "backup-destination-add-modal"),
            t::BACKUP_DESTINATION_FORM_EDIT_TITLE
        );
    }

    /// The remove-confirm dialog arms for the row it was opened on and closes
    /// the add dialog — never a one-click removal, and never two dialogs at once.
    #[test]
    fn remove_arms_its_own_row_and_closes_the_other_dialog() {
        let mut app = backups_app();
        app.backups.destinations = vec![
            destination("d1", "https://a.example", Some("A")),
            destination("d2", "https://b.example", Some("B")),
        ];
        apply_local(&mut app, Action::OpenAddForm);
        assert!(apply_local(&mut app, Action::OpenRemoveConfirm { index: 1 }).is_none());

        assert_eq!(app.backups.removing.as_deref(), Some("d2"));
        assert_eq!(count_id(&app, "backup-destination-add-modal"), 0);
        assert_eq!(count_id(&app, "backup-destination-remove-confirm-modal"), 1);
        assert_eq!(
            count_id(&app, "backup-destination-remove-confirm-button"),
            1
        );
        assert_eq!(count_id(&app, "backup-destination-remove-cancel-button"), 1);

        assert!(apply_local(&mut app, Action::CancelRemove).is_none());
        assert_eq!(app.backups.removing, None);
        assert_eq!(count_id(&app, "backup-destination-remove-confirm-modal"), 0);
    }

    /// Cancelling the add dialog drops the buffers, so a later add never
    /// inherits an abandoned URL.
    #[test]
    fn cancelling_the_add_dialog_clears_its_buffers() {
        let mut app = backups_app();
        apply_local(&mut app, Action::OpenAddForm);
        let _ = app.set_field(
            Field::Backups(BackupsField::Url),
            "https://typo.example".to_string(),
        );
        apply_local(&mut app, Action::CancelForm);

        assert_eq!(app.backups.form, None);
        assert_eq!(app.backups.url_input, "");
        apply_local(&mut app, Action::OpenAddForm);
        assert_eq!(text_of(&app, "backup-destination-url-input"), "");
    }

    /// A failed enroll lands on the page `error-message` and leaves the dialog
    /// OPEN so the URL can be corrected (`backups.md:284`); a success closes it
    /// and repaints from what the nest returned, never from the keystroke.
    #[test]
    fn a_failure_keeps_the_dialog_open_and_a_success_closes_it() {
        let mut app = backups_app();
        apply_local(&mut app, Action::OpenAddForm);
        let _ = app.set_field(
            Field::Backups(BackupsField::Url),
            "https://unreachable.example".to_string(),
        );

        apply_outcome(&mut app, Outcome::Failed("connect refused".to_string()));
        assert_eq!(
            app.errors.get(&Page::Backups).map(String::as_str),
            Some("connect refused")
        );
        assert_eq!(count_id(&app, "backup-destination-add-modal"), 1);
        assert_eq!(
            text_of(&app, "backup-destination-url-input"),
            "https://unreachable.example"
        );

        apply_outcome(
            &mut app,
            Outcome::Loaded {
                destinations: vec![destination("d1", "https://a.example", Some("A"))],
                unattested_destination_marks: Vec::new(),
                statuses: BTreeMap::new(),
                restore: RestoreData::default(),
                // No pass rode this outcome — the field under test is the
                // dialog/error fold, and `None` leaves the audit untouched.
                audit: None,
                restored: None,
                snapshots_page: None,
                // Not measured by this outcome — the row keeps what it paints.
                orphaned_store: None,
            },
        );
        assert!(!app.errors.contains_key(&Page::Backups));
        assert_eq!(app.backups.form, None);
        assert_eq!(count_id(&app, "backup-destination-add-modal"), 0);
        assert_eq!(count_id(&app, "backup-destination-status-row"), 1);
    }

    /// `attach_backup_destination_folder` clones the enrolled row
    /// per covered folder, so a destination with two attached folders arrives
    /// in `Outcome::Loaded` as three rows sharing one `destination_id`. The
    /// page must still paint exactly one status row for it.
    #[test]
    fn a_destination_with_covered_folders_paints_one_status_row() {
        let mut app = backups_app();
        let enrolled = destination("d1", "https://a.example", Some("A"));
        let covered_a = BackupDestination {
            folder_name: "__folder/deadbeef/1".into(),
            ..enrolled.clone()
        };
        let covered_b = BackupDestination {
            folder_name: "__folder/deadbeef/2".into(),
            ..enrolled.clone()
        };

        apply_outcome(
            &mut app,
            Outcome::Loaded {
                destinations: vec![enrolled, covered_a, covered_b],
                unattested_destination_marks: Vec::new(),
                statuses: BTreeMap::new(),
                restore: RestoreData::default(),
                audit: None,
                restored: None,
                snapshots_page: None,
                // Not measured by this outcome — the row keeps what it paints.
                orphaned_store: None,
            },
        );

        assert_eq!(app.backups.destinations.len(), 1);
        assert_eq!(count_id(&app, "backup-destination-status-row"), 1);
    }

    /// A destination the nest has no status row for still renders both status
    /// cells, at the not-yet-backed-up baseline — a blank cell would read as
    /// "the row never rendered" to a scoped e2e query.
    #[test]
    fn a_destination_without_a_status_row_renders_the_baseline() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://a.example", Some("A"))];
        assert_eq!(
            text_of(&app, "backup-destination-last-upload-time"),
            fauna_client_backup::row_text::destination_last_upload_text(None, 0)
        );
        assert_eq!(
            text_of(&app, "backup-destination-backlog-count"),
            fauna_client_backup::row_text::destination_backlog_text(None)
        );
        assert!(!text_of(&app, "backup-destination-backlog-count").is_empty());
    }

    /// A nest-projected status is matched to its row by `destination_id`, not by
    /// position — the projection's order is the nest's, not the config's.
    #[test]
    fn statuses_match_their_row_by_destination_id_not_position() {
        let mut app = backups_app();
        app.backups.destinations = vec![
            destination("d1", "https://a.example", Some("A")),
            destination("d2", "https://b.example", Some("B")),
        ];
        app.backups.statuses = BTreeMap::from([(
            "d2".to_string(),
            // Struct-update, not a hand-listed field set: this is a WIRE type
            // that grows, and every field this fixture does not care about is
            // noise that breaks the build the next time one arrives (it just
            // did — `held_bytes`/`cap_state` came with the client-custodian
            // wire). Prefer `..Default::default()` in fixtures over any growing
            // wire type, so two branches growing the same struct merge cleanly.
            BackupDestinationStatusItem {
                destination_id: "d2".to_string(),
                backlog_count: 42,
                ..Default::default()
            },
        )]);

        let backlogs: Vec<String> = elements(&app)
            .into_iter()
            .filter(|e| e.id == "backup-destination-backlog-count")
            .map(|e| e.text)
            .collect();
        assert_eq!(backlogs.len(), 2);
        assert!(
            !backlogs[0].contains("42"),
            "row 0 has no status: {backlogs:?}"
        );
        assert!(backlogs[1].contains("42"), "row 1 is d2's: {backlogs:?}");
    }

    /// Entering the tab implies a refresh — the numbers advance nest-side while
    /// no client runs, so a cached paint would be stale by construction.
    #[test]
    fn entering_the_tab_implies_a_refresh_when_authenticated() {
        let app = backups_app();
        assert!(matches!(
            nav_enter_op(&app.backups, Default::default()),
            Some(Op::Refresh { .. })
        ));
        // Pre-auth there is nothing to read and no secret to read it with.
        assert!(nav_enter_op(&BackupsState::default(), Default::default()).is_none());
    }

    // ── the restore half ─────────────────────────────────────────────────────

    fn snapshot(id: i64, kind: &str) -> SnapshotSummaryRow {
        SnapshotSummaryRow {
            id,
            created_at: 1_700_000_000,
            message_kind: Some(kind.to_string()),
            file_count: 3,
            total_bytes: 4096,
            ..Default::default()
        }
    }

    fn history(id: i64, snapshot_id: i64, source: Option<Vec<u8>>) -> RestoreHistoryRow {
        RestoreHistoryRow {
            id,
            completed_at: 1_700_000_500,
            snapshot_id,
            kinds_restored: "mail".to_string(),
            source_member_id: source,
            extra: Default::default(),
        }
    }

    fn divergence_row(snapshot_id: i64, mua: Option<&str>) -> RestoreDivergenceRow {
        RestoreDivergenceRow {
            id: 1,
            snapshot_id,
            observed_at: 1_700_000_600,
            protocol: "caldav".to_string(),
            collection: "INBOX".to_string(),
            mua_id: mua.map(str::to_string),
            client_modseq: 99,
            server_modseq: 1,
            lost_event_count: 98,
            extra: Default::default(),
        }
    }

    // ── The snapshot half (`ui/backups.md` § Snapshot-list shape) ────────────
    //
    // Every value the section paints is READ from the machine, so these pin the
    // RENDER contract (what an id shows, when a control is live, which surface a
    // result lands on). The derivations themselves are pinned in the machine's
    // own tier_1 suite — re-asserting them here would be the divergence the
    // shared machine exists to end.

    fn fs_row(name: &str) -> fauna_backups_machine::BackupFolderRow {
        fauna_backups_machine::BackupFolderRow {
            name: name.to_string(),
            snapshot_count: 1,
            last_snapshot_at: Some(1_700_000_000),
        }
    }

    fn snap_row(id: i64) -> fauna_backups_machine::SnapshotRow {
        fauna_backups_machine::SnapshotRow {
            id,
            created_at: 1_700_000_000,
            file_count: 3,
            total_bytes: 4096,
            ..Default::default()
        }
    }

    /// An app whose snapshot half is painted from `page`.
    fn app_with_page(page: BackupsSnapshot) -> App {
        let mut app = backups_app();
        app.backups.snap = Some(page);
        app
    }

    fn loaded_page(rows: Vec<fauna_backups_machine::SnapshotRow>) -> BackupsSnapshot {
        BackupsSnapshot {
            folders: vec![fs_row("alpha")],
            selected_folder: Some("alpha".to_string()),
            last_backed_up: rows.iter().map(|r| r.created_at).max(),
            snapshots: rows,
            ..Default::default()
        }
    }

    #[test]
    fn the_snapshot_half_paints_its_whole_id_set_once_loaded() {
        // The section's presence contract: the list container and the three
        // action controls are painted whenever the page has loaded, so no id
        // vanishes mid-poll.
        let app = app_with_page(loaded_page(vec![snap_row(7)]));
        for id in [
            "backup-folder-selector",
            "last-backed-up",
            "snapshot-list",
            "snapshot-item",
            "snapshot-create-button",
            "snapshot-prune-button",
            "snapshot-check-button",
            "snapshot-delete-button",
            "snapshot-immediate-delete-button",
        ] {
            assert_eq!(count_id(&app, id), 1, "{id} should paint exactly once");
        }
    }

    #[test]
    fn an_empty_list_still_paints_the_container_and_the_controls() {
        // The id must not vanish just because the set has no snapshots — an e2e
        // that polls `snapshot-list` would otherwise read "page not loaded".
        let app = app_with_page(loaded_page(vec![]));
        assert_eq!(count_id(&app, "snapshot-list"), 1);
        assert_eq!(count_id(&app, "snapshot-item"), 0);
        assert_eq!(count_id(&app, "snapshot-create-button"), 1);
    }

    #[test]
    fn last_backed_up_reads_never_on_a_set_with_no_snapshots() {
        let app = app_with_page(loaded_page(vec![]));
        assert_eq!(text_of(&app, "last-backed-up"), t::LAST_BACKED_UP_NEVER);
    }

    #[test]
    fn last_backed_up_renders_the_machines_derivation_not_a_selector_column() {
        // The selector row's `last_snapshot_at` is deliberately a DIFFERENT
        // value here: rendering it would be web's live `last_change_at` bug in
        // another costume.
        let mut page = loaded_page(vec![snap_row(7)]);
        page.last_backed_up = Some(1_600_000_000);
        page.folders[0].last_snapshot_at = Some(1_900_000_000);
        let app = app_with_page(page);
        let expected = crate::format::format_epoch_us(1_600_000_000i64.saturating_mul(1_000_000));
        assert!(
            text_of(&app, "last-backed-up").contains(&expected),
            "last-backed-up must render the machine's derivation, got {:?}",
            text_of(&app, "last-backed-up")
        );
    }

    #[test]
    fn an_in_flight_op_disables_every_mutating_control() {
        // Single-flight is ONE predicate; a control that missed it is the
        // ad-hoc partial busy flag the ratified section retires.
        let mut page = loaded_page(vec![snap_row(7)]);
        page.in_progress_op = Some(BackupOp::Create);
        let app = app_with_page(page);
        for id in [
            "snapshot-create-button",
            "snapshot-prune-button",
            "snapshot-check-button",
            "snapshot-delete-button",
            "snapshot-immediate-delete-button",
            "backup-folder-selector",
        ] {
            assert!(!enabled_of(&app, id), "{id} must be disabled while busy");
        }
    }

    #[test]
    fn a_busy_page_says_which_op_is_running() {
        // Copy comprehensibility rule 5: a disabled control the user can see
        // states why.
        let mut page = loaded_page(vec![snap_row(7)]);
        page.in_progress_op = Some(BackupOp::Prune);
        let app = app_with_page(page);
        assert!(
            elements(&app).iter().any(|e| e.text == t::BUSY_PRUNE),
            "the busy line names the running op"
        );
    }

    #[test]
    fn a_row_renders_its_id_time_count_and_size_never_a_raw_dump() {
        // The § Row content contract — this is what windows' record `ToString()`
        // fails today.
        let app = app_with_page(loaded_page(vec![snap_row(7)]));
        let text = text_of(&app, "snapshot-item");
        assert!(text.contains("#7"), "row names its id: {text:?}");
        assert!(
            text.contains(&crate::format::byte_size(4096)),
            "row renders a FORMATTED size: {text:?}"
        );
        assert!(
            !text.contains("4096"),
            "a raw byte count is the dump this retires: {text:?}"
        );
    }

    #[test]
    fn every_row_exposes_its_snapshot_id_to_automation() {
        // A cross-app contract, not a test nicety — the friction bar reads the
        // id back off the row it targets.
        let app = app_with_page(loaded_page(vec![snap_row(42)]));
        let row = elements(&app)
            .into_iter()
            .find(|e| e.id == "snapshot-item")
            .expect("row painted");
        assert_eq!(
            row.attrs
                .iter()
                .find(|(k, _)| k == "snapshot-id")
                .map(|(_, v)| v.as_str()),
            Some("42")
        );
    }

    #[test]
    fn the_row_id_is_readable_through_the_shared_scoped_query() {
        // The cross-app read is `get_attr("snapshot-item", "snapshot-id",
        // scope="snapshot-item[i]")`, and `Registry::matches` is a strict path
        // PREFIX test — so the row's own path must start with its own scope
        // step or every scoped read returns Null while the row paints
        // perfectly. That exact shape cost a red e2e on 2026-08-05, whose
        // diagnostic printed the row it could not read.
        let app = app_with_page(loaded_page(vec![snap_row(7), snap_row(8)]));
        for (index, expected) in [(0usize, "7"), (1usize, "8")] {
            let scope = ("snapshot-item".to_string(), index);
            let row = elements(&app)
                .into_iter()
                .find(|e| e.id == "snapshot-item" && e.path.first() == Some(&scope))
                .unwrap_or_else(|| panic!("no snapshot-item scoped to index {index}"));
            assert_eq!(
                row.attrs
                    .iter()
                    .find(|(k, _)| k == "snapshot-id")
                    .map(|(_, v)| v.as_str()),
                Some(expected)
            );
        }
        // …and the unscoped read still sees exactly one entry per row.
        assert_eq!(count_id(&app, "snapshot-item"), 2);
    }

    #[test]
    fn a_deletion_pending_row_renders_the_deadline_the_user_can_still_act_on() {
        let mut row = snap_row(7);
        row.state = SnapshotState::DeletionPending {
            execute_after: Some(1_800_000_000),
        };
        let app = app_with_page(loaded_page(vec![row]));
        let expected = crate::format::format_epoch_us(1_800_000_000i64.saturating_mul(1_000_000));
        assert!(text_of(&app, "snapshot-item").contains(&expected));
    }

    #[test]
    fn an_undated_lifecycle_state_still_renders_the_state() {
        // A pre-lifecycle-fields nest serves the state with no date. Hiding the
        // state because the date is missing is the failure; dropping only the
        // date is correct.
        let mut row = snap_row(7);
        row.state = SnapshotState::SoftDeleted { purge_after: None };
        let app = app_with_page(loaded_page(vec![row]));
        assert!(text_of(&app, "snapshot-item").contains(t::SNAPSHOT_STATE_SOFT_DELETED_UNDATED));
    }

    #[test]
    fn integrity_is_absent_until_a_check_runs() {
        // `Unknown` paints NOTHING — the word "unknown" on every row would read
        // as a finding, on a page whose whole subject is whether data is intact.
        let app = app_with_page(loaded_page(vec![snap_row(7)]));
        let text = text_of(&app, "snapshot-item");
        assert!(!text.contains(t::SNAPSHOT_INTEGRITY_OK));
        assert!(!text.contains(t::SNAPSHOT_INTEGRITY_IMPLICATED));
    }

    #[test]
    fn an_implicated_row_says_so_after_a_check() {
        let mut row = snap_row(7);
        row.integrity = RowIntegrity::Implicated;
        let app = app_with_page(loaded_page(vec![row]));
        assert!(text_of(&app, "snapshot-item").contains(t::SNAPSHOT_INTEGRITY_IMPLICATED));
    }

    #[test]
    fn a_completed_check_with_findings_is_a_result_not_an_error_message() {
        // § Architectural rules rule 6 — and an e2e that reads `error_text()` as
        // a failure witness depends on it.
        let mut page = loaded_page(vec![snap_row(7)]);
        page.check_result = Some(fauna_backups_machine::CheckOutcome {
            is_ok: false,
            missing_chunks: 2,
            ..Default::default()
        });
        let app = app_with_page(page);
        assert!(
            elements(&app)
                .iter()
                .any(|e| e.text.contains('2') && e.text.contains("Integrity check found problems")),
            "the verdict renders in the result surface"
        );
        assert!(
            !app.errors.contains_key(&Page::Backups),
            "and NOT on error-message"
        );
    }

    /// The four result-surface ids (`ui.yaml` backups `optional_elements`,
    /// approved 2026-08-13). Until they existed, every app painted these two
    /// surfaces as untagged chrome — so nothing cross-app could tell a dry run
    /// from an executed prune, which is where the six implementations had
    /// diverged most (`ui/backups.md` § Snapshot-list shape).
    #[test]
    fn the_result_surfaces_carry_their_ids_only_while_they_stand() {
        // Nothing standing: neither surface registers at all. This is the half
        // that makes presence a sound observable — a permanently-registered
        // empty element would make "a preview stands" unaskable.
        let app = app_with_page(loaded_page(vec![snap_row(7)]));
        assert_eq!(count_id(&app, "snapshot-check-result"), 0);
        assert_eq!(count_id(&app, "snapshot-prune-preview"), 0);
        assert_eq!(count_id(&app, "snapshot-prune-cancel-button"), 0);
        assert_eq!(count_id(&app, "snapshot-prune-execute-button"), 0);

        let mut page = loaded_page(vec![snap_row(7)]);
        page.check_result = Some(fauna_backups_machine::CheckOutcome {
            is_ok: true,
            snapshots_checked: 1,
            ..Default::default()
        });
        let app = app_with_page(page);
        assert_eq!(count_id(&app, "snapshot-check-result"), 1);
        assert!(
            text_of(&app, "snapshot-check-result").contains("Integrity check passed"),
            "the id carries the verdict itself, not an empty container: {:?}",
            text_of(&app, "snapshot-check-result")
        );
    }

    /// Execute renders ONLY over a preview that names candidates, so an e2e can
    /// read its presence as "a candidate still exists" — the observable that
    /// distinguishes a dry run from an applied prune without a row count (the
    /// nest's list keeps soft-deleted rows, so the count does not move).
    #[test]
    fn the_prune_preview_ids_track_the_execute_offer() {
        let mut page = loaded_page(vec![snap_row(7)]);
        page.prune_preview = Some(fauna_backups_machine::PrunePreview {
            would_prune: 0,
            remaining: 3,
            candidates: vec![],
            policy_state: PolicyState::Applied,
        });
        let app = app_with_page(page);
        assert_eq!(count_id(&app, "snapshot-prune-preview"), 1);
        assert_eq!(count_id(&app, "snapshot-prune-cancel-button"), 1);
        assert_eq!(
            count_id(&app, "snapshot-prune-execute-button"),
            0,
            "no candidates ⇒ no execute offered"
        );

        let mut page = loaded_page(vec![snap_row(7)]);
        page.prune_preview = Some(fauna_backups_machine::PrunePreview {
            would_prune: 1,
            remaining: 3,
            candidates: vec![fauna_backups_machine::PruneCandidate {
                id: 7,
                created_at: 1_700_000_000,
                tags: vec![],
            }],
            policy_state: PolicyState::Applied,
        });
        let app = app_with_page(page);
        assert_eq!(count_id(&app, "snapshot-prune-preview"), 1);
        assert_eq!(
            count_id(&app, "snapshot-prune-execute-button"),
            1,
            "a named candidate ⇒ execute offered"
        );
    }

    #[test]
    fn a_prune_preview_with_no_policy_says_so_instead_of_an_empty_success() {
        let mut page = loaded_page(vec![snap_row(7)]);
        page.prune_preview = Some(fauna_backups_machine::PrunePreview {
            policy_state: PolicyState::NotSet,
            ..Default::default()
        });
        let app = app_with_page(page);
        // On the ID'D element, not on chrome beside it: the two no-op states are
        // indistinguishable from outside unless `snapshot-prune-preview` itself
        // says which one it is.
        assert!(
            text_of(&app, "snapshot-prune-preview").contains(t::PRUNE_POLICY_NOT_SET),
            "the preview id must carry the no-policy verdict: {:?}",
            text_of(&app, "snapshot-prune-preview")
        );
        assert!(
            !elements(&app)
                .iter()
                .any(|e| e.text == t::PRUNE_EXECUTE_BUTTON),
            "nothing to execute when no policy is configured"
        );
    }

    #[test]
    fn a_prune_preview_offers_execute_only_when_it_names_candidates() {
        let mut page = loaded_page(vec![snap_row(7)]);
        page.prune_preview = Some(fauna_backups_machine::PrunePreview {
            would_prune: 0,
            remaining: 1,
            candidates: vec![],
            policy_state: PolicyState::Applied,
        });
        let app = app_with_page(page);
        assert!(
            text_of(&app, "snapshot-prune-preview").contains(t::PRUNE_PREVIEW_NOTHING),
            "the preview id must carry the nothing-to-prune verdict: {:?}",
            text_of(&app, "snapshot-prune-preview")
        );
        assert!(
            !elements(&app)
                .iter()
                .any(|e| e.text == t::PRUNE_EXECUTE_BUTTON)
        );

        let mut page = loaded_page(vec![snap_row(7)]);
        page.prune_preview = Some(fauna_backups_machine::PrunePreview {
            would_prune: 1,
            remaining: 2,
            candidates: vec![fauna_backups_machine::PruneCandidate {
                id: 7,
                created_at: 1_700_000_000,
                tags: vec![],
            }],
            policy_state: PolicyState::Applied,
        });
        let app = app_with_page(page);
        assert!(
            elements(&app)
                .iter()
                .any(|e| e.text == t::PRUNE_EXECUTE_BUTTON)
        );
    }

    #[test]
    fn the_immediate_delete_modal_opens_disarmed_and_arms_on_both_exact_fields() {
        // Rule 4 is a behavioural invariant, not styling: BOTH inputs must
        // exact-match before the confirm is live.
        let mut app = app_with_page(loaded_page(vec![snap_row(7)]));
        apply_local(&mut app, Action::OpenImmediateDelete { index: 0 });
        assert_eq!(count_id(&app, "immediate-delete-confirm-modal"), 1);
        assert!(!enabled_of(&app, "immediate-delete-confirm-button"));

        set_field(&mut app.backups, BackupsField::ImmediateConfirm, "7".into());
        assert!(
            !enabled_of(&app, "immediate-delete-confirm-button"),
            "the id alone must not arm it"
        );
        set_field(
            &mut app.backups,
            BackupsField::ImmediateAck,
            fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT.into(),
        );
        assert!(enabled_of(&app, "immediate-delete-confirm-button"));
    }

    #[test]
    fn a_wrong_snapshot_id_never_arms_the_friction_bar() {
        let mut app = app_with_page(loaded_page(vec![snap_row(7)]));
        apply_local(&mut app, Action::OpenImmediateDelete { index: 0 });
        set_field(&mut app.backups, BackupsField::ImmediateConfirm, "8".into());
        set_field(
            &mut app.backups,
            BackupsField::ImmediateAck,
            fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT.into(),
        );
        assert!(!enabled_of(&app, "immediate-delete-confirm-button"));
        // And the keyboard backstop refuses too, not just the paint.
        assert!(apply_local(&mut app, Action::ConfirmImmediateDelete).is_none());
    }

    #[test]
    fn cancelling_the_modal_clears_both_buffers() {
        // A retained ack phrase would leave the NEXT snapshot's modal one field
        // away from armed — a one-click delete by another route.
        let mut app = app_with_page(loaded_page(vec![snap_row(7), snap_row(8)]));
        apply_local(&mut app, Action::OpenImmediateDelete { index: 0 });
        set_field(&mut app.backups, BackupsField::ImmediateConfirm, "7".into());
        set_field(
            &mut app.backups,
            BackupsField::ImmediateAck,
            fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT.into(),
        );
        apply_local(&mut app, Action::CancelImmediateDelete);
        assert_eq!(count_id(&app, "immediate-delete-confirm-modal"), 0);

        apply_local(&mut app, Action::OpenImmediateDelete { index: 1 });
        assert!(
            !enabled_of(&app, "immediate-delete-confirm-button"),
            "the next modal must start disarmed"
        );
    }

    #[test]
    fn the_open_detail_paints_its_files_and_a_download_for_regular_files_only() {
        let mut page = loaded_page(vec![snap_row(7)]);
        page.detail = Some(fauna_backups_machine::SnapshotDetail {
            snapshot_id: 7,
            files: vec![
                fauna_backups_machine::SnapshotFileRow {
                    path: "docs/a.txt".to_string(),
                    size_bytes: 12,
                    file_type: "regular".to_string(),
                    manifest_hash: "ab".repeat(32),
                },
                fauna_backups_machine::SnapshotFileRow {
                    path: "docs".to_string(),
                    size_bytes: 0,
                    file_type: "dir".to_string(),
                    manifest_hash: String::new(),
                },
            ],
        });
        let app = app_with_page(page);
        assert_eq!(count_id(&app, "snapshot-detail-files"), 1);
        assert_eq!(
            count_scoped_in(
                &app,
                "snapshot-file-download-button",
                "snapshot-detail-files",
                0
            ),
            1,
            "a directory row gets no download affordance — a dead button is worse \
             than none"
        );
    }

    #[test]
    fn no_detail_paints_no_detail_container() {
        let app = app_with_page(loaded_page(vec![snap_row(7)]));
        assert_eq!(count_id(&app, "snapshot-detail-files"), 0);
    }

    #[test]
    fn a_download_on_a_non_regular_row_is_refused_rather_than_dispatched() {
        let mut page = loaded_page(vec![snap_row(7)]);
        page.detail = Some(fauna_backups_machine::SnapshotDetail {
            snapshot_id: 7,
            files: vec![fauna_backups_machine::SnapshotFileRow {
                path: "docs".to_string(),
                size_bytes: 0,
                file_type: "dir".to_string(),
                manifest_hash: String::new(),
            }],
        });
        let mut app = app_with_page(page);
        assert!(apply_local(&mut app, Action::DownloadFile { index: 0 }).is_none());
    }

    #[test]
    fn a_machine_error_becomes_the_page_error_message_and_clears_on_success() {
        // The machine holds the snapshot half's only error state; this is the
        // one place it crosses onto the page banner, and a cleared machine error
        // must clear the banner or a successful retry leaves the old failure up.
        let mut app = backups_app();
        let mut page = loaded_page(vec![]);
        page.error = Some(fauna_core::localized::LocalizedText::key_arg(
            "backups.error_refresh",
            "message",
            "connection reset".to_string(),
        ));
        fold_snapshots_page(&mut app, page);
        assert!(
            app.errors
                .get(&Page::Backups)
                .is_some_and(|m| m.contains("connection reset"))
        );

        fold_snapshots_page(&mut app, loaded_page(vec![snap_row(7)]));
        assert!(!app.errors.contains_key(&Page::Backups));
    }

    #[test]
    fn a_deleted_snapshot_closes_the_modal_armed_for_it() {
        // Otherwise the friction bar stays armed for a row that is gone, and the
        // confirm would fire at a snapshot id the nest no longer has.
        let mut app = app_with_page(loaded_page(vec![snap_row(7)]));
        apply_local(&mut app, Action::OpenImmediateDelete { index: 0 });
        assert_eq!(count_id(&app, "immediate-delete-confirm-modal"), 1);

        fold_snapshots_page(&mut app, loaded_page(vec![snap_row(8)]));
        assert_eq!(count_id(&app, "immediate-delete-confirm-modal"), 0);
        assert!(app.backups.immediate_confirm.is_empty());
    }

    #[test]
    fn confirming_immediate_delete_does_not_close_the_modal_eagerly() {
        // The modal closes on the row LEAVING the machine's list
        // (`fold_snapshots_page`), never on the confirm dispatch itself — a
        // regression here would hide every `hard_floor_breach` rejection
        // behind an already-dismissed modal (linux's / windows' "close only
        // on landed" pattern, both inherited from the same lesson).
        let mut app = app_with_page(loaded_page(vec![snap_row(7)]));
        apply_local(&mut app, Action::OpenImmediateDelete { index: 0 });
        set_field(&mut app.backups, BackupsField::ImmediateConfirm, "7".into());
        set_field(
            &mut app.backups,
            BackupsField::ImmediateAck,
            fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT.into(),
        );
        assert!(apply_local(&mut app, Action::ConfirmImmediateDelete).is_some());
        assert_eq!(count_id(&app, "immediate-delete-confirm-modal"), 1);
    }

    #[test]
    fn a_rejected_immediate_delete_leaves_the_modal_open_with_inputs_standing() {
        // A `hard_floor_breach` refusal returns from the same call and must
        // leave the modal AND the typed inputs standing, so the user can
        // retry without re-typing (the row is still present, unlike the
        // landed-delete case above).
        let mut app = app_with_page(loaded_page(vec![snap_row(7)]));
        apply_local(&mut app, Action::OpenImmediateDelete { index: 0 });
        set_field(&mut app.backups, BackupsField::ImmediateConfirm, "7".into());
        set_field(
            &mut app.backups,
            BackupsField::ImmediateAck,
            fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT.into(),
        );
        apply_local(&mut app, Action::ConfirmImmediateDelete);

        let mut rejected = loaded_page(vec![snap_row(7)]);
        rejected.error = Some(fauna_core::localized::LocalizedText::key_arg(
            "backups.error_refresh",
            "message",
            "hard floor breach".to_string(),
        ));
        fold_snapshots_page(&mut app, rejected);

        assert_eq!(
            count_id(&app, "immediate-delete-confirm-modal"),
            1,
            "modal stays open after a rejected confirm"
        );
        assert_eq!(app.backups.immediate_confirm, "7");
        assert_eq!(
            app.backups.immediate_ack,
            fauna_protocol::filesync::IMMEDIATE_DELETE_ACK_TEXT
        );
        assert!(
            app.errors
                .get(&Page::Backups)
                .is_some_and(|m| m.contains("hard floor breach"))
        );
    }

    // ── `snapshot-undelete-button` (§ *Soft-deleted rows* ruling) ───────────

    /// A soft-deleted row inside its recovery window.
    fn soft_deleted_row(id: i64) -> fauna_backups_machine::SnapshotRow {
        fauna_backups_machine::SnapshotRow {
            state: SnapshotState::SoftDeleted {
                purge_after: Some(1_700_100_000),
            },
            ..snap_row(id)
        }
    }

    #[test]
    fn the_undelete_button_paints_only_on_a_soft_deleted_row() {
        // Presence IS the state observable, so an Active or deletion-pending row
        // must paint no such control — a always-painted button would offer
        // recovery of something that was never deleted, and would make the id
        // useless to an e2e that asks "is this row recoverable".
        let active_only = app_with_page(loaded_page(vec![snap_row(7)]));
        assert_eq!(count_id(&active_only, "snapshot-undelete-button"), 0);

        let pending = app_with_page(loaded_page(vec![fauna_backups_machine::SnapshotRow {
            state: SnapshotState::DeletionPending {
                execute_after: Some(1_700_100_000),
            },
            ..snap_row(7)
        }]));
        assert_eq!(count_id(&pending, "snapshot-undelete-button"), 0);

        let recoverable = app_with_page(loaded_page(vec![soft_deleted_row(7)]));
        assert_eq!(count_id(&recoverable, "snapshot-undelete-button"), 1);
    }

    #[test]
    fn the_undelete_button_is_scoped_to_its_own_row() {
        // The scoped read `is_visible("snapshot-undelete-button",
        // scope="snapshot-item[1]")` is how a shared e2e asks which row is
        // recoverable; an unscoped paint answers for the wrong row.
        let app = app_with_page(loaded_page(vec![snap_row(7), soft_deleted_row(8)]));
        assert_eq!(
            count_scoped_in(&app, "snapshot-undelete-button", "snapshot-item", 0),
            0,
            "the Active row owns no recover control"
        );
        assert_eq!(
            count_scoped_in(&app, "snapshot-undelete-button", "snapshot-item", 1),
            1,
            "the soft-deleted row owns exactly one"
        );
    }

    #[test]
    fn the_undelete_button_takes_the_single_flight_rule_like_every_mutating_control() {
        let mut page = loaded_page(vec![soft_deleted_row(7)]);
        page.in_progress_op = Some(BackupOp::Create);
        let app = app_with_page(page);
        assert!(!enabled_of(&app, "snapshot-undelete-button"));
    }

    #[test]
    fn undeleting_dispatches_the_machine_gesture_with_that_rows_id() {
        // The row CLICKED, not the first soft-deleted row of the set — the same
        // rule the detail read is pinned on.
        let mut app = app_with_page(loaded_page(vec![soft_deleted_row(7), soft_deleted_row(9)]));
        let op = apply_local(&mut app, Action::UndeleteSnapshot { index: 1 });
        assert!(matches!(
            op,
            Some(Op::Snapshots {
                gesture: SnapshotGesture::Undelete(9),
                ..
            })
        ));
    }

    #[test]
    fn undelete_stays_live_offline() {
        // `fauna.filesync.snapshot.undelete` is OfflineSafe on the shared
        // registry, and the page must not desensitize it — recovering a row the
        // nest still holds is exactly as replayable as the delete beside it.
        assert_eq!(
            Action::UndeleteSnapshot { index: 0 }.wire_kind(),
            Some("fauna.filesync.snapshot.undelete")
        );
    }

    #[test]
    fn the_folder_selector_states_why_it_is_disabled_with_no_sets() {
        let app = app_with_page(BackupsSnapshot::default());
        assert!(!enabled_of(&app, "backup-folder-selector"));
        assert!(elements(&app).iter().any(|e| e.text == t::NO_FOLDERS));
    }

    #[test]
    fn selecting_a_folder_dispatches_the_machine_gesture_by_name() {
        // The raw-value picker contract: `select(id, name)` carries the set NAME.
        let mut app = app_with_page(loaded_page(vec![snap_row(7)]));
        let op = apply_local(&mut app, Action::SelectFolder("beta".to_string()));
        assert!(matches!(
            op,
            Some(Op::Snapshots {
                gesture: SnapshotGesture::Select(ref n),
                ..
            }) if n == "beta"
        ));
    }

    fn enabled_of(app: &App, id: &str) -> bool {
        elements(app)
            .into_iter()
            .find(|e| e.id == id)
            .map(|e| e.enabled)
            .unwrap_or_else(|| panic!("{id} not painted"))
    }

    /// The whole restore section paints with **zero** destinations configured.
    ///
    /// Load-bearing: the local restore path has nothing to do with having a
    /// backup destination, and zero-destinations is the exact state all three
    /// `test_backups_restore.py` cases run in. The destination half used to
    /// return early on an empty list, which would have hidden every id here.
    #[test]
    fn the_restore_section_paints_with_no_destinations_configured() {
        let app = backups_app();
        assert!(app.backups.destinations.is_empty());
        for id in [
            "restore-source-select",
            "restore-snapshot-select",
            "restore-kinds-checkboxes",
            "restore-confirm-input",
            "restore-confirm-button",
            "restore-progress",
            "restore-history-section",
            "restore-history-list",
        ] {
            assert_eq!(count_id(&app, id), 1, "{id} missing at zero destinations");
        }
        assert_eq!(count_id(&app, "restore-kind-checkbox"), RESTORE_KINDS.len());
    }

    /// `restore-source-select` is disabled with nothing to pick and enabled once
    /// a destination exists — `backups.md:59` ("Disabled when zero destinations
    /// are configured"), windows' shape for the same element.
    #[test]
    fn the_source_picker_is_disabled_until_a_destination_exists() {
        let mut app = backups_app();
        assert!(!enabled_of(&app, "restore-source-select"));

        app.backups.destinations = vec![destination("d1", "https://a.example", Some("A"))];
        assert!(enabled_of(&app, "restore-source-select"));
        assert_eq!(text_of(&app, "restore-source-select"), "A");
    }

    /// The friction bar: armed only by the SELECTED snapshot's exact id. This is
    /// the assertion `test_local_restore_action_restores_mail` drives — disabled,
    /// still disabled on a wrong string, enabled on the id.
    #[test]
    fn the_friction_bar_arms_only_on_the_selected_snapshots_exact_id() {
        let mut app = backups_app();
        app.backups.snapshots = vec![snapshot(7, "mail")];

        assert!(!enabled_of(&app, "restore-confirm-button"));

        let _ = app.set_field(
            Field::Backups(BackupsField::RestoreConfirm),
            "not-the-id".to_string(),
        );
        assert!(!enabled_of(&app, "restore-confirm-button"));

        let _ = app.set_field(
            Field::Backups(BackupsField::RestoreConfirm),
            "7".to_string(),
        );
        assert!(enabled_of(&app, "restore-confirm-button"));

        // A typed id that matches a DIFFERENT snapshot than the selected one
        // must not arm it — the bar is per-selection, not per-list.
        app.backups.snapshots.push(snapshot(8, "calendar"));
        app.backups.selected_snapshot = 1;
        assert!(!enabled_of(&app, "restore-confirm-button"));
    }

    /// An in-flight restore disarms the button (`backups.md:63`), so a second
    /// click cannot start a concurrent restore of the same snapshot.
    #[test]
    fn an_in_flight_restore_disarms_the_button() {
        let mut app = backups_app();
        app.backups.snapshots = vec![snapshot(7, "mail")];
        let _ = app.set_field(
            Field::Backups(BackupsField::RestoreConfirm),
            "7".to_string(),
        );

        let op = apply_local(&mut app, Action::SubmitRestore);
        assert!(matches!(op, Some(Op::Restore { snapshot_id: 7, .. })));
        assert_eq!(app.backups.restore_progress, RestoreProgress::Running);
        assert!(!enabled_of(&app, "restore-confirm-button"));
        assert!(apply_local(&mut app, Action::SubmitRestore).is_none());
    }

    /// A failed restore returns the page to idle. Without this the `Running`
    /// guard above would latch and the friction bar could never re-arm — the
    /// user would have to restart the app to retry.
    #[test]
    fn a_failed_restore_re_arms_the_friction_bar() {
        let mut app = backups_app();
        app.backups.snapshots = vec![snapshot(7, "mail")];
        let _ = app.set_field(
            Field::Backups(BackupsField::RestoreConfirm),
            "7".to_string(),
        );
        apply_local(&mut app, Action::SubmitRestore);

        apply_outcome(&mut app, Outcome::Failed("nest refused".to_string()));
        assert_eq!(app.backups.restore_progress, RestoreProgress::Idle);
        assert_eq!(
            app.errors.get(&Page::Backups).map(String::as_str),
            Some("nest refused")
        );
        assert!(enabled_of(&app, "restore-confirm-button"));
    }

    /// A completed restore reports done and clears the typed id, so the bar is
    /// not left armed for an accidental repeat.
    #[test]
    fn a_completed_restore_reports_done_and_clears_the_bar() {
        let mut app = backups_app();
        app.backups.snapshots = vec![snapshot(7, "mail")];
        let _ = app.set_field(
            Field::Backups(BackupsField::RestoreConfirm),
            "7".to_string(),
        );
        apply_local(&mut app, Action::SubmitRestore);

        apply_outcome(
            &mut app,
            Outcome::Loaded {
                destinations: Vec::new(),
                unattested_destination_marks: Vec::new(),
                statuses: BTreeMap::new(),
                restore: RestoreData {
                    snapshots: vec![snapshot(7, "mail")],
                    history: vec![history(1, 7, None)],
                    divergence: BTreeMap::new(),
                },
                audit: None,
                restored: Some(Restored {
                    config_present: true,
                }),
                snapshots_page: None,
                // Not measured by this outcome — the row keeps what it paints.
                orphaned_store: None,
            },
        );
        assert_eq!(app.backups.restore_progress, RestoreProgress::Done);
        assert_eq!(text_of(&app, "restore-progress"), t::RESTORE_PROGRESS_DONE);
        assert_eq!(text_of(&app, "restore-confirm-input"), "");
        assert!(!enabled_of(&app, "restore-confirm-button"));
        assert_eq!(count_id(&app, "restore-history-item"), 1);
    }

    /// A restore whose reply said `config_present == false` paints
    /// `restore-warning` beside the DONE line — the reply is the advisory's only
    /// carrier — and a complete one, or the next plain read, paints none. Never
    /// through `error-message`: the restore succeeded.
    #[test]
    fn a_restore_without_the_configuration_warns_and_a_complete_one_does_not() {
        fn restored(restored: Option<Restored>) -> Outcome {
            Outcome::Loaded {
                destinations: Vec::new(),
                unattested_destination_marks: Vec::new(),
                statuses: BTreeMap::new(),
                restore: RestoreData {
                    snapshots: vec![snapshot(7, "mail")],
                    history: vec![history(1, 7, None)],
                    divergence: BTreeMap::new(),
                },
                audit: None,
                restored,
                snapshots_page: None,
                orphaned_store: None,
            }
        }
        let mut app = backups_app();
        assert_eq!(count_id(&app, "restore-warning"), 0);

        apply_outcome(
            &mut app,
            restored(Some(Restored {
                config_present: false,
            })),
        );
        assert_eq!(app.backups.restore_progress, RestoreProgress::Done);
        assert_eq!(
            text_of(&app, "restore-warning"),
            t::RESTORE_WARNING_CONFIG_ABSENT
        );
        assert!(!app.errors.contains_key(&Page::Backups));

        // The next plain read (a re-arrival) retires the one-shot advisory.
        apply_outcome(&mut app, restored(None));
        assert_eq!(count_id(&app, "restore-warning"), 0);

        apply_outcome(
            &mut app,
            restored(Some(Restored {
                config_present: true,
            })),
        );
        assert_eq!(app.backups.restore_progress, RestoreProgress::Done);
        assert_eq!(count_id(&app, "restore-warning"), 0);
    }

    /// A history row with no `source_member_id` reads "local snapshot" — the
    /// state every row is in today. `test_restore_history_renders` asserts
    /// exactly this string plus the kind.
    #[test]
    fn a_local_restore_row_names_the_local_snapshot_source() {
        let mut app = backups_app();
        app.backups.restore_history = vec![history(1, 7, None)];

        let text = text_of(&app, "restore-history-item");
        assert!(
            text.contains(t::RESTORE_SOURCE_LOCAL),
            "row should name the local source: {text:?}"
        );
        assert!(text.contains(t::RESTORE_KINDS_MAIL), "{text:?}");
    }

    /// A row that DID come from a destination renders the SHARED short hex, so
    /// the seven apps label the same destination member identically.
    #[test]
    fn a_destination_sourced_row_uses_the_shared_short_hex() {
        let mut app = backups_app();
        let member = vec![0xab, 0xcd, 0xef, 0x01, 0x99];
        app.backups.restore_history = vec![history(1, 7, Some(member.clone()))];

        let text = text_of(&app, "restore-history-item");
        assert!(
            text.contains(&fauna_core::format::hex_short(&member)),
            "{text:?}"
        );
        assert!(!text.contains(t::RESTORE_SOURCE_LOCAL), "{text:?}");
    }

    /// The banner renders ONLY for a snapshot with ≥1 divergence row
    /// (`backups.md:75`) — and, critically, registers inside its OWN history
    /// row. `actions/backups.py` reads it with the single-step scope
    /// `restore-history-item[i]`, which the registry resolves as a container the
    /// banner must be *inside* — so a banner painted flat resolves to nothing
    /// while the page looks perfect. (Nesting it under the list container is no
    /// longer fatal: since the 2026-08-14 descendant ruling a scope resolves
    /// through unnamed ancestors.)
    #[test]
    fn the_banner_renders_only_when_diverged_and_inside_its_own_row() {
        let mut app = backups_app();
        app.backups.restore_history = vec![history(1, 7, None), history(2, 8, None)];
        // Row 0's snapshot has an entry but no rows; row 1's has one.
        app.backups.divergence = BTreeMap::from([
            (7, Vec::new()),
            (8, vec![divergence_row(8, Some("Fauna-UI-Test/1.0"))]),
        ]);

        assert_eq!(count_id(&app, "restore-divergence-banner"), 1);
        assert_eq!(
            count_scoped_in(&app, "restore-divergence-banner", "restore-history-item", 0),
            0
        );
        assert_eq!(
            count_scoped_in(&app, "restore-divergence-banner", "restore-history-item", 1),
            1
        );

        // The banner counts the rows, which is what the e2e reads off it.
        let banner = elements(&app)
            .into_iter()
            .find(|e| e.id == "restore-divergence-banner")
            .expect("banner");
        assert!(banner.text.contains('1'), "{:?}", banner.text);
    }

    /// Opening a row's banner paints the forensic modal: one detail item per
    /// divergence row, an `(unknown)` MUA when the protocol offered none, and no
    /// action button beyond the untagged close (`backups.md:76`).
    #[test]
    fn the_divergence_modal_lists_its_rows_and_closes_cleanly() {
        let mut app = backups_app();
        app.backups.restore_history = vec![history(1, 7, None)];
        app.backups.divergence = BTreeMap::from([(
            7,
            vec![
                divergence_row(7, Some("Fauna-UI-Test/1.0")),
                divergence_row(7, None),
            ],
        )]);

        assert_eq!(count_id(&app, "restore-divergence-details-modal"), 0);
        assert!(apply_local(&mut app, Action::OpenDivergenceModal { index: 0 }).is_none());

        assert_eq!(count_id(&app, "restore-divergence-details-modal"), 1);
        assert_eq!(count_id(&app, "restore-divergence-details-item"), 2);
        let items: Vec<String> = elements(&app)
            .into_iter()
            .filter(|e| e.id == "restore-divergence-details-item")
            .map(|e| e.text)
            .collect();
        // "~98 writes lost" — the modseq delta the e2e asserts on.
        assert!(items[0].contains("98"), "{items:?}");
        assert!(items[0].contains("Fauna-UI-Test/1.0"), "{items:?}");
        assert!(
            items[1].contains(t::RESTORE_DIVERGENCE_UNKNOWN_MUA),
            "{items:?}"
        );

        assert!(apply_local(&mut app, Action::CloseDivergenceModal).is_none());
        assert_eq!(count_id(&app, "restore-divergence-details-modal"), 0);
    }

    /// A banner-less row cannot open the modal — the actuation is only reachable
    /// from a banner that paints, so a stale one must not open an empty modal.
    #[test]
    fn a_row_without_divergence_cannot_open_the_modal() {
        let mut app = backups_app();
        app.backups.restore_history = vec![history(1, 7, None)];

        assert!(apply_local(&mut app, Action::OpenDivergenceModal { index: 0 }).is_none());
        assert_eq!(app.backups.divergence_modal, None);
        assert_eq!(count_id(&app, "restore-divergence-details-modal"), 0);
    }

    /// Both kinds start checked (`backups.md:61`) and each toggles on its own.
    #[test]
    fn both_restore_kinds_start_checked_and_toggle_independently() {
        let mut app = backups_app();
        assert!(app.backups.restore_kinds.checked(0));
        assert!(app.backups.restore_kinds.checked(1));

        apply_local(&mut app, Action::ToggleRestoreKind { index: 0 });
        assert!(!app.backups.restore_kinds.checked(0));
        assert!(app.backups.restore_kinds.checked(1));
    }

    /// The pickers select by their painted label, which is all a `select`
    /// carries across the driver boundary.
    #[test]
    fn the_pickers_select_by_their_painted_label() {
        let mut app = backups_app();
        app.backups.snapshots = vec![snapshot(7, "mail"), snapshot(8, "calendar")];
        app.backups.destinations = vec![
            destination("d1", "https://a.example", Some("A")),
            destination("d2", "https://b.example", Some("B")),
        ];

        apply_local(
            &mut app,
            Action::SelectSnapshot(snapshot_label(&snapshot(8, "calendar"))),
        );
        assert_eq!(app.backups.selected_snapshot, 1);
        assert_eq!(text_of(&app, "restore-snapshot-select"), "calendar (#8)");

        apply_local(&mut app, Action::SelectRestoreSource("B".to_string()));
        assert_eq!(app.backups.selected_source, 1);

        // An unknown label leaves the selection where it was.
        apply_local(&mut app, Action::SelectRestoreSource("gone".to_string()));
        assert_eq!(app.backups.selected_source, 1);
    }

    /// A refresh that returns a SHORTER list pulls the selection back in bounds.
    /// Left past the end, `selected_snapshot_id` would answer `None` and the
    /// friction bar would be permanently unarmable with nothing on screen to
    /// explain why.
    #[test]
    fn a_shrinking_snapshot_list_clamps_the_selection() {
        let mut app = backups_app();
        app.backups.snapshots = vec![snapshot(7, "mail"), snapshot(8, "calendar")];
        app.backups.selected_snapshot = 1;

        apply_outcome(
            &mut app,
            Outcome::Loaded {
                destinations: Vec::new(),
                unattested_destination_marks: Vec::new(),
                statuses: BTreeMap::new(),
                restore: RestoreData {
                    snapshots: vec![snapshot(7, "mail")],
                    ..RestoreData::default()
                },
                audit: None,
                restored: None,
                snapshots_page: None,
                // Not measured by this outcome — the row keeps what it paints.
                orphaned_store: None,
            },
        );
        assert_eq!(app.backups.selected_snapshot, 0);

        let _ = app.set_field(
            Field::Backups(BackupsField::RestoreConfirm),
            "7".to_string(),
        );
        assert!(enabled_of(&app, "restore-confirm-button"));
    }

    /// With no snapshots the picker is disabled and says so, rather than
    /// offering an empty control that can never arm the bar.
    #[test]
    fn an_empty_snapshot_list_disables_the_picker() {
        let app = backups_app();
        assert_eq!(count_id(&app, "restore-snapshot-select"), 1);
        assert!(!enabled_of(&app, "restore-snapshot-select"));
        assert!(!enabled_of(&app, "restore-confirm-button"));
    }

    /// Sign-out drops the page state — load-bearing because it holds the owner's
    /// identity seed, not merely because the rows would be stale.
    ///
    /// A `tokio::test` because the teardown's first move is `session::sign_out`,
    /// which spawns the WS disconnect.
    #[tokio::test]
    async fn sign_out_drops_the_secret_with_the_page() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://a.example", Some("A"))];
        app.backups.restore_history = vec![history(1, 7, None)];
        app.backups.snapshots = vec![snapshot(7, "mail")];
        app.backups.restore_confirm = "7".to_string();
        assert!(app.backups.secret.is_some());

        app.drop_authenticated_state(fauna_client_account_runtime::StopReason::AccountSwitch);
        assert!(app.backups.secret.is_none());
        assert!(app.backups.nest.is_none());
        assert!(app.backups.destinations.is_empty());
        // The restore half is another account's data too — a switch must not
        // leave the previous owner's snapshot ids or history on screen.
        assert!(app.backups.restore_history.is_empty());
        assert!(app.backups.snapshots.is_empty());
        assert!(app.backups.restore_confirm.is_empty());
    }

    // ── The offline gate's backups declarations (W4 (account-data-plane.md § Workstreams) phase 4, row 43) ────────

    /// One instance of **every** [`Action`] variant, plus the two extra
    /// [`SubmitTarget`]s because that gesture's three arms answer three kinds.
    ///
    /// Hand-built for the reason the admin corpus records: walk invariants
    /// I6/I7 see only what a page *paints*, and this page paints almost nothing
    /// offline — the destination rows, the snapshot list and the restore half
    /// are all rendered from reads that never land without a nest, and half of
    /// these gestures live inside modals that only a loaded row can open. Every
    /// declaration below would go unchecked, and a typo in one reads as
    /// `Available` forever (`affordance`'s ruling 2).
    fn every_action() -> Vec<Action> {
        let s = || "x".to_string();
        vec![
            Action::OpenAddForm,
            Action::OpenEditForm { index: 0 },
            Action::CancelForm,
            Action::SubmitForm(SubmitTarget::AddNest),
            Action::SubmitForm(SubmitTarget::AddCustodian),
            Action::SubmitForm(SubmitTarget::Edit),
            Action::OpenRemoveConfirm { index: 0 },
            Action::KeepDestination { index: 0 },
            Action::CancelRemove,
            Action::ConfirmRemove,
            Action::SelectDestinationKind(s()),
            Action::SelectRestoreSource(s()),
            Action::SelectSnapshot(s()),
            Action::SelectFolder(s()),
            Action::CreateSnapshot,
            Action::DeleteSnapshot { index: 0 },
            Action::OpenImmediateDelete { index: 0 },
            Action::CancelImmediateDelete,
            Action::ConfirmImmediateDelete,
            Action::PrunePreview,
            Action::PruneExecute,
            Action::PruneCancel,
            Action::CheckIntegrity,
            Action::OpenSnapshot { index: 0 },
            Action::CloseSnapshotDetail,
            Action::DownloadFile { index: 0 },
            Action::ToggleRestoreKind { index: 0 },
            Action::SubmitRestore,
            Action::OpenDivergenceModal { index: 0 },
            Action::CloseDivergenceModal,
        ]
    }

    /// `Action` has this many variants; [`every_action`] carries one instance of
    /// each, plus the two extra `SubmitForm` targets.
    const ACTION_COUNT: usize = 28;

    #[test]
    fn every_backups_action_is_in_the_corpus() {
        assert_eq!(
            every_action().len(),
            ACTION_COUNT + 2,
            "a new `Action` variant must be added to `every_action` — otherwise \
             its wire-kind declaration is never checked against the registry"
        );
    }

    /// I7 at the type level: every kind this page declares must be one the
    /// shared table knows. An unregistered kind reads as `Available` by design
    /// (ruling 2 — forward compatibility), so a misspelling here silently
    /// *ungates* that affordance and nothing reports it.
    #[test]
    fn every_declared_backups_kind_is_registered() {
        crate::test_support::assert_every_wire_kind_is_registered(every_action(), |a| {
            a.wire_kind()
        });
    }

    /// The **exact kind** per gesture, not just its class — with five sibling
    /// `fauna.filesync.snapshot.*` kinds and four `fauna.backup.*` ones in play,
    /// a class-only assertion passes when two arms are swapped, and the
    /// declaration's whole value is that a later reclassification reaches this
    /// page for free.
    #[test]
    fn backups_declares_the_exact_kind_per_gesture() {
        use fauna_protocol::offline_class::{OfflineClass, offline_class};
        for (action, expected, class) in [
            // ── the enrollment plane: a destination has to be reachable ─────
            (
                Action::SubmitForm(SubmitTarget::AddNest),
                "fauna.backup.nest_key.grant",
                OfflineClass::OnlineOnly,
            ),
            (
                Action::SubmitForm(SubmitTarget::AddCustodian),
                "fauna.backup.destination.register",
                OfflineClass::OnlineOnly,
            ),
            (
                Action::ConfirmRemove,
                "fauna.backup.destination.remove",
                OfflineClass::OnlineOnly,
            ),
            // ── the three irreversible snapshot verbs ───────────────────────
            (
                Action::ConfirmImmediateDelete,
                "fauna.filesync.snapshot.delete_immediate",
                OfflineClass::OnlineOnly,
            ),
            (
                Action::PruneExecute,
                "fauna.filesync.snapshot.prune_set_policy",
                OfflineClass::OnlineOnly,
            ),
            (
                Action::SubmitRestore,
                "fauna.filesync.snapshot.restore_message_kind",
                OfflineClass::OnlineOnly,
            ),
            // ── and the halves that STAY LIVE, which is the page's point ────
            (
                Action::SubmitForm(SubmitTarget::Edit),
                "fauna.account.state.put",
                OfflineClass::OfflineSafe,
            ),
            (
                Action::KeepDestination { index: 0 },
                "fauna.account.state.put",
                OfflineClass::OfflineSafe,
            ),
            (
                Action::CreateSnapshot,
                "fauna.filesync.snapshot.create_folder",
                OfflineClass::OfflineQueued,
            ),
            (
                Action::DeleteSnapshot { index: 0 },
                "fauna.filesync.snapshot.delete",
                OfflineClass::OfflineQueued,
            ),
            (
                Action::CheckIntegrity,
                "fauna.filesync.snapshot.check",
                OfflineClass::Read,
            ),
            (
                Action::OpenSnapshot { index: 0 },
                "fauna.filesync.snapshot.get",
                OfflineClass::Read,
            ),
        ] {
            assert_eq!(
                action.wire_kind(),
                Some(expected),
                "{action:?} must declare {expected}"
            );
            assert_eq!(
                offline_class(expected),
                Some(class),
                "{expected} changed class — the gate's behaviour on this page \
                 changed with it, so re-read the declaration's reasoning"
            );
        }
    }

    /// The pairs a reader is most likely to get backwards, asserted as
    /// **behaviour** through the shared rule rather than as kind strings: the
    /// soft delete and the manual snapshot survive a dead link, the hard delete
    /// and the restore do not; **Keep** survives, **Remove** does not.
    #[test]
    fn the_backups_gate_greys_only_what_needs_a_nest() {
        use fauna_protocol::offline_class::{Affordance, affordance};
        let live = |action: &Action| {
            action
                .wire_kind()
                .map(|k| affordance(k, "disconnected"))
                .unwrap_or(Affordance::Available)
                == Affordance::Available
        };
        for action in [
            Action::CreateSnapshot,
            Action::DeleteSnapshot { index: 0 },
            Action::KeepDestination { index: 0 },
            Action::SubmitForm(SubmitTarget::Edit),
            Action::CheckIntegrity,
            Action::OpenSnapshot { index: 0 },
        ] {
            assert!(
                live(&action),
                "{action:?} works without a nest and must NOT be desensitized"
            );
        }
        for action in [
            Action::ConfirmImmediateDelete,
            Action::SubmitRestore,
            Action::PruneExecute,
            Action::PrunePreview,
            Action::ConfirmRemove,
            Action::SubmitForm(SubmitTarget::AddNest),
            Action::SubmitForm(SubmitTarget::AddCustodian),
        ] {
            assert!(
                !live(&action),
                "{action:?} cannot finish without a nest and must be \
                 desensitized offline"
            );
        }
    }

    /// The dialog's confirm carries the ceremony it was painted for — the
    /// property the gate's exact-kind answer rests on. Driven through the real
    /// paint, because a hand-built action would prove nothing about what a
    /// press actually dispatches.
    #[test]
    fn the_confirm_button_carries_the_ceremony_it_was_painted_for() {
        let mut app = backups_app();
        app.backups.destinations = vec![destination("d1", "https://box.example", Some("Box"))];

        apply_local(&mut app, Action::OpenAddForm);
        assert_eq!(
            confirm_target(&app),
            Some(SubmitTarget::AddNest),
            "the add dialog defaults to the nest kind"
        );

        apply_local(
            &mut app,
            Action::SelectDestinationKind(DestinationKindChoice::ClientDevice.label()),
        );
        assert_eq!(
            confirm_target(&app),
            Some(SubmitTarget::AddCustodian),
            "picking `This device` re-points the confirm at the custodian enroll"
        );

        apply_local(&mut app, Action::OpenEditForm { index: 0 });
        assert_eq!(
            confirm_target(&app),
            Some(SubmitTarget::Edit),
            "reopening on a row edits it — even though the prefilled kind select \
             still paints that row's own kind"
        );
    }

    /// I6 at the **paint** level for a newly swept page — the half the walks
    /// cannot supply.
    ///
    /// The exhaustive and random walks assert I6 after every step, but they
    /// reach a page's gated controls only if the offline fixture *paints* them,
    /// and this dialog opens only from a loaded destination row. So the walk
    /// red-verify for this leg necessarily names an older page. This drives the
    /// real `page_elements()` — gate included — into the state where an
    /// `OnlineOnly` affordance exists, and pins both halves of the gate's
    /// contract on it: desensitized, and saying why per affordance.
    ///
    /// It red-verifies by construction: unplug `App::apply_offline_gate` and
    /// this fails naming `backup-destination-add-confirm-button`.
    #[test]
    fn the_add_destination_confirm_is_desensitized_offline_and_says_why() {
        let mut app = backups_app();
        apply_local(&mut app, Action::OpenAddForm);
        // A blank URL paints the confirm disabled on the page's own account,
        // and the gate deliberately leaves an already-disabled element alone —
        // so type one, or this would pass for the wrong reason.
        set_field(
            &mut app.backups,
            BackupsField::Url,
            "https://box.example".to_string(),
        );

        let confirm = app
            .page_elements()
            .into_iter()
            .find(|el| el.id == "backup-destination-add-confirm-button")
            .expect("the open add dialog paints its confirm");
        assert!(
            !confirm.enabled,
            "enrolling a destination needs a nest — `fauna.backup.nest_key.grant` \
             is the first call of the ceremony"
        );
        assert!(
            confirm.label.is_some(),
            "the gate states its reason per affordance, never as a global banner"
        );

        // The counterweight, on the same paint: Cancel is local and must stay
        // live, so this cannot pass by desensitizing the whole dialog.
        let cancel = app
            .page_elements()
            .into_iter()
            .find(|el| el.id == "backup-destination-add-cancel-button")
            .expect("the open add dialog paints its cancel");
        assert!(cancel.enabled, "closing a dialog needs no nest");
    }

    /// The `SubmitTarget` on the painted `backup-destination-add-confirm-button`.
    fn confirm_target(app: &App) -> Option<SubmitTarget> {
        app.page_elements()
            .into_iter()
            .find_map(|el| match (el.id.as_str(), el.gesture()) {
                (
                    "backup-destination-add-confirm-button",
                    Some(Gesture::Backups(Action::SubmitForm(target))),
                ) => Some(target),
                _ => None,
            })
    }
}
