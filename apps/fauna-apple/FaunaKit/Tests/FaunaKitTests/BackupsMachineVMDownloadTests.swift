import Testing
@testable import FaunaKit

// `BackupsMachineVM.downloadSnapshotFile` backs
// `snapshot-file-download-button` on BOTH apple targets (iOS
// `SnapshotFileListView`, macOS `MacSnapshotFileListView`). Its two refusal
// arms used to be bare `return`s — one in each view — which is e2e convention
// 11's forbidden shape: an automation command that is neither honoured nor
// surfaced on `error-message`. Downstream that reads exactly like apple row 41
// (the click succeeds, the e2e download dir stays empty, nothing on screen),
// and it is why that regression was chased through the harness save path
// instead of the app. These pin that a refusal is always SAID.
//
// Both arms are assertable with no API configured because the device-id check
// deliberately precedes the `api` guard (same ordering as
// `NostrVM.addRelay`'s scheme validation).

@Test @MainActor func downloadSnapshotFileRefusesMissingDeviceIdLoudly() async {
    let vm = BackupsMachineVM()
    let data = await vm.downloadSnapshotFile(deviceId: nil, snapshotId: 1, path: "docs/a.bin")
    #expect(data == nil)
    #expect(vm.errorMessage == L.errors.snapshotDeviceUnknown)
}

@Test @MainActor func downloadSnapshotFileRefusesUnconfiguredApiLoudly() async {
    let vm = BackupsMachineVM()
    let data = await vm.downloadSnapshotFile(
        deviceId: "device-abc", snapshotId: 1, path: "docs/a.bin")
    #expect(data == nil)
    #expect(vm.errorMessage == L.errors.notConnectedToNest)
}

// The device-id arm must win when BOTH are wrong: it names the specific defect
// rather than blaming the connection, and the ordering is what keeps the arm
// above testable at all.
@Test @MainActor func downloadSnapshotFileNamesTheDeviceIdWhenBothAreMissing() async {
    let vm = BackupsMachineVM()
    _ = await vm.downloadSnapshotFile(deviceId: nil, snapshotId: 1, path: "docs/a.bin")
    #expect(vm.errorMessage == L.errors.snapshotDeviceUnknown)
    #expect(vm.errorMessage != L.errors.notConnectedToNest)
}
