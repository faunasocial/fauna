import Foundation
import Testing
@testable import FaunaKit

// A preference page whose load the first-listing gate refused says why on
// `error-message` and paints no empty state (`account-client-lifecycle.md`
// § The client-side lifecycle → *The first listing*, clauses (4) and (5)).

/// An API whose destination-list read is refused by the gate.
private final class RefusedListAPI: APIClient {
    init() { super.init(nodeUrl: URL(string: "https://nest.invalid/")!) }

    override func listBackupDestinations() async throws -> [FfiBackupDestinationView] {
        throw FfiError.General(msg: L.common.needsNest)
    }
}

/// An API whose default-conflict-policy read is refused by the gate.
private final class RefusedPolicyAPI: APIClient {
    init() { super.init(nodeUrl: URL(string: "https://nest.invalid/")!) }

    override func defaultConflictPolicy() async throws -> String? {
        throw FfiError.General(msg: L.common.needsOtherDevice)
    }
}

@Test @MainActor func refusedDestinationListShowsTheReasonAndNoEmptyState() async {
    let vm = BackupDestinationsVM()
    vm.configure(api: RefusedListAPI(), deviceId: "dev-1", custodianStore: nil)
    await vm.hydrate()
    #expect(vm.errorMessage == L.common.needsNest)
    #expect(vm.isEmpty)
    #expect(!vm.showsEmptyState)
}

@Test @MainActor func refusedDefaultConflictPolicyShowsTheReason() async {
    let vm = DevicesMachineVM()
    vm.adoptAPIForTest(RefusedPolicyAPI())
    await vm.loadDefaultConflictPolicy()
    #expect(vm.errorMessage == L.common.needsOtherDevice)
    #expect(vm.defaultConflictPolicy == nil)
}

/// Only the gate's two reasons count as a refusal; any other failure text stays
/// the quiet default (no banner).
@Test func firstListingRefusalRecognisesOnlyTheGatesReasons() {
    #expect(DisplayError.firstListingRefusal(FfiError.General(msg: L.common.needsNest)) == L.common.needsNest)
    #expect(DisplayError.firstListingRefusal(FfiError.General(msg: L.common.needsOtherDevice)) == L.common.needsOtherDevice)
    #expect(DisplayError.firstListingRefusal(FfiError.General(msg: "socket closed")) == nil)
}
