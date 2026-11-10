import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it. Also required
// for correctness: it drives `APIClient.folderSetCustodyJson`, itself
// `#if DEBUG`, over a UniFFI export (`FfiNestClient.folderSetCustodyJson`)
// that only the `test-helpers` flavor carries.
#if DEBUG

/// Shared handler for the cross-app `folder_set_custody` TestAgent command —
/// the succession journeys' cross-seat custody comparison
/// (`succession-aftermath.md` § Re-key scope): which content-key generations
/// THIS app's folder custody holds for one set, `{"found", "channel_id_hex",
/// "generations": [{"version", "key_fp"}]}`, from the shared
/// `fauna_client_account_runtime::folder_set_custody_json` reader tui's and
/// linux's arms call. The apple twin of those arms.
///
/// The `ReadMarkerStateTestCommand` shape: the answer rides
/// `state.machine_method_result`; a selector shared Rust refuses (neither
/// given, or a malformed channel) is a loud refusal (convention 11), never a
/// quiet "not found". Lives in FaunaKit so macOS + iOS share ONE implementation.
public enum FolderSetCustodyTestCommand {
    /// What the shell does with the command's result — the same two-case shape
    /// as every sibling command.
    public enum Outcome: Equatable {
        /// The reader's JSON, for the shell to stash in `machineMethodResultBox`.
        case report(String)
        /// The command cannot be honoured; the shell surfaces the reason as a
        /// **loud** TestAgent failure and leaves the result slot empty.
        case refused(String)
    }

    /// What the shared reader answers when there is no store to read; reproduced
    /// for the one case that never reaches Rust — no connected client.
    public static let notFoundJSON = #"{"found":false}"#

    public static func apply(_ command: [String: Any], api: APIClient?) async -> Outcome {
        // Which selectors are valid is shared Rust's call
        // (`fauna_client_folders::custody::read_held_generations` refuses
        // neither-given and a malformed channel); nothing is checked here.
        let setName = command["set_name"] as? String
        let channelIdHex = command["channel_id_hex"] as? String
        guard let api else { return .report(notFoundJSON) }
        do {
            guard let json = try await api.folderSetCustodyJson(
                setName: setName, channelIdHex: channelIdHex
            ) else {
                return .report(notFoundJSON)
            }
            return .report(json)
        } catch {
            return .refused("folder_set_custody: \(error)")
        }
    }
}

#endif
