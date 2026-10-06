import Testing
import Foundation
import Security
@testable import FaunaKit

/// The iCloud-backup accessibility policy (`docs/goal/architecture/apps/ios.md`
/// § Credential Storage, ratified 2026-07-10).
///
/// The real `SecItem` attribute — and whether iCloud actually syncs across two physical
/// devices — is the genuine "last inch" that needs a signed app + a real keychain (an
/// unsigned SPM test process cannot even *create* a `kSecAttrSynchronizable` item on macOS:
/// it lacks the data-protection-keychain entitlement). What these tests pin is the whole
/// testable *mechanism*: which accessibility constant + sync flag is chosen for each state,
/// the query invariant that keeps opting in from locking the user out, and the device-local
/// preference round-trip. Everything a wrong constant or a missing `SynchronizableAny` would
/// break is caught here, without touching the system keychain.
@MainActor
struct KeychainAccessibilityTests {
    // MARK: - Policy: which accessibility class + sync flag per state

    /// Default (opt-out) is device-bound and non-synchronizable: the identity secret is
    /// excluded from iCloud Keychain sync AND from encrypted-device-backup restore.
    @Test func defaultPolicyIsDeviceBoundAndNonSynchronizable() {
        let policy = KeychainStore.AccessPolicy.forBackup(enabled: false)
        #expect(policy.accessibleString == (kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String))
        #expect(policy.synchronizable == false)
    }

    /// Opt-in re-writes the item as `synchronizable` + `AfterFirstUnlock` — iCloud sync
    /// requires `kSecAttrSynchronizable`, which is incompatible with the `ThisDeviceOnly`
    /// classes (hence a re-write, not a flag flip).
    @Test func backupPolicyIsSynchronizableAfterFirstUnlock() {
        let policy = KeychainStore.AccessPolicy.forBackup(enabled: true)
        #expect(policy.accessibleString == (kSecAttrAccessibleAfterFirstUnlock as String))
        #expect(policy.synchronizable == true)
        // AfterFirstUnlock, not the stricter ThisDeviceOnly — otherwise sync is impossible.
        #expect(policy.accessibleString != (kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String))
    }

    // MARK: - The anti-lockout invariant

    /// Every read/delete matches `kSecAttrSynchronizableAny`. This is the one rule that keeps
    /// opting into iCloud backup from locking the user out: once a row is re-written
    /// synchronizable, a plain (implicitly non-sync) lookup can no longer find it.
    @Test func readAndDeleteQueriesMatchEitherSyncState() {
        let match = KeychainStore.matchQuery(service: "svc", account: "secret_key", plane: .legacy)
        #expect(isSyncAny(match[kSecAttrSynchronizable as String]))

        let enumerate = KeychainStore.enumerateQuery(service: "svc", plane: .legacy)
        #expect(isSyncAny(enumerate[kSecAttrSynchronizable as String]))
        // Enumeration returns account + data + attributes for the whole service.
        #expect(enumerate[kSecReturnData as String] as? Bool == true)
        #expect(enumerate[kSecReturnAttributes as String] as? Bool == true)
        #expect((enumerate[kSecMatchLimit as String] as? String) == (kSecMatchLimitAll as String))
    }

    /// The write payload carries exactly the policy's accessibility + sync flag — a save
    /// under the current preference lands the row at the right protection class.
    @Test func addQueryReflectsThePolicy() {
        let deviceBound = KeychainStore.addQuery(
            service: "svc", account: "secret_key", data: Data("hi".utf8),
            policy: .forBackup(enabled: false), plane: .legacy)
        #expect(cfString(deviceBound[kSecAttrAccessible as String])
                == (kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String))
        #expect(deviceBound[kSecAttrSynchronizable as String] as? Bool == false)

        let synced = KeychainStore.addQuery(
            service: "svc", account: "secret_key", data: Data("hi".utf8),
            policy: .forBackup(enabled: true), plane: .legacy)
        #expect(cfString(synced[kSecAttrAccessible as String]) == (kSecAttrAccessibleAfterFirstUnlock as String))
        #expect(synced[kSecAttrSynchronizable as String] as? Bool == true)
    }

    /// The plane decoration (`ios.md` § Credential Storage, *keychain plane*): on macOS a
    /// data-protection query selects that keychain AND addresses the app-only account access
    /// group (never the File Provider's shared group — the extension must not be able to read
    /// the seed), while the legacy plane adds nothing, so a legacy query addresses the default
    /// file-based keychain. On iOS both add nothing.
    @Test func planeAttributesSelectTheKeychainAndTheAppOnlyGroup() {
        let dp = KeychainStore.matchQuery(service: "svc", account: "secret_key", plane: .dataProtection)
        let legacy = KeychainStore.matchQuery(service: "svc", account: "secret_key", plane: .legacy)
        #if os(macOS)
            #expect(dp[kSecUseDataProtectionKeychain as String] as? Bool == true)
            #expect(dp[kSecAttrAccessGroup as String] as? String == AppleIdentifiers.accountKeychainGroup)
            #expect(AppleIdentifiers.accountKeychainGroup != AppleIdentifiers.appGroup)
        #else
            #expect(dp[kSecUseDataProtectionKeychain as String] == nil)
            #expect(dp[kSecAttrAccessGroup as String] == nil)
        #endif
        #expect(legacy[kSecUseDataProtectionKeychain as String] == nil)
        #expect(legacy[kSecAttrAccessGroup as String] == nil)
        // Everything else is identical between the two planes.
        #expect(cfString(legacy[kSecAttrService as String]) == cfString(dp[kSecAttrService as String]))
        #expect(isSyncAny(dp[kSecAttrSynchronizable as String]))
    }

    // MARK: - Device-local preference round-trip (hermetic, in-memory)

    /// Fresh install defaults to OFF (device-bound). The preference round-trips, and flipping
    /// it never disturbs the stored credentials — the values every read depends on survive.
    @Test func preferenceRoundTripsAndPreservesCredentials() {
        setenv("FAUNA_E2E_BRIDGE", "1", 1)
        let keychain = KeychainStore()
        keychain.deleteAll()
        defer { keychain.deleteAll(); unsetenv("FAUNA_E2E_BRIDGE") }

        // A real identity sitting in the store.
        try? keychain.save(rawKey: "secret_key", value: String(repeating: "11", count: 32))
        try? keychain.save(rawKey: "device_id", value: "device-1")

        // Default: OFF.
        #expect(keychain.iCloudBackupEnabled() == false)

        // Opt in → reads back ON, credentials untouched.
        keychain.setICloudBackup(enabled: true)
        #expect(keychain.iCloudBackupEnabled() == true)
        #expect(keychain.load(rawKey: "secret_key") == String(repeating: "11", count: 32))
        #expect(keychain.load(rawKey: "device_id") == "device-1")

        // Opt back out → OFF, credentials still intact.
        keychain.setICloudBackup(enabled: false)
        #expect(keychain.iCloudBackupEnabled() == false)
        #expect(keychain.load(rawKey: "secret_key") == String(repeating: "11", count: 32))

        // A factory reset clears the preference too — a fresh identity defaults back to OFF.
        keychain.setICloudBackup(enabled: true)
        keychain.deleteAll()
        #expect(keychain.iCloudBackupEnabled() == false)
    }

    /// The launch reconcile is safe to run against an empty and a populated store (it is the
    /// self-healing every-launch pass).
    @Test func reconcileIsIdempotentAndHarmless() {
        setenv("FAUNA_E2E_BRIDGE", "1", 1)
        let keychain = KeychainStore()
        keychain.deleteAll()
        defer { keychain.deleteAll(); unsetenv("FAUNA_E2E_BRIDGE") }

        keychain.reconcileCredentialAccessibility()  // empty store
        try? keychain.save(rawKey: "secret_key", value: "abc")
        keychain.reconcileCredentialAccessibility()
        keychain.reconcileCredentialAccessibility()
        #expect(keychain.load(rawKey: "secret_key") == "abc")
        #expect(keychain.iCloudBackupEnabled() == false)
    }

    // MARK: - helpers

    private func isSyncAny(_ value: Any?) -> Bool {
        cfString(value) == (kSecAttrSynchronizableAny as String)
    }

    /// Compare a query-dict value that may arrive as `CFString` or bridged `String`.
    private func cfString(_ value: Any?) -> String? {
        if let s = value as? String { return s }
        if let cf = value, CFGetTypeID(cf as CFTypeRef) == CFStringGetTypeID() {
            return (cf as! CFString) as String
        }
        return nil
    }
}
