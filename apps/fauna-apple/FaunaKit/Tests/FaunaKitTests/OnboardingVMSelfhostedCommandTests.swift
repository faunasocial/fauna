import Foundation
import Testing
@testable import FaunaKit

#if DEBUG
// The self-hosted recovery line resolves through the pre-login FFI, so a dead (or
// unset) saved nest is not a reason to give up: the device's own store is read
// regardless (`box-recovery.md` § The plane-era recovery floor, (b) The reads —
// "never either-or"). This pins the apple wiring: with a nest that cannot answer
// the resolve returns (nil here, as the hermetic store holds no seed for the box)
// instead of bailing before the store is read or throwing. The positive witness —
// the real `FAUNA_DEPLOYMENT_SEED=…` line from the device's own custody — is
// `test_box_recovery_two_nest.py` T4's macos leg.

@MainActor
private func withSelectedBox(nestUrl: String, _ body: (OnboardingVM) async -> Void) async {
    setenv("FAUNA_E2E_BRIDGE", "1", 1)
    let vm = OnboardingVM()
    vm.keychain.deleteAll()
    defer {
        vm.keychain.deleteAll()
        unsetenv("FAUNA_E2E_BRIDGE")
    }
    vm.machine.setRendersRecoveryKit(renders: true)
    vm.machine.beginCreateIdentity()
    try? vm.confirmGeneratedIdentity()
    vm.machine.setNestUrl(url: nestUrl)
    vm.machine.selectRecoveryBox(nestActorId: String(repeating: "ab", count: 32))
    await body(vm)
}

@Test(.sharedKeychain) @MainActor func aDeadSavedNestStillResolvesThroughTheDevicesOwnStore() async {
    await withSelectedBox(nestUrl: "http://127.0.0.1:1") { vm in
        #expect(vm.machine.effectiveSecret() != nil, "the fixture committed an identity")
        let line = await vm.resolveSelfhostedCommand()
        #expect(line == nil, "no seed in the hermetic store, and the dead nest adds none")
    }
}

@Test(.sharedKeychain) @MainActor func anUnsetNestUrlStillReadsTheDevicesOwnStore() async {
    await withSelectedBox(nestUrl: "") { vm in
        let line = await vm.resolveSelfhostedCommand()
        #expect(line == nil)
    }
}
#endif
