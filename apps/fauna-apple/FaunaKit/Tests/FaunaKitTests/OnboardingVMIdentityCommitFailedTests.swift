import Foundation
import Testing
@testable import FaunaKit

#if DEBUG
// A confirm whose moment-1 keychain write is refused does not stay advanced
// (`apps/common.md` § Credential storage: a store failure "is a loud error";
// `long-term-store.md` § Save-failure handling). The shared machine advances on
// confirm before the app writes the secret, so `OnboardingVM.persistConfirmedSecret`
// reports a failed write through `machine.identityCommitFailed`, which puts the
// wizard back on the screen the person confirmed on with the error on
// `error-message`. Same shared machine as `identity_commit_failed.rs`; this pins
// the apple wiring, the e2e witness being
// `test_identity_store_unreachable.py::test_a_keychain_that_keeps_nothing_is_a_loud_error_and_the_wizard_stays_put`.
//
// Hermetic: `FAUNA_E2E_BRIDGE` forces `KeychainStore` into its in-memory mode, and
// `refuse_secret_writes_for_test` makes that store keep no identity secret.

@MainActor
private func withRefusedSecretWrites(_ body: (OnboardingVM) throws -> Void) rethrows {
    setenv("FAUNA_E2E_BRIDGE", "1", 1)
    let vm = OnboardingVM()
    vm.keychain.deleteAll()
    _ = RegistryTestBridge.call(name: "refuse_secret_writes_for_test", jsonArg: #"{"refuse":true}"#)
    defer {
        _ = RegistryTestBridge.call(name: "refuse_secret_writes_for_test", jsonArg: #"{"refuse":false}"#)
        vm.keychain.deleteAll()
        unsetenv("FAUNA_E2E_BRIDGE")
    }
    try body(vm)
}

@Test(.sharedKeychain) @MainActor func aRefusedSaveOfAGeneratedIdentityLeavesTheWizardOnTheConfirmScreen() throws {
    try withRefusedSecretWrites { vm in
        vm.machine.setRendersRecoveryKit(renders: true)
        vm.machine.beginCreateIdentity()

        try vm.confirmGeneratedIdentity()

        #expect(vm.step == .identityCreated, "a key stored nowhere must not advance the wizard")
        #expect(vm.errorMessage?.isEmpty == false, "the failure is loud, on error-message")
    }
}

@Test(.sharedKeychain) @MainActor func aRefusedSaveOfAnImportedIdentityLeavesTheWizardOnTheImportScreen() throws {
    try withRefusedSecretWrites { vm in
        vm.machine.beginImportIdentity()

        try vm.confirmImportedIdentity(secret: String(repeating: "a", count: 64))

        #expect(vm.step == .identityImport)
        #expect(vm.errorMessage?.isEmpty == false)
    }
}
#endif
