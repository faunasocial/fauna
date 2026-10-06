import Foundation
import Security
import Testing

@testable import FaunaKit

/// The retired keychain-service spellings are **never read** — the refusal pin that
/// replaced `KeychainServiceMigrationTests` when the compat-remnant sweep removed the
/// read-forward (`docs/goal/architecture/version-compatibility.md` § Dimension 2, the
/// fourth ratified exception: no Fauna installation, and so no row under a retired
/// spelling, exists anywhere).
///
/// The store addresses exactly one service — `AppleIdentifiers.KeychainService.account`
/// — for every read, write, delete and converge. A row seeded under a former spelling
/// (`social.fauna.desktop`/`.ios`/`.watch`, then `social.fauna.fauna`) is foreign to it:
/// not read, not copied forward, not parked, and left untouched by a delete. Each case
/// here is red against the pre-sweep store, which read those spellings forward.
@MainActor
struct KeychainRetiredServiceTests {
    private static let secret = String(repeating: "ab", count: 32)

    /// Every former `account` spelling, across the three platforms.
    private static let retiredSpellings = [
        "social.fauna.fauna", "social.fauna.desktop", "social.fauna.ios", "social.fauna.watch",
    ]

    private func seed(_ fake: FakeKeychainBackend, service: String, _ account: String, _ value: String) {
        fake.seedRow(
            service: service, account: account, synchronizable: false,
            accessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String,
            data: Data(value.utf8))
    }

    @Test func theStoreAddressesOnlyTheCurrentService() {
        let store = KeychainStore(testBackend: FakeKeychainBackend())
        #expect(store.service == AppleIdentifiers.KeychainService.account)
        #expect(!Self.retiredSpellings.contains(store.service))
    }

    /// A row under a retired spelling reads as absent — lazily and after the launch
    /// reconcile alike — and nothing is copied into the current service.
    @Test func aRowUnderARetiredSpellingIsNeverRead() {
        let fake = FakeKeychainBackend()
        let store = KeychainStore(testBackend: fake)
        for retired in Self.retiredSpellings {
            seed(fake, service: retired, "secret_key", Self.secret)
            seed(fake, service: retired, "fauna/index", "{\"accounts\":[]}")
        }

        #expect(store.load(rawKey: "secret_key") == nil)
        #expect(store.load(rawKey: "fauna/index") == nil)
        store.reconcileCredentialAccessibility()
        #expect(store.load(rawKey: "secret_key") == nil)
        #expect(store.load(rawKey: "fauna/index") == nil)

        for account in ["secret_key", "fauna/index"] {
            #expect(fake.rows(service: store.service, account: account, sync: false, dataProtection: false).isEmpty)
            #expect(fake.rows(service: store.service, account: account, sync: false, dataProtection: true).isEmpty)
        }
        #expect(fake.rows(account: "fauna-parked/fauna/index", sync: false).isEmpty)
    }

    /// The current service's own row is what a read returns, whatever a retired
    /// spelling holds beside it.
    @Test func theCurrentServiceIsTheOnlySource() {
        let fake = FakeKeychainBackend()
        let store = KeychainStore(testBackend: fake)
        seed(fake, service: "social.fauna.fauna", "secret_key", "RETIRED")
        try? store.save(rawKey: "secret_key", value: Self.secret)

        store.reconcileCredentialAccessibility()
        #expect(store.load(rawKey: "secret_key") == Self.secret)
    }

    /// A delete and a factory reset address the current service only: a row under a
    /// retired spelling is not this store's, so it is neither read nor swept.
    @Test func deletesTouchOnlyTheCurrentService() {
        let fake = FakeKeychainBackend()
        let store = KeychainStore(testBackend: fake)
        seed(fake, service: "social.fauna.desktop", "secret_key", "RETIRED")
        try? store.save(rawKey: "secret_key", value: Self.secret)

        store.delete(rawKey: "secret_key")
        #expect(store.load(rawKey: "secret_key") == nil)
        store.deleteAll()

        #expect(fake.rows(service: store.service, account: "secret_key", sync: false, dataProtection: fake.dataProtectionAvailable).isEmpty)
        #expect(fake.rows(service: "social.fauna.desktop", account: "secret_key", sync: false, dataProtection: fake.dataProtectionAvailable).count == 1)
    }
}
