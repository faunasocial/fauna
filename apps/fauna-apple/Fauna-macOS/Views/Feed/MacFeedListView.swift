import SwiftUI
import FaunaKit

struct MacFeedListView: View {
    let vm: FeedVM

    @State private var showBridgeSubscribe = false
    @State private var bridgeName = "bluesky"
    @State private var bridgeFeedUri = ""
    @State private var bridgeFeedName = ""

    // Rendered as an eager `ScrollView { VStack }` rather than a lazy `List`
    // (apple-e2e-automation.md registration rule 6): a macOS `List` is an
    // NSTableView that realizes rows lazily, so a `feed-item`/`bridge-feed-*`
    // row appended to `vm.feeds`/`vm.bridgeFeeds` can go unbuilt indefinitely —
    // no view, no `.onAppear`, no registration (measured on the Events agenda,
    // 2026-07-30; `test_create_feed_body_contains` etc. assert `feed-item`
    // count growth the same way). Cost: rows lose `.listStyle(.sidebar)`
    // styling — the accepted rule-6 production-UI tradeoff every other
    // converted page already pays.
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text(L.feed.list.title)
                            .font(.headline)
                        Spacer()
                        Button(vm.showCreateForm ? L.common.cancel : L.feed.create.title) {
                            vm.showCreateForm.toggle()
                        }
                        .controlSize(.small)
                        .accessibilityIdentifier(Ids.feedCreateFeedButton)
                        .automationActivate(
                            Ids.feedCreateFeedButton,
                            value: { vm.showCreateForm ? "on" : "off" }
                        ) { vm.showCreateForm.toggle() }
                    }

                    if vm.showCreateForm {
                        MacFeedFormView(vm: vm)
                    }

                    // Built-in Trending virtual feed (trending.md § The Trending
                    // feed) — above the user's own feeds, mutually exclusive with
                    // Local/custom selection via `vm.trendingSelected`
                    // (shared-Rust-enforced: `FeedManager::select_feed` clears it,
                    // `select_trending_feed` sets it and clears `selectedFeedId`).
                    Button {
                        Task { await vm.selectTrendingFeed() }
                    } label: {
                        HStack {
                            Text(L.feed.list.trending)
                                .fontWeight(vm.trendingSelected ? .bold : .regular)
                            Spacer()
                        }
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier(Ids.feedTrendingItem)
                    .automationActivate(
                        Ids.feedTrendingItem,
                        value: { vm.trendingSelected ? "on" : "off" }
                    ) { Task { await vm.selectTrendingFeed() } }

                    ForEach(Array(vm.feeds.enumerated()), id: \.element.feedId) { index, feed in
                        HStack {
                            Button {
                                Task { await vm.selectFeed(feed.feedId) }
                            } label: {
                                HStack {
                                    Text(feed.name)
                                        .fontWeight(vm.selectedFeedId == feed.feedId ? .bold : .regular)
                                    Spacer()
                                }
                            }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier(Ids.feedItem)
                            .automationActivate(Ids.feedItem, value: { feed.name }) {
                                Task { await vm.selectFeed(feed.feedId) }
                            }
                            .contextMenu {
                                Button(L.common.delete, role: .destructive) {
                                    Task { await vm.deleteFeed(id: feed.feedId) }
                                }
                            }

                            Button {
                                Task { await vm.deleteFeed(id: feed.feedId) }
                            } label: {
                                Image(systemName: "xmark.circle")
                                    .foregroundStyle(.secondary)
                            }
                            .buttonStyle(.borderless)
                            .accessibilityIdentifier(Ids.feedDeleteButton)
                            .automationActivate(Ids.feedDeleteButton) {
                                Task { await vm.deleteFeed(id: feed.feedId) }
                            }
                        }
                        // Scoped container (rule 5): `feed-delete-button` is driven
                        // by `scope="feed-item[N]"`, and the flat occurrence-index
                        // heuristic can only resolve N=0 for the 1-per-row
                        // `feed-delete-button` id once a row is deleted out of
                        // order — a real subtree path makes the scoped query
                        // containment-based instead (device-card/admin-dns-domain
                        // precedent).
                        .automationScope(Ids.feedItem, index: index)
                    }
                }

                // `|| !searchText.isEmpty` keeps the search field (and its clear
                // button) reachable while a search is active even if the selection
                // falls back to Local (`selectedFeedId == nil`), so the user can
                // always clear their own query — the same reachable-during-active-
                // search invariant iOS's FeedListView guarantees (priority #1;
                // macOS normally rides a selected feed, so this is a defensive
                // no-op in the common path).
                if vm.selectedFeedId != nil || !vm.searchText.isEmpty {
                    Divider()
                    HStack {
                        Image(systemName: "magnifyingglass")
                            .foregroundStyle(.secondary)
                        TextField(L.feed.post.searchPlaceholder, text: Binding(
                            get: { vm.searchText },
                            set: { vm.onSearchTextChanged($0) }
                        ))
                        .textFieldStyle(.plain)
                        .accessibilityIdentifier(Ids.feedSearchField)
                        .automationField(Ids.feedSearchField, text: Binding(
                            get: { vm.searchText },
                            set: { vm.onSearchTextChanged($0) }
                        ))

                        if !vm.searchText.isEmpty {
                            Button {
                                vm.clearSearch()
                            } label: {
                                Image(systemName: "xmark.circle.fill")
                                    .foregroundStyle(.secondary)
                            }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier(Ids.feedSearchClear)
                            .automationActivate(Ids.feedSearchClear) { vm.clearSearch() }
                        }
                    }
                }

                Divider()

                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text(L.feed.list.bridgeFeeds)
                            .font(.headline)
                        Spacer()
                        // Subscribe affordance only when the nest supports ≥1 bridge
                        // (snapshot.available_bridges non-empty) — Dim 3 capability
                        // consumption: never offer a protocol the nest can't serve
                        // (version-compatibility.md § Dim 3). One toggle, matching
                        // iOS/web/linux.
                        if !vm.availableBridges.isEmpty {
                            Button(showBridgeSubscribe ? L.common.cancel : L.feed.list.subscribeBridge) {
                                showBridgeSubscribe.toggle()
                            }
                            .controlSize(.small)
                            .accessibilityIdentifier(Ids.bridgeFeedSubscribeToggle)
                            .automationActivate(
                                Ids.bridgeFeedSubscribeToggle,
                                value: { showBridgeSubscribe ? "on" : "off" }
                            ) { showBridgeSubscribe.toggle() }
                        }
                    }

                    if showBridgeSubscribe {
                        VStack(spacing: 8) {
                            // Populated from the nest's available bridges (id+name),
                            // not a hard-coded protocol list.
                            Picker(L.feed.bridgeForm.kind, selection: $bridgeName) {
                                ForEach(vm.availableBridges, id: \.id) { b in
                                    Text(b.name).tag(b.id)
                                }
                            }
                            .pickerStyle(.menu)
                            .accessibilityIdentifier(Ids.bridgeFormBridgeSelect)
                            .automationSelect(
                                Ids.bridgeFormBridgeSelect,
                                value: { bridgeName },
                                set: { bridgeName = $0 }
                            )

                            TextField(L.feed.bridgeForm.uri, text: $bridgeFeedUri)
                                .textFieldStyle(.roundedBorder)
                                .accessibilityIdentifier(Ids.bridgeFormUriInput)
                                .automationField(Ids.bridgeFormUriInput, text: $bridgeFeedUri)
                            TextField(L.feed.bridgeForm.name, text: $bridgeFeedName)
                                .textFieldStyle(.roundedBorder)
                                .accessibilityIdentifier(Ids.bridgeFormNameInput)
                                .automationField(Ids.bridgeFormNameInput, text: $bridgeFeedName)
                            HStack {
                                Button(L.feed.list.subscribeBridge) {
                                    subscribeBridgeAction()
                                }
                                .disabled(bridgeFeedUri.isEmpty || bridgeFeedName.isEmpty)
                                .buttonStyle(.borderedProminent)
                                .controlSize(.small)
                                .accessibilityIdentifier(Ids.bridgeFormSubscribeButton)
                                .automationActivate(
                                    Ids.bridgeFormSubscribeButton,
                                    isEnabled: { !(bridgeFeedUri.isEmpty || bridgeFeedName.isEmpty) }
                                ) { subscribeBridgeAction() }
                                Button(L.common.cancel) {
                                    showBridgeSubscribe = false
                                }
                                .controlSize(.small)
                                .accessibilityIdentifier(Ids.bridgeFormCancelButton)
                                .automationActivate(Ids.bridgeFormCancelButton) {
                                    showBridgeSubscribe = false
                                }
                            }
                        }
                        .padding(.vertical, 4)
                    }

                    ForEach(vm.bridgeFeeds) { sub in
                        HStack {
                            VStack(alignment: .leading) {
                                Text(sub.name).font(.caption)
                                Text(sub.feedUri).font(.caption2).foregroundStyle(.secondary)
                            }
                            Spacer()
                            Button(L.feed.list.unsubscribe) {
                                Task { await vm.unsubscribeBridge(id: sub.id) }
                            }
                            .controlSize(.mini)
                            .foregroundStyle(.red)
                            .accessibilityIdentifier(Ids.bridgeFeedUnsubscribeButton)
                            .automationActivate(Ids.bridgeFeedUnsubscribeButton, value: { sub.name }) {
                                Task { await vm.unsubscribeBridge(id: sub.id) }
                            }
                        }
                    }
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .frame(minWidth: 250)
        .accessibilityIdentifier(Ids.feedView)
        .automationValue(Ids.feedView, text: { "" })
        // Keep the selector default valid: if the current pick isn't one of the
        // nest's available bridges, fall back to the first available one
        // (mirrors web's reconciliation), so a subscribe never sends a protocol
        // the nest can't serve.
        .onChange(of: vm.availableBridges) {
            if !vm.availableBridges.contains(where: { $0.id == bridgeName }),
               let first = vm.availableBridges.first {
                bridgeName = first.id
            }
        }
    }

    /// The bridge-subscribe button's action, factored out so the automation
    /// sibling drives the exact same code path the Button does.
    private func subscribeBridgeAction() {
        Task {
            await vm.subscribeBridge(bridge: bridgeName, feedUri: bridgeFeedUri, name: bridgeFeedName)
            bridgeFeedUri = ""
            bridgeFeedName = ""
            showBridgeSubscribe = false
        }
    }
}
