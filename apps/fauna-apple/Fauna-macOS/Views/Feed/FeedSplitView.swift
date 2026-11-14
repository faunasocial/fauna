import SwiftUI
import FaunaKit

struct FeedSplitView: View {
    @Environment(MacAppState.self) private var appState
    @Environment(FaunaClient.self) private var client: FaunaClient?
    // Shared app-level `FeedVM` (injected by `FaunaMacApp`) so the TestAgent's
    // `feed_inject_posts` seeds the SAME manager this view renders off (mirrors
    // `conversationsVM`). Was a view-local `@State` — which the handler couldn't
    // reach, so injected posts never reached the rendered cards.
    @Environment(FeedVM.self) private var vm

    var body: some View {
        NavigationSplitView {
            MacFeedListView(vm: vm)
        } detail: {
            // Always mount the detail pane — `MacFeedDetailView` already handles
            // every state internally (an unconditional composer, then
            // isLoading/posts.isEmpty/post-list). A feed-selection gate here was
            // redundant with that internal handling and had one unwanted side
            // effect: on a brand-new author's nest (zero feeds, `fauna.feed.list`
            // starts genuinely empty — priority #1 divergence from linux/web,
            // neither of which gates composing on feed existence), the composer
            // never mounted at all. The cross-app `feed_inject_posts` test seam
            // (which seeds `posts` with `selected_feed: None`, libs/fauna-feed
            // test_support) still renders fine since `MacFeedDetailView` reads
            // `vm.posts` directly, not `vm.selectedFeedId`.
            MacFeedDetailView(vm: vm)
                .accessibilityElement(children: .contain)
        }
        // The reader's post spam marks reach the manager (report-sharing.md
        // § Report capture → The post's mark-as-spam verb).
        .feedSpamMarkSync(vm: vm)
        // task(id:) keys on the ACTOR (the session secret), so it re-fires not
        // only on nil → connected (login) and on every fresh mount (`.id(selectedSidebar)`
        // in ContentView forces one on each Feed-tab re-entry), but ALSO when the
        // actor CHANGES — a re-login to a different account. `applySessionPatch`
        // swaps in a fresh `FaunaClient` on a re-login while `client != nil` stays
        // true, so keying on `client != nil` alone would never reconfigure the feed
        // for the new actor, and `FeedVM` would keep the prior actor's snapshot
        // (cross-actor leak — see `FeedVM.configure`). `applySessionPatch` sets the
        // secret and the client together, so the guard below still has both.
        .task(id: appState.session.secretHex) {
            guard let client, let secret = appState.session.secretHex else { return }
            await vm.configure(api: client.api, secretHex: secret, deviceIdHex: client.deviceId)
            // Re-pull the CURRENT selection (Local if none) every time this page
            // becomes visible — the feed has no poll backstop, so a nav back must
            // re-fetch (mirrors linux's `connect_map` re-pull on visibility).
            // `rehydrate` is `loadFeeds` + the shared `refreshCurrentFeed` seam,
            // the iOS Feed tab's own entry path (priority #1): it re-runs whatever
            // is selected — Local, Trending, a custom feed or the imported-posts
            // selection — without changing it, and reloads the sealed scorers so
            // a mute set elsewhere in Settings applies on return. Never re-select
            // by hand here: `select_feed(nil)` clears `imported_selected`, so the
            // archive import's View imported landed on plain Local (`feed.md`
            // § State & data shape → *The imported-posts selection*), and
            // `select_feed(current)` drops a viewer off Trending.
            await vm.rehydrate()
        }
        // Re-pull the feed on WS-RPC reconnect (no poll backstop) so posts that
        // arrived while disconnected surface without a manual refresh.
        .onReconnect { await vm.rehydrate() }
    }
}
