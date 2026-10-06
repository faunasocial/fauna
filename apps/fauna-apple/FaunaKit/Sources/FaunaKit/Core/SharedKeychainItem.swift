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
enum SharedKeychainItem {
    static func save(
        service: String, account: String, accessGroup: String, data: Data,
        accessible: CFString = kSecAttrAccessibleAfterFirstUnlock,
        useDataProtectionKeychain: Bool = false
    ) {
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
