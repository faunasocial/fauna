import Foundation
import Testing
@testable import FaunaKit

// A folder row's background read paints its failure on `error-message` while the
// row is open, and its next good answer retires exactly that text
// (`ui/folders.md` § Errors & edge cases, "A folder row's own reads retire their
// own error"). The which-answer-retires-which-text rule is shared Rust
// (`FfiRowReadErrors`, unit-tested in `fauna-devices-machine`); these pin
// `DevicesMachineVM`'s use of it: the open-row gate, the text, and the row close.

private struct NestGone: Error {}

private final class FlakyRowAPI: APIClient {
    var failing = true
    init() { super.init(nodeUrl: URL(string: "https://nest.invalid/")!) }
    override func listFolderDevices(name: String) async throws -> [DeviceInfo] {
        if failing { throw NestGone() }
        return []
    }
}

@MainActor private func vmOverFlakyAPI() -> (DevicesMachineVM, FlakyRowAPI) {
    let api = FlakyRowAPI()
    let vm = DevicesMachineVM()
    vm.adoptAPIForTest(api)
    return (vm, api)
}

@Test @MainActor func anOpenRowsFailedReadPaintsItsWireKindOnErrorMessage() async {
    let (vm, _) = vmOverFlakyAPI()
    vm.setOpenFolderRow("Photos")

    await vm.loadFolderDeviceActivity(name: "Photos")

    #expect(vm.errorMessage?.hasPrefix("fauna.folders.devices: ") == true)
    #expect(vm.folderDeviceActivity["Photos"]?.isEmpty == true, "the read still degrades")
}

@Test @MainActor func aCollapsedRowsFailedReadStaysSilent() async {
    let (vm, _) = vmOverFlakyAPI()
    vm.setOpenFolderRow("Notes")

    await vm.loadFolderDeviceActivity(name: "Photos")

    #expect(vm.errorMessage == nil)
    #expect(vm.folderDeviceActivity["Photos"]?.isEmpty == true)
}

@Test @MainActor func theReadsNextGoodAnswerRetiresItsText() async {
    let (vm, api) = vmOverFlakyAPI()
    vm.setOpenFolderRow("Photos")
    await vm.loadFolderDeviceActivity(name: "Photos")
    #expect(vm.errorMessage != nil, "sanity: the failure painted")

    api.failing = false
    await vm.loadFolderDeviceActivity(name: "Photos")

    #expect(vm.errorMessage == nil)
}

@Test @MainActor func aClosingRowTakesItsReadsTextWithIt() async {
    let (vm, _) = vmOverFlakyAPI()
    vm.setOpenFolderRow("Photos")
    await vm.loadFolderDeviceActivity(name: "Photos")
    #expect(vm.errorMessage != nil, "sanity: the failure painted")

    vm.setOpenFolderRow(nil)

    #expect(vm.errorMessage == nil)
}

@Test @MainActor func aGoodAnswerLeavesAGesturesTextStanding() async {
    let (vm, api) = vmOverFlakyAPI()
    vm.setOpenFolderRow("Photos")
    await vm.loadFolderDeviceActivity(name: "Photos")
    vm.overwriteSharingErrorForTest("Couldn't share the set.")

    api.failing = false
    await vm.loadFolderDeviceActivity(name: "Photos")

    #expect(vm.errorMessage == "Couldn't share the set.")
}
