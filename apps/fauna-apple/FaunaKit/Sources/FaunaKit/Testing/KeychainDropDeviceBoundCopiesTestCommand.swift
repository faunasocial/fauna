import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app shells'
// whole `handleTestCommand` surface that calls into it, and like the
// `KeychainStore.e2eDropDeviceBoundCopies` it drives.
#if DEBUG

/// Shared handler for the `keychain_drop_device_bound_copies` TestAgent command — the
/// restored-device model of the `identity-in-your-cloud-keychain` outcome 3 witness
/// (`docs/goal/architecture/apps/ios.md` § Credential Storage, the keychain witness seam):
/// it deletes every device-bound row of the store's service and answers
/// `{"dropped":<n>}`. What is left is the set a restore leaves, since the platform keeps
/// `ThisDeviceOnly` items out of a restore and brings synchronizable ones through iCloud
/// Keychain — so the device-bound preference row goes too, with no exception.
///
/// The `KeychainItemAttributesTestCommand` shape: the answer rides
/// `state.machine_method_result`, and a launch whose store is the in-memory e2e store has
/// no such rows, so it is REFUSED loudly (convention 11) rather than answered with a
/// count a test could read as "nothing to drop". So is a delete the keychain refused.
/// Lives in FaunaKit so macOS + iOS share ONE implementation.
public enum KeychainDropDeviceBoundCopiesTestCommand {
    public typealias Outcome = KeychainItemAttributesTestCommand.Outcome

    public static func apply(store: KeychainStore = KeychainStore()) -> Outcome {
        guard let drop = store.e2eDropDeviceBoundCopies() else {
            return .refused(
                "keychain_drop_device_bound_copies: this launch's credential store is the "
                    + "in-memory e2e store, which has no device-bound rows — launch with "
                    + "`real_keychain`")
        }
        guard drop.status == errSecSuccess || drop.status == errSecItemNotFound else {
            return .refused(
                "keychain_drop_device_bound_copies: the keychain refused the delete: "
                    + "\(drop.status)")
        }
        guard let data = try? JSONSerialization.data(withJSONObject: ["dropped": drop.dropped]),
              let json = String(data: data, encoding: .utf8)
        else {
            return .refused("keychain_drop_device_bound_copies: could not encode the report")
        }
        return .report(json)
    }
}

#endif
