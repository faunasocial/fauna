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
    fn snapshot_integrity_text_mirrors_the_shared_machine() {
        assert_eq!(
            snapshot_integrity_text(RowIntegrity::Implicated),
            fauna_backups_machine::snapshot_integrity_text(RowIntegrity::Implicated)
        );
        assert_eq!(snapshot_integrity_text(RowIntegrity::Unknown), None);
    }
}
