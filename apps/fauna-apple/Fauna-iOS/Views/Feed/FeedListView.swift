import SwiftUI
import FaunaKit

struct FeedListView: View {
    @Environment(AppState.self) private var appState
    @Environment(FaunaClient.self) private var client: FaunaClient?
    // Shared app-level `FeedVM` (injected by `FaunaApp`) so the TestAgent's
    // `feed_inject_posts` seeds the SAME manager this view renders off (mirrors
    // `conversationsVM`). Was a view-local `@State` — which the handler couldn't
    // reach, so injected posts never reached the rendered cards.
    @Environment(FeedVM.self) private var vm

    // Bridge subscribe form (view-local inputs; submit goes through the manager)
    @State private var showBridgeSubscribe = false
    @State private var bridgeFeedUri = ""
    @State private var bridgeFeedName = ""
    @State private var bridgeFormBridge = "bluesky"

    // Rich compose dialog
    @State private var showComposeDialog = false

    // Push to the create-feed form. A tap-driven `NavigationLink` doesn't
    // perform its push when actuated via the in-process automation registry
    // (the same class as the iOS Events GAP A `event-card` fix) — a
    // `Button` + `.navigationDestination(isPresented:)` does.
    @State private var pushCreateFeed = false

    // Push to a post's detail. Same story as `pushCreateFeed`: a tap-driven
    // `NavigationLink` label doesn't push when actuated via the automation
    // registry, so `post-card` is a `Button` that sets this + a
    // `.navigationDestination(item:)` below (mirrors macOS `MacFeedDetailView`'s
    // `selectedPost` sheet and the Events GAP A `event-card` fix).
    @State private var selectedPost: PostSummary?

    var body: some View {
        let searchBinding = Binding(
            get: { vm.searchText },
            set: { vm.onSearchTextChanged($0) }
        )
        // Manager-backed compose fields (the FfiFeedManager lift moved compose
        // state off local @State). One binding each, shared by the TextField and
        // its `automationField`, so a typed value reaches the manager exactly as a
        // keystroke would.
        let composeBinding = Binding(
            get: { vm.composeText },
            set: { vm.setComposeText($0) }
        )
        let tagsBinding = Binding(
            get: { vm.composeTags },
            set: { vm.setComposeTags($0) }
        )
        // Gate-to-tier bindings (mirror composeBinding/tagsBinding). The select
        // value is the tier name, a room's "Room: ‹label›" string, "Sell this
        // post…", or "Public".
        let gateBinding = Binding(
            get: { vm.composeGateSelection },
            set: { vm.setComposeGateSelection($0) }
        )
        // The teaser is staged ALONE (`FeedVM.setComposeGatePreview` →
        // `update_compose_preview`) — routing it through whichever answer's
        // own setter would re-read that mode and could flip it (a staged room
        // answer would be dropped by `update_compose_gate`'s unconditional
        // `gate_room = None`; `ui/feed.md` § Encryption at rest → *The
        // composer's fourth answer*). Mirrors macOS `MacFeedDetailView`.
        let gatePreviewBinding = Binding(
            get: { vm.composeGatePreview },
            set: { vm.setComposeGatePreview($0) }
        )
        // Sell-this-post bindings (mirror gateBinding/gatePreviewBinding).
        let sellPriceBinding = Binding(
            get: { vm.composeSellPrice },
            set: { vm.setComposeSell(price: $0, subscribersGetItFree: vm.composeSellSubscribersFree) }
        )
        // The machine-comparable threshold (monetization.md § The asking
        // price) — independent of sellPriceBinding above, never inferred
        // from it.
        let sellAskingPriceBinding = Binding(
            get: { vm.composeSellAskingPrice },
            set: { vm.setComposeSell(price: vm.composeSellPrice,
                                      subscribersGetItFree: vm.composeSellSubscribersFree,
                                      askingPrice: $0) }
        )
        return NavigationStack {
            ScrollView {
                // Eager `ScrollView { VStack }`, NOT a lazy `List { Section }` (rule 6 —
                // apple-e2e-automation.md § Registration rules): an iOS `List` lazily
                // realizes AND POOLS its rows, so a deleted feed (`feed-delete-button`),
                // an unsubscribed bridge feed (`bridge-feed-unsubscribe-button`), or a
                // post filtered out by `feed-search-field` (`test_feed_search.py`, which
                // checks the `post-card` count narrows) can linger on-screen past its
                // real removal — the same delete-zombie class rule 6 fixed for iOS
                // Events. Cost: rows lose `.insetGrouped` inset styling (accepted rule-6
                // production-UI tradeoff).
                VStack(alignment: .leading, spacing: 16) {
                // Feed picker
                VStack(alignment: .leading, spacing: 8) {
                    Text(L.feed.list.title)
                        .font(.headline)
                    // Always visible, matching macOS's `feed-create-feed-button`
                    // (a persistent trigger, not gated behind `showCreateForm` —
                    // that flag has no ios visibility role; only the toolbar
                    // `compose-button`'s unrelated ComposerView sheet uses it).
                    Button(L.feed.create.title) {
                        pushCreateFeed = true
                    }
                    .accessibilityIdentifier(Ids.feedCreateFeedButton)
                    .automationActivate(Ids.feedCreateFeedButton) {
                        pushCreateFeed = true
                    }

                    // Built-in Trending virtual feed (trending.md § The
                    // Trending feed) — above the user's own feeds, mutually
                    // exclusive with Local/custom selection via
                    // `vm.trendingSelected` (shared-Rust-enforced). Mirrors
                    // macOS's MacFeedListView row.
                    Button {
                        Task { await vm.selectTrendingFeed() }
                    } label: {
                        HStack {
                            Text(L.feed.list.trending)
                                .fontWeight(vm.trendingSelected ? .bold : .regular)
                            Spacer()
                        }
                    }
                    .tint(.primary)
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
                            .tint(.primary)
                            .accessibilityIdentifier(Ids.feedItem)
                            .automationActivate(Ids.feedItem, value: { feed.name }) {
                                Task { await vm.selectFeed(feed.id) }
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
                                Task { await vm.deleteFeed(id: feed.id) }
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

                Divider()

                // Bridge Feeds
                VStack(alignment: .leading, spacing: 8) {
                    HStack {
                        Text(L.feed.list.bridgeFeeds)
                        Spacer()
                        // Subscribe affordance only when the nest supports ≥1
                        // bridge (snapshot.available_bridges non-empty) — Dim 3
                        // capability consumption: never offer a protocol the
                        // nest can't serve (version-compatibility.md § Dim 3).
                        if !vm.availableBridges.isEmpty {
                            Button(showBridgeSubscribe ? L.common.cancel : L.feed.list.subscribeBridge) {
                                showBridgeSubscribe.toggle()
                            }
                            .font(.caption)
                            .accessibilityIdentifier(Ids.bridgeFeedSubscribeToggle)
                            .automationActivate(
                                Ids.bridgeFeedSubscribeToggle,
                                value: { showBridgeSubscribe ? "on" : "off" }
                            ) { showBridgeSubscribe.toggle() }
                        }
                    }
                    if showBridgeSubscribe {
                        VStack(spacing: 8) {
                            Picker(L.feed.bridgeForm.kind, selection: $bridgeFormBridge) {
                                ForEach(vm.availableBridges, id: \.id) { b in
                                    Text(b.name).tag(b.id)
                                }
                            }
                            .accessibilityIdentifier(Ids.bridgeFormBridgeSelect)
                            .automationSelect(
                                Ids.bridgeFormBridgeSelect,
                                value: { bridgeFormBridge },
                                set: { bridgeFormBridge = $0 }
                            )
                            TextField(L.feed.bridgeForm.uri, text: $bridgeFeedUri)
                                .textInputAutocapitalization(.never)
                                .autocorrectionDisabled()
                                .accessibilityIdentifier(Ids.bridgeFormUriInput)
                                .automationField(Ids.bridgeFormUriInput, text: $bridgeFeedUri)
                            TextField(L.feed.bridgeForm.name, text: $bridgeFeedName)
                                .accessibilityIdentifier(Ids.bridgeFormNameInput)
                                .automationField(Ids.bridgeFormNameInput, text: $bridgeFeedName)
                            HStack {
                                Button(L.feed.list.subscribeBridge) {
                                    subscribeBridgeAction()
                                }
                                .buttonStyle(.borderedProminent)
                                .controlSize(.small)
                                .disabled(bridgeFeedUri.isEmpty || bridgeFeedName.isEmpty)
                                .accessibilityIdentifier(Ids.bridgeFormSubscribeButton)
                                .automationActivate(
                                    Ids.bridgeFormSubscribeButton,
                                    isEnabled: { !(bridgeFeedUri.isEmpty || bridgeFeedName.isEmpty) }
                                ) { subscribeBridgeAction() }
                                Button(L.common.cancel) { showBridgeSubscribe = false }
                                    .controlSize(.small)
                                    .accessibilityIdentifier(Ids.bridgeFormCancelButton)
                                    .automationActivate(Ids.bridgeFormCancelButton) {
                                        showBridgeSubscribe = false
                                    }
                            }
                        }
                    }

                    ForEach(vm.bridgeFeeds) { sub in
                        HStack {
                            VStack(alignment: .leading) {
                                Text(sub.name).font(.subheadline)
                                Text(sub.feedUri)
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                            Button {
                                Task { await vm.unsubscribeBridge(id: sub.id) }
                            } label: {
                                Image(systemName: "xmark.circle")
                                    .foregroundStyle(.secondary)
                            }
                            .buttonStyle(.borderless)
                            .accessibilityIdentifier(Ids.bridgeFeedUnsubscribeButton)
                            .automationActivate(Ids.bridgeFeedUnsubscribeButton, value: { sub.name }) {
                                Task { await vm.unsubscribeBridge(id: sub.id) }
                            }
                        }
                        // bridge-feed-item retired 2026-06-28 (Feed-only decision,
                        // bridges.md § Layout) — the per-item unsubscribe button keeps
                        // its own id; no container shim id.
                    }
                }
                // bridge-feed-subscriptions retired 2026-06-28 (Feed-only decision,
                // bridges.md § Layout) — the subscribe form (bridge-form-*) +
                // bridge-feed-subscribe-toggle + bridge-feed-unsubscribe-button remain.

                Divider()

                // Search bar — shown whenever posts are shown (same gate as the
                // posts section below), NOT only when a custom feed is selected:
                // search is a nest re-query that works on the LOCAL feed too
                // (`query_local_feed_core` applies `search`; pinned by
                // tests/api/test_feed_search_api.py), and every other app
                // offers search on the default view (macOS auto-selects the
                // first feed; windows/linux/web always render the box). The old
                // `selectedFeedId != nil` gate made `feed-search-field`
                // unreachable on a fresh iOS login — the test_feed_search 404s.
                // `|| !searchText.isEmpty` completes that fix for the ZERO-RESULT
                // case: on the Local feed (`selectedFeedId == nil`) a search that
                // matches nothing empties `posts`, so a `posts`-only gate would
                // yank the search field (and its clear button) out from under the
                // user mid-search, stranding them unable to clear their own query
                // (`test_feed_search_clear_restores_feed[ios]`). An active search
                // buffer keeps the bar reachable until the user clears it.
                if vm.selectedFeedId != nil || !vm.posts.isEmpty || !vm.searchText.isEmpty {
                    Group {
                        HStack {
                            Image(systemName: "magnifyingglass")
                                .foregroundStyle(.secondary)
                            TextField(L.feed.post.searchPlaceholder, text: searchBinding)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .accessibilityIdentifier(Ids.feedSearchField)
                            .automationField(Ids.feedSearchField, text: searchBinding)

                            if !vm.searchText.isEmpty {
                                Button {
                                    vm.clearSearch()
                                } label: {
                                    Image(systemName: "xmark.circle.fill")
                                        .foregroundStyle(.secondary)
                                }
                                .accessibilityIdentifier(Ids.feedSearchClear)
                                .automationActivate(Ids.feedSearchClear) { vm.clearSearch() }
                            }
                        }
                    }
                }

                Divider()
                VStack(alignment: .leading, spacing: 8) {
                    Text(L.common.posts)
                        .font(.headline)
                    // Compose (text/tags live in the manager — compose-error from
                    // snapshot). Always mounted, NOT gated on a feed selection — a
                    // brand-new author's nest has zero feeds (`fauna.feed.list`
                    // starts genuinely empty) and must still be able to compose a
                    // first post, matching linux/web (neither gates composing on
                    // feed existence — priority #1; identical fix landed on macOS's
                    // `MacFeedDetailView` the same session).
                    VStack(spacing: 8) {
                            HStack {
                                TextField(L.feed.post.whatsOnYourMind, text: composeBinding)
                                .accessibilityIdentifier(Ids.composeTextField)
                                .automationField(Ids.composeTextField, text: composeBinding)
                                Button {
                                    Task { await vm.submitPost() }
                                } label: {
                                    Image(systemName: "arrow.up.circle.fill")
                                }
                                .accessibilityIdentifier(Ids.postSubmitButton)
                                .disabled(
                                    vm.composeText.trimmingCharacters(in: .whitespaces).isEmpty
                                    || !vm.composeReady
                                )
                                .automationActivate(
                                    Ids.postSubmitButton,
                                    isEnabled: {
                                        !vm.composeText.trimmingCharacters(in: .whitespaces).isEmpty
                                        && vm.composeReady
                                    }
                                ) { Task { await vm.submitPost() } }
                            }
                            TextField(L.feed.post.tagsPlaceholder, text: tagsBinding)
                            .font(.caption)
                            .accessibilityIdentifier(Ids.composeTagsField)
                            .automationField(Ids.composeTagsField, text: tagsBinding)

                            // Gate-to-tier controls (`compose-gate-tier-select` /
                            // `compose-gate-preview-field`; feed.md § Encryption at
                            // rest, monetization.md § Pillars 2+3). "Public" (ungated
                            // default) + the author's own tiers from `own_tiers` +
                            // one option per room from `own_rooms` ("Room: ‹label›")
                            // + "Sell this post…" always last; picking any of them
                            // reveals the public-teaser field. Mirrors macOS
                            // `MacFeedDetailView` + linux `post_list.rs`.
                            Picker(L.feed.post.gateAudience, selection: gateBinding) {
                                Text(L.feed.post.gatePublic).tag(L.feed.post.gatePublic)
                                ForEach(vm.ownTiers, id: \.name) { tier in
                                    Text(tier.name).tag(tier.name)
                                }
                                ForEach(vm.ownRooms, id: \.room) { room in
                                    Text(L.feed.post.gateRoom(room: room.label)).tag(L.feed.post.gateRoom(room: room.label))
                                }
                                Text(L.feed.post.gateSell).tag(L.feed.post.gateSell)
                            }
                            .pickerStyle(.menu)
                            .accessibilityIdentifier(Ids.composeGateTierSelect)
                            .automationSelect(
                                Ids.composeGateTierSelect,
                                value: { vm.composeGateSelection },
                                options: { vm.gateOptions }
                            ) { newValue in
                                vm.setComposeGateSelection(newValue)
                            }
                            if vm.composeGateTier != nil || vm.composeGateRoom != nil || vm.composeSell != nil {
                                TextField(L.feed.post.gatePreviewPlaceholder, text: gatePreviewBinding)
                                .font(.caption)
                                .accessibilityIdentifier(Ids.composeGatePreviewField)
                                .automationField(Ids.composeGatePreviewField, text: gatePreviewBinding)
                            }
                            // "Sell this post…" controls (monetization.md § Per-post
                            // pay-to-unlock; IDs user-approved 2026-07-29) — visible
                            // only while Sell is the select's current answer.
                            if vm.composeSell != nil {
                                TextField(L.feed.post.sellPricePlaceholder, text: sellPriceBinding)
                                .font(.caption)
                                .accessibilityIdentifier(Ids.composeSellPrice)
                                .automationField(Ids.composeSellPrice, text: sellPriceBinding)
                                #if !FAUNA_EXCISE_PAYMENTS
                                // The money plane's compose-side half, excised
                                // with the tier form's own asking-price input
                                // (ProfileView.swift).
                                TextField(L.feed.post.sellAskingPricePlaceholder, text: sellAskingPriceBinding)
                                .font(.caption)
                                .accessibilityIdentifier(Ids.composeSellAskingPrice)
                                .automationField(Ids.composeSellAskingPrice, text: sellAskingPriceBinding)
                                #endif
                                HStack {
                                    Text(L.feed.post.sellSubscribersFree)
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                    // Defaults CHECKED (user-ratified 2026-07-29): an
                                    // existing paying subscriber is not charged twice
                                    // for a post their subscription would cover.
                                    Toggle("", isOn: Binding(
                                        get: { vm.composeSellSubscribersFree },
                                        set: { vm.setComposeSell(price: vm.composeSellPrice, subscribersGetItFree: $0) }
                                    ))
                                    .labelsHidden()
                                    .accessibilityIdentifier(Ids.composeSellSubscribersFree)
                                    .automationActivate(Ids.composeSellSubscribersFree,
                                                        value: { vm.composeSellSubscribersFree ? "on" : "off" }) {
                                        vm.setComposeSell(price: vm.composeSellPrice,
                                                           subscribersGetItFree: !vm.composeSellSubscribersFree)
                                    }
                                }
                            }

                            if let composeError = vm.composeError {
                                Text(composeError)
                                    .font(.caption)
                                    .foregroundStyle(.red)
                                    .accessibilityIdentifier(Ids.composeError)
                                    // Register the error READ with the in-process
                                    // driver — a bare `.accessibilityIdentifier`
                                    // never enters the AutomationRegistry, so
                                    // without this a real submit failure paints on
                                    // screen yet reads as absent to `is_visible`/
                                    // `error_text` (e2e points 2/6/11 — a failure
                                    // must diagnose itself). macOS's compose-error
                                    // already carries this (MacFeedDetailView).
                                    .automationValue(Ids.composeError, text: { vm.composeError })
                            }

                            ComposeAttachButton(vm: vm)
                            if let attached = vm.composeAttachedFile {
                                HStack(spacing: 4) {
                                    automationText(Ids.composeFileReady,
                                                   "\(attached.name)  \(ValueFormat.byteSize(attached.size))")
                                        .foregroundStyle(.green)
                                    Button {
                                        vm.removeComposeAttachment()
                                    } label: {
                                        Image(systemName: "xmark.circle.fill")
                                    }
                                    .buttonStyle(.plain)
                                    .accessibilityIdentifier(Ids.composeFileRemove)
                                    .automationActivate(Ids.composeFileRemove) { vm.removeComposeAttachment() }
                                }
                                .font(.caption)
                            }
                            Button {
                                showComposeDialog = true
                            } label: {
                                Image(systemName: "doc.richtext")
                            }
                            .buttonStyle(.borderless)
                            .accessibilityIdentifier(Ids.composeDialogButton)
                            .automationActivate(Ids.composeDialogButton) {
                                showComposeDialog = true
                            }
                        }

                        // Posts for selected feed — also render when posts are
                        // present even without a selection: the cross-app
                        // `feed_inject_posts` test seam seeds `posts` with
                        // `selected_feed: None`, and linux/web render post-cards
                        // straight from `posts` (priorities #1/#3). In production
                        // the two coincide (posts only load for a selected feed).
                        // `|| !searchText.isEmpty` so a zero-result search on the
                        // Local feed still enters this block (its spinner, then
                        // `feed-no-results`) instead of collapsing the whole
                        // section — the read-side twin of the search-bar gate
                        // above (test_feed_search_clear_restores_feed[ios]); and
                        // `|| emptyState != nil` so the shared empty-state answer
                        // is painted whenever it is given.
                        if vm.selectedFeedId != nil || !vm.posts.isEmpty || !vm.searchText.isEmpty
                            || vm.emptyState != nil {
                            if vm.showsLoadingSpinner {
                                ProgressView()
                            } else if let emptyState = vm.emptyState {
                                FeedEmptyStateView(state: emptyState)
                            } else {
                                // `enumerated` so each card pushes `post-card[offset]` as
                                // the ancestor scope for its in-process-driver children
                                // (the `quoted-post` embed + `unverified-source-badge`),
                                // giving `post-card[i]/quoted-post` real subtree
                                // resolution. `offset` is the document-order row index the
                                // driver's `post-card[i]` addresses. Mirrors macOS
                                // `MacFeedDetailView`.
                                ForEach(Array(vm.posts.enumerated()), id: \.element.id) { offset, post in
                                    PostCardView(
                                        post: post, vm: vm,
                                        onNavigateToPersonalization: {
                                            // Settings lives under the "more" tab on iOS
                                            // (mirrors `SupervisedIndicatorBar`'s identical
                                            // cross-tab jump), not its own top-level tab.
                                            appState.selectedSettingsPage = .personalization
                                            appState.moreSelectedView = "settings"
                                            appState.selectedTab = "more"
                                        }
                                    )
                                    // A whole-card tap opens detail. `.onTapGesture` (not a
                                    // wrapping `Button`) so the card's own inner buttons
                                    // (load-remote-content, the interaction bar) keep working
                                    // — mirrors macOS `MacFeedDetailView`.
                                    .contentShape(Rectangle())
                                    .onTapGesture { openPostCard(post) }
                                    .accessibilityElement(children: .contain)
                                    .accessibilityIdentifier(Ids.postCard)
                                    // Engagement-cue capture (engagement-cues.md
                                    // § Layer B) — publishes this card's frame so
                                    // `cueCapture` on the ScrollView can measure
                                    // its honest viewport dwell. Keyed on the
                                    // bound post's own id, never `offset`: a
                                    // re-rank reorders rows, and an index-derived
                                    // key would credit one post's dwell to another.
                                    .cueCard(postId: post.postId)
                                    // A tap-driven NavigationLink label doesn't push when
                                    // invoked via the in-process registry, so open detail
                                    // through `selectedPost` + `.navigationDestination(item:)`
                                    // below (see `pushCreateFeed` / the Events GAP A
                                    // `event-card` fix). One Entry (activate + body read via
                                    // `value:`) so `/element/click` and `/element/text`
                                    // resolve to the same slot (AutomationRegistry
                                    // § automationActivate) — not a split `.automationValue`.
                                    .automationActivate(Ids.postCard, value: { post.body }) {
                                        openPostCard(post)
                                    }
                                    .automationScope(Ids.postCard, index: offset)
                                }

                                if vm.hasMore {
                                    Button(L.common.loadMore) {
                                        Task { await vm.loadMore() }
                                    }
                                }
                            }
                        }

                        if let error = vm.errorMessage {
                            // `ErrorBanner`, not a hand-rolled `Text`, so the error
                            // also publishes into `AppMessages.error` (its
                            // `onAppear`) — the funnel `error_text()`/`has_error()`
                            // actually read (e2e-conventions.md convention 2's
                            // rider, shape iii). The prior raw `Text` left the id
                            // in the a11y tree but published nothing, so every feed
                            // error read back as `""` to the harness. Mirrors
                            // macOS's `MacFeedDetailView`.
                            ErrorBanner(message: error)
                        }
                    }
                }
                .padding()
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .accessibilityIdentifier(Ids.feedView)
            .automationValue(Ids.feedView, text: { "" })
            // Engagement-cue capture: reads this ScrollView's frame as the
            // viewport every `cueCard` row is measured against, ticks while the
            // feed is on screen, and drains + seals on disappear.
            .cueCapture(feedVM: vm) { generation, message in
                vm.landClientErrorMessage(generation: generation, message: message)
            }
            .pageTitle(L.common.feed)
            .toolbar {
                // Opens the full ComposerView sheet (ui.yaml `feed-compose-bar`
                // note: "compose-button is a real toggle on platforms with
                // collapsible compose (iOS, ...)"). Previously mis-wired to
                // `vm.showCreateForm` (the unrelated create-feed trigger,
                // which is now always-visible on its own) — no e2e action
                // depends on the old wiring (`_open_composer` never clicks
                // this id on iOS; it selects a feed-item instead).
                Button {
                    showComposeDialog = true
                } label: {
                    Image(systemName: "plus")
                }
                .accessibilityIdentifier(Ids.composeButton)
                .automationActivate(Ids.composeButton) { showComposeDialog = true }
            }
            .refreshable {
                await vm.rehydrate()
            }
            .task {
                await configureAndLoadFeeds()
            }
            // Key on the ACTOR (session secret), not just `client != nil`: a
            // re-login to a different account swaps in a fresh `FaunaClient` while
            // `client != nil` stays true, so keying on nil-ness alone would never
            // reconfigure the feed for the new actor and `FeedVM` would keep the
            // prior actor's snapshot (cross-actor leak — see `FeedVM.configure`).
            .onChange(of: appState.session.secretHex) {
                Task { await configureAndLoadFeeds() }
            }
            // A re-login rebuilds the `FeedManager` (managerGeneration bumps):
            // dismiss any detail the previous actor left pushed, so its fire-once
            // gated-unlock `.task` can't fire against the new actor's feed and
            // leak the full body into the list before the reader opens it. Keyed
            // on managerGeneration, NOT session.secretHex, because the secret can
            // change while this view is off screen (another tab) — where an
            // `.onChange(of: secretHex)` never fires — whereas the manager rebuild
            // runs through `configure` once the feed is on screen at login
            // (`test_gated_post_compose` subscriber leg).
            .onChange(of: vm.managerGeneration) { selectedPost = nil }
            // Re-pull the current selection every time the Feed tab becomes
            // selected again — the feed has no poll backstop, so a nav back
            // (e.g. from muting a term in Settings) must re-fetch to reload the
            // sealed scorers (mirrors linux's `connect_map` re-pull on visibility
            // and macOS `FeedSplitView`'s `.task(id:)`-on-remount re-pull).
            // Driven off `appState.selectedTab`, NOT `.onAppear`: SwiftUI's
            // TabView keeps every tab mounted, so `.onAppear` fires only on the
            // first mount and NOT on tab re-selection — which is exactly why a
            // mute set in Settings never reached the feed's scorers on return
            // (test_feed_muted_posts.py, iOS). The nav-back sets
            // `selectedTab = "feed"` (FaunaApp), which this observes reliably.
            // Harmless no-op via `rehydrate`'s optional chaining if it fires
            // before `.task` finishes the first configure.
            .onChange(of: appState.selectedTab) {
                if appState.selectedTab == "feed" { Task { await vm.rehydrate() } }
            }
            // Keep the selector default valid: if the current pick isn't one of
            // the nest's available bridges, fall back to the first available one
            // (mirrors web's reconciliation), so a subscribe never sends a
            // protocol the nest can't serve.
            .onChange(of: vm.availableBridges) {
                if !vm.availableBridges.contains(where: { $0.id == bridgeFormBridge }),
                   let first = vm.availableBridges.first {
                    bridgeFormBridge = first.id
                }
            }
            // Re-pull the feed on WS-RPC reconnect (no poll backstop) so posts
            // that arrived while disconnected surface without a manual refresh.
            .onReconnect { await vm.rehydrate() }
            // A cross-tab deep link (`search-result-item` activation,
            // `ui/search.md` § Where logic lives → Result navigation): switch
            // to this tab happens synchronously at the call site
            // (`appState.selectedTab`), this task then resolves the target
            // post — fast path if the timeline already has it, else the one
            // round trip `resolvePost` needs — and pushes it the same way an
            // in-list card tap does. Fires regardless of which tab is
            // visible when staged (SwiftUI's `TabView` keeps every tab
            // mounted). Mirrors macOS `MacFeedDetailView`.
            .task(id: vm.pendingPostOpen) {
                guard let postId = vm.pendingPostOpen else { return }
                vm.pendingPostOpen = nil
                if let post = vm.findPost(postId: postId) {
                    selectedPost = post
                    return
                }
                await vm.resolvePost(postId)
                if let post = vm.findPost(postId: postId) {
                    selectedPost = post
                }
            }
            .sheet(isPresented: $showComposeDialog) {
                FeedComposeDialog(vm: vm, onClose: { showComposeDialog = false })
            }
            .navigationDestination(isPresented: $pushCreateFeed) {
                FeedFormView(vm: vm)
            }
            // Post detail push, driven by `selectedPost` (a `post-card` tap or its
            // automation activate) rather than a NavigationLink label the registry
            // can't actuate. Mirrors macOS `MacFeedDetailView`'s `selectedPost` sheet.
            .navigationDestination(item: $selectedPost) { post in
                PostDetailView(vm: vm, post: post)
            }
        }
    }

    private func configureAndLoadFeeds() async {
        guard let client, let secretHex = appState.session.secretHex else { return }
        await vm.configure(api: client.api, secretHex: secretHex)
        // Load feeds + the current selection (the nest's Local timeline when none),
        // retrying while the client is still authenticating. `rehydrate` runs
        // loadFeeds() + selectFeed(current | nil→Local), so posts on the default
        // feed appear PROMPTLY even on a fresh nest with no custom feeds — the
        // subscriber leg's `wait_post_count` needs the select INSIDE the retry (a
        // post-configure select gated behind a "feeds non-empty" loop lands too
        // late: on a fresh nest `feeds` never fills, so the loop burns its whole
        // budget before selecting the Local feed). Mirrors macOS FeedSplitView's
        // post-configure select; the loop just covers the client still authenticating.
        for _ in 0..<10 {
            await vm.rehydrate()
            if !vm.posts.isEmpty || !vm.feeds.isEmpty { break }
            try? await Task.sleep(for: .seconds(1))
        }
    }
    /// The bridge-subscribe button's action, factored out so the automation
    /// sibling drives the exact same code path the Button does.
    private func subscribeBridgeAction() {
        Task {
            await vm.subscribeBridge(bridge: bridgeFormBridge,
                                     feedUri: bridgeFeedUri,
                                     name: bridgeFeedName)
            bridgeFeedUri = ""
            bridgeFeedName = ""
            showBridgeSubscribe = false
        }
    }

    /// Shared logic — FaunaKit's `openPostCard(_:vm:selectedPost:)`.
    private func openPostCard(_ post: PostSummary) {
        FaunaKit.openPostCard(post, vm: vm, selectedPost: &selectedPost)
    }
}
