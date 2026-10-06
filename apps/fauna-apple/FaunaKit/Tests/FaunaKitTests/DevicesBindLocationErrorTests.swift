import Foundation
import Testing
@testable import FaunaKit

// `DevicesMachineVM.bindLocationError` — the macOS-only local-folder-binding
// refusal's route onto the shared page `error-message`. `MacFolderBindingSection.bindLocation()` has no other way to reach
// `error-message`: the desktop local-folder binding is `LocationsModel`-driven,
// not a `DevicesMachine` gesture. Pins the setter + precedence only — the
// refusal DECISION itself (`folder.folderRef == nil`) is macOS View code, out
// of FaunaKit's reach; `on-demand-files.md` § Hosting multiple on-demand
// folders owns the "refused, never bound by name" rule this backs.

@Test @MainActor func setBindLocationErrorSurfacesOnTheSharedErrorMessage() {
    let vm = DevicesMachineVM()
    #expect(vm.errorMessage == nil)

    vm.setBindLocationError("Failed to sync this location: the folder's identity could not be resolved")

    #expect(vm.bindLocationError == "Failed to sync this location: the folder's identity could not be resolved")
    #expect(vm.errorMessage == "Failed to sync this location: the folder's identity could not be resolved")
}

@Test @MainActor func setBindLocationErrorNilClearsIt() {
    let vm = DevicesMachineVM()
    vm.setBindLocationError("a refusal")
    #expect(vm.errorMessage != nil)

    vm.setBindLocationError(nil)

    #expect(vm.bindLocationError == nil)
    #expect(vm.errorMessage == nil)
}

// Precedence: a connect failure (the VM couldn't even build its machine) is
// the page's most serious fault and must keep painting over a bind refusal —
// `errorMessage`'s doc comment orders `connectError` first.
@Test @MainActor func connectErrorOutranksBindLocationError() async {
    struct StubBuildError: Error {}
    final class FailingAPI: APIClient {
        init() { super.init(nodeUrl: URL(string: "https://nest.invalid/")!) }
        override func devicesMachine(observer: DevicesObserver) async throws -> DevicesMachine {
            throw StubBuildError()
        }
    }
    let vm = DevicesMachineVM()
    await vm.configure(api: FailingAPI())
    #expect(vm.errorMessage != nil, "sanity: the failed build reads as a connect error")

    vm.setBindLocationError("a bind refusal")

    #expect(vm.errorMessage != "a bind refusal", "the connect error must still win")
}
