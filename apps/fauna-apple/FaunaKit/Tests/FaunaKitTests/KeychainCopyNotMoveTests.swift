import Testing
import Foundation
import Security
@testable import FaunaKit

/// Copy-not-move iCloud-backup keychain mechanics (`docs/goal/architecture/apps/ios.md`
/// § Credential Storage).
///
/// The genuine "last inch" — whether iCloud actually syncs a `synchronizable` item across two
/// physical devices and whether a delete propagates circle-wide — needs a signed app on two real
/// devices in one Apple-ID iCloud-Keychain circle. What these tests pin is the whole testable
/// *mechanism*: that opt-in KEEPS the device-bound working copy (so no device's only copy is ever
/// the shared circle row), that opt-out removes only the synchronizable backup copy, that `load`
/// reads this device's own device-bound copy over a clobbered circle copy, and that a restored
/// device re-lands a device-bound copy. They drive the exact production code path (`inMemory` is
/// forced off) against a `FakeKeychainBackend` that models the keychain primary key
/// `(service, account, synchronizable)` with `SynchronizableAny`-matched deletes — everything a
/// move-semantics regression or a missing device-bound copy would break, without a real keychain.
@MainActor
struct KeychainCopyNotMoveTests {
    // MARK: - Opt-in keeps the device-bound working copy (the move-semantics regression guard)

    /// Opting in must ADD a synchronizable backup copy while KEEPING the `ThisDeviceOnly`
    /// device-bound working copy. Under the old move-semantics the device-bound copy was deleted,
    /// leaving the iCloud-circle row as the device's only copy — the lockout footgun the review
    /// found. (Red-verified: reverting the copy-keep drops the `sync:false` count to 0 here.)
    @Test func optInKeepsDeviceBoundWorkingCopyAndAddsSyncCopy() {
        let fake = FakeKeychainBackend()
        let keychain = KeychainStore(testBackend: fake)

        try? keychain.save(rawKey: "secret_key", value: "SECRET")
        #expect(fake.rows(account: "secret_key", sync: false).count == 1)
        #expect(fake.rows(account: "secret_key", sync: true).isEmpty)

        keychain.setICloudBackup(enabled: true)

        // Copy-not-move: BOTH copies now exist, and the device-bound copy still carries the value.
        #expect(fake.rows(account: "secret_key", sync: false).count == 1)
        #expect(fake.rows(account: "secret_key", sync: true).count == 1)
        #expect(fake.data(account: "secret_key", sync: false) == Data("SECRET".utf8))
        #expect(fake.data(account: "secret_key", sync: true) == Data("SECRET".utf8))
        // The device-bound copy stays ThisDeviceOnly even while backup is on; only the backup
        // copy is AfterFirstUnlock + synchronizable.
        #expect(fake.accessible(account: "secret_key", sync: false)
                == (kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String))
        #expect(fake.accessible(account: "secret_key", sync: true)
                == (kSecAttrAccessibleAfterFirstUnlock as String))
        #expect(keychain.load(rawKey: "secret_key") == "SECRET")
    }

    /// A save made while backup is already ON lands BOTH copies immediately (the invariant holds
    /// without waiting for the next launch reconcile).
    @Test func saveWhileBackupOnLandsBothCopies() {
        let fake = FakeKeychainBackend()
        let keychain = KeychainStore(testBackend: fake)
        keychain.setICloudBackup(enabled: true)  // pref ON, no credentials yet

        try? keychain.save(rawKey: "device_id", value: "device-1")

        #expect(fake.rows(account: "device_id", sync: false).count == 1)
        #expect(fake.rows(account: "device_id", sync: true).count == 1)
        #expect(keychain.load(rawKey: "device_id") == "device-1")
    }

    // MARK: - Opt-out removes ONLY the circle copy

    /// Opting out deletes the synchronizable (circle) copy and keeps the device-bound working
    /// copy — so a circle-wide delete propagation can never take a device's working copy with it.
    @Test func optOutDeletesOnlyTheSynchronizableCopy() {
        let fake = FakeKeychainBackend()
        let keychain = KeychainStore(testBackend: fake)
        try? keychain.save(rawKey: "secret_key", value: "SECRET")
        keychain.setICloudBackup(enabled: true)
        #expect(fake.rows(account: "secret_key", sync: true).count == 1)

        keychain.setICloudBackup(enabled: false)

        #expect(fake.rows(account: "secret_key", sync: true).isEmpty)      // circle copy gone
        #expect(fake.rows(account: "secret_key", sync: false).count == 1)  // working copy stays
        #expect(fake.data(account: "secret_key", sync: false) == Data("SECRET".utf8))
        #expect(keychain.load(rawKey: "secret_key") == "SECRET")
    }

    // MARK: - load prefers the device-bound copy (face (b): cross-identity clobber)

    /// Under a shared Apple ID with a *different* fauna identity, the synchronizable copy of a
    /// fixed-key row can be clobbered circle-wide with the sibling identity's secret. `load`
    /// must still return THIS device's own device-bound value, never the clobbered circle value.
    @Test func loadPrefersDeviceBoundCopyOverClobberedCircleCopy() {
        let fake = FakeKeychainBackend()
        let keychain = KeychainStore(testBackend: fake)
        try? keychain.save(rawKey: "secret_key", value: "MINE")
        keychain.setICloudBackup(enabled: true)

        // A sibling device on the same Apple ID overwrites the circle (synchronizable) row.
        fake.clobberSyncCopy(account: "secret_key", data: Data("THEIRS".utf8))

        #expect(keychain.load(rawKey: "secret_key") == "MINE")
    }

    /// With no device-bound copy at all (a freshly-restored device that received only the synced
    /// item), `load` falls back to the synchronizable copy — the anti-lockout invariant.
    @Test func loadFallsBackToSynchronizableCopyWhenNoDeviceBoundCopyExists() {
        let fake = FakeKeychainBackend()
        let keychain = KeychainStore(testBackend: fake)
        fake.seedSyncOnlyRow(service: keychain.service, account: "secret_key", data: Data("RESTORED".utf8))

        #expect(keychain.load(rawKey: "secret_key") == "RESTORED")
    }

    // MARK: - Restored device re-lands a device-bound copy on reconcile

    /// A restored device holds only the synced copy and the backup preference ON. The launch
    /// reconcile lands a device-bound working copy so the device no longer depends on the shared
    /// circle row as its sole copy — closing the same lockout window from the restore direction.
    @Test func restoredDeviceGainsDeviceBoundCopyOnReconcile() {
        let fake = FakeKeychainBackend()
        let keychain = KeychainStore(testBackend: fake)
        keychain.setICloudBackup(enabled: true)  // pref ON, no credentials yet
        fake.seedSyncOnlyRow(service: keychain.service, account: "secret_key", data: Data("RESTORED".utf8))
        #expect(fake.rows(account: "secret_key", sync: false).isEmpty)

        keychain.reconcileCredentialAccessibility()

        #expect(fake.rows(account: "secret_key", sync: false).count == 1)
        #expect(fake.rows(account: "secret_key", sync: true).count == 1)
        #expect(fake.data(account: "secret_key", sync: false) == Data("RESTORED".utf8))
        #expect(keychain.load(rawKey: "secret_key") == "RESTORED")
    }

    // MARK: - deleteAll sweeps both copies

    /// A factory reset wipes both the device-bound and the synchronizable copy — an opted-in
    /// identity cannot silently survive a reset in the iCloud circle.
    @Test func deleteAllSweepsBothCopies() {
        let fake = FakeKeychainBackend()
        let keychain = KeychainStore(testBackend: fake)
        try? keychain.save(rawKey: "secret_key", value: "SECRET")
        keychain.setICloudBackup(enabled: true)
        #expect(fake.rows(account: "secret_key", sync: false).count == 1)
        #expect(fake.rows(account: "secret_key", sync: true).count == 1)

        keychain.deleteAll()

        #expect(fake.rows(account: "secret_key", sync: false).isEmpty)
        #expect(fake.rows(account: "secret_key", sync: true).isEmpty)
    }
}

// MARK: - Fake keychain modeling the (service, account, synchronizable) primary key

/// An in-process keychain model faithful enough to pin the copy-not-move mechanism: a row is keyed
/// by `(service, account, synchronizable)`, `add` on a duplicate key returns `errSecDuplicateItem`,
/// and a delete/read/update honours the `synchronizable` predicate — including
/// `kSecAttrSynchronizableAny`, which matches (and a delete removes) BOTH sync states, the way a
/// real circle-wide delete would. It does not model cross-device propagation (that is the manual
/// last inch); the tests simulate a sibling device's effect directly via `clobberSyncCopy`.
final class FakeKeychainBackend: KeychainBackend {
    struct Row {
        var service: String
        var account: String
        var synchronizable: Bool
        var accessible: String
        var data: Data
        /// Which of macOS's two keychains the row is in (`KeychainStore.Plane`): a query
        /// carrying `kSecUseDataProtectionKeychain: true` sees only data-protection rows, one
        /// without it only legacy rows — the strict split the one-plane-per-build store relies on.
        var dataProtection: Bool
    }
    private var rows: [Row] = []

    /// Whether this "signature" reaches the data-protection plane. `false` models an ad-hoc
    /// dev build: every data-protection-plane operation returns `errSecMissingEntitlement`
    /// (−34018, measured 2026-07-20), exactly what the store's probe keys on.
    let dataProtectionAvailable: Bool

    init(dataProtectionAvailable: Bool = true) {
        self.dataProtectionAvailable = dataProtectionAvailable
    }

    private enum SyncMatch { case any, yes, no }

    private func syncMatch(_ query: [String: Any]) -> SyncMatch {
        guard let raw = query[kSecAttrSynchronizable as String] else { return .no }
        if let s = Self.cfString(raw), s == (kSecAttrSynchronizableAny as String) { return .any }
        if let b = raw as? Bool { return b ? .yes : .no }
        if let n = raw as? NSNumber { return n.boolValue ? .yes : .no }
        return .no
    }

    private static func isDataProtection(_ query: [String: Any]) -> Bool {
        (query[kSecUseDataProtectionKeychain as String] as? Bool) ?? false
    }

    /// The plane gate every operation passes first: a data-protection query from a
    /// "signature" that cannot reach that plane fails the way the real keychain does.
    private func planeRefusal(_ query: [String: Any]) -> OSStatus? {
        Self.isDataProtection(query) && !dataProtectionAvailable ? errSecMissingEntitlement : nil
    }

    private func matches(_ row: Row, _ query: [String: Any]) -> Bool {
        if row.dataProtection != Self.isDataProtection(query) {
            return false
        }
        if let service = query[kSecAttrService as String] as? String, service != row.service {
            return false
        }
        if let account = query[kSecAttrAccount as String] as? String, account != row.account {
            return false
        }
        switch syncMatch(query) {
        case .any: return true
        case .yes: return row.synchronizable
        case .no: return !row.synchronizable
        }
    }

    func add(_ attributes: [String: Any]) -> OSStatus {
        if let refusal = planeRefusal(attributes) { return refusal }
        let service = attributes[kSecAttrService as String] as? String ?? ""
        let account = attributes[kSecAttrAccount as String] as? String ?? ""
        let sync = (attributes[kSecAttrSynchronizable as String] as? Bool) ?? false
        let dp = Self.isDataProtection(attributes)
        if rows.contains(where: {
            $0.service == service && $0.account == account && $0.synchronizable == sync
                && $0.dataProtection == dp
        }) {
            return errSecDuplicateItem
        }
        rows.append(Row(
            service: service, account: account, synchronizable: sync,
            accessible: Self.cfString(attributes[kSecAttrAccessible as String]) ?? "",
            data: attributes[kSecValueData as String] as? Data ?? Data(),
            dataProtection: dp))
        return errSecSuccess
    }

    func copyMatching(_ query: [String: Any]) -> (status: OSStatus, item: AnyObject?) {
        if let refusal = planeRefusal(query) { return (refusal, nil) }
        let matched = rows.filter { matches($0, query) }
        guard !matched.isEmpty else { return (errSecItemNotFound, nil) }
        let all = (query[kSecMatchLimit as String] as? String) == (kSecMatchLimitAll as String)
        if all {
            let dicts: [[String: Any]] = matched.map {
                [
                    kSecAttrAccount as String: $0.account,
                    kSecAttrSynchronizable as String: $0.synchronizable,
                    kSecAttrAccessible as String: $0.accessible,
                    kSecValueData as String: $0.data,
                ]
            }
            return (errSecSuccess, dicts as AnyObject)
        }
        // Limit-one, returning data (the only single-item shape the store asks for).
        return (errSecSuccess, matched[0].data as AnyObject)
    }

    func update(_ query: [String: Any], _ attributesToUpdate: [String: Any]) -> OSStatus {
        if let refusal = planeRefusal(query) { return refusal }
        var updated = false
        for i in rows.indices where matches(rows[i], query) {
            if let accessible = Self.cfString(attributesToUpdate[kSecAttrAccessible as String]) {
                rows[i].accessible = accessible
            }
            if let data = attributesToUpdate[kSecValueData as String] as? Data {
                rows[i].data = data
            }
            updated = true
        }
        return updated ? errSecSuccess : errSecItemNotFound
    }

    func delete(_ query: [String: Any]) -> OSStatus {
        if let refusal = planeRefusal(query) { return refusal }
        let before = rows.count
        rows.removeAll { matches($0, query) }
        return rows.count < before ? errSecSuccess : errSecItemNotFound
    }

    // MARK: test inspection + seeding

    /// Rows for one account in one sync state, across BOTH planes (the copy-not-move
    /// assertions are about sync-state copies, whichever plane the build writes).
    func rows(account: String, sync: Bool) -> [Row] {
        rows.filter { $0.account == account && $0.synchronizable == sync }
    }

    /// Rows for one account in one sync state in ONE plane (the plane assertions).
    func rows(account: String, sync: Bool, dataProtection: Bool) -> [Row] {
        rows(account: account, sync: sync).filter { $0.dataProtection == dataProtection }
    }

    /// Rows for one account in one sync state, in one plane, under ONE service — the
    /// retired-spelling refusals, which are precisely about telling two spellings of the
    /// same account apart (`KeychainRetiredServiceTests`). The service-blind overloads
    /// above would count a retired row and a current one as one set.
    func rows(service: String, account: String, sync: Bool, dataProtection: Bool) -> [Row] {
        rows(account: account, sync: sync, dataProtection: dataProtection)
            .filter { $0.service == service }
    }

    func data(account: String, sync: Bool) -> Data? {
        rows(account: account, sync: sync).first?.data
    }

    func accessible(account: String, sync: Bool) -> String? {
        rows(account: account, sync: sync).first?.accessible
    }

    /// Every account name present in one plane (the whole-namespace sweep assertions).
    func accounts(dataProtection: Bool) -> Set<String> {
        Set(rows.filter { $0.dataProtection == dataProtection }.map(\.account))
    }

    /// Inject a raw row (for restore scenarios and the refusal pins). `service` must be the
    /// store's own `service` for the store's enumerate/load queries to find it. The plane
    /// defaults to the one this "signature" writes; pass `dataProtection: false` to seed a
    /// legacy-keychain row on a build that can reach the data-protection plane — a row the
    /// store must never read (`KeychainPlaneTests`).
    func seedRow(
        service: String, account: String, synchronizable: Bool, accessible: String, data: Data,
        dataProtection: Bool? = nil
    ) {
        rows.append(Row(
            service: service, account: account, synchronizable: synchronizable,
            accessible: accessible, data: data,
            dataProtection: dataProtection ?? dataProtectionAvailable))
    }

    /// A row that arrived on this device only as the synchronizable (iCloud-circle) copy — the
    /// freshly-restored-device state.
    func seedSyncOnlyRow(service: String, account: String, data: Data) {
        seedRow(
            service: service, account: account, synchronizable: true,
            accessible: kSecAttrAccessibleAfterFirstUnlock as String, data: data)
    }

    /// Simulate a sibling device on the same Apple ID overwriting the shared circle (sync) copy's
    /// value — the cross-identity clobber of face (b). The row must already exist.
    func clobberSyncCopy(account: String, data: Data) {
        for i in rows.indices where rows[i].account == account && rows[i].synchronizable {
            rows[i].data = data
        }
    }

    private static func cfString(_ value: Any?) -> String? {
        if let s = value as? String { return s }
        if let cf = value, CFGetTypeID(cf as CFTypeRef) == CFStringGetTypeID() {
            return (cf as! CFString) as String
        }
        return nil
    }
}
