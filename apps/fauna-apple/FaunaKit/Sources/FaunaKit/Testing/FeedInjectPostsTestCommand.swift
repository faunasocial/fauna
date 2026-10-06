import Foundation

// Compiled out of release artifacts (testing.md convention 15), like the app
// shells' whole `handleTestCommand` surface that calls into it.
#if DEBUG

/// `feed_inject_posts` — seed the in-process feed snapshot so the cross-app
/// `test_feed_unverified_source.py` (and any feed-render test) drives the feed
/// without a live nest post stream. The apple peer of web's
/// `WasmFeedManager.injectPostsForTest` / Linux's `handle_feed_inject_posts`:
/// hand the `posts` spec list (the shared `fauna_feed::test_support::TestPostSpec`
/// shape `{post_id, author, body?, verification?, quoted?, …}`, order preserved →
/// `posts[i]` ⇒ `post-card[i]`) to the shared `FfiFeedManager.injectPostsForTest`
/// (→ `set_feed_snapshot_for_test`), then mirror the snapshot into
/// `FeedVM.lastLoadedPosts` — the state the in-process feed read serializes from
/// (the lazy List doesn't register cards in-process). `injectPostsForTest` is a
/// `test-helpers` FFI export (always built in the `apple-ffi` recipes).
///
/// Lives in FaunaKit so macOS + iOS share ONE implementation (priority #2) —
/// was a byte-identical per-target twin (differing only in doc-comment prose,
/// which is why an exact-body-diff pass over raw text had filed it as
/// genuinely divergent) until this harvest pass normalized away comments
/// before comparing . Takes
/// `client`/`session` directly rather than `AppState`/`MacAppState` (no
/// common protocol between them) — the same shape `FeedInjectErrorTestCommand`
/// takes instead of the app's whole state.
public enum FeedInjectPostsTestCommand {
    @MainActor
    public static func apply(_ command: [String: Any], feedVM: FeedVM, client: FaunaClient?, session: SessionState) {
        guard let posts = command["posts"],
              let data = try? JSONSerialization.data(withJSONObject: posts),
              let specsJson = String(data: data, encoding: .utf8) else {
            logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] feed_inject_posts: missing/unserializable posts")
            return
        }
        guard let api = client?.api, let secret = session.secretHex else {
            logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] feed_inject_posts: no client or secret")
            return
        }
        Task { @MainActor in
            // Inject into the SHARED app-level `feedVM` — the SAME manager the
            // page view renders off (it reads `feedVM` from the environment).
            // `configure` is idempotent: reuses the view's manager if built, else
            // builds it now. A fresh `api.feedManager(secret:)` here would seed a
            // throwaway the view never observes.
            await feedVM.configure(api: api, secretHex: secret)
            guard let mgr = feedVM.manager else {
                logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] feed_inject_posts: feedVM manager unavailable")
                return
            }
            mgr.injectPostsForTest(specsJson: specsJson)
            logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] feed_inject_posts: injected (\(mgr.snapshot().posts.count) posts in feed)")
        }
    }
}

#endif
