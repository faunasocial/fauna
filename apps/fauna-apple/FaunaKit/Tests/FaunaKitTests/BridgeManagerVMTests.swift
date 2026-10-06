import Testing
@testable import FaunaKit

// `BridgeManagerVM` is the one shared bridge VM driving both the macOS
// `BridgesSettingsView` and the iOS `BridgesView` (the former iOS-only
// `BridgesVM` was retired when iOS lifted onto the unified generic form,
// 2026-06-28). Without a configured API both collections start empty.

@Test @MainActor func bridgeManagerVMStartsEmpty() {
    let vm = BridgeManagerVM()
    #expect(vm.bridges.isEmpty)
    #expect(vm.follows.isEmpty)
}

@Test @MainActor func bridgeManagerVMStartsNotLoading() {
    let vm = BridgeManagerVM()
    #expect(!vm.isLoading)
    #expect(vm.errorMessage == nil)
}
