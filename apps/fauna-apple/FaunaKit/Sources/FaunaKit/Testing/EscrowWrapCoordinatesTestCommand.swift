import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it. Also required
// for correctness: it drives `APIClient.escrowWrapCoordinatesJson`, itself
// `#if DEBUG`, over a UniFFI export (`FfiNestClient.escrowWrapCoordinatesJson`)
// that only the `test-helpers` flavor carries.
#if DEBUG

/// Shared handler for the cross-app `escrow_wrap_coordinates` TestAgent command —
/// the recovery-kit let-go journey's staging read
/// (`fleet-scope-reclamation.md` § Fleet-scope reclamation, clause (3)(j)):
/// `{"found":true,"rows":[...]}` or `{"found":false}`, from THIS app's own
/// account runtime. The apple twin of tui's arm.
///
/// The `ReadMarkerStateTestCommand` shape: the answer rides
/// `state.machine_method_result`, and any non-`nil` answer means "the reader is
/// built", so the no-client fallback is a *report*, never a refusal. The command
/// takes no payload. Lives in FaunaKit so macOS + iOS share ONE implementation.
public enum EscrowWrapCoordinatesTestCommand {
    /// What the shell does with the command's result. Always a report today; the
    /// two-case shape matches every sibling command so the shells' arms read alike.
    public enum Outcome: Equatable {
        /// The reader's JSON, for the shell to stash in `machineMethodResultBox`.
        case report(String)
        /// The command cannot be honoured; the shell surfaces the reason as a
        /// **loud** TestAgent failure (convention 11).
        case refused(String)
    }

    /// What the shared reader answers when there is no store to read; reproduced
    /// for the one case that never reaches Rust — no connected client.
    public static let notFoundJSON = #"{"found":false}"#

    public static func apply(api: APIClient?) async -> Outcome {
        guard let api, let json = await api.escrowWrapCoordinatesJson() else {
            return .report(notFoundJSON)
        }
        return .report(json)
    }
}

#endif
