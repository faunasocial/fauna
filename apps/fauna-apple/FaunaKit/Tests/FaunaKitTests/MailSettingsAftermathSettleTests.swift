import Foundation
import SwiftData
import Testing
@testable import FaunaKit

// The aftermath's `configStageSettled` hook re-reads an open Mail settings page
// (`succession-aftermath.md` § Implementation status today, the MSEK leg). The
// machine is a concrete UniFFI class, so a live hydrate is proven by the
// cross-app e2e (`test_identity_succession_aftermath.py::test_the_successors_mail_passwords_are_burned`);
// these guards cover the two Swift seams in front of it.

@Test @MainActor func settleWithoutAMountedMachineLeavesTheMailVMUntouched() async {
    let vm = MailSettingsVM()

    await vm.rehydrateOnSettle()

    #expect(vm.snapshot == nil)
    #expect(vm.errorMessage == nil)
    #expect(vm.isLoading == false)
}

@Test @MainActor func eachSettleBumpsTheMailGenerationThePageWatches() throws {
    let schema = Schema([PhotoBackupRecord.self])
    let config = ModelConfiguration(schema: schema, isStoredInMemoryOnly: true)
    let context = ModelContext(try ModelContainer(for: schema, configurations: [config]))
    let client = FaunaClient(
        nodeUrl: URL(string: "http://127.0.0.1:1")!,
        secretHex: String(repeating: "33", count: 32),
        deviceId: String(repeating: "00", count: 32), modelContext: context,
        keychain: KeychainStore(testBackend: FakeKeychainBackend()))
    let before = client.mailSettledGeneration

    client.noteMailSettled()
    client.noteMailSettled()

    #expect(client.mailSettledGeneration == before + 2)
}
