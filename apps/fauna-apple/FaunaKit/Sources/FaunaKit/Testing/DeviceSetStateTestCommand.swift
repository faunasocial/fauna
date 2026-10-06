import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it. Also required
// for correctness: it drives `APIClient.deviceSetStateJson`, itself `#if DEBUG`,
// over a UniFFI export (`FfiNestClient.deviceSetStateJson`) that only the
// `test-helpers` flavor carries.
#if DEBUG

/// Shared handler for the cross-app `device_set_state` TestAgent command — the
/// e2e reader for a fleet-scope removal (`account-data-taxonomy.md` § The
/// generation machinery → *Fleet-scope reclamation*, clause (4)): whether
/// `device_id_hex`'s plane `fauna.state.device-set` row reads Removed/Enrolled
/// from THIS app's own account runtime. The apple twin of tui's arm
/// (`apps/fauna-tui/src/automation.rs`), linux's and android's; the
/// `test_crash_recovery_journeys.py` kill-between-the-legs journey reads it.
///
/// Answers a JSON object on `state.machine_method_result` — the slot
/// `HttpBridgeDriver.call_command` reads back, which is also what
/// `_require_device_set_reader` probes: **any non-`nil` answer means the reader is
/// built**, so a build that answered `nil` here would skip the journey as
/// unbuilt. Hence the not-found fallback below is a *report*, never a refusal.
///
/// Lives in FaunaKit so the macOS + iOS shells share ONE implementation,
/// mirroring `CustodianPullTestCommand`; the shells' switch arms are the same
/// few lines and the slot-clearing rule lives in the arm.
public enum DeviceSetStateTestCommand {
    /// What the shell does with the command's result — the same two-case shape as
    /// `CustodianPullTestCommand.Outcome`, so both shells' arms read alike.
    public enum Outcome: Equatable {
        /// The reader's JSON, for the shell to stash in `machineMethodResultBox`.
        case report(String)
        /// The command cannot be honoured; the shell must surface the reason as a
        /// **loud** TestAgent failure (convention 11) and leave the result slot
        /// empty — never a stale neighbour's answer.
        case refused(String)
    }

    /// What the shared reader (`fauna_client_account_runtime::device_set_state_json`)
    /// answers when there is no store to read. Reproduced here for the one case
    /// that never reaches Rust — no connected client, so `APIClient` has no
    /// `FfiNestClient` to ask — which tui/linux/android likewise answer as a quiet
    /// "not found" rather than a refusal.
    public static let notFoundJSON = #"{"found":false}"#

    public static func apply(_ command: [String: Any], api: APIClient?) async -> Outcome {
        // Convention 11's bad-payload clause. A missing id must not fold into
        // `notFoundJSON`: a journey asserting "the row is NOT there" would then pass
        // on a typo'd key, which is a vacuous green rather than a failure.
        guard let deviceIdHex = command["device_id_hex"] as? String else {
            return .refused("device_set_state: needs a `device_id_hex` string")
        }
        guard let api, let json = await api.deviceSetStateJson(deviceIdHex: deviceIdHex) else {
            return .report(notFoundJSON)
        }
        return .report(json)
    }
}

#endif
