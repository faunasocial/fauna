//! The page-level renderable [`BackupsSnapshot`] + its sub-types. Clients read a
//! fresh copy on every observer tick and render the whole snapshot half of the
//! Backups page off it; they never see the internal state. Mirrors
//! `fauna_devices_machine::snapshots`.
//!
//! The sub-types transcribe the wire shapes the WS-RPC kinds return
//! (`fauna.folders.list`, `fauna.filesync.snapshot.{list,prune_set_policy,check}`)
//! into clean `uniffi::Record`s — dropping the wire types' `extra` flatten maps
//! so they cross the FFI boundary. The transcribes live on the machine (not in
//! `nest_api`), because two of them are *derivations* the design ratified rather
//! than field copies: `last_backed_up` and per-row `integrity`.
//!
//! Shape owner: `docs/goal/ui/backups.md` § Snapshot-list shape.

use serde::{Deserialize, Serialize};

use fauna_core::localized::LocalizedText;

/// One row of the folder selector (`backup-folder-selector`).
///
/// Transcribes the `fauna.folders.list` row
/// (`fauna_protocol::folders::FolderSummary`) down to the four fields the
/// selector renders. The counts come from the row's `cached_*` columns, which is
/// why the selector needs no per-set snapshot read.
///
/// ⚠ `last_snapshot_at` here is the **unselected** sets' story — the selected
/// set's `last-backed-up` element is [`BackupsSnapshot::last_backed_up`],
/// derived from the actual snapshot list. The ratified section splits these
/// deliberately: `cached_last_snapshot_at` is a denormalized column that can lag
/// a just-created snapshot, and the selected set has real rows to derive from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BackupFolderRow {
    /// The folder name. Every non-reserved folder belongs in this list —
    /// snapshot create refuses only custody copies (§ Snapshot-list shape,
    /// *Selector source*).
    pub name: String,
    pub snapshot_count: i64,
    pub last_snapshot_at: Option<i64>,
}

/// Where a snapshot row sits in the delete → soft-delete → purge lifecycle
/// (`backup-restore.md` § 2 *Row lifecycle fields*, § 7 *Deletion safety*).
///
/// A non-[`Active`](Self::Active) row renders its state on the row itself
/// (§ Snapshot-list shape, *Row content contract*).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum SnapshotState {
    /// Live. What a row reads as when a current nest's lifecycle flags are all
    /// false.
    #[default]
    Active,
    /// A cancellable deletion window is open — a `delete`'s 48 h pending action
    /// or an automatic prune's 7-day bulk action. Not yet deleted.
    ///
    /// `execute_after` is when it fires; `None` when the nest served
    /// `deletion_pending` without a joinable pending action (an action
    /// whose row has already been consumed).
    DeletionPending { execute_after: Option<i64> },
    /// Soft-deleted and inside its 30-day recovery window;
    /// `fauna.filesync.snapshot.undelete` still serves it.
    ///
    /// `purge_after` is when GC's phase-3 purge becomes eligible — the
    /// "recoverable until" the row renders.
    SoftDeleted { purge_after: Option<i64> },
}

impl SnapshotState {
    /// The deadline this state carries, if any — the one field an app has to
    /// format before handing it back to [`snapshot_state_text`]. `None` for
    /// [`Active`](Self::Active), and for a non-`Active` state the wire served
    /// without a joinable deadline.
    ///
    /// The two variants spell it differently on purpose (`execute_after` is when
    /// deletion *fires*; `purge_after` is when recovery *ends*), which is why
    /// this accessor exists rather than a shared field name: callers that only
    /// need "the date on this row" should not have to re-match the variants.
    pub fn deadline(&self) -> Option<i64> {
        match self {
            Self::Active => None,
            Self::DeletionPending { execute_after } => *execute_after,
            Self::SoftDeleted { purge_after } => *purge_after,
        }
    }
}

/// What this session's integrity check said about one row.
///
/// **Derived, never nest state** (§ Snapshot-list shape, *Check* ruling + the
/// *two draft fields deliberately dropped* note): there is no per-row integrity
/// column on the wire and there should not be — integrity is a per-*set* check
/// whose reply implicates individual rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum RowIntegrity {
    /// No check has run this session. The state every row starts in, and the
    /// state every row returns to when the selection changes.
    #[default]
    Unknown,
    /// A check ran this session and did not implicate this row.
    CheckedOk,
    /// A check ran this session and implicated this row by `snapshot_id`.
    Implicated,
}

/// One row of the snapshot list (`snapshot-item[i]`).
///
/// Transcribes `fauna_protocol::filesync::SnapshotSummaryRow`. Order is the
/// wire's (newest-first) and apps do not re-sort.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SnapshotRow {
    pub id: i64,
    pub created_at: i64,
    pub file_count: i64,
    pub total_bytes: i64,
    /// Hex-encoded capturing-device id (the wire's raw 32 bytes); `None` =
    /// unattributed. Hex rather than bytes so the record crosses UniFFI and
    /// JSON the same way `DeviceSummary::device_id` does.
    pub device_id: Option<String>,
    /// The snapshot's plaintext tags. Empty for an untagged row — which, since
    /// the *Create* ruling, is what every manual snapshot is.
    pub tags: Vec<String>,
    pub state: SnapshotState,
    pub integrity: RowIntegrity,
}

/// The mutating operation currently in flight. While this is `Some`, every
/// mutating control is disabled — the single-flight rule that retires the six
/// apps' ad-hoc partial busy flags (§ Snapshot-list shape, *Create* ruling).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum BackupOp {
    Create,
    Delete,
    ImmediateDelete,
    /// Recovering a soft-deleted row (`snapshot-undelete-button[i]`).
    Undelete,
    Prune,
    Check,
    Refresh,
    /// Opening one snapshot's file list (`snapshot-item[i]` → `snapshot-detail-files`).
    Detail,
}

/// The `in_progress_op` line's text for a [`BackupOp`]. Shared decision
/// (tui↔linux twin harvest; previously hand-rolled
/// identically on both apps). `Refresh` and `Detail` share one line — neither
/// mutates, so both read as an ordinary load in progress.
pub fn busy_text(op: BackupOp) -> LocalizedText {
    LocalizedText::key(match op {
        BackupOp::Create => "backups.busy_create",
        BackupOp::Delete => "backups.busy_delete",
        BackupOp::ImmediateDelete => "backups.busy_immediate_delete",
        BackupOp::Undelete => "backups.busy_undelete",
        BackupOp::Prune => "backups.busy_prune",
        BackupOp::Check => "backups.busy_check",
        BackupOp::Refresh | BackupOp::Detail => "backups.busy_refresh",
    })
}

/// The `snapshot-item[i]` **lifecycle suffix**: a non-`Active` state renders ON
/// the row, carrying the deadline the user can still act on — that deadline is
/// the whole reason the wire gained the lifecycle fields
/// (§ Snapshot-list shape, *Row content contract*). `None` for
/// [`SnapshotState::Active`], which renders no suffix at all.
///
/// `formatted_deadline` is the app's own rendering of [`SnapshotState::deadline`]
/// — timestamp formatting stays per-app (each shell owns a locale-aware
/// relative-time seam; `behavior/value-formatting.md` § Relative time), so this
/// function owns only the *rule*: which key, and the dated/undated fallback.
/// Passing `None` for a state that does carry a deadline is not a corruption —
/// it takes the same undated arm a wire-absent deadline takes, so the STATE
/// still renders. Inventing a date, or hiding the state because there is none,
/// is the failure this shape avoids.
///
/// Shared decision (the linux↔tui twin harvest that lifted [`busy_text`];
/// previously hand-rolled identically on all seven apps — the two comments above
/// travelled verbatim through five of them). Apple's FaunaKit twin is
/// `BackupsVerdictViews.swift::SnapshotRowText`.
pub fn snapshot_state_text(
    state: &SnapshotState,
    formatted_deadline: Option<&str>,
) -> Option<LocalizedText> {
    let (dated, undated) = match state {
        SnapshotState::Active => return None,
        SnapshotState::DeletionPending { .. } => (
            "backups.snapshot_state_deletion_pending",
            "backups.snapshot_state_deletion_pending_undated",
        ),
        SnapshotState::SoftDeleted { .. } => (
            "backups.snapshot_state_soft_deleted",
            "backups.snapshot_state_soft_deleted_undated",
        ),
    };
    Some(
        match formatted_deadline.filter(|_| state.deadline().is_some()) {
            Some(when) => LocalizedText::key_arg(dated, "when", when),
            None => LocalizedText::key(undated),
        },
    )
}

/// The `snapshot-item[i]` **integrity suffix** — derived by the machine from a
/// check reply's `structured_errors`, never nest state.
///
/// **Absent until a check runs this session**: [`RowIntegrity::Unknown`] paints
/// nothing rather than the word "unknown", which on this page would read as a
/// finding (tui's ruling, § Snapshot-list shape, *Check*). Shared for the same
/// reason [`snapshot_state_text`] is.
pub fn snapshot_integrity_text(integrity: RowIntegrity) -> Option<LocalizedText> {
    match integrity {
        RowIntegrity::Unknown => None,
        RowIntegrity::CheckedOk => Some(LocalizedText::key("backups.snapshot_integrity_ok")),
        RowIntegrity::Implicated => {
            Some(LocalizedText::key("backups.snapshot_integrity_implicated"))
        }
    }
}

/// One file inside an opened snapshot (`snapshot-detail-files`'s rows).
///
/// Transcribes `fauna_protocol::filesync::SnapshotFileEntry`. The `path` is what
/// the **custody-wired** seam produced: a sealed set's rows arrive opened
/// because the `SnapshotsClient` was constructed with the reader's
/// `LabelCustody` (`behavior/path-sealing.md` § THE CONSUMER-WIRING RULE) — a
/// keyless client renders this list empty rather than leaking envelopes, which
/// is exactly why the read belongs to the machine's seam and not to each app.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SnapshotFileRow {
    pub path: String,
    pub size_bytes: i64,
    /// The wire's `file_type` (`"regular"`, `"dir"`, `"symlink"`). The download
    /// affordance is a regular-file gesture; the rest render as rows only.
    pub file_type: String,
    /// Hex-encoded `manifest_hash` — the key the shared single-file download
    /// walk (`fauna_core::file_download`) resolves. Hex rather than bytes for
    /// the same boundary reason `SnapshotRow::device_id` is.
    pub manifest_hash: String,
}

/// The opened snapshot's file list. `None` on [`BackupsSnapshot`] means no row
/// is open — the page renders its "select a snapshot" prompt instead.
///
/// Carries `snapshot_id` rather than an index: the list underneath can be
/// re-read by any mutation, and an index would silently re-point the open detail
/// at a different snapshot. A detail whose id is no longer in `snapshots` is
/// dropped by the machine rather than rendered against a row that vanished.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct SnapshotDetail {
    pub snapshot_id: i64,
    pub files: Vec<SnapshotFileRow>,
}

/// The result of an integrity check — session-local, from the last check reply.
///
/// A completed check with errors is a **result, not an error**: it renders here,
/// never in `error-message` (§ Architectural rules, rule 6).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct CheckOutcome {
    /// The shared `SnapshotCheckReply::is_ok()` predicate, called — never
    /// re-derived from `status == "ok"` or from error counts (ratified
    /// 2026-06-01; § Snapshot-list shape, *Check* ruling).
    pub is_ok: bool,
    pub snapshots_checked: i64,
    pub files_checked: i64,
    pub manifests_checked: i64,
    pub chunks_checked: i64,
    pub missing_manifests: i64,
    pub missing_chunks: i64,
    pub corrupt_manifests: i64,
    /// Snapshot ids the reply's `structured_errors` implicated. Drives each
    /// row's [`RowIntegrity`].
    pub implicated: Vec<i64>,
}

/// What the nest could make of the folder's resting `retention_policy`
/// column. Transcribes the wire's three `policy_state` constants
/// (`backup-restore.md` § 8).
///
/// Load-bearing: `NotSet` and `Unparseable` both prune nothing, and the page
/// renders *why* the count is zero rather than an empty success.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum PolicyState {
    /// The set has a parseable policy and it was evaluated.
    Applied,
    /// No retention policy is configured for this set — the folder wizard is
    /// where one is set, never this page.
    #[default]
    NotSet,
    /// A policy rests in the column but the nest could not parse it. Surfaces
    /// loudly — the source is a `policy_state` token this build does not know.
    Unparseable,
}

impl PolicyState {
    /// Transcribe the wire's `policy_state` string. An unrecognized value from a
    /// newer nest degrades to [`Self::Unparseable`] — the arm that renders
    /// loudly and prunes nothing, which is the fail-safe direction: a client
    /// that cannot understand the verdict must not present it as a clean apply.
    pub fn from_wire(value: &str) -> Self {
        match value {
            "applied" => Self::Applied,
            "not_set" => Self::NotSet,
            _ => Self::Unparseable,
        }
    }
}

/// One snapshot a prune dry-run would remove.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PruneCandidate {
    pub id: i64,
    pub created_at: i64,
    pub tags: Vec<String>,
}

/// A prune dry-run awaiting execute-or-cancel.
///
/// Execute is offered **only from a preview** (§ Snapshot-list shape, *Prune*
/// ruling — apple's preview-first flow, blessed as the uniform shape).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PrunePreview {
    /// The count that *would* be pruned.
    pub would_prune: i64,
    /// The count that would remain — the **active** population only, the same
    /// population the policy binds over.
    pub remaining: i64,
    pub candidates: Vec<PruneCandidate>,
    pub policy_state: PolicyState,
}

/// The whole renderable snapshot half of the Backups page.
///
/// Architectural rule 1 binds from this crate's landing: an app renders
/// `backups_snapshot()` and dispatches gestures; it holds no page logic.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct BackupsSnapshot {
    /// Owner-scoped, reserved (`__`) names excluded, name-ordered.
    pub folders: Vec<BackupFolderRow>,
    /// Page-lifetime selection; defaults to the first row once loaded and
    /// resets on page mount. Never persisted at rest.
    pub selected_folder: Option<String>,
    /// The selected set's rows, wire order (newest-first).
    pub snapshots: Vec<SnapshotRow>,
    /// The selected set's newest snapshot `created_at`; `None` renders "never".
    pub last_backed_up: Option<i64>,
    /// `Some` while a mutating op is in flight — every mutating control is
    /// disabled.
    pub in_progress_op: Option<BackupOp>,
    /// Session-local, from the last check reply.
    pub check_result: Option<CheckOutcome>,
    /// A dry-run result awaiting execute/cancel.
    pub prune_preview: Option<PrunePreview>,
    /// The opened snapshot's file list, or `None` when no row is open. Scoped to
    /// the selected set: selecting another set closes it, and a re-read that no
    /// longer lists the open snapshot closes it too.
    pub detail: Option<SnapshotDetail>,
    /// The `error-message` element; `None` when clear. **Never carries a
    /// success** (§ Architectural rules, rule 6) — a completed prune or check
    /// reports through its own result surface above.
    pub error: Option<LocalizedText>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_text_maps_every_op_to_a_distinct_key_and_refresh_covers_detail_too() {
        let cases = [
            (BackupOp::Create, "backups.busy_create"),
            (BackupOp::Delete, "backups.busy_delete"),
            (BackupOp::ImmediateDelete, "backups.busy_immediate_delete"),
            (BackupOp::Undelete, "backups.busy_undelete"),
            (BackupOp::Prune, "backups.busy_prune"),
            (BackupOp::Check, "backups.busy_check"),
            (BackupOp::Refresh, "backups.busy_refresh"),
            (BackupOp::Detail, "backups.busy_refresh"),
        ];
        for (op, key) in cases {
            assert_eq!(busy_text(op).key, key, "{op:?}");
        }
    }

    #[test]
    fn state_text_is_none_for_active_and_carries_the_deadline_the_app_formatted() {
        assert_eq!(snapshot_state_text(&SnapshotState::Active, None), None);

        let pending = SnapshotState::DeletionPending {
            execute_after: Some(1_700_000_000),
        };
        let lt = snapshot_state_text(&pending, Some("in 2 days")).expect("pending renders");
        assert_eq!(lt.key, "backups.snapshot_state_deletion_pending");
        assert_eq!(lt.args.get("when").map(String::as_str), Some("in 2 days"));

        let soft = SnapshotState::SoftDeleted {
            purge_after: Some(1_700_000_000),
        };
        let lt = snapshot_state_text(&soft, Some("in 30 days")).expect("soft-deleted renders");
        assert_eq!(lt.key, "backups.snapshot_state_soft_deleted");
        assert_eq!(lt.args.get("when").map(String::as_str), Some("in 30 days"));
    }

    #[test]
    fn an_undated_state_still_renders_its_state_and_only_drops_the_date() {
        // The wire's deadline is `Option` (a pending action whose row was
        // already consumed). Inventing a date — or
        // hiding the state because there is none — is the failure this avoids.
        for (state, key) in [
            (
                SnapshotState::DeletionPending {
                    execute_after: None,
                },
                "backups.snapshot_state_deletion_pending_undated",
            ),
            (
                SnapshotState::SoftDeleted { purge_after: None },
                "backups.snapshot_state_soft_deleted_undated",
            ),
        ] {
            let lt = snapshot_state_text(&state, None).expect("a non-Active state always renders");
            assert_eq!(lt.key, key, "{state:?}");
            assert!(lt.args.is_empty(), "{state:?} carries no deadline arg");
        }
    }

    #[test]
    fn a_deadline_the_caller_did_not_format_degrades_to_the_undated_line() {
        // The same fallback a wire-absent deadline takes: the STATE still
        // renders. A caller that formatted nothing gets a bare state, never a
        // dropped row and never a raw epoch leaking into the sentence.
        let lt = snapshot_state_text(
            &SnapshotState::DeletionPending {
                execute_after: Some(1_700_000_000),
            },
            None,
        )
        .expect("still renders");
        assert_eq!(lt.key, "backups.snapshot_state_deletion_pending_undated");
    }

    #[test]
    fn state_deadline_is_the_one_field_the_app_has_to_format() {
        assert_eq!(SnapshotState::Active.deadline(), None);
        assert_eq!(
            SnapshotState::DeletionPending {
                execute_after: Some(7)
            }
            .deadline(),
            Some(7)
        );
        assert_eq!(
            SnapshotState::SoftDeleted {
                purge_after: Some(9)
            }
            .deadline(),
            Some(9)
        );
        assert_eq!(
            SnapshotState::SoftDeleted { purge_after: None }.deadline(),
            None
        );
    }

    #[test]
    fn unknown_integrity_paints_nothing_rather_than_the_word_unknown() {
        assert_eq!(snapshot_integrity_text(RowIntegrity::Unknown), None);
        assert_eq!(
            snapshot_integrity_text(RowIntegrity::CheckedOk)
                .expect("checked-ok renders")
                .key,
            "backups.snapshot_integrity_ok"
        );
        assert_eq!(
            snapshot_integrity_text(RowIntegrity::Implicated)
                .expect("implicated renders")
                .key,
            "backups.snapshot_integrity_implicated"
        );
    }
}
