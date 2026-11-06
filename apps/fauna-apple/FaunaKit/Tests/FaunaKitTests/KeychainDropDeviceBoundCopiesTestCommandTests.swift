import Testing
import Foundation
@testable import FaunaKit

// Non-e2e pins for the shared `keychain_drop_device_bound_copies` TestAgent command
// (`KeychainDropDeviceBoundCopiesTestCommand`) over the fake keychain backend — what the
// restored-device witness (`test_icloud_backup_copy.py`) can be fooled by without a
// simulator: a drop that takes the circle copy too, a count read after the delete, a
// row spared by name. The real keychain's answer is the witness's.
#if DEBUG

@Suite("keychain_drop_device_bound_copies test-agent command")
struct KeychainDropDeviceBoundCopiesTestCommandTests {
    private let account = "fauna/ab/secret"

    private func report(_ outcome: KeychainDropDeviceBoundCopiesTestCommand.Outcome) throws
        -> [String: Any]
    {
        guard case .report(let json) = outcome else {
            Issue.record("expected a report, got \(outcome)")
            return [:]
        }
        return try #require(
            JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
    }

    @Test func itLeavesOnlyTheSynchronizableCopiesAndCountsWhatItDropped() throws {
        let fake = FakeKeychainBackend()
        let store = KeychainStore(testBackend: fake)
        store.setICloudBackup(enabled: true)
        try store.save(rawKey: account, value: "00ff")
        try store.save(rawKey: "fauna/ab/nest_url", value: "https://nest.example")

        let body = try report(KeychainDropDeviceBoundCopiesTestCommand.apply(store: store))

        // Two device-bound credential rows and the device-bound preference row.
        #expect(body["dropped"] as? Int == 3)
        #expect(fake.rows(account: account, sync: false).isEmpty)
        #expect(fake.rows(account: account, sync: true).count == 1)
        #expect(fake.rows(account: "fauna/ab/nest_url", sync: true).count == 1)
    }

    /// The set a restore leaves holds no preference either: the preference row is
    /// device-bound, so the platform keeps it out of a restore like every other
    /// `ThisDeviceOnly` row, and sparing it would model a state no restore reaches.
    @Test func thePreferenceRowGoesLikeAnyOtherDeviceBoundRow() throws {
        let store = KeychainStore(testBackend: FakeKeychainBackend())
        store.setICloudBackup(enabled: true)
        #expect(store.iCloudBackupEnabled())

        _ = KeychainDropDeviceBoundCopiesTestCommand.apply(store: store)

        #expect(!store.iCloudBackupEnabled())
    }

    @Test func aKeychainWithNoDeviceBoundRowsDropsNothing() throws {
        let store = KeychainStore(testBackend: FakeKeychainBackend())
        let body = try report(KeychainDropDeviceBoundCopiesTestCommand.apply(store: store))
        #expect(body["dropped"] as? Int == 0)
    }

    @Test func theInMemoryStoreIsRefusedNotReadAsNothingToDrop() {
        let store = KeychainStore(testBackend: FakeKeychainBackend(), inMemory: true)
        guard case .refused = KeychainDropDeviceBoundCopiesTestCommand.apply(store: store) else {
            Issue.record("expected a refusal on the in-memory store")
            return
        }
    }
}

#endif
