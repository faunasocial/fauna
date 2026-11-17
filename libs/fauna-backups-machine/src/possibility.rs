//! The Backups page's possibility answers (`docs/goal/ui/README.md` § Offer
//! only what is possible, rules 2, 4, 6 and 7; the page's rulings:
//! `docs/goal/ui/backups.md` § Errors & edge cases → *What the page offers*).
//!
//! One predicate per gated affordance, each answering
//! [`fauna_core::possibility::Possibility`]: every app paints the control from
//! it (enabled, or disabled with the reason beside it) and the action asks the
//! same predicate before it runs, refusing with the same reason on
//! `error-message`. The bool getters the apps already read
//! ([`BackupsMachine::immediate_delete_enabled`](crate::BackupsMachine::immediate_delete_enabled)
//! and its siblings) stay; these sit beside them.
//!
//! They live here, in the page's shared crate, for both halves of the page:
//! the snapshot half is this machine's, and the destination half's form is a
//! draft over plain strings that needs nothing from the config crate that
//! performs the write. The offline half of the carve-out fetches (a file's
//! download, the whole-snapshot restore) is
//! `fauna_protocol::offline_class::carve_out_affordance`, folded in by the app
//! with the connection word it holds.

use fauna_core::localized::LocalizedText;
use fauna_core::possibility::Possibility;

use crate::snapshots::{BackupsSnapshot, SnapshotRow, SnapshotState, busy_text};

fn blocked(key: &str) -> Possibility {
    Possibility::blocked(LocalizedText::key(key))
}

// ── The destination dialog (the form case, rule 4) ──────────────────

/// Which ceremony the destination dialog is running: what decides which of
/// its fields the draft must hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestinationForm {
    /// Enrolling another nest: the URL is what the enroll resolves.
    AddNest,
    /// Enrolling this device as a custodian: no URL; an optional capacity.
    AddClientDevice,
    /// Editing a nest destination: rename and/or re-point, so the URL stays
    /// required.
    EditNest,
    /// Editing a this-device destination: a rename. The row has no URL and
    /// the shared edit re-resolves only a URL that changed, so the URL field
    /// is not read and a blank one never blocks the save
    /// (`backup-destinations.md` § Create / edit / remove protocol → *Edit*).
    EditClientDevice,
}

/// A typed capacity that is not a readable size (`"lots"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MalformedCapacity;

/// The capacity the user typed, read the one way both the paint and the submit
/// read it: blank is `Ok(None)` (uncapped, a real choice), a readable size is
/// `Ok(Some(bytes))`, anything else is `Err(MalformedCapacity)` — never a substituted default,
/// because guessing a cap is how a device's disk fills.
pub fn parse_destination_capacity(typed: &str) -> Result<Option<u64>, MalformedCapacity> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Ok(None);
    }
    fauna_core::format::parse_byte_size(typed)
        .map(Some)
        .ok_or(MalformedCapacity)
}

/// The destination dialog's confirm: whether what has been typed can be
/// saved. A nest form needs its URL; the add-device form needs a readable
/// capacity, if one was typed; the this-device rename needs nothing.
pub fn destination_form_possibility(
    form: DestinationForm,
    url: &str,
    capacity: &str,
) -> Possibility {
    match form {
        DestinationForm::AddNest | DestinationForm::EditNest => {
            if url.trim().is_empty() {
                blocked("backups.backup_destination_url_required")
            } else {
                Possibility::Possible
            }
        }
        DestinationForm::AddClientDevice => match parse_destination_capacity(capacity) {
            Ok(_) => Possibility::Possible,
            Err(MalformedCapacity) => blocked("backups.backup_destination_capacity_invalid"),
        },
        DestinationForm::EditClientDevice => Possibility::Possible,
    }
}

// ── The snapshot half ───────────────────────────────────────────────

impl BackupsSnapshot {
    /// The single-flight gate as an answer: blocked, naming the running op,
    /// while one is in flight (§ Errors & edge cases, *Snapshot creation
    /// while one is in progress*). Every mutating control folds it in.
    pub fn idle_possibility(&self) -> Possibility {
        match self.in_progress_op {
            Some(op) => Possibility::blocked(busy_text(op)),
            None => Possibility::Possible,
        }
    }

    /// `snapshot-create-button`, `snapshot-prune-button` and
    /// `snapshot-check-button`: set-scoped, so they need a selected folder —
    /// and, with no folder at all, the reason names where one is made.
    pub fn set_action_possibility(&self) -> Possibility {
        let selection = if self.selected_folder.is_some() {
            Possibility::Possible
        } else if self.folders.is_empty() {
            blocked("backups.no_folders")
        } else {
            blocked("backups.snapshot_select_folder_first")
        };
        selection.and(self.idle_possibility())
    }

    /// `snapshot-delete` on the row at `index`: the 48-hour soft delete, which
    /// only an active row can enter. A row already pending deletion, or
    /// already soft-deleted (its *Recover* sits beside it), would have the
    /// nest schedule a second pending action for it, so it is not offered
    /// there. An index past the list answers absent.
    pub fn delete_possibility(&self, index: usize) -> Possibility {
        let Some(row) = self.snapshots.get(index) else {
            return Possibility::Absent;
        };
        snapshot_delete_possibility(row).and(self.idle_possibility())
    }

    /// `snapshot-immediate-delete-button`: the owner's hard delete, which is a
    /// real choice in every row state (it ends a pending or soft-deleted row
    /// now), so only the single-flight gate holds it.
    pub fn immediate_delete_possibility(&self, index: usize) -> Possibility {
        if index >= self.snapshots.len() {
            return Possibility::Absent;
        }
        self.idle_possibility()
    }
}

/// [`BackupsSnapshot::delete_possibility`]'s row half, for an app holding the
/// row alone.
pub fn snapshot_delete_possibility(row: &SnapshotRow) -> Possibility {
    match row.state {
        SnapshotState::Active => Possibility::Possible,
        SnapshotState::DeletionPending { .. } => blocked("backups.snapshot_delete_already_pending"),
        SnapshotState::SoftDeleted { .. } => blocked("backups.snapshot_delete_already_deleted"),
    }
}

// ── Restore ─────────────────────────────────────────────────────────

/// The selected local snapshot as the restore section holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreSelection<'a> {
    /// The snapshot's id, as the friction bar must re-type it.
    pub snapshot_id: &'a str,
    /// Whether that snapshot's kind has its `restore-kind-checkbox` ticked (a
    /// kind with no box is not the checkboxes' to refuse: `true`).
    pub kind_ticked: bool,
}

/// `restore-kind-checkbox` and `restore-confirm-input`: there is nothing to
/// choose or re-type until a snapshot exists to restore.
pub fn restore_inputs_possibility(selection: Option<&RestoreSelection<'_>>) -> Possibility {
    match selection {
        Some(_) => Possibility::Possible,
        None => blocked("backups.restore_no_snapshots"),
    }
}

/// `restore-confirm-button` (`backups.md` § Restore from backup destination):
/// a snapshot to restore, its kind ticked, its id re-typed exactly, and no
/// restore already running.
pub fn restore_possibility(
    running: bool,
    selection: Option<&RestoreSelection<'_>>,
    typed: &str,
) -> Possibility {
    let Some(selection) = selection else {
        return blocked("backups.restore_no_snapshots");
    };
    if running {
        return blocked("backups.restore_progress_running");
    }
    if !selection.kind_ticked {
        return blocked("backups.restore_kind_unticked");
    }
    if typed != selection.snapshot_id {
        return blocked("backups.restore_progress_idle");
    }
    Possibility::Possible
}

/// `snapshot-restore-button` (`backups.md` § Whole-snapshot restore): a
/// destination typed and no whole-snapshot restore already running. Whether
/// the typed path is absolute is the restore's own refusal, with the path in
/// its message.
pub fn snapshot_restore_possibility(running: bool, destination: &str) -> Possibility {
    if running {
        blocked("backups.snapshot_restore_running")
    } else if destination.trim().is_empty() {
        blocked("backups.snapshot_restore_destination_required")
    } else {
        Possibility::Possible
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshots::{BackupFolderRow, BackupOp};

    fn key(answer: &Possibility) -> Option<&str> {
        answer.reason().map(|r| r.key.as_str())
    }

    #[test]
    fn a_nest_form_needs_its_url_and_a_this_device_rename_needs_nothing() {
        for form in [DestinationForm::AddNest, DestinationForm::EditNest] {
            let blank = destination_form_possibility(form, "  ", "");
            assert_eq!(key(&blank), Some("backups.backup_destination_url_required"));
            assert!(destination_form_possibility(form, "https://b.example", "").is_possible());
        }
        assert!(
            destination_form_possibility(DestinationForm::EditClientDevice, "", "junk")
                .is_possible()
        );
    }

    #[test]
    fn the_add_device_form_refuses_a_malformed_capacity_but_not_a_blank_one() {
        let form = DestinationForm::AddClientDevice;
        assert!(destination_form_possibility(form, "", "").is_possible());
        assert!(destination_form_possibility(form, "", "50 GB").is_possible());
        assert_eq!(
            key(&destination_form_possibility(form, "", "lots")),
            Some("backups.backup_destination_capacity_invalid")
        );
        assert_eq!(parse_destination_capacity(" "), Ok(None));
        assert_eq!(parse_destination_capacity("nope"), Err(MalformedCapacity));
    }

    fn page(rows: Vec<SnapshotState>) -> BackupsSnapshot {
        BackupsSnapshot {
            folders: vec![BackupFolderRow::default()],
            selected_folder: Some("f".into()),
            snapshots: rows
                .into_iter()
                .map(|state| SnapshotRow {
                    state,
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn set_actions_need_a_selection_and_name_where_folders_are_made() {
        let mut p = page(vec![]);
        assert!(p.set_action_possibility().is_possible());
        p.selected_folder = None;
        assert_eq!(
            key(&p.set_action_possibility()),
            Some("backups.snapshot_select_folder_first")
        );
        p.folders.clear();
        assert_eq!(key(&p.set_action_possibility()), Some("backups.no_folders"));
    }

    #[test]
    fn a_running_op_blocks_every_mutation_with_its_own_busy_line() {
        let mut p = page(vec![SnapshotState::Active]);
        p.in_progress_op = Some(BackupOp::Create);
        for answer in [
            p.set_action_possibility(),
            p.delete_possibility(0),
            p.immediate_delete_possibility(0),
        ] {
            assert_eq!(key(&answer), Some("backups.busy_create"));
        }
    }

    #[test]
    fn only_an_active_row_offers_the_soft_delete_while_the_hard_delete_stays() {
        let p = page(vec![
            SnapshotState::Active,
            SnapshotState::DeletionPending {
                execute_after: None,
            },
            SnapshotState::SoftDeleted { purge_after: None },
        ]);
        assert!(p.delete_possibility(0).is_possible());
        assert_eq!(
            key(&p.delete_possibility(1)),
            Some("backups.snapshot_delete_already_pending")
        );
        assert_eq!(
            key(&p.delete_possibility(2)),
            Some("backups.snapshot_delete_already_deleted")
        );
        for i in 0..3 {
            assert!(p.immediate_delete_possibility(i).is_possible(), "{i}");
        }
        assert_eq!(p.delete_possibility(3), Possibility::Absent);
        assert_eq!(p.immediate_delete_possibility(3), Possibility::Absent);
    }

    #[test]
    fn restore_needs_a_snapshot_its_kind_ticked_and_its_id_retyped() {
        let ticked = RestoreSelection {
            snapshot_id: "7",
            kind_ticked: true,
        };
        let unticked = RestoreSelection {
            kind_ticked: false,
            ..ticked.clone()
        };
        assert_eq!(
            key(&restore_possibility(false, None, "7")),
            Some("backups.restore_no_snapshots")
        );
        assert_eq!(
            key(&restore_inputs_possibility(None)),
            Some("backups.restore_no_snapshots")
        );
        assert!(restore_inputs_possibility(Some(&ticked)).is_possible());
        assert_eq!(
            key(&restore_possibility(true, Some(&ticked), "7")),
            Some("backups.restore_progress_running")
        );
        assert_eq!(
            key(&restore_possibility(false, Some(&unticked), "7")),
            Some("backups.restore_kind_unticked")
        );
        assert_eq!(
            key(&restore_possibility(false, Some(&ticked), "8")),
            Some("backups.restore_progress_idle")
        );
        assert!(restore_possibility(false, Some(&ticked), "7").is_possible());
    }

    #[test]
    fn the_whole_snapshot_restore_needs_a_destination_and_an_idle_restore() {
        assert_eq!(
            key(&snapshot_restore_possibility(false, " ")),
            Some("backups.snapshot_restore_destination_required")
        );
        assert_eq!(
            key(&snapshot_restore_possibility(true, "/tmp/x")),
            Some("backups.snapshot_restore_running")
        );
        assert!(snapshot_restore_possibility(false, "~/Restored").is_possible());
    }

    /// Every reason key this module names resolves in the shared string table,
    /// so no app paints a raw key.
    #[test]
    fn every_reason_key_is_a_real_string() {
        for k in [
            "backups.backup_destination_url_required",
            "backups.backup_destination_capacity_invalid",
            "backups.no_folders",
            "backups.snapshot_select_folder_first",
            "backups.snapshot_delete_already_pending",
            "backups.snapshot_delete_already_deleted",
            "backups.restore_no_snapshots",
            "backups.restore_progress_running",
            "backups.restore_kind_unticked",
            "backups.restore_progress_idle",
            "backups.snapshot_restore_running",
            "backups.snapshot_restore_destination_required",
        ] {
            assert!(fauna_i18n::strings::lookup(k).is_some(), "{k}");
        }
    }
}
