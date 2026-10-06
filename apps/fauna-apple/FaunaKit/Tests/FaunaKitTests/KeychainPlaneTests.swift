import Foundation
import Security
import Testing

@testable import FaunaKit

/// The macOS keychain plane (`docs/goal/architecture/apps/ios.md` § Credential Storage,
/// *keychain plane*): the store writes — and reads — the data-protection keychain
/// whenever the running signature can reach it, and the legacy file-based keychain
/// otherwise. Exactly one plane per build, never both: the cross-plane read-fallback and
/// copy-forward were removed by the compat-remnant sweep
/// (`version-compatibility.md` § Dimension 2, the fourth ratified exception — no row
/// predating the data-protection plane exists), and the refusal cases below pin that.
///
/// The genuine last inch — whether a Developer-ID-signed build's probe actually comes
/// back `errSecItemNotFound` on a real machine — is one supervised launch reading one
/// log line (`[keychain] write plane: …`). Everything else is mechanism, and this file
/// pins all of it against the `FakeKeychainBackend` that models the plane split
/// (a `kSecUseDataProtectionKeychain` query sees only data-protection rows) and the
/// ad-hoc refusal (`errSecMissingEntitlement` on every data-protection query).
@MainActor
struct KeychainPlaneTests {
    private static let secret = String(repeating: "ab", count: 32)

    private func seedLegacy(_ fake: FakeKeychainBackend, _ store: KeychainStore, _ account: String, _ value: String) {
        fake.seedRow(
            service: store.service, account: account, synchronizable: false,
            accessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String,
            data: Data(value.utf8), dataProtection: false)
    }

    // MARK: - The ad-hoc build: the legacy plane is its write plane

    /// −34018 on the data-protection plane → every write and read is a legacy-plane
    /// operation. (Red-verified: dropping the probe's status check puts the rows in the
    /// refused plane and `load` returns nil.)
    @Test func anAdHocBuildStaysOnTheLegacyPlane() {
        let fake = FakeKeychainBackend(dataProtectionAvailable: false)
        let store = KeychainStore(testBackend: fake)

        try? store.save(rawKey: "secret_key", value: Self.secret)

        #expect(fake.rows(account: "secret_key", sync: false, dataProtection: false).count == 1)
        #expect(fake.rows(account: "secret_key", sync: false, dataProtection: true).isEmpty)
        #expect(store.load(rawKey: "secret_key") == Self.secret)
        store.reconcileCredentialAccessibility()
        #expect(fake.rows(account: "secret_key", sync: false, dataProtection: true).isEmpty)
        #expect(store.load(rawKey: "secret_key") == Self.secret)
    }

    // MARK: - The signed build: data-protection plane

    @Test func aSignedBuildWritesOnlyTheDataProtectionPlane() {
        let fake = FakeKeychainBackend(dataProtectionAvailable: true)
        let store = KeychainStore(testBackend: fake)

        try? store.save(rawKey: "secret_key", value: Self.secret)
        try? store.save(rawKey: "device_id", value: "device-1")

        for account in ["secret_key", "device_id"] {
            #expect(fake.rows(account: account, sync: false, dataProtection: true).count == 1)
            #expect(fake.rows(account: account, sync: false, dataProtection: false).isEmpty)
        }
        #expect(store.load(rawKey: "secret_key") == Self.secret)
        #expect(store.load(rawKey: "device_id") == "device-1")
    }

    // MARK: - Refusals: a signed build never reads the legacy plane

    /// A legacy-plane row is not this build's: a read returns absent and nothing is
    /// copied into the data-protection plane, lazily or at the launch reconcile.
    @Test func aSignedBuildNeverReadsOrCopiesALegacyPlaneRow() {
        let fake = FakeKeychainBackend(dataProtectionAvailable: true)
        let store = KeychainStore(testBackend: fake)
        seedLegacy(fake, store, "secret_key", Self.secret)
        seedLegacy(fake, store, "fauna/index", "{\"accounts\":[]}")
        seedLegacy(fake, store, "icloud_backup_enabled", "1")

        #expect(store.load(rawKey: "secret_key") == nil)
        #expect(store.load(rawKey: "fauna/index") == nil)
        #expect(store.iCloudBackupEnabled() == false)
        store.reconcileCredentialAccessibility()
        #expect(store.load(rawKey: "secret_key") == nil)

        #expect(fake.accounts(dataProtection: true).isEmpty)
    }

    /// Beside a legacy row of its own name, the data-protection copy is the only one read.
    @Test func theDataProtectionCopyIsTheOnlySource() {
        let fake = FakeKeychainBackend(dataProtectionAvailable: true)
        let store = KeychainStore(testBackend: fake)
        seedLegacy(fake, store, "secret_key", "OLD-LEGACY")
        try? store.save(rawKey: "secret_key", value: "NEW")

        #expect(store.load(rawKey: "secret_key") == "NEW")
        store.delete(rawKey: "secret_key")
        #expect(store.load(rawKey: "secret_key") == nil)
    }

    // MARK: - Deletes

    /// Factory reset: the whole service namespace in the write plane, both sync states.
    @Test func deleteAllSweepsTheWholeNamespace() {
        let fake = FakeKeychainBackend(dataProtectionAvailable: true)
        let store = KeychainStore(testBackend: fake)
        try? store.save(rawKey: "secret_key", value: Self.secret)
        try? store.save(rawKey: "fauna/index", value: "{}")
        try? store.save(rawKey: "device_id", value: "device-1")
        store.setICloudBackup(enabled: true)

        store.deleteAll()

        #expect(fake.accounts(dataProtection: true).isEmpty)
        #expect(store.load(rawKey: "secret_key") == nil)
        #expect(store.iCloudBackupEnabled() == false)
    }

    // MARK: - The preference and the probe

    /// The copy-not-move policy lands the synchronizable copies in the write plane.
    @Test func theBackupPreferenceDrivesTheDataProtectionPolicy() {
        let fake = FakeKeychainBackend(dataProtectionAvailable: true)
        let store = KeychainStore(testBackend: fake)
        try? store.save(rawKey: "secret_key", value: Self.secret)
        store.setICloudBackup(enabled: true)

        #expect(fake.rows(account: "secret_key", sync: false, dataProtection: true).count == 1)
        #expect(fake.rows(account: "secret_key", sync: true, dataProtection: true).count == 1)
        #expect(fake.rows(account: "secret_key", sync: true, dataProtection: false).isEmpty)
    }

    @Test func theProbeNeverWritesItsProbeRow() {
        let fake = FakeKeychainBackend(dataProtectionAvailable: true)
        let store = KeychainStore(testBackend: fake)
        try? store.save(rawKey: "secret_key", value: Self.secret)
        store.reconcileCredentialAccessibility()
        _ = store.load(rawKey: "secret_key")
        #expect(!fake.accounts(dataProtection: true).contains("__plane_probe__"))
        #expect(!fake.accounts(dataProtection: false).contains("__plane_probe__"))
    }
}
