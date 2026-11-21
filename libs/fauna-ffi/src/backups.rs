//! Re-exports the page-level Backups state machine so its UniFFI exports surface
//! in the generated Swift / Kotlin / C# bindings, plus a free-fn constructor
//! that builds the machine over an [`FfiNestClient`]'s WS-RPC connection. The
//! machine itself lives in libs/fauna-backups-machine; this file is a thin glue
//! layer (mirrors src/devices.rs).

use std::sync::Arc;

pub use fauna_backups_machine::{
    BackupFolderRow, BackupOp, BackupsMachine, BackupsObserver, BackupsSnapshot, CheckOutcome,
    PolicyState, PruneCandidate, PrunePreview, RowIntegrity, SnapshotDetail, SnapshotFileRow,
    SnapshotRow, SnapshotState,
};
use fauna_core::localized::LocalizedText;

use crate::FfiNestClient;

/// `fauna_backups_machine::busy_text` → the `in_progress_op` line for a
/// [`BackupOp`], so every native shell renders the same text without
/// re-deriving which key an operation maps to (`docs/goal/ui/backups.md` §
/// Where logic lives).
#[uniffi::export]
pub fn busy_text(op: BackupOp) -> LocalizedText {
    fauna_backups_machine::busy_text(op)
}

/// `fauna_backups_machine::snapshot_state_text` → the `snapshot-item[i]`
/// lifecycle suffix for a [`SnapshotState`]. `formatted_deadline` is the
/// shell's own locale-aware rendering of [`SnapshotState::deadline`]
/// (`behavior/value-formatting.md` § Relative time) — this face owns only the
/// rule (which key, and the dated/undated fallback), never timestamp
/// formatting.
#[uniffi::export]
pub fn snapshot_state_text(
    state: SnapshotState,
    formatted_deadline: Option<String>,
) -> Option<LocalizedText> {
    fauna_backups_machine::snapshot_state_text(&state, formatted_deadline.as_deref())
}

/// `fauna_backups_machine::snapshot_integrity_text` → the `snapshot-item[i]`
/// integrity suffix for a [`RowIntegrity`]. `None` for
/// [`RowIntegrity::Unknown`] — absent until a check runs this session, never
/// the word "unknown" (`docs/goal/ui/backups.md` § Snapshot-list shape,
/// *Check*).
#[uniffi::export]
pub fn snapshot_integrity_text(integrity: RowIntegrity) -> Option<LocalizedText> {
    fauna_backups_machine::snapshot_integrity_text(integrity)
}

/// `fauna_backups_machine::snapshot_restore_result_text` → the
/// `snapshot-restore-result` line for a finished whole-snapshot restore:
/// "done" when `unverified` is empty, otherwise the partial line naming the
/// files not written (`docs/goal/ui/backups.md` § Whole-snapshot restore →
/// *Outcome*). `size` is the shell's own byte-size rendering.
#[uniffi::export]
pub fn snapshot_restore_result_text(
    files_restored: u64,
    size: String,
    path: String,
    unverified: Vec<String>,
) -> LocalizedText {
    fauna_backups_machine::snapshot_restore_result_text(files_restored, &size, &path, &unverified)
}

// ── Offer only what is possible: the page's Possibility answers ─────────
//
// `fauna_backups_machine::possibility` (`docs/goal/ui/README.md` § Offer only
// what is possible, rules 2, 4, 6 and 7; `docs/goal/ui/backups.md` § Errors &
// edge cases → *What the page offers*) carried across the boundary so every
// native shell paints each control from the SAME answer tui reads, and its
// handler asks it again before acting. Each face forwards; none restates a
// rule. Gated `value-format` like `offline_possibility`: a bare
// `fauna_core::possibility::Possibility` crosses the boundary.

/// UniFFI face of [`fauna_backups_machine::possibility::DestinationForm`] —
/// which ceremony the destination dialog is running. A mirror enum because the
/// machine crate's enum carries no UniFFI derive.
#[cfg(feature = "value-format")]
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FfiDestinationForm {
    /// Enrolling another nest: the URL is required.
    AddNest,
    /// Enrolling this device as a custodian: an optional, readable capacity.
    AddClientDevice,
    /// Editing a nest destination: the URL stays required.
    EditNest,
    /// Editing a this-device destination: a rename; a blank URL never blocks.
    EditClientDevice,
}

#[cfg(feature = "value-format")]
impl From<FfiDestinationForm> for fauna_backups_machine::possibility::DestinationForm {
    fn from(form: FfiDestinationForm) -> Self {
        use fauna_backups_machine::possibility::DestinationForm as D;
        match form {
            FfiDestinationForm::AddNest => D::AddNest,
            FfiDestinationForm::AddClientDevice => D::AddClientDevice,
            FfiDestinationForm::EditNest => D::EditNest,
            FfiDestinationForm::EditClientDevice => D::EditClientDevice,
        }
    }
}

/// `backup-destination-add-confirm-button`: whether the typed draft can be
/// saved ([`fauna_backups_machine::possibility::destination_form_possibility`]).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn destination_form_possibility(
    form: FfiDestinationForm,
    url: String,
    capacity: String,
) -> fauna_core::possibility::Possibility {
    fauna_backups_machine::possibility::destination_form_possibility(form.into(), &url, &capacity)
}

/// [`fauna_backups_machine::possibility::parse_destination_capacity`]'s
/// readable half: the capacity in bytes, `None` for blank (uncapped). A
/// malformed capacity also answers `None` here — the shell never submits one,
/// because [`destination_form_possibility`] blocks that draft first.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn parse_destination_capacity(typed: String) -> Option<u64> {
    fauna_backups_machine::possibility::parse_destination_capacity(&typed)
        .ok()
        .flatten()
}

/// The restore section's selection, flattened: `snapshot_id` is the selected
/// local snapshot (`None` = there is none), `kind_ticked` whether its kind's
/// `restore-kind-checkbox` is ticked.
#[cfg(feature = "value-format")]
fn restore_selection(
    snapshot_id: &Option<String>,
    kind_ticked: bool,
) -> Option<fauna_backups_machine::possibility::RestoreSelection<'_>> {
    snapshot_id.as_deref().map(
        |snapshot_id| fauna_backups_machine::possibility::RestoreSelection {
            snapshot_id,
            kind_ticked,
        },
    )
}

/// `restore-kind-checkbox` and `restore-confirm-input`
/// ([`fauna_backups_machine::possibility::restore_inputs_possibility`]).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn restore_inputs_possibility(
    snapshot_id: Option<String>,
    kind_ticked: bool,
) -> fauna_core::possibility::Possibility {
    fauna_backups_machine::possibility::restore_inputs_possibility(
        restore_selection(&snapshot_id, kind_ticked).as_ref(),
    )
}

/// `restore-confirm-button`
/// ([`fauna_backups_machine::possibility::restore_possibility`]).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn restore_possibility(
    running: bool,
    snapshot_id: Option<String>,
    kind_ticked: bool,
    typed: String,
) -> fauna_core::possibility::Possibility {
    fauna_backups_machine::possibility::restore_possibility(
        running,
        restore_selection(&snapshot_id, kind_ticked).as_ref(),
        &typed,
    )
}

/// `snapshot-restore-button`'s page half
/// ([`fauna_backups_machine::possibility::snapshot_restore_possibility`]); the
/// shell folds in `carve_out_possibility` for the offline half.
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn snapshot_restore_possibility(
    running: bool,
    destination: String,
) -> fauna_core::possibility::Possibility {
    fauna_backups_machine::possibility::snapshot_restore_possibility(running, &destination)
}

/// `snapshot-delete` for a row the shell holds alone
/// ([`fauna_backups_machine::possibility::snapshot_delete_possibility`]).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn snapshot_delete_possibility(row: SnapshotRow) -> fauna_core::possibility::Possibility {
    fauna_backups_machine::possibility::snapshot_delete_possibility(&row)
}

/// `snapshot-create-button` / `snapshot-prune-button` /
/// `snapshot-check-button` over the machine's snapshot
/// ([`BackupsSnapshot::set_action_possibility`]).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn backups_set_action_possibility(
    snapshot: BackupsSnapshot,
) -> fauna_core::possibility::Possibility {
    snapshot.set_action_possibility()
}

/// `snapshot-delete` on the row at `index`, single-flight gate folded in
/// ([`BackupsSnapshot::delete_possibility`]).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn backups_delete_possibility(
    snapshot: BackupsSnapshot,
    index: u32,
) -> fauna_core::possibility::Possibility {
    snapshot.delete_possibility(index as usize)
}

/// `snapshot-immediate-delete-button` on the row at `index`
/// ([`BackupsSnapshot::immediate_delete_possibility`]).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn backups_immediate_delete_possibility(
    snapshot: BackupsSnapshot,
    index: u32,
) -> fauna_core::possibility::Possibility {
    snapshot.immediate_delete_possibility(index as usize)
}

/// The single-flight gate as an answer ([`BackupsSnapshot::idle_possibility`]).
#[cfg(feature = "value-format")]
#[uniffi::export]
pub fn backups_idle_possibility(snapshot: BackupsSnapshot) -> fauna_core::possibility::Possibility {
    snapshot.idle_possibility()
}

/// Build a [`BackupsMachine`] for the Backups page's **snapshot half** over
/// `nest`'s authenticated WS-RPC connection. `observer` ticks on every snapshot
/// change. The machine owns the folder + snapshot reads (`refresh` /
/// `select_folder`) and the page write gestures (`create_snapshot` /
/// `delete_snapshot` / `undelete_snapshot` / `delete_snapshot_immediate` /
/// `prune_preview` / `prune_execute` / `check`), plus the `snapshot-detail-files` read
/// (`open_snapshot` / `close_snapshot_detail`).
///
/// The detail read is on the machine rather than on `FfiSnapshotsClient` for the
/// custody reason below: it is a sealed-plane read, and routing it through the
/// machine's already-custody-wired seam is what stops each native shell from
/// wiring custody a second time (`behavior/path-sealing.md` § THE
/// CONSUMER-WIRING RULE).
///
/// The page's *destination* half is unaffected and keeps its existing seams
/// (`fauna_client_config::{enroll,edit,deregister}_backup_destination`,
/// `read_backup_status`).
///
/// **Custody wiring — the read-direction shape, deliberately.** The machine is
/// given the owner `BackupKey` even when no resolver is available, which is the
/// opposite of `FfiSnapshotsClient::client()`'s seal-direction rule. That is
/// correct *here* and is not a copy of the trap: this machine never seals
/// a label. Its only writing call is `create_folder` with `tags: []`, and
/// `SnapshotsClient::seal_tags` returns before touching custody on an empty tag
/// list — so the owner-root arm can render **more** rows and can mis-seal
/// nothing. Refusing the owner key here would reintroduce bug: an empty
/// listing where the reader holds the key.
///
/// `None` keypair (a bearer-only connection) ⇒ keyless custody, which renders
/// sealed labels not at all — the honest degrade, never a wrong root.
#[uniffi::export]
pub fn build_backups_machine(
    nest: Arc<FfiNestClient>,
    observer: Arc<dyn BackupsObserver>,
) -> Arc<BackupsMachine> {
    let inner = nest.nest_arc();
    let custody = match inner.auth().keypair() {
        Some(keypair) => {
            let secret = *keypair.secret_bytes();
            fauna_core::label_custody::LabelCustody::new(
                crate::snapshots_client::folder_key_resolver(&inner, &secret),
                Some(fauna_core::crypto::BackupKey::derive(&secret)),
            )
        }
        None => fauna_core::label_custody::LabelCustody::default(),
    };
    fauna_backups_machine::build_backups_machine(inner, observer, custody)
}

/// Wire the shell's stable sync device id onto an already-built machine, so
/// manual snapshots carry row provenance (`ui/backups.md` § Snapshot-list shape,
/// *Create* ruling). Hex-encoded because the raw-bytes setter has no FFI ABI;
/// an unparseable or wrong-length value clears the id rather than half-setting
/// it — an unattributed snapshot is exactly what the wire's `Option` means, and
/// is strictly better than a wrong provenance.
#[uniffi::export]
pub fn backups_machine_set_device_id(machine: Arc<BackupsMachine>, device_id_hex: String) {
    let bytes = hex::decode(&device_id_hex).ok().filter(|b| b.len() == 32);
    machine.set_device_id(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exported faces ARE the shared machine's rules — a door-crossing app
    /// that re-derived which key an operation/state/integrity maps to would
    /// silently decouple from every other app (the failure mode
    /// `docs/goal/ui/backups.md` § Where logic lives names these faces to end).
    #[test]
    fn busy_text_mirrors_the_shared_machine() {
        assert_eq!(
            busy_text(BackupOp::Create),
            fauna_backups_machine::busy_text(BackupOp::Create)
        );
    }

    #[test]
    fn snapshot_state_text_mirrors_the_shared_machine() {
        let state = SnapshotState::DeletionPending {
            execute_after: Some(1_700_000_000),
        };
        assert_eq!(
            snapshot_state_text(state.clone(), Some("in 3 days".to_string())),
            fauna_backups_machine::snapshot_state_text(&state, Some("in 3 days"))
        );
        assert_eq!(snapshot_state_text(SnapshotState::Active, None), None);
    }

    #[test]
    fn snapshot_restore_result_text_mirrors_the_shared_machine() {
        let unverified = vec!["x/y".to_string()];
        assert_eq!(
            snapshot_restore_result_text(2, "1 KB".into(), "/r".into(), unverified.clone()),
            fauna_backups_machine::snapshot_restore_result_text(2, "1 KB", "/r", &unverified)
        );
    }

    #[cfg(feature = "value-format")]
    fn reason_key(answer: &fauna_core::possibility::Possibility) -> Option<String> {
        answer.reason().map(|r| r.key.clone())
    }

    /// The destination face forwards the shared answer: the this-device rename
    /// with a blank URL saves, a nest form with a blank URL is refused, a
    /// malformed device capacity is refused.
    #[cfg(feature = "value-format")]
    #[test]
    fn destination_form_faces_forward_the_shared_answer() {
        assert!(
            destination_form_possibility(
                FfiDestinationForm::EditClientDevice,
                String::new(),
                String::new()
            )
            .is_possible()
        );
        for form in [FfiDestinationForm::AddNest, FfiDestinationForm::EditNest] {
            assert_eq!(
                reason_key(&destination_form_possibility(
                    form,
                    " ".into(),
                    String::new()
                )),
                Some("backups.backup_destination_url_required".to_string())
            );
        }
        assert_eq!(
            reason_key(&destination_form_possibility(
                FfiDestinationForm::AddClientDevice,
                String::new(),
                "lots".into()
            )),
            Some("backups.backup_destination_capacity_invalid".to_string())
        );
        assert_eq!(parse_destination_capacity(String::new()), None);
        assert!(parse_destination_capacity("50 GB".into()).is_some());
    }

    #[cfg(feature = "value-format")]
    #[test]
    fn snapshot_faces_forward_the_shared_answer() {
        let row = |state| SnapshotRow {
            state,
            ..Default::default()
        };
        let snapshot = BackupsSnapshot {
            folders: vec![BackupFolderRow::default()],
            selected_folder: Some("f".into()),
            snapshots: vec![
                row(SnapshotState::Active),
                row(SnapshotState::DeletionPending {
                    execute_after: None,
                }),
            ],
            ..Default::default()
        };
        assert!(backups_set_action_possibility(snapshot.clone()).is_possible());
        assert!(backups_delete_possibility(snapshot.clone(), 0).is_possible());
        assert_eq!(
            reason_key(&backups_delete_possibility(snapshot.clone(), 1)),
            Some("backups.snapshot_delete_already_pending".to_string())
        );
        assert_eq!(
            reason_key(&snapshot_delete_possibility(snapshot.snapshots[1].clone())),
            Some("backups.snapshot_delete_already_pending".to_string())
        );
        assert!(backups_immediate_delete_possibility(snapshot.clone(), 1).is_possible());
        assert_eq!(
            backups_immediate_delete_possibility(snapshot.clone(), 9),
            fauna_core::possibility::Possibility::Absent
        );
        assert!(backups_idle_possibility(snapshot).is_possible());
    }

    #[cfg(feature = "value-format")]
    #[test]
    fn restore_faces_forward_the_shared_answer() {
        assert_eq!(
            reason_key(&restore_inputs_possibility(None, true)),
            Some("backups.restore_no_snapshots".to_string())
        );
        assert!(restore_inputs_possibility(Some("7".into()), false).is_possible());
        assert_eq!(
            reason_key(&restore_possibility(
                false,
                Some("7".into()),
                false,
                "7".into()
            )),
            Some("backups.restore_kind_unticked".to_string())
        );
        assert!(restore_possibility(false, Some("7".into()), true, "7".into()).is_possible());
        assert_eq!(
            reason_key(&snapshot_restore_possibility(false, " ".into())),
            Some("backups.snapshot_restore_destination_required".to_string())
        );
        assert!(snapshot_restore_possibility(false, "/r".into()).is_possible());
    }

    #[test]
    fn snapshot_integrity_text_mirrors_the_shared_machine() {
        assert_eq!(
            snapshot_integrity_text(RowIntegrity::Implicated),
            fauna_backups_machine::snapshot_integrity_text(RowIntegrity::Implicated)
        );
        assert_eq!(snapshot_integrity_text(RowIntegrity::Unknown), None);
    }
}
