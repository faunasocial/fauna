import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it.
#if DEBUG

/// `launch_refresh_token` (`fauna_e2e_agent::LAUNCH_REFRESH_TOKEN`) — force the
/// held session bearer's refresh NOW, awaited so the ack lands after the
/// outcome: the wrong-clock refresh witness's ceremony leg (launch-routing smoke
/// case M, `login.md` § E2E test login). The UniFFI apps' held bearer is
/// `FfiNestClient`'s, not the launch machine's, so this drives
/// `refreshHeldBearerForTest` — clear the cached bearer, re-mint over the silent
/// challenge — and the paired `launch_token` state key
/// (`AppStateObservables`) reads the re-anchored deadline back.
///
/// Lives in FaunaKit so macOS + iOS share ONE implementation (priority #2).
/// Returns the refusal reason, or `nil` when the refresh ran — the shell turns a
/// reason into `testAgentFailure` (convention 11: never silently dropped).
public enum LaunchRefreshTokenTestCommand {
    @MainActor
    public static func apply(client: FaunaClient?) async -> String? {
        guard let api = client?.api else {
            return "launch_refresh_token: no live authenticated client, so no held bearer to refresh"
        }
        do {
            if try await api.refreshHeldBearerForTest() { return nil }
            return "launch_refresh_token: no live nest connection, so no held bearer to refresh"
        } catch {
            return "launch_refresh_token: the re-mint failed: \(error)"
        }
    }
}

#endif
