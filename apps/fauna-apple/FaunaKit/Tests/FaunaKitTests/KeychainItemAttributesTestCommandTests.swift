import CryptoKit
import Testing
import Foundation
@testable import FaunaKit

// Non-e2e pins for the shared `keychain_item_attributes` TestAgent command
// (`KeychainItemAttributesTestCommand`) over the fake keychain backend — the answers
// the outcome-4 witness (`test_identity_key_stays_on_the_device.py`, the ios arm)
// can be fooled by without a simulator: a dropped account folded into "no copies",
// a report that leaks the item's data, an opted-in backup copy hidden behind the
// working copy. The real read-back of a real item is the witness's.
#if DEBUG

@Suite("keychain_item_attributes test-agent command")
struct KeychainItemAttributesTestCommandTests {
    private func report(_ outcome: KeychainItemAttributesTestCommand.Outcome) throws -> [String: Any] {
        guard case .report(let json) = outcome else {
            Issue.record("expected a report, got \(outcome)")
            return [:]
        }
        return try #require(
            JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
    }

    @Test func aRowWrittenByTheProductionPathReadsBackDeviceBound() throws {
        let store = KeychainStore(testBackend: FakeKeychainBackend())
        try store.save(rawKey: "fauna/ab/secret", value: "00ff-the-secret")

        let outcome = KeychainItemAttributesTestCommand.apply(
            ["account": "fauna/ab/secret"], store: store)

        guard case .report(let json) = outcome else {
            Issue.record("expected a report, got \(outcome)")
            return
        }
        #expect(!json.contains("00ff-the-secret"), "the report must carry attributes only")
        let body = try report(outcome)
        #expect(body["status"] as? Int == Int(errSecSuccess))
        let copies = try #require(body["copies"] as? [[String: Any]])
        #expect(copies.count == 1)
        #expect(copies.first?["accessible"] as? String
            == "kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly")
        #expect(copies.first?["synchronizable"] as? Bool == false)
    }

    @Test func theDigestIsOfTheStoredValueAndOnlyWhenAsked() throws {
        let store = KeychainStore(testBackend: FakeKeychainBackend())
        try store.save(rawKey: "fauna/ab/secret", value: "00ff-the-secret")

        let plain = try report(KeychainItemAttributesTestCommand.apply(
            ["account": "fauna/ab/secret"], store: store))
        #expect(plain["value_sha256"] == nil)

        let outcome = KeychainItemAttributesTestCommand.apply(
            ["account": "fauna/ab/secret", "digest": true], store: store)
        guard case .report(let json) = outcome else {
            Issue.record("expected a report, got \(outcome)")
            return
        }
        #expect(!json.contains("00ff-the-secret"), "the report must carry a digest only")
        #expect(try report(outcome)["value_sha256"] as? String
            == SHA256.hash(data: Data("00ff-the-secret".utf8))
                .map { String(format: "%02x", $0) }.joined())
    }

    @Test func anOptedInBackupCopyIsReportedBesideTheWorkingCopy() throws {
        let store = KeychainStore(testBackend: FakeKeychainBackend())
        store.setICloudBackup(enabled: true)
        try store.save(rawKey: "fauna/ab/secret", value: "00ff")

        let copies = try #require(
            try report(KeychainItemAttributesTestCommand.apply(
                ["account": "fauna/ab/secret"], store: store))["copies"] as? [[String: Any]])
        let seen = Set(copies.map {
            "\($0["accessible"] as? String ?? "") sync=\($0["synchronizable"] as? Bool ?? false)"
        })
        #expect(seen == [
            "kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly sync=false",
            "kSecAttrAccessibleAfterFirstUnlock sync=true",
        ])
    }

    @Test func anAbsentRowIsAReportOfNoCopiesWithItsStatus() throws {
        let store = KeychainStore(testBackend: FakeKeychainBackend())
        let body = try report(KeychainItemAttributesTestCommand.apply(
            ["account": "fauna/none/secret"], store: store))
        #expect(body["status"] as? Int == Int(errSecItemNotFound))
        #expect((body["copies"] as? [Any])?.isEmpty == true)
    }

    @Test func aMissingOrMistypedAccountIsRefusedNotReadAsNoCopies() {
        let store = KeychainStore(testBackend: FakeKeychainBackend())
        for command: [String: Any] in [[:], ["account": ""], ["account": 7]] {
            guard case .refused = KeychainItemAttributesTestCommand.apply(command, store: store) else {
                Issue.record("expected a refusal for \(command)")
                continue
            }
        }
    }

    #if os(macOS)
    /// macOS never runs a launch on the real keychain: an ad-hoc build reaches only the
    /// box's login keychain (convention 12's venue, never an e2e launch's).
    @Test func theRealKeychainLaunchIsNeverTakenOnMacOS() {
        #expect(KeychainStore.e2eRealKeychain == false)
    }
    #endif
}

#endif
