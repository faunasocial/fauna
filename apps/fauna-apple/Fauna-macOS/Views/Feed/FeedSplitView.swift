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
            await vm.configure(api: client.api, secretHex: secret)
            await vm.loadFeeds()
            // Re-pull the CURRENT selection (or auto-select the first feed if
            // none) every time this page becomes visible — the feed has no poll
            // backstop, so a nav back must re-fetch (mirrors linux's
            // `connect_map` re-pull on visibility). Un-gating this from a
            // `selectedFeedId == nil` check is what lets a mute set elsewhere in
            // Settings reach the sealed scorers on return: `vm` is the
            // persistent app-root FeedVM, so `selectedFeedId` survives across
            // this view's remounts and the old nil-only guard skipped every
            // re-entry after the first. `trendingSelected` is checked FIRST —
            // selecting Trending also clears `selectedFeedId` (shared Rust), so
            // without this branch a remount would silently kick the user off
            // Trending back to their first custom feed (same class of bug
            // `rehydrate()` already fixes for reconnect).
            if vm.trendingSelected {
                await vm.selectTrendingFeed()
            } else if let current = vm.selectedFeedId {
                await vm.selectFeed(current)
            } else if let first = vm.feeds.first {
                await vm.selectFeed(first.feedId)
            } else {
                // No custom feed and not on Trending → load the nest's Local
                // timeline (mirrors linux/web's `select_feed(None)`). Without this
                // a fresh nest with no saved feeds shows an empty feed, so a post a
                // just-logged-in actor is entitled to (e.g. a subscriber's gated
                // post) never appears until they build a custom feed.
                await vm.selectLocalFeed()
            }
        }
        // Re-pull the feed on WS-RPC reconnect (no poll backstop) so posts that
        // arrived while disconnected surface without a manual refresh.
        .onReconnect { await vm.rehydrate() }
    }
}
