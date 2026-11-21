import Foundation
import Testing
@testable import FaunaKit

// The Backups page's "offer only what is possible" wiring on apple
// (`docs/goal/ui/README.md` § Offer only what is possible, rules 4, 6 and 7;
// `docs/goal/ui/backups.md` § Errors & edge cases → *What the page offers*).
// The predicates themselves are shared Rust (`fauna_backups_machine::possibility`,
// tested there); these pin that FaunaKit asks the right one with the right
// inputs, paints from it, and refuses a press on `error-message` with the
// same reason — never a silent no-op.

private func row(_ id: Int64, _ state: SnapshotState) -> SnapshotRow {
    SnapshotRow(id: id, createdAt: 1_754_000_000, fileCount: 1, totalBytes: 1,
                deviceId: nil, tags: [], state: state, integrity: .unknown)
}

private func snapshot(
    folders: [String] = ["documents"], selected: String? = "documents",
    rows: [SnapshotRow] = [], busy: BackupOp? = nil
) -> BackupsSnapshot {
    BackupsSnapshot(
        folders: folders.map {
            BackupFolderRow(name: $0, snapshotCount: 0, lastSnapshotAt: nil, messageKind: nil)
        },
        selectedFolder: selected, snapshots: rows, lastBackedUp: nil,
        inProgressOp: busy, checkResult: nil, prunePreview: nil, detail: nil, error: nil)
}

private func clientDeviceRow(_ id: String) -> FfiBackupDestinationView {
    FfiBackupDestinationView(
        destinationId: id, destinationNestUrl: "", displayName: "This Mac",
        kind: destinationKindClientDevice(), custodianDeviceId: "aabb", capacityCapBytes: nil)
}

private func nestRow(_ id: String) -> FfiBackupDestinationView {
    FfiBackupDestinationView(
        destinationId: id, destinationNestUrl: "https://aunt.example.com/", displayName: nil,
        kind: "nest", custodianDeviceId: nil, capacityCapBytes: nil)
}

// MARK: - Destination dialog

@Test @MainActor func thisDeviceEditIsARenameThatNeedsNoUrl() {
    let vm = BackupDestinationsVM()
    vm.openEditForm(clientDeviceRow("d1"))
    vm.urlText = ""
    #expect(vm.destinationForm == .editClientDevice)
    #expect(vm.formPossibility == .possible)
}

@Test @MainActor func nestFormWithBlankUrlIsBlockedWithItsReason() {
    let vm = BackupDestinationsVM()
    vm.openAddForm()
    vm.kindInput = "nest"
    vm.urlText = "   "
    #expect(vm.destinationForm == .addNest)
    #expect(vm.formPossibility.reasonText == L.backups.backupDestinationUrlRequired)

    vm.openEditForm(nestRow("n1"))
    vm.urlText = ""
    #expect(vm.destinationForm == .editNest)
    #expect(vm.formPossibility.reasonText == L.backups.backupDestinationUrlRequired)
}

@Test @MainActor func blankUrlSubmitRefusesOnErrorMessageNotSilently() async {
    // The old shape returned silently on a blank URL; the press now asks the
    // same answer the confirm painted from and says why.
    let vm = BackupDestinationsVM()
    vm.openAddForm()
    vm.kindInput = "nest"
    vm.urlText = ""
    await vm.submitForm()
    #expect(vm.errorMessage == L.backups.backupDestinationUrlRequired)
    #expect(vm.formVisible)
}

@Test @MainActor func addDeviceUnreadableCapacityIsBlocked() {
    let vm = BackupDestinationsVM()
    vm.openAddForm()
    vm.kindInput = destinationKindClientDevice()
    vm.capacityText = ""
    #expect(vm.formPossibility == .possible)
    vm.capacityText = "lots"
    #expect(vm.formPossibility.reasonText == L.backups.backupDestinationCapacityInvalid)
}

// MARK: - Snapshot rows and set actions

@Test @MainActor func deleteOnAPendingRowIsRefusedWithItsReason() async {
    let vm = BackupsMachineVM()
    vm.probeSnapshot = snapshot(rows: [
        row(1, .active),
        row(2, .deletionPending(executeAfter: 1_754_172_800)),
    ])
    #expect(vm.deletePossibility(id: 1) == .possible)
    #expect(vm.deletePossibility(id: 2).reasonText == L.backups.snapshotDeleteAlreadyPending)
    // A row the machine does not list is not offered at all.
    #expect(vm.deletePossibility(id: 99) == .absent)
    // The hard delete stays a real choice on the pending row.
    #expect(vm.immediateDeletePossibility(id: 2) == .possible)

    await vm.deleteSnapshot(id: 2)
    #expect(vm.errorMessage == L.backups.snapshotDeleteAlreadyPending)
}

@Test @MainActor func setActionsNeedASelectedFolder() async {
    let vm = BackupsMachineVM()
    #expect(!vm.setActionPossibility.isPossible)  // nothing read yet

    vm.probeSnapshot = snapshot(selected: nil)
    #expect(vm.setActionPossibility.reasonText == L.backups.snapshotSelectFolderFirst)
    await vm.createSnapshot()
    #expect(vm.errorMessage == L.backups.snapshotSelectFolderFirst)

    vm.probeSnapshot = snapshot()
    #expect(vm.setActionPossibility == .possible)
}

@Test @MainActor func aBusyMachineHoldsEveryMutation() {
    let vm = BackupsMachineVM()
    vm.probeSnapshot = snapshot(rows: [row(1, .active)], busy: .create)
    #expect(!vm.setActionPossibility.isPossible)
    #expect(!vm.deletePossibility(id: 1).isPossible)
    #expect(!vm.immediateDeletePossibility(id: 1).isPossible)
    #expect(!vm.idlePossibility.isPossible)
}

// MARK: - Restore section

@Test @MainActor func restoreConfirmWaitsForASnapshotAndTheRetypedId() async {
    let vm = RestoreVM()
    #expect(!vm.inputsPossibility.isPossible)  // no snapshot to restore
    #expect(!vm.confirmPossibility.isPossible)
    await vm.restore()
    #expect(vm.errorMessage != nil)  // refused, not silent

    vm.selectedSnapshotId = 7
    #expect(vm.inputsPossibility == .possible)
    vm.confirmText = "8"
    #expect(!vm.confirmPossibility.isPossible)
    vm.confirmText = "7"
    #expect(vm.confirmPossibility == .possible)
}
