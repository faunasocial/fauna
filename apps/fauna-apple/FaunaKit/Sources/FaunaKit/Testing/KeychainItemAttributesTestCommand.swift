import CryptoKit
import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it, and like the
// `KeychainStore.e2eItemAttributes` reader it drives.
#if DEBUG

/// Shared handler for the `keychain_item_attributes` TestAgent command — the
/// outcome-4 witness's read-back (`docs/features/identity-protected-on-this-device.md`
/// outcomes 3 and 4, the ios and macos columns; `docs/goal/architecture/apps/common.md`
/// § Credential storage): the accessibility class and sync flag of every copy the REAL
/// keychain holds of `account`, as
/// `{"status":0,"copies":[{"accessible":"kSecAttrAccessible…","synchronizable":false}]}`.
/// Attributes only — the attribute read never asks the keychain for the item's data.
///
/// `"digest": true` adds `"value_sha256"`, the hex digest of what the store's own read
/// path loads for `account` (absent when it loads nothing), never the value. It is the
/// macos arm's positive control that the row holds the secret: `security … -w` cannot
/// stand in, because the item's ACL names only the app that wrote it and any other
/// binary blocks on a dialog (measured 2026-10-30).
///
/// The `ReadMarkerStateTestCommand` shape: the answer rides
/// `state.machine_method_result`; a launch whose store is the in-memory e2e store
/// (every launch without `FAUNA_E2E_REAL_KEYCHAIN`) has no class to report, so it is
/// REFUSED loudly (convention 11) rather than answered with an empty list a test could
/// read as "no item". Lives in FaunaKit so macOS + iOS share ONE implementation.
public enum KeychainItemAttributesTestCommand {
    public enum Outcome: Equatable {
        /// The reader's JSON, for the shell to stash in `machineMethodResultBox`.
        case report(String)
        /// The command cannot be honoured; the shell surfaces the reason as a loud
        /// TestAgent failure and leaves the result slot empty.
        case refused(String)
    }

    public static func apply(_ command: [String: Any], store: KeychainStore = KeychainStore())
        -> Outcome
    {
        // Convention 11's bad-payload clause: a missing account must not read as "no
        // copies", or an absence assertion passes on a typo'd key.
        guard let account = command["account"] as? String, !account.isEmpty else {
            return .refused("keychain_item_attributes: needs a non-empty `account` string")
        }
        guard let read = store.e2eItemAttributes(account: account) else {
            return .refused(
                "keychain_item_attributes: this launch's credential store is the in-memory "
                    + "e2e store, which has no keychain class to report — launch with "
                    + "`real_keychain`")
        }
        var body: [String: Any] = [
            "status": Int(read.status),
            "copies": read.copies.map {
                ["accessible": $0.accessible, "synchronizable": $0.synchronizable] as [String: Any]
            },
        ]
        if command["digest"] as? Bool == true, let value = store.load(rawKey: account) {
            body["value_sha256"] = SHA256.hash(data: Data(value.utf8))
                .map { String(format: "%02x", $0) }.joined()
        }
        guard let data = try? JSONSerialization.data(withJSONObject: body, options: [.sortedKeys]),
              let json = String(data: data, encoding: .utf8)
        else {
            return .refused("keychain_item_attributes: could not encode the report")
        }
        return .report(json)
    }
}

#endif
