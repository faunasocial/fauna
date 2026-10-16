import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it. Also required
// for correctness: it drives `APIClient.readMarkerStateJson`, itself `#if DEBUG`,
// over a UniFFI export (`FfiNestClient.readMarkerStateJson`) that only the
// `test-helpers` flavor carries.
#if DEBUG

/// Shared handler for the cross-app `read_marker_state` TestAgent command — the
/// read-state relaunch witness's durability barrier
/// (`docs/goal/behavior/conversation-read-state.md` § The read-marker record):
/// `{"found":true,"through":N}` or `{"found":false}` for `channel_id_hex`, from
/// THIS app's own account store. The apple twin of tui's and linux's arm.
///
/// The `DeviceSetStateTestCommand` shape exactly: the answer rides
/// `state.machine_method_result`, any non-`nil` answer means "the reader is
/// built" (a `nil` would make the witness skip as unbuilt), so the not-found
/// fallback is a *report*, never a refusal. Lives in FaunaKit so macOS + iOS
/// share ONE implementation.
public enum ReadMarkerStateTestCommand {
    /// What the shell does with the command's result — the same two-case shape
    /// as `DeviceSetStateTestCommand.Outcome`.
    public enum Outcome: Equatable {
        /// The reader's JSON, for the shell to stash in `machineMethodResultBox`.
        case report(String)
        /// The command cannot be honoured; the shell must surface the reason as a
        /// **loud** TestAgent failure (convention 11) and leave the result slot
        /// empty.
        case refused(String)
    }

    /// What the shared reader answers when there is no store to read; reproduced
    /// for the one case that never reaches Rust — no connected client.
    public static let notFoundJSON = #"{"found":false}"#

    public static func apply(_ command: [String: Any], api: APIClient?) async -> Outcome {
        // Convention 11's bad-payload clause: a missing id must not fold into
        // `notFoundJSON`, or an "unread" assertion passes on a typo'd key.
        guard let channelIdHex = command["channel_id_hex"] as? String else {
            return .refused("read_marker_state: needs a `channel_id_hex` string")
        }
        guard let api, let json = await api.readMarkerStateJson(channelIdHex: channelIdHex) else {
            return .report(notFoundJSON)
        }
        return .report(json)
    }
}

#endif
