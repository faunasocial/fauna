import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it.
#if DEBUG

/// `feed_inject_error` — drive the feed page's `error-message` directly, the
/// feed twin of `conversations_inject_send_failure`. There is no real path
/// to fail a feed fetch on demand, so this reaches
/// `FeedManager::inject_error_for_test` via the shared `FfiFeedManager` — pins that a page error actually publishes
/// into `AppMessages` through `ErrorBanner`, rather than only painting on
/// screen. Payload: `{"key": str, "message": str}`.
///
/// Lives in FaunaKit so macOS + iOS share ONE implementation (priority #2) —
/// was a byte-identical per-target twin until this harvest pass found it
/// . Takes `client`/`session`
/// directly rather than `AppState`/`MacAppState` (no common protocol between
/// them) — the same shape `ConversationsSendTestCommand` takes `vm:
/// ConversationsVM` instead of the app's whole state.
public enum FeedInjectErrorTestCommand {
    @MainActor
    public static func apply(_ command: [String: Any], feedVM: FeedVM, client: FaunaClient?, session: SessionState) {
        let key = (command["key"] as? String) ?? "feed.error_load"
        let message = (command["message"] as? String) ?? ""
        guard let api = client?.api, let secret = session.secretHex else {
            logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] feed_inject_error: no client or secret")
            return
        }
        Task { @MainActor in
            await feedVM.configure(api: api, secretHex: secret)
            guard let mgr = feedVM.manager else {
                logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] feed_inject_error: feedVM manager unavailable")
                return
            }
            mgr.injectErrorForTest(key: key, message: message)
        }
    }
}

#endif
