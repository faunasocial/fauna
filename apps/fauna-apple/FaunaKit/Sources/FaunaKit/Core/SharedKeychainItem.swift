import Foundation
import Security

/// Low-level shared-Keychain (App Group) `kSecClassGenericPassword` primitives —
/// the common `(service, account, accessGroup)`-keyed shape `PushManager` and
/// `FileProviderCredentialStore` both use for their App-Group items (each writer
/// process, each reader process, no seam beyond a matching Keychain query).
///
/// `useDataProtectionKeychain` is per-caller, not a shared default: it is
/// load-bearing on macOS for `FileProviderCredentialStore` (cross-process
/// rendezvous with a sandboxed, app-dead extension — see its own call sites'
/// comment) but a no-op for `PushManager`'s reader, the iOS-only `Fauna-NSE`,
/// where the data-protection keychain is the only one that exists; omitted
/// there since the flag would change nothing.
///
/// **An e2e launch on macOS never reaches the system keychain through here**
/// (convention 10, `e2e-launch-isolation.md`). The harness relocates `HOME` and
/// `CFFIXED_USER_HOME`, so a `SecItem*` call from the bare e2e binary finds no
/// login keychain, and macOS raises a REAL dialog on the box's console ("A
/// keychain cannot be found to store …", offering *Reset To Defaults* over the
/// box's own login keychain). Nobody answers it on macOS, and it can hold the
/// calling thread for hours. So under `FaunaE2E.isActive` every operation runs
/// on a process-static map instead, the way `KeychainStore`'s in-memory e2e
/// store does. The gate sits at this one primitive, not at each caller
/// (`PushManager`'s keys, `FileProviderCredentialStore`'s app-dead capability).
/// No e2e launch needs the cross-process rendezvous these items exist for: the
/// File Provider witnesses drive the extension through `FileProviderTestCLI`,
/// a launch with no `FaunaE2E` environment, and the bare binary has no
/// notification host. iOS keeps the real keychain, because a simulator's
/// keychain belongs to the seat, never the box, and raises no dialog.
/// `#if DEBUG` for the payload's severity, as `KeychainStore.e2eFileURL`: this
/// decides where key material is written.
enum SharedKeychainItem {
    #if DEBUG
    /// True when this process's operations run on the e2e map, never `SecItem*`.
    static var e2eHeldInMemory: Bool {
        #if os(macOS)
        return FaunaE2E.isActive
        #else
        return false
        #endif
    }

    private static let e2eLock = NSLock()
    private static var e2eItems: [String: Data] = [:]
    private static var e2eAnnounced = false
    /// How many operations reached the system keychain. The unit test that pins
    /// an e2e launch to zero reads it; it is never a production signal.
    private(set) static var systemKeychainCalls = 0

    private static func e2eKey(_ service: String, _ account: String, _ group: String) -> String {
        [service, account, group].joined(separator: "\u{1F}")
    }

    /// Run `body` on the e2e map when this is an e2e launch. `nil` means "not
    /// e2e, go to the system keychain", and counts that call.
    private static func e2e<T>(_ body: (inout [String: Data]) -> T) -> T? {
        e2eLock.lock()
        defer { e2eLock.unlock() }
        guard e2eHeldInMemory else {
            systemKeychainCalls += 1
            return nil
        }
        if !e2eAnnounced {
            e2eAnnounced = true
            // The launch's log witness (`app.err`) that this process holds its
            // shared items off the box's keychain.
            FileHandle.standardError.write(Data(
                "[keychain] e2e launch: shared keychain items held in memory, never the box's keychain\n".utf8))
        }
        return body(&e2eItems)
    }
    #endif

    static func save(
        service: String, account: String, accessGroup: String, data: Data,
        accessible: CFString = kSecAttrAccessibleAfterFirstUnlock,
        useDataProtectionKeychain: Bool = false
    ) {
        #if DEBUG
        if e2e({ $0[e2eKey(service, account, accessGroup)] = data }) != nil { return }
        #endif
        var query = baseQuery(
            service: service, account: account, accessGroup: accessGroup,
            useDataProtectionKeychain: useDataProtectionKeychain)
        query[kSecAttrAccessible as String] = accessible
        query[kSecValueData as String] = data
        SecItemDelete(query as CFDictionary)
        SecItemAdd(query as CFDictionary, nil)
    }

    static func load(
        service: String, account: String, accessGroup: String,
        useDataProtectionKeychain: Bool = false
    ) -> Data? {
        #if DEBUG
        if let held = e2e({ $0[e2eKey(service, account, accessGroup)] }) { return held }
        #endif
        var query = baseQuery(
            service: service, account: account, accessGroup: accessGroup,
            useDataProtectionKeychain: useDataProtectionKeychain)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        guard status == errSecSuccess, let data = result as? Data else { return nil }
        return data
    }

    static func delete(
        service: String, account: String, accessGroup: String,
        useDataProtectionKeychain: Bool = false
    ) {
        #if DEBUG
        if e2e({ $0.removeValue(forKey: e2eKey(service, account, accessGroup)) }) != nil {
            return
        }
        #endif
        let query = baseQuery(
            service: service, account: account, accessGroup: accessGroup,
            useDataProtectionKeychain: useDataProtectionKeychain)
        SecItemDelete(query as CFDictionary)
    }

    /// Add-then-read round trip against a throwaway account, returning the raw
    /// `(SecItemAdd, SecItemCopyMatching)` status pair — for diagnostics that need
    /// to see *why* a rendezvous failed instead of a bare `nil`/no-op.
    static func probeRoundTrip(
        service: String, probeAccount: String, accessGroup: String,
        accessible: CFString = kSecAttrAccessibleAfterFirstUnlock,
        useDataProtectionKeychain: Bool = false
    ) -> (add: OSStatus, read: OSStatus) {
        #if DEBUG
        if let held = e2e({ _ in (add: errSecSuccess, read: errSecSuccess) }) { return held }
        #endif
        var addQuery = baseQuery(
            service: service, account: probeAccount, accessGroup: accessGroup,
            useDataProtectionKeychain: useDataProtectionKeychain)
        addQuery[kSecAttrAccessible as String] = accessible
        addQuery[kSecValueData as String] = Data("probe".utf8)
        SecItemDelete(addQuery as CFDictionary)
        let addStatus = SecItemAdd(addQuery as CFDictionary, nil)

        var readQuery = baseQuery(
            service: service, account: probeAccount, accessGroup: accessGroup,
            useDataProtectionKeychain: useDataProtectionKeychain)
        readQuery[kSecReturnData as String] = true
        readQuery[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: AnyObject?
        let readStatus = SecItemCopyMatching(readQuery as CFDictionary, &result)

        SecItemDelete(addQuery as CFDictionary)
        return (addStatus, readStatus)
    }

    private static func baseQuery(
        service: String, account: String, accessGroup: String,
        useDataProtectionKeychain: Bool
    ) -> [String: Any] {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecAttrAccessGroup as String: accessGroup,
        ]
        if useDataProtectionKeychain {
            query[kSecUseDataProtectionKeychain as String] = true
        }
        return query
    }
}
