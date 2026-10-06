import SwiftUI

/// Thin SwiftUI-friendly proxy over the shared, stateful `FfiFeedManager`
/// (`fauna_feed::FeedManager` via UniFFI). All post-list / search / feed-rule /
/// compose / quoted-post / media state lives in the shared Rust crate
/// `libs/fauna-feed`; this class:
///
///   1. owns the manager instance (constructed at login over the WS-RPC
///      connection — `APIClient.feedManager(secret:)`),
///   2. implements `FeedSnapshotObserver` (via `FeedObserverBox`) to translate
///      manager notifications into `@Observable` invalidations on the main actor —
///      mirrors `ConversationsVM`'s observer trampoline,
///   3. exposes convenience getters so SwiftUI views read `vm.posts` / `vm.feeds`
///      / `vm.composeError` instead of `vm.manager.snapshot()...` everywhere —
///      they're equivalent, but the getters touch the observer-tick so
///      `@Observable` re-renders on change,
///   4. forwards every mutator to the manager. No client-side post-list cache, no
///      client-side search filter, no client-side rule encoding, no decode cache
///      (`docs/goal/ui/feed.md` § Architectural rules — the lift that retired
///      FeedVM's ad-hoc state, priority #1/#2).
///
/// Shared by the macOS and iOS apps; identical behaviour on both. The macOS
/// `TestAgent` (`FaunaMacApp.swift`) reads `FeedVM.lastLoadedPosts` for E2E state
/// serialization (kept fresh from the manager snapshot on every change).
///
/// The interaction-bar (like / repost / reply) is **not** a manager action — it
/// stays a thin `APIClient.interactWithPost` call, the same pattern the
/// Linux/Android/web feed clients use (`feed.md` § interaction-bar). The manager
/// owns the snapshot, not per-post engagement.
@MainActor @Observable
public final class FeedVM {
    /// The shared manager, `nil` until `configure` runs at login (it needs an
    /// authed `NestClient` + the actor secret, so there is no bare/E2E-mock
    /// constructor — unlike `ConversationsVM`, whose manager works offline).
    public private(set) var manager: FfiFeedManager?

    /// Most-recently-loaded posts, shared across `FeedVM` instances. The macOS
    /// `TestAgent` reads this directly for `data.feed.posts[]` serialization;
    /// refreshed from `manager.snapshot().posts` on every observer tick.
    public static var lastLoadedPosts: [PostSummary] = []

    /// Post ids the user has tapped "Show anyway" on (`FeedPostMutedPlaceholder`
    /// reveal — topic-factors.md § Scoring: session-local, never persisted,
    /// never un-mutes the term). Deliberately NOT manager state (unlike blocked
    /// remote-image reveals, which the manager tracks) — mirrors
    /// `DmMessageBubble.mutedRevealed`'s per-card `@State`. Exists as a shared
    /// static, like `lastLoadedPosts`, ONLY so the TestAgent's state
    /// serialization (`data.feed.posts[].is_muted`) can see it: apple's lazy
    /// feed list falls back to state reads for post-text assertions
    /// (`_use_state_for_feed_reads` — the in-process element doesn't register
    /// for off-screen cards), so a muted-post test's "must not leak the body"
    /// / "reveal shows the body" checks need this reachable outside the view.
    public static var revealedMutedPostIds: Set<String> = []

    /// Pure-UI toggle for the inline create-feed form (`feed-create-feed-button`).
    /// Not snapshot state — the form's *inputs* are view-local until submit, which
    /// builds `[FilterRuleInput]` and calls `manager.create_feed` (shared encoder).
    public var showCreateForm = false

    /// View-local "create in flight" flag (the manager has no create-feed
    /// submitting flag; create_feed is one async call). Disables the submit button.
    public var creatingFeed = false

    /// The search field's live buffer (UI glue). Debounced into the manager's
    /// `set_search_query` (a nest re-query, never a client-side filter — feed.md
    /// § Where logic lives → Search). The committed term lives in
    /// `snapshot.search_query`; this is only the in-flight text the field shows.
    public var searchText: String = ""

    /// Public (read-only) so `FeedPostActionsButton` can lazily hydrate the
    /// shared `WebPublishStore` — the own-post web-publishing verbs need the
    /// same `FfiWebClient` this VM already configures, and the store's own
    /// `hydrate` takes `api` as a parameter rather than storing one itself
    /// (mirroring `ContentPolicyStore.refresh(api:)`).
    public private(set) var api: APIClient?
    private var observerBox: FeedObserverBox?
    private var searchTask: Task<Void, Never>?
    private var _observerTick: UInt64 = 0

    // ── Draft persistence (reserved-folders.md § Drafts Sync, rail "posts") ────
    //
    // The apple twin of ConversationsVM's own drafts glue (which this mirrors
    // exactly — same launch-gate/debounce shape, `ConversationsVM.
    // draftsSaveDebounce` shared rather than a second literal): pure trigger
    // glue over the shared `FfiDraftsSync`, built once per manager (inside
    // `configure`, alongside the manager itself) and held so a re-entrant
    // `configure` for the same actor doesn't re-close the launch gate. We
    // restore the owner's persisted draft on the manager's first build and let
    // `onManagerChanged` drive a debounced autosave after compose edits.
    private var draftsSync: FfiDraftsSync?
    private var draftsSaveTask: Task<Void, Never>?

    // ── Room-restricted posts: the feed's room-post seam (`ui/feed.md` §
    //    Encryption at rest → *Room-restricted — the app half*) ─────────────
    //
    // `ConversationsSession` is the seam: its key answers are the MLS
    // backend's own, both directions, and it is the one place that meets
    // both owners a room post needs (feed.md:498). Held here — like
    // `configuredSecret`/`draftsSync` — so a manager rebuilt on an actor
    // change (`configure`) re-installs it automatically, and so a call
    // before the Feed tab has built a manager yet is never lost. Mirrors
    // linux's `conv_backend.rs::attach_own_rooms_refresh` and web's
    // `syncFeedRoomPosts` — the app-level login glue is where the feed
    // manager and the conversations session meet on apple.
    private var roomPostSession: ConversationsSession?
    /// The conversations manager currently wired to re-read `own_rooms` on
    /// every conversations-plane tick. `ConversationsVM.manager` is never
    /// swapped across a re-login (its own doc), so this stays the same
    /// instance across every re-install — the guard just avoids piling up a
    /// duplicate observer.
    private weak var roomPostObservedManager: ConversationsManager?
    private var roomPostObserverBox: FeedRoomPostObserverBox?

    /// Whether `submitPost()` can act — the manager is built. `configure` runs
    /// asynchronously after login, so a compose typed in the ~1s window before
    /// it resolves must not be silently droppable (e2e point 11). Gates
    /// `post-submit-button` on all three submit surfaces (iOS `FeedListView`,
    /// macOS `MacFeedDetailView`, `FeedComposeDialog`), mirroring web's
    /// `feedReady` fix (`git log origin/main -S composeReady`) so the e2e
    /// driver's click auto-waits instead of racing the build.
    public var composeReady: Bool { manager != nil }

    /// `submitPost()`'s fallback for the (should-be UI-unreachable, since
    /// `composeReady` now gates every submit surface) case where it's invoked
    /// before the manager exists — `compose.error` is manager-owned
    /// (`snapshot.compose.error`), which doesn't exist yet either, so there is
    /// nowhere else to fail loudly onto. Shadowed forever once a manager is
    /// built (`composeError` only consults it while `snapshot == nil`).
    private var preManagerComposeError: String?

    /// A compose failure raised by client glue rather than by the manager — the
    /// attachment seal and its upload, which return an error instead of
    /// recording one on `snapshot.compose.error`. Cleared at the top of every
    /// submit, so a stale one never masks a fresh manager error.
    private var composeGlueError: String?

    public init() {}

    /// The actor secret the current `manager` was built for — so `configure`
    /// can tell a same-actor re-entry (keep the manager) from an actor change (a
    /// re-login to a different account, which MUST rebuild).
    private var configuredSecret: String?
    /// The actor secret a `configure` is currently building for (concurrency guard
    /// — two triggers (`.task` + `.onChange(of: secretHex)`) can fire together).
    private var configuringSecret: String?
    /// True while an actor CHANGE is rebuilding the manager. The feed HIDES the
    /// previous actor's posts for the duration (no cross-actor leak) WITHOUT nulling
    /// `manager` — so a concurrent read (a gated-post unlock's fire-once `.task`)
    /// never strands on a nil manager.
    public private(set) var isReconfiguring = false

    /// Construct the manager for `secretHex`. Idempotent for the *same* actor (a
    /// Feed-tab remount keeps the existing manager), but rebuilds when the actor
    /// **changes** (`secretHex` differs). Rebuilding on actor change is load-bearing:
    /// `applySessionPatch` swaps in a fresh `FaunaClient` on a re-login without the
    /// manager knowing, so a stale manager would leak the prior actor's snapshot
    /// (a gated post they unsealed) into the new actor's feed (`test_gated_post_compose`
    /// subscriber leg). The rebuild must satisfy BOTH invariants at once, which a
    /// naive pre-clear or build-then-swap each half-miss: (a) never SHOW the old
    /// actor's posts (leak) and (b) never leave `manager == nil` (a fire-once unlock
    /// `.task` reading nil strands the post sealed). So: mark `isReconfiguring`
    /// (which blanks `posts` — invariant a) while keeping the old `manager` live
    /// (invariant b), build the new one, swap, then clear the flag. `defer` clears
    /// it even if this `configure`'s `.task` is cancelled mid-build (the list being
    /// covered by a pushed detail), so the feed never gets stuck blank.
    /// Bumped each time a *new* `manager` instance is installed (login / actor
    /// change / a reconnect that rebuilds it). Views observe it to drop UI that
    /// belonged to the previous actor's manager: the feed's detail push/sheet
    /// keys a dismissal on it, so a re-login can't leave the prior actor's post
    /// detail open — whose fire-once gated-unlock `.task` would otherwise unseal a
    /// post straight into the new actor's feed before the reader opens it
    /// (`test_gated_post_compose` subscriber leg). Bumped *inside* `configure`,
    /// which runs while the feed is on screen at login — unlike `session.secretHex`,
    /// which can change while the feed is off screen (another tab), so a
    /// `secretHex`-keyed dismissal misses the change. `@Observable`.
    public private(set) var managerGeneration: Int = 0
    /// Drop everything this view model holds for the account it was scoped to —
    /// called by ``ActorScope/dropAppOwnedState(criticalAlertsHost:conversationsVM:feedVM:screenTime:modelContainer:appState:newModelContainer:)``,
    /// the ONE canonical drop (`account-scoping.md` § The scoping taxonomy, the
    /// in-memory corollary), and deliberately **not** by a page seam.
    ///
    /// `feedVM` is App-scene-level `@State` injected through `.environment`
    /// (`FaunaApp`/`FaunaMacApp`), so a teardown function CAN reach it: it is the
    /// *reachable* side of this file's § *State a view owns* note, exactly like
    /// `conversationsVM` beside it. linux clears its twin from the same canonical
    /// list and for the same stated reason — "a reader between teardown and the
    /// next `init` serializes the previous identity's posts"
    /// (`apps/fauna-linux/src/actor_scope.rs`'s `feed::host::clear()`) — so this is
    /// the cross-app shape, not an apple invention .
    ///
    /// Nearly every rendered field on this page is DERIVED from `manager.snapshot()`
    /// — the posts, the feed list, the compose buffer, the gate options — so
    /// dropping the manager drops them all in one line. What needs naming is the
    /// handles, the in-flight tasks and the view-glue state.
    ///
    /// ⚠ The drop closes a hole the page's own actor seam structurally could not:
    /// ``configure`` returns early when `manager != nil, configuredSecret ==
    /// secretHex`, so a factory-reset that re-claims the **same** actor
    /// (`factoryResetReonboard`, which sets `isOnboarding = true` and unmounts this
    /// page, so no page-level seam ever fires there) left this view model holding a
    /// manager built on the DISCARDED `APIClient` for the rest of the process — the
    /// latched-cadence class `account-scoping.md`'s ledger already records for
    /// `DnsAutoRenewCadence`. **It is dropping the MANAGER that breaks that early
    /// return**, since the condition needs both halves; mutation-testing this drop
    /// showed as much, so don't re-attribute it to `configuredSecret`.
    /// `configuredSecret` is cleared beside it to keep the bookkeeping honest — a
    /// view model holding no manager must not still report itself configured for an
    /// actor — and that half is deliberately NOT separately pinned, because with the
    /// manager gone no observable behaviour distinguishes it.
    ///
    /// `roomPostSession` goes too, and can: every login path re-installs the seam
    /// (``installRoomPostKeys(session:conversationsManager:)``, twice per target), so
    /// the incoming actor rebuilds it instead of inheriting the outgoing actor's.
    ///
    /// The two statics are NOT here — they are on ``ActorScope/resetSharedState()``,
    /// which is the one owner for them (the corollary's first rule: no field
    /// hand-listed at two sites).
    public func reset() {
        searchTask?.cancel()
        searchTask = nil
        draftsSaveTask?.cancel()
        draftsSaveTask = nil
        draftsSync = nil
        // The manager holds the rendered state, so this one line drops every derived
        // getter. Its observer box goes with it — the box's `target` is this VM, and
        // the manager holds the box.
        observerBox?.target = nil
        observerBox = nil
        manager = nil
        roomPostSession = nil
        roomPostObservedManager = nil
        roomPostObserverBox = nil
        api = nil
        configuredSecret = nil
        configuringSecret = nil
        isReconfiguring = false
        // A dropped manager IS a manager change, so the page's
        // `.onChange(of: vm.managerGeneration)` dismisses any pushed post detail
        // whose fire-once gated-unlock `.task` would otherwise fire against the
        // incoming actor's feed.
        managerGeneration &+= 1
        showCreateForm = false
        creatingFeed = false
        searchText = ""
        pendingPostOpen = nil
        pendingAttachment = nil
        preManagerComposeError = nil
        composeGlueError = nil
        clientErrorMessage = nil
        verbErrorMessage = nil
        // The decrypted media the outgoing actor opened (`ui/media.md`): blob bytes
        // and C2PA verdicts, keyed by hash with no actor in the key.
        dropMediaCaches()
        // Re-evaluate the derived getters for SwiftUI. Safe with everything nil: the
        // autosave it re-arms is a no-op without a `draftsSync`, and the post mirror
        // it writes resolves to `[]` off the dropped manager.
        onManagerChanged()
    }

    public func configure(api: APIClient, secretHex: String) async {
        self.api = api
        if manager != nil, configuredSecret == secretHex { return }
        if configuringSecret == secretHex { return }   // a build for this actor is already in flight
        configuringSecret = secretHex
        let isActorChange = configuredSecret != nil && configuredSecret != secretHex
        if isActorChange {
            isReconfiguring = true
            // Before the new manager is even built: the previous actor's opened
            // media must not survive into this one's feed, the same way
            // `isReconfiguring` blanks its posts.
            dropMediaCaches()
            onManagerChanged()
        }
        defer {
            if configuringSecret == secretHex {
                configuringSecret = nil
                if isReconfiguring {
                    isReconfiguring = false
                    onManagerChanged()
                }
            }
        }
        do {
            let mgr = try await api.feedManager(secret: secretHex)
            guard configuringSecret == secretHex else { return }   // a newer configure superseded us
            // Build once per (re)configure, alongside the manager it restores into —
            // `try?` mirrors the app-level `try? await faunaClient.api.draftsSync(...)`
            // call the conversations rail uses: a failure here just leaves persistence
            // off for this session, non-fatal. Fetched into a local first (not
            // `self.draftsSync` yet) so a superseding `configure` can't have this one
            // clobber its already-installed handle after losing the race below.
            let drafts = try? await api.draftsSync(rail: "posts")
            guard configuringSecret == secretHex else { return }   // a newer configure superseded us
            let box = FeedObserverBox()
            mgr.addObserver(observer: box)
            box.target = self
            self.manager = mgr
            self.observerBox = box
            self.configuredSecret = secretHex
            managerGeneration &+= 1
            dropMediaCaches()
            draftsSaveTask?.cancel()
            draftsSaveTask = nil
            self.draftsSync = drafts
            // A fresh manager (first build, or an actor-change rebuild) needs
            // the room-post seam re-installed — `installRoomPostKeys` may
            // have run before this manager existed (login races the Feed
            // tab's own `configure`).
            if let roomPostSession {
                mgr.setRoomPostKeys(session: roomPostSession)
                refreshOwnRooms()
            }
            onManagerChanged()
            restoreDraftsOnLaunch()
        } catch {
            // The manager couldn't be built (no connection yet); the existing
            // manager (or nil on first build) stays and the next `configure` retries.
        }
    }

    // ── Observer ────────────────────────────────────────────────────────────
    fileprivate func onManagerChanged() {
        // Convenience getters read `_observerTick`, so bumping it re-evaluates
        // them and SwiftUI re-renders (matches ConversationsVM). The manager holds
        // the real state; we also mirror the post list into the static the macOS
        // TestAgent serializes from.
        _observerTick &+= 1
        // Blank the mirrored list while an actor change is rebuilding, so the
        // TestAgent's state serialization can't surface the previous actor's posts
        // either (mirrors the `posts` getter's `isReconfiguring` guard).
        Self.lastLoadedPosts = isReconfiguring ? [] : (manager?.snapshot().posts ?? [])
        // Every manager notification (incl. a compose text/tags/attachment edit)
        // re-arms the debounced draft autosave — a cheap no-op when no
        // `draftsSync` is attached yet or the snapshot is unchanged. Mirrors
        // `ConversationsVM.onManagerChanged`.
        scheduleDraftsSave()
    }

    // ── Draft persistence triggers ─────────────────────────────────────────────
    /// Restore the owner's persisted feed compose draft. Called from `configure`
    /// right after the manager + `draftsSync` handle are both installed. A
    /// restore failure is logged and left non-fatal — the `FfiDraftsSync` launch
    /// gate then stays closed, so a later autosave can't clobber the unread blob
    /// (no-data-loss). Mirrors `ConversationsVM.restoreDraftsOnLaunch`.
    private func restoreDraftsOnLaunch() {
        guard let sync = draftsSync, let manager else { return }
        Task { @MainActor in
            do {
                if let bytes = try await sync.load() {
                    manager.restoreDrafts(bytes: bytes)
                }
            } catch {
                logMessage(level: .warn, target: "fauna.feed.drafts",
                           message: "[drafts] restore on launch failed (non-fatal): \(error)")
            }
        }
    }

    /// Debounced persist after a compose change: each manager notification re-arms
    /// the shared `ConversationsVM.draftsSaveDebounce` timer and only the last
    /// surviving task fires, so a burst of keystrokes coalesces into one upload.
    /// `saveIfChanged` then no-ops before the launch restore completes (the
    /// no-data-loss gate) and for an unchanged snapshot, so a tick fired by a
    /// non-draft manager change costs only a cheap snapshot compare. A no-op when
    /// no `draftsSync` is attached (pre-configure / E2E). Mirrors
    /// `ConversationsVM.scheduleDraftsSave` (`scheduleDraftsAutosave`,
    /// `ViewModels/DraftsAutosave.swift`).
    private func scheduleDraftsSave() {
        guard let sync = draftsSync, let manager else { return }
        draftsSaveTask?.cancel()
        draftsSaveTask = scheduleDraftsAutosave(sync: sync, logTarget: "fauna.feed.drafts") {
            manager.draftsSnapshotBytes()
        }
    }

    /// Flush the pending autosave immediately — the leave-flush promise
    /// (`reserved-folders.md` § The leave-flush promise): a leave door must not
    /// lose the debounce window's last edit. Called from each target's leave-door
    /// observer — macOS's `AppDelegate.applicationShouldTerminate` bounded quit
    /// gate, iOS's `applicationDidEnterBackground` background-task extension —
    /// which reach this VM through `AppState.feedVM`. Mirrors
    /// `ConversationsVM.flushDraftsNow` exactly (`flushDraftsAutosave`,
    /// `ViewModels/DraftsAutosave.swift`).
    ///
    /// Cancels the debounced task first so the two cannot both fire, then runs
    /// the save it would have run. A no-op when no `draftsSync`/`manager` is
    /// attached (pre-configure / E2E), which is what lets the leave doors call
    /// it unconditionally.
    public func flushDraftsNow() async {
        draftsSaveTask?.cancel()
        draftsSaveTask = nil
        guard let sync = draftsSync, let manager else { return }
        await flushDraftsAutosave(sync: sync, logTarget: "fauna.feed.drafts") {
            manager.draftsSnapshotBytes()
        }
    }

    // ── Convenience getters (read freshly on every access) ─────────────────
    public var snapshot: FeedSnapshot? {
        _ = _observerTick
        return manager?.snapshot()
    }
    public var feeds: [FeedSummaryView] { snapshot?.feeds ?? [] }
    public var bridgeFeeds: [BridgeFeedView] { snapshot?.bridgeFeeds ?? [] }
    /// The bridges the nest can actually serve (`bridge-form-bridge-select`
    /// option set), from the server-filtered `fauna.bridges.list` via
    /// `refresh_available_bridges`. Empty ⇒ the nest supports no bridges ⇒ the
    /// view hides the `bridge-feed-subscribe-toggle` (Dim 3 capability
    /// consumption — `version-compatibility.md`: never offer an unsupported
    /// protocol). Drives the selector instead of a hard-coded protocol list.
    public var availableBridges: [AvailableBridge] { snapshot?.availableBridges ?? [] }
    public var selectedFeedId: String? { snapshot?.selectedFeed }
    /// Whether the built-in **Trending** virtual feed is the current selection
    /// (`feed-trending-item`, `trending.md` § The Trending feed) — mutually
    /// exclusive with `selectedFeedId`/local, enforced shared-Rust-side
    /// (`FeedManager::select_feed` clears this; `select_trending_feed` sets it
    /// and clears `selectedFeedId`).
    public var trendingSelected: Bool { snapshot?.trendingSelected ?? false }
    /// The rendered post list. Blanked while an actor change is rebuilding the
    /// manager (`isReconfiguring`) so the previous actor's posts — which stay in the
    /// still-live old manager's snapshot until the swap — never reach the new
    /// actor's feed (no cross-actor leak); the ensuing reload repopulates it.
    public var posts: [PostSummary] { isReconfiguring ? [] : (snapshot?.posts ?? []) }
    /// Re-read a post's live snapshot state so manager re-emits — remote-image
    /// reveal (D3) and the lazy media / quoted-post fold — reach the detail
    /// view. `post` is a value snapshot captured at navigation and never
    /// updates on its own; `posts` is the observed live list (feed.md § read
    /// from state). Falls back to `post` itself if it has left the current
    /// feed. Shared by the macOS/iOS post-detail views.
    public func livePost(for post: PostSummary) -> PostSummary {
        posts.first { $0.postId == post.postId } ?? post
    }
    /// Find `postId` in what's already loaded — the timeline's own `posts`, or
    /// the deep-link slot [`FeedSnapshot.deepLinkedPost`] a prior [`resolvePost`]
    /// parked (`ui/search.md` § Where logic lives → Result navigation (deep
    /// link)). `nil` when neither holds it (not yet resolved, or genuinely
    /// unavailable). Never a re-derived join — both are the manager's own
    /// snapshot fields.
    public func findPost(postId: String) -> PostSummary? {
        posts.first { $0.postId == postId } ?? snapshot?.deepLinkedPost.flatMap {
            $0.postId == postId ? $0 : nil
        }
    }
    /// Resolve `postId` via the shared `FeedManager::resolve_post` — idempotent
    /// for a post the timeline already holds (returns `.loaded` with no round
    /// trip); otherwise fetches it and parks the result in
    /// `FeedSnapshot.deepLinkedPost` for [`findPost`] to pick up. The single
    /// door for a caller that has only a post's id, not an on-screen card — a
    /// `search-result-item` deep link is the first such caller.
    @discardableResult
    public func resolvePost(_ postId: String) async -> PostResolution {
        await manager?.resolvePost(postId: postId) ?? .unavailable
    }
    /// A cross-page deep-link target (a search-result activation) asking the
    /// Feed detail surface to open this post id — consumed once by the surface,
    /// which resolves it (fast path via `findPost` if already loaded, else
    /// `resolvePost`) and clears this back to `nil`. Every ordinary in-list
    /// post-card tap sets its own local `selectedPost` directly instead, with
    /// no round trip and no need for this slot.
    public var pendingPostOpen: String?
    public var isLoading: Bool { isReconfiguring ? true : ((snapshot?.status ?? .loading) == .loading) }
    /// Whether the page paints its loading spinner in place of the post list:
    /// only while a load is in flight AND there is nothing to show. A REFRESH
    /// keeps its posts on screen until the new page lands — the manager keeps
    /// them in the snapshot while `status == Loading` — and only a SWITCH (which
    /// clears them up front) or a first load shows the spinner (`ui/feed.md` §
    /// The read model). `posts` is already blank while `isReconfiguring`.
    public var showsLoadingSpinner: Bool { isLoading && posts.isEmpty }
    /// Which empty state the page paints (`feed-empty-state` /
    /// `feed-no-results`), if any — the shared `FeedSnapshot::empty_state`
    /// answer, never re-derived here (`ui/feed.md` § Errors & edge cases).
    /// `nil` while an actor change rebuilds the manager, like `posts`.
    public var emptyState: FeedEmptyState? {
        guard !isReconfiguring, let snapshot else { return nil }
        return feedEmptyState(snapshot: snapshot)
    }
    /// Whether a `rehydrate()` is running — the coalescing flag it keeps.
    @ObservationIgnored private var rehydrateInFlight = false
    public var hasMore: Bool { snapshot?.hasMore ?? false }
    /// The composer text/tags live in the manager so `compose-error` /
    /// `compose-file-ready` render from snapshot fields (feed.md § Architectural
    /// rules). The fields below are bound through `setComposeText`/`setComposeTags`.
    public var composeText: String { snapshot?.compose.text ?? "" }
    public var composeTags: String { snapshot?.compose.tags ?? "" }
    /// The composer's attached file — `compose-file-ready`'s name + size
    /// source and `compose-file-remove`'s target — off the manager snapshot,
    /// never `pendingAttachment`: `attachComposeFile(atPath:)` stages the
    /// manager's hash-less handle synchronously alongside `pendingAttachment`,
    /// so this is non-nil from the moment a pick is made, and for a restored
    /// draft's handle alike (which has no `pendingAttachment` at all).
    public var composeAttachedFile: AttachedFile? { snapshot?.compose.attachedFile }
    public var composeError: String? {
        // A client-glue failure wins while it stands: the attachment seal and its
        // upload return an error rather than recording one on the snapshot, and
        // every submit clears this first, so it can never mask a later one.
        if let composeGlueError { return composeGlueError }
        guard let snapshot else { return preManagerComposeError }
        return snapshot.compose.error.map(renderLocalizedText)
    }
    /// Page-level error (`error-message`) ← `snapshot.error`, falling back to a
    /// client-glue error the manager snapshot cannot carry.
    public var errorMessage: String? {
        (snapshot?.error).map(renderLocalizedText) ?? clientErrorMessage
    }

    /// Client-glue error for the page's `error-message` element — set by shell
    /// code whose failure has no manager-snapshot home, currently the
    /// engagement-cue capture shell (`CueViewportObserver`: a `hydrate_cues`
    /// that could not open the stored rollup, or a failing `record_observation`).
    /// The snapshot's own error always wins, so this can never mask a real
    /// manager failure.
    ///
    /// Exists because `errorMessage` is derived, not stored: linux hands its
    /// capture shell the page's `error-message` widget directly
    /// (`viewport.rs::wire(..., error_label)`) and sets its text, which is the
    /// behaviour this reproduces through apple's snapshot-derived rendering
    /// rather than minting a second error element.
    ///
    /// `private(set)`: every writer that runs detached from this account's own
    /// lifecycle (past an `await`, inside a `Task`, or an FFI callback) owes
    /// the same account-switch guard `reset()` needs — a late write from the
    /// outgoing actor must not paint into the incoming one's feed
    /// (`account-scoping.md` § The scoping taxonomy, the in-flight landing
    /// rules, `:208-236`). Write
    /// through ``landClientErrorMessage(generation:message:)`` after an
    /// await/Task/callback, or ``setClientErrorMessage(_:)`` for a write that
    /// never crosses one (no account-switch window exists to guard against).
    public private(set) var clientErrorMessage: String?

    /// Land a client-glue error into ``clientErrorMessage``, refusing if
    /// `generation` no longer matches ``managerGeneration`` — the guard every
    /// post-await/post-Task/post-callback writer captures before its first
    /// suspension point and lands through. Absorbed from the former free
    /// `landFeedClientErrorMessage` (three web-publish call sites)
    /// once every `FeedVM` writer needed the same guard, not just those three;
    /// mirrors `WebPublishStore.landActionFailure`'s shape.
    @discardableResult
    public func landClientErrorMessage(generation: Int, message: String) -> Bool {
        guard generation == managerGeneration else { return false }
        clientErrorMessage = message
        return true
    }

    /// Write ``clientErrorMessage`` unconditionally — for a writer whose whole
    /// call frame is synchronous, so there is no account-switch window between
    /// capture and write to guard against (`openPaymentURL`'s `setError`,
    /// `ComposeAttachButton`'s picker-open/cancel failures).
    public func setClientErrorMessage(_ message: String?) {
        clientErrorMessage = message
    }

    // ── Feed list / selection ───────────────────────────────────────────────
    /// Refresh the feed selector + subscribed bridge-feed lists + the nest's
    /// available-bridge set. Called on entering the page and after
    /// create/delete/subscribe/unsubscribe. `refreshAvailableBridges` drives the
    /// list-driven subscribe selector + its gating (Dim 3 consumption).
    public func loadFeeds() async {
        await manager?.refreshFeeds()
        await manager?.refreshBridgeFeeds()
        await manager?.refreshAvailableBridges()
    }
    public func selectFeed(_ id: String) async {
        searchText = ""
        searchTask?.cancel()
        await manager?.selectFeed(feedId: id)
    }
    /// Select the nest's built-in **Local** timeline — `select_feed(nil)`, the
    /// default view when the user has no custom feed and isn't on Trending
    /// (mirrors linux/web's `select_feed(None)`). Without an explicit default a
    /// fresh nest with no saved feeds would render an empty feed.
    public func selectLocalFeed() async {
        searchText = ""
        searchTask?.cancel()
        await manager?.selectFeed(feedId: nil)
    }
    /// Select the built-in Trending virtual feed (`feed-trending-item`) —
    /// the `selectFeed`/`rehydrate` sibling that drives `select_trending_feed`.
    public func selectTrendingFeed() async {
        searchText = ""
        searchTask?.cancel()
        await manager?.selectTrendingFeed()
    }
    /// Re-pull on WS-RPC reconnect (no poll backstop). Re-query reloads the
    /// selected feed; refreshing the lists picks up feeds added while offline.
    public func rehydrate() async {
        // ONE re-pull per page entry. Two entry signals can land together — iOS's
        // Feed tab re-runs its `.task` on re-appearance AND observes
        // `.onChange(of: selectedTab)` — and a second reload started behind the
        // first supersedes it: the one-shot reload hold parks the first while the
        // second commits, so a refresh reads as never in flight (linux hit the
        // same double re-pull). A call arriving while one is in flight joins it.
        if rehydrateInFlight { return }
        rehydrateInFlight = true
        defer { rehydrateInFlight = false }
        await loadFeeds()
        // Re-pull the CURRENT selection via the shared `refreshCurrentFeed`
        // seam so a mute (or any sealed-scorer change) set elsewhere reaches
        // this feed on return, and so posts that arrived while disconnected
        // surface on reconnect — WITHOUT changing which feed is selected.
        // Never `selectFeed(feedId: selectedFeedId)`: that id is nil both for
        // the local feed and while Trending is selected, so re-selecting it
        // would silently drop the user off Trending back to local (trending.md
        // § The Trending feed). The old `if let id = selectedFeedId` guard
        // skipped the reload whenever the local OR trending feed was selected
        // (the default views), so a mute set in Settings never applied to them
        // on return — the iOS bug test_feed_muted_posts.py caught once it
        // finally ran on iOS.
        await manager?.refreshCurrentFeed()
    }
    public func loadMore() async { await manager?.loadMore() }

    // ── Search (debounced re-query — never a client-side filter) ────────────
    public func onSearchTextChanged(_ text: String) {
        searchText = text
        searchTask?.cancel()
        let trimmed = text.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty {
            // No committed search → nothing to clear. Skipping the redundant
            // un-searched reload matters: the driver's `clear_and_type` (and a
            // user clearing an empty box) fired it just before the debounced
            // `setSearchQuery`, and its SLOW unfiltered fetch landing last was
            // the reload race that left a committed search with unfiltered
            // posts (dropped rust-side now via `reload_gen`, but don't pay
            // a wasted nest round-trip for a no-op).
            guard manager?.snapshot().searchQuery != nil else { return }
            searchTask = Task { await manager?.clearSearch() }
            return
        }
        searchTask = Task {
            try? await Task.sleep(for: .milliseconds(400))
            guard !Task.isCancelled else { return }
            await manager?.setSearchQuery(term: trimmed)
        }
    }
    public func clearSearch() {
        searchText = ""
        searchTask?.cancel()
        searchTask = Task { await manager?.clearSearch() }
    }

    // ── Compose (validation + build/sign are in the manager) ────────────────
    public func setComposeText(_ text: String) {
        manager?.updateCompose(text: text, tags: composeTags, attachedFile: snapshot?.compose.attachedFile)
    }
    public func setComposeTags(_ tags: String) {
        manager?.updateCompose(text: composeText, tags: tags, attachedFile: snapshot?.compose.attachedFile)
    }

    /// A file the author picked but which has NOT been uploaded — held here until
    /// submit, because the composer's audience is what decides the seal and it is
    /// not known when the picker returns (`ui/media.md` § Encryption at rest).
    /// tui, web and windows all hold the picked bytes the same way.
    private struct PendingAttachment {
        let name: String
        let bytes: Data
    }
    private var pendingAttachment: PendingAttachment?

    /// Stage a file onto the composer (`compose-file-ready`): read it and HOLD
    /// the bytes locally — nothing is uploaded here, that happens at submit
    /// once the audience is known — AND stage a hash-less handle
    /// (`{name, size}`, `blobHash: nil`) on the manager the moment the pick is
    /// made, so a draft saved before submit still carries the file rather than
    /// losing it on a relaunch with nothing left to refuse (`ui/feed.md` §
    /// Persistence → *Attachments by content address*; the same shape
    /// tui/linux/web/android stage at pick).
    ///
    /// Until 2026-09-08 this POSTed the bytes immediately as a `.publicPost`
    /// (plaintext) blob and staged the resulting hash. Attaching a photo and
    /// *then* picking a tier therefore left a readable copy of a restricted
    /// post's picture on the nest under a hash anyone can fetch: blob `GET` is
    /// unauthenticated by design and the nest exposes no blob `DELETE`, so the
    /// only way to have no plaintext copy is never to upload one. Shared Rust
    /// enforces the ordering from its side — `prepare_gated_blob` refuses a body
    /// whose photo was sealed for a stale audience
    /// (`feed.compose_attachment_stale`) rather than publishing a photo nobody
    /// can open. Staging the hash-less handle at pick time doesn't reopen that
    /// gap: the handle carries no bytes and no hash, so there's nothing for a
    /// submit under a mismatched audience to expose — only the later seal (this
    /// method's caller, `sealAndUploadPendingAttachment`) uploads anything.
    ///
    /// Shared by the macOS + iOS TestAgents' `compose.file` e2e test-injection
    /// path and by the real `ComposeAttachButton` picker.
    public func attachComposeFile(atPath path: String) async throws {
        guard let manager else { throw APIError.ffiError("compose.file: no feed manager") }
        let url = URL(fileURLWithPath: path)
        let data = try Data(contentsOf: url)
        let name = url.lastPathComponent
        pendingAttachment = PendingAttachment(name: name, bytes: data)
        let composeNow = manager.snapshot().compose
        manager.updateCompose(text: composeNow.text, tags: composeNow.tags,
                              attachedFile: AttachedFile(name: name, size: UInt64(data.count),
                                                         blobHash: nil, mediaType: nil))
    }

    /// Drop the composer's attached file (`compose-file-remove`) — a fresh pick
    /// or a restored draft's handle alike. Clears BOTH copies: the locally-held
    /// bytes (`pendingAttachment`, so a stale pick can't still upload after a
    /// submit) and the manager's own `attached_file` (so a stale handle can't
    /// still be posted/refused). Text and tags are untouched.
    public func removeComposeAttachment() {
        guard let manager else { return }
        pendingAttachment = nil
        let composeNow = manager.snapshot().compose
        manager.updateCompose(text: composeNow.text, tags: composeNow.tags, attachedFile: nil)
    }

    // ── Compose gate-to-tier (feed.md § Encryption at rest; monetization.md §
    //    Pillars 2+3) ────────────────────────────────────────────────────────
    /// The author's own subscriber tiers (`compose-gate-tier-select` option set)
    /// ← `snapshot.own_tiers`, folded in by the manager on feed reload. Empty ⇒
    /// the author has no tiers ⇒ the composer offers only "Public".
    public var ownTiers: [GateTierOption] { snapshot?.ownTiers ?? [] }
    /// The rooms the composer can address a post to right now
    /// (`snapshot.own_rooms`) — a projection of the conversations plane, kept
    /// current by `installRoomPostKeys`'s conversations-tick observer as well
    /// as every ordinary feed reload (`ui/feed.md` § Encryption at rest →
    /// *The rooms offered*).
    public var ownRooms: [GateRoomOption] { snapshot?.ownRooms ?? [] }
    /// The staged gate tier (`nil` ⇒ ungated / "Public"). Drives the select value.
    public var composeGateTier: String? { snapshot?.compose.gateTier }
    /// The staged room answer (`nil` ⇒ no room selected) — a hex channel id
    /// from `ownRooms`, mutually exclusive with `composeGateTier`/`composeSell`
    /// shared-Rust-side.
    public var composeGateRoom: String? { snapshot?.compose.gateRoom }
    /// The staged public teaser (`compose-gate-preview-field`).
    public var composeGatePreview: String { snapshot?.compose.gatePreview ?? "" }
    /// Stage the composer's gate fields (`compose-gate-tier-select` /
    /// `compose-gate-preview-field`). `tier: nil` composes a normal (ungated)
    /// post — the gate sibling of `setComposeText`/`setComposeTags`.
    public func setComposeGate(tier: String?, preview: String) {
        manager?.updateComposeGate(gateTier: tier, gatePreview: preview)
    }
    /// Stage the composer's **room** answer (`compose-gate-tier-select`'s room
    /// option) — the room sibling of `setComposeGate`, sharing its teaser.
    /// `room` is the hex channel id from `ownRooms`.
    public func setComposeGateRoom(_ room: String?) {
        manager?.updateComposeRoom(gateRoom: room, gatePreview: composeGatePreview)
    }
    /// Stage the teaser (`compose-gate-preview-field`) ALONE — touches no
    /// audience answer, so editing it never flips `compose-gate-tier-select`'s
    /// current selection (a tier, a room, or a sale). The composer views bind
    /// every audience's teaser field through this, never through
    /// `setComposeGate`/`setComposeGateRoom`/`setComposeSell`, whose own
    /// setters would re-assert (and, for a mismatched tier, drop) the wrong
    /// answer (`ui/feed.md` § Encryption at rest → *The composer's fourth
    /// answer*; `FeedManager::update_compose_preview`).
    public func setComposeGatePreview(_ preview: String) {
        manager?.updateComposePreview(gatePreview: preview)
    }

    /// Install `session` as the feed's room-post key seam and start re-reading
    /// its rooms on every change of `conversationsManager` — call wherever the
    /// feed manager and the conversations session first coexist (login,
    /// re-auth); the last call wins. Safe to call before the Feed tab has
    /// built a manager: the session is held and re-installed the moment
    /// `configure` builds one. Without it every room-restricted post stays
    /// locked and no room is offered on `compose-gate-tier-select` — the
    /// honest state for a device with no conversations plane yet.
    public func installRoomPostKeys(session: ConversationsSession, conversationsManager: ConversationsManager) {
        roomPostSession = session
        manager?.setRoomPostKeys(session: session)
        if roomPostObservedManager !== conversationsManager {
            let box = FeedRoomPostObserverBox()
            box.target = self
            conversationsManager.addObserver(obs: box)
            roomPostObserverBox = box
            roomPostObservedManager = conversationsManager
        }
        refreshOwnRooms()
    }

    /// Re-read `snapshot().own_rooms` (local read, no WS-RPC; notifies only on
    /// change) — called after installing the seam, on every subsequent
    /// conversations-plane tick, and by `refreshFeeds`'s own reload.
    fileprivate func refreshOwnRooms() {
        guard let manager else { return }
        Task { await manager.refreshOwnRooms() }
    }

    // ── Compose sell-this-post (monetization.md § Per-post pay-to-unlock;
    //    the select's THIRD answer, mutually exclusive with a gate tier —
    //    `updateComposeGate`/`updateComposeSell` each clear the other
    //    shared-Rust-side) ───────────────────────────────────────────────
    /// The staged sell fields (`compose-sell-price` / `compose-sell-subscribers-free`),
    /// or `nil` when not in sell mode.
    public var composeSell: SellComposeState? { snapshot?.compose.sell }
    /// `compose-sell-price` — free text; `""` when not staged.
    public var composeSellPrice: String { composeSell?.price ?? "" }
    /// `compose-sell-asking-price` — free text (machine-comparable sats), `""`
    /// when unset. **No apple UI writes this yet** — the tier asking-price
    /// input landed on tui first, apple is one of six apps still owed the trickle-down. This
    /// getter exists so `setComposeSell`'s default below preserves rather than
    /// blanks a value once a future apple UI does stage one.
    public var composeSellAskingPrice: String { composeSell?.askingPrice ?? "" }
    /// `compose-sell-subscribers-free` — the ratified rank knob, **defaults true**
    /// (`monetization.md:126`) even before anything is staged, so the picker's
    /// first switch into Sell mode stages the correct default.
    public var composeSellSubscribersFree: Bool { composeSell?.subscribersGetItFree ?? true }
    /// Stage the composer's sell fields — the sell sibling of `setComposeGate`,
    /// sharing its teaser (`composeGatePreview` by default) exactly as a
    /// tier-gated post does. `preview` is explicit (not defaulted to
    /// `composeGatePreview` unconditionally) so the shared teaser field's
    /// binding can route a NEW keystroke through here while in sell mode —
    /// routing it through `setComposeGate` instead would call
    /// `updateComposeGate`, which unconditionally clears `compose.sell`
    /// shared-Rust-side, silently dropping the post back to ungated.
    /// `askingPrice` defaults to the currently-staged value (`nil` ⇒
    /// preserve) — no apple call site sets it explicitly yet, matching
    /// linux/web's own `asking_price: String::new()` stub.
    public func setComposeSell(price: String, subscribersGetItFree: Bool, askingPrice: String? = nil, preview: String? = nil) {
        manager?.updateComposeSell(
            sell: SellComposeState(price: price, askingPrice: askingPrice ?? composeSellAskingPrice, subscribersGetItFree: subscribersGetItFree),
            gatePreview: preview ?? composeGatePreview)
    }

    /// The `compose-gate-tier-select` picker's value: `composeGateTier`, the
    /// staged room's "Room: ‹label›" string, "Sell this post…" while in sell
    /// mode, or "Public" when none (ungated). Shared by the macOS/iOS
    /// composer views. Mirrors tui's own select-value derivation
    /// (`feed/mod.rs`'s `room_option` + `selected`) — a room whose label this
    /// device can no longer resolve (a lost seat) falls back the same way tui
    /// does, to `composeGateTier` / Public.
    public var composeGateSelection: String {
        if composeSell != nil { return L.feed.post.gateSell }
        if let room = composeGateRoom, let match = ownRooms.first(where: { $0.room == room }) {
            return L.feed.post.gateRoom(room: match.label)
        }
        return composeGateTier ?? L.feed.post.gatePublic
    }
    /// Apply a `compose-gate-tier-select` value: "Public" ⇒ ungated (`nil`),
    /// "Sell this post…" ⇒ sell mode (staged at its current — or default —
    /// price/toggle), one of `ownRooms`'s "Room: ‹label›" strings ⇒ that room,
    /// anything else ⇒ a tier name. Checked in this order — mirrors tui's
    /// `Action::SetGateTier` (`feed/mod.rs`) — so the reserved Sell/room
    /// answers are recognized before falling back to "must be a tier",
    /// exactly as tui and apple's own prior Sell-vs-tier check already did;
    /// `tiers.create` refuses only the bare name `room`, so a user-named tier
    /// could otherwise collide with a room's display string. Keeps the
    /// currently-staged teaser (`composeGatePreview`), which every gated
    /// answer shares.
    public func setComposeGateSelection(_ tier: String) {
        if tier == L.feed.post.gateSell {
            setComposeSell(price: composeSellPrice, subscribersGetItFree: composeSellSubscribersFree)
            return
        }
        if tier == L.feed.post.gatePublic {
            setComposeGate(tier: nil, preview: composeGatePreview)
            return
        }
        if let room = ownRooms.first(where: { L.feed.post.gateRoom(room: $0.label) == tier }) {
            setComposeGateRoom(room.room)
            return
        }
        setComposeGate(tier: tier, preview: composeGatePreview)
    }

    /// The `compose-gate-tier-select` picker's full option list, in display
    /// order: "Public" + `ownTiers`' names + `ownRooms`'s "Room: ‹label›"
    /// strings + "Sell this post…" always last. Shared by the macOS/iOS
    /// composer views' `automationSelect(options:)`, which duplicated this
    /// list verbatim before extraction.
    public var gateOptions: [String] {
        [L.feed.post.gatePublic] + ownTiers.map(\.name)
            + ownRooms.map { L.feed.post.gateRoom(room: $0.label) }
            + [L.feed.post.gateSell]
    }

    /// The sold post's machine-comparable asking price in sats. Empty OR
    /// unparseable ⇒ `nil`: the post carries no machine price, so a zap on it
    /// stays a tip (`monetization.md` § The asking price). The FFI face converts
    /// sats → msat internally; this VM only parses text → `UInt64`.
    ///
    /// Shared by ``submitPost()``'s two sell calls — `stage_sell_tier` (phase one
    /// of the mint) and `prepare_sell_post` (phase two) — because phase one
    /// decides the tier's rank and the two must agree.
    private static func askingPriceSats(_ sell: SellComposeState) -> UInt64? {
        let trimmed = sell.askingPrice.trimmingCharacters(in: .whitespaces)
        return trimmed.isEmpty ? nil : UInt64(trimmed)
    }

    /// Seal the held pick for the composer's staged audience, upload it, and
    /// stage the resulting hash on the manager. The ordering is the whole of it
    /// (`ui/media.md` § Encryption at rest); see ``submitPost()``'s call site.
    private func sealAndUploadPendingAttachment(_ manager: FfiFeedManager) async throws {
        guard let pending = pendingAttachment else { return }
        guard let api else { throw APIError.ffiError(L.common.notConnected) }

        // A SOLD post's photo seals under the tier the sale itself mints, and that
        // tier does not exist yet — so the mint splits in two and its first half
        // runs here, before the seal (`monetization.md` § Per-post pay-to-unlock).
        // Only when there IS an attachment: with none, `prepare_sell_post` runs it
        // inline and the single-call sell path is untouched. The arguments must
        // match the `prepare_sell_post` below, since phase one decides the rank.
        if let sell = composeSell {
            try await manager.stageSellTier(subscribersGetItFree: sell.subscribersGetItFree,
                                            askingPriceSats: Self.askingPriceSats(sell))
        }

        let prepared = try await manager.sealComposeAttachment(raw: pending.bytes)
        let hash = try await api.uploadPreparedBlob(primary: prepared.primary,
                                                    thumbnail: prepared.thumbnail)
        // The sealed class's sidecar says `application/octet-stream` by contract;
        // the real MIME rides inside the seal, so the `MediaItem` must take it
        // from the seal's own answer — never from the sidecar, and never from an
        // OS filename guess (which is what this path used before).
        let attached = AttachedFile(name: pending.name, size: UInt64(pending.bytes.count),
                                    blobHash: hash, mediaType: prepared.mediaType)
        let composeNow = manager.snapshot().compose
        manager.updateCompose(text: composeNow.text, tags: composeNow.tags, attachedFile: attached)
    }

    /// Submit the composed post (`post-submit-button`). A sold post (staged via
    /// `setComposeSell`) takes `prepareSellPost`; a tier-gated post (staged via
    /// `setComposeGate`) takes `prepareGatedBlob`; an ungated one takes the plain
    /// `submit_post`. **Sell is checked first** — `prepareGatedBlob` only ever
    /// looks at `gate_tier` (`nil` while selling), so checking it alone would
    /// silently drop a staged sell back to the plain, UNGATED path and publish
    /// the sold body in plaintext. Both gated paths finish through the identical
    /// upload + `submitGatedPost`/`abortGatedSubmit` pair below — mirrors linux
    /// `client.rs::submit_post`. The manager validates + builds + signs; errors
    /// land on `snapshot.compose.error` (→ compose-error).
    public func submitPost() async {
        composeGlueError = nil
        guard let manager else {
            // Should be unreachable — every submit surface disables itself on
            // `composeReady` — but a silent no-op here is exactly what e2e
            // point 11 forbids, so fail loudly rather than drop the post.
            preManagerComposeError = L.errors.feedNotReady
            return
        }
        // ── Seal the attachment for the staged audience, THEN upload it ──
        // `ui/media.md` § Encryption at rest: "The seal is resolved BEFORE the
        // attachment is uploaded, never after — this is a rule, not an
        // implementation detail." The audience itself is already staged: apple's
        // picker binds `setComposeGateSelection` live, so by the time submit runs
        // the manager holds the final gate/sell state. A public compose passes
        // through as plaintext, byte-identical to the pre-2026-09-08
        // `uploadBlob(.publicPost)` shape; an audience-restricted one is sealed by
        // shared Rust under this post's own `seal_id`, and the same id seals the
        // `TextWithMedia` body below — one key opens body and photo.
        if pendingAttachment != nil {
            do {
                try await sealAndUploadPendingAttachment(manager)
            } catch {
                // The seal or its upload failed — nothing was published and, for a
                // restricted post, nothing plaintext was uploaded either. Surface it
                // and keep the composer (text and pick both) for a retry.
                composeGlueError = DisplayError.message(error)
                return
            }
        }
        do {
            // `nil` ⇒ the composer isn't gated → the normal path; `Some(sealed)` ⇒
            // the manager has built + signed the gated (or sold) post and returns
            // its sealed full-body blob for us to upload before creating it.
            let sealed: Data?
            if let sell = composeSell {
                sealed = try await manager.prepareSellPost(
                    priceHint: sell.price.trimmingCharacters(in: .whitespaces).isEmpty ? nil : sell.price,
                    subscribersGetItFree: sell.subscribersGetItFree,
                    askingPriceSats: Self.askingPriceSats(sell))
            } else {
                sealed = try await manager.prepareGatedBlob()
            }
            guard let sealed else {
                try await manager.submitPost()
                pendingAttachment = nil
                return
            }
            guard let api else {
                manager.abortGatedSubmit(message: "No connection")
                return
            }
            do {
                // The sidecar class comes off the staged post — `GroupRestrictedPost`
                // for a room post, a tier's `PeriodRestrictedPost` otherwise — and is
                // never decided here (`FfiFeedManager.gatedUploadSidecar()`, in place
                // of the free `gatedPostSidecar()`, which knows only the tier class
                // and would mis-tag a room post's body). Mirrors linux's
                // `client.rs::submit_post` (`manager.gated_upload_sidecar()`).
                let hash = try await api.uploadGatedPostBlob(sealed: sealed, sidecar: manager.gatedUploadSidecar())
                try await manager.submitGatedPost(uploadedHash: hash)
                pendingAttachment = nil
            } catch {
                // The upload failed after the post was staged — drop it and surface
                // the reason on compose-error; the composer keeps its text for a retry.
                manager.abortGatedSubmit(message: DisplayError.message(error) ?? "")
            }
        } catch {
            // A validation/build error from `prepare_gated_blob` (empty teaser, no
            // key for the tier) is already on snapshot.compose.error; the banner
            // renders it. Same swallow as the ungated path above.
        }
    }

    /// Unlock a gated post's full body for an entitled reader (author custody, or
    /// a subscriber's KeyBlob wrap): resolve the sealed-blob hash, fetch the bytes
    /// over HTTP, and hand them to the manager, which decrypts + swaps the full
    /// body into the snapshot (`gated_unlocked`); the open detail repaints off the
    /// re-emit. Best-effort — a post under a rotated-out period the KeyBlob no
    /// longer carries stays locked (the archival path is a follow-on). Mirrors
    /// linux `client.rs::unlock_gated_post` (hash → `nest.get` → `unlock_gated_post`).
    public func unlockGatedPost(_ postId: String) async {
        guard let manager, let api else { return }
        guard let hash = await manager.gatedBlobHash(postId: postId) else { return }
        do {
            let bytes = try await api.get(url: api.blobUrl(hash: hash))
            try await manager.unlockGatedPost(postId: postId, blobBytes: bytes)
        } catch {
            // Best-effort stays best-effort — the post keeps its teaser — but the
            // REASON is not the shell's to discard. Every sibling logs it
            // (linux `client.rs::unlock_gated_post` tracing::debug, web
            // `+page.svelte::unlockGated` logMessage warn); apple alone swallowed
            // it silently, which cost a session its whole diagnosis: with no
            // record of this failure, a stray `hydrate_cues` error that the cue
            // shell had painted into the same page's `error-message` read as
            // this unlock's own reason and sent the investigation at the wrong
            // decrypt (2026-08-25).
            logMessage(level: .warn, target: "fauna.feed.gated",
                       message: "gated unlock (post stays teased): \(error)")
        }
    }

    // ── Feeds (create / delete; rules encoded by the shared codec) ──────────
    // `factors` is the `feed-factor-*` editor's staged entries
    // (content-moderation-and-ranking.md § Composition).
    public func createFeed(
        name: String, combination: String, rules: [FilterRuleInput],
        factors: [FactorWeightInput] = []
    ) async {
        creatingFeed = true
        defer { creatingFeed = false }
        do {
            _ = try await manager?.createFeed(
                name: name, rules: rules, combination: combination,
                scope: nil, contributorSeeds: nil, factors: factors)
            showCreateForm = false
        } catch { /* reason already on snapshot.error */ }
    }
    public func deleteFeed(id: String) async {
        try? await manager?.deleteFeed(feedId: id)
    }

    // ── Bridge feeds ────────────────────────────────────────────────────────
    public func subscribeBridge(bridge: String, feedUri: String, name: String) async {
        try? await manager?.subscribeBridge(kind: bridge, uri: feedUri, name: name)
    }
    public func unsubscribeBridge(id: Int64) async {
        try? await manager?.unsubscribeBridge(id: id)
    }

    // ── Lazy per-post resolution (media + quoted post — feed.md § read model) ─
    /// Resolve the first media blob hash for a `has_media` post into
    /// `PostSummary.media_hash`; the card paints when the snapshot notifies.
    public func resolveMedia(_ postId: String) async { await manager?.resolveMedia(postId: postId) }
    /// Project the embedded quoted-post card (`quoted-post`) for `quotedPostId` —
    /// shared `resolve_quoted_post`, rendered identically in list card + detail.
    public func resolveQuotedPost(_ quotedPostId: String) async -> QuotedPostView? {
        await manager?.resolveQuotedPost(quotedPostId: quotedPostId) ?? nil
    }
    /// Resolve a D4 `LinkPreview` block (render-model.md § D4): the manager fetches preview
    /// metadata once via `fauna.linkpreview.resolve` (cached per url) and folds
    /// `PreviewState::Resolved`/`Failed` onto the post's document; the card paints when the
    /// snapshot notifies. Mirrors `resolveQuotedPost`/`resolveMedia` (lazy-resolve→re-emit, the
    /// feed.md read-model pattern) — the og:image then loads through `blobURL`, reveal-gated.
    public func resolveLinkPreview(_ url: String) async { await manager?.resolveLinkPreview(url: url) }
    // `FfiFeedManager.resolvePostTips` is `payments`-feature-gated on the Rust
    // side (`monetization.md` § Tips) — the store-safe flavor's binding has no
    // such method at all, so the call site must be excised, not merely dead.
    #if !FAUNA_EXCISE_PAYMENTS
    /// Resolve this post's tip surface (`post-tip-total` / `post-tip-count` /
    /// `post-tip-list-button`) into `PostSummary.tips`; the card/detail repaints
    /// when the snapshot notifies. Fire-once by construction (the manager writes
    /// a view on every outcome, including "no tips") — mirrors
    /// `resolveMedia`/`resolveQuotedPost`/`resolveLinkPreview`.
    public func resolvePostTips(_ postId: String) async { await manager?.resolvePostTips(postId: postId) }
    #endif
    /// Resolve the buyer's price read for a sold post (gap (2c), `monetization.md` §
    /// Per-post pay-to-unlock → *the buyer's price read is post-addressed*) into
    /// `PostSummary.unlockOffer`; the card/detail repaints when the snapshot
    /// notifies. A no-op unless `gatedTier` names a `post-unlock-*` tier and the
    /// offer isn't already resolved (checked by the caller's fire-once trigger, not
    /// here). Mirrors `resolveMedia`/`resolvePostTips` — unlike tips, this face is
    /// NOT `payments`-feature-gated on the Rust side (`libs/fauna-ffi/src/
    /// feed_manager.rs`), since a sold post's own teaser exists in every flavor.
    public func resolvePostUnlockOffer(_ postId: String) async { await manager?.resolvePostUnlockOffer(postId: postId) }
    /// Buy the sold post's unlock tier off the resolved teaser offer
    /// (`gated-post-buy-button`) — the existing subscribe flow against the
    /// resolved offer's `tierName`, no new nest write. `nil` when the post isn't
    /// loaded or its offer hasn't resolved yet; throws on a real failure (an
    /// unreachable nest, a rejected subscribe) for the caller to surface.
    public func buyUnlockOffer(postId: String) async throws -> Bool? {
        try await manager?.buyUnlockOffer(postId: postId)
    }
    /// Reveal a post's blocked remote images (D3 — render-model.md § D3). The manager flips its
    /// in-memory per-post reveal set and re-emits the snapshot with `RemoteImage.revealed = true`
    /// for that post (covering both the list card and the detail, which read the same snapshot
    /// post); the card/detail repaints off it (no client-side reveal state). In-memory only — the
    /// no-persistence posture (html-mail.md § Rendering) holds.
    public func revealRemoteImages(_ postId: String) { manager?.revealRemoteImages(postId: postId) }
    /// Construct the download URL for a media blob from its hex hash (client glue;
    /// the blob byte fetch stays HTTP — `api-layers.md`). Delegates to the single
    /// `APIClient.blobUrl` so the canonical `GET /api/v1/blob/<hash>` path lives in
    /// one place (the nest serves blobs at the SINGULAR `/api/v1/blob/{id}` —
    /// `bins/fauna-nest` `blob_routes`; android `blobUrl`, web all match). Used for
    /// `post-image` media AND the D4 link-preview og:image (both content-addressed
    /// nest blobs).
    public func blobURL(_ hash: String) -> URL? {
        api?.blobUrl(hash: hash)
    }

    /// Decoded bytes for sealed post media, keyed by blob hash. Observed, so a
    /// landing fetch repaints the card that read a ``PostImageSource/sealedPending``.
    /// Dropped whenever the manager is rebuilt — see ``dropMediaCaches()``.
    private var sealedMediaImages: [String: FaunaPlatformImage] = [:]
    /// Completed `c2pa-badge` verdicts, keyed by blob hash — only ones that cost a
    /// full-image fetch to reach (see ``hasC2pa(_:)``), so a card scrolling back
    /// into view does not download its image again. Deliberately NOT observed: the
    /// badge receives its answer as ``hasC2pa(_:)``'s return value, and nothing
    /// paints from this dictionary. Dropped with the manager, like the images.
    @ObservationIgnored private var c2paVerdicts: [String: Bool] = [:]
    /// Hashes with a fetch in flight. Deliberately NOT observed: it is mutated
    /// from ``postImage(_:)``, which runs inside a SwiftUI `body`, and an observed
    /// write there would invalidate the very view being evaluated.
    @ObservationIgnored private var sealedMediaPending: Set<String> = []

    /// What the `post-image` surfaces should paint for one media hash — the
    /// two-shape branch `ui/media.md` § Encryption at rest declares for apple,
    /// living in the view model and never in the post card, which paints whatever
    /// source it is handed.
    ///
    /// A sealed item's bytes are fetched once per hash and opened through the
    /// shared manager; until they land the caller gets ``PostImageSource/sealedPending``
    /// and paints the placeholder, never the ciphertext. The cache contract is
    /// the conversations-attachment one: an item that does not open is NOT
    /// cached, so a later render retries (matching web's `mediaUrl`).
    public func postImage(_ hash: String) -> PostImageSource {
        let isSealed = manager?.isSealedMedia(blobHash: hash) ?? false
        let source = PostImageSource.resolve(isSealed: isSealed,
                                             blobURL: blobURL(hash),
                                             opened: sealedMediaImages[hash])
        if case .sealedPending = source { startSealedMediaFetch(hash) }
        return source
    }

    /// What one post's `post-image` slot paints, from its document: the blob
    /// image first (``postImage(_:)``), else a bridged post's `ProxiedImage`
    /// (``proxiedPostImage(_:)``), else `nil` — the precedence tui's
    /// `proxied_post_image` keeps (`render-model.md` § D6c). The one pick the
    /// list card and both post details share.
    public func documentPostImage(_ document: RenderDocument) -> PostImageSource? {
        if let hash = documentMediaImageHash(document) { return postImage(hash) }
        if let path = documentMediaProxiedImagePath(document) { return proxiedPostImage(path) }
        return nil
    }

    /// Decoded bytes for bridged post pictures, keyed by their nest-relative
    /// path. Observed, so a landing fetch repaints the card that read a
    /// ``PostImageSource/proxiedPending(_:)``. Dropped with the manager.
    private var proxiedMediaImages: [String: FaunaPlatformImage] = [:]
    /// Paths with a fetch in flight — unobserved for the reason
    /// ``sealedMediaPending`` is.
    @ObservationIgnored private var proxiedMediaPending: Set<String> = []

    /// What a bridged post's `ProxiedImage` paints (`render-model.md` § D6c): its
    /// bytes, fetched once per path from this nest with the session bearer —
    /// the same authorized GET a sealed blob takes, no open step (the nest
    /// proxies a third party's public picture) — and the path-labelled
    /// placeholder until they land. A fetch that fails or will not decode is not
    /// cached, so a later render retries.
    public func proxiedPostImage(_ path: String) -> PostImageSource {
        let source = PostImageSource.resolveProxied(path: path, signedIn: api != nil,
                                                    opened: proxiedMediaImages[path])
        if case .proxiedPending = source { startProxiedMediaFetch(path) }
        return source
    }

    private func startProxiedMediaFetch(_ path: String) {
        guard !proxiedMediaPending.contains(path), let api, let url = api.contentUrl(path: path) else { return }
        proxiedMediaPending.insert(path)
        // The bearer belongs to the session that asked; bytes landing after an
        // actor change are dropped with the rest of the media caches.
        let generation = managerGeneration
        Task { @MainActor [weak self] in
            defer { self?.proxiedMediaPending.remove(path) }
            guard let bytes = try? await api.get(url: url),
                  let self, generation == self.managerGeneration,
                  let image = FaunaImage.decode(bytes) else { return }
            self.proxiedMediaImages[path] = image
        }
    }

    /// Fetch and open one sealed media item. Kicked from ``postImage(_:)`` rather
    /// than a view's `.task(id:)`: a `.task` attached to a conditionally-absent
    /// view does not reliably fire on apple (the same trap `PostImageC2paBadge`
    /// works around with a zero-size anchor).
    private func startSealedMediaFetch(_ hash: String) {
        guard !sealedMediaPending.contains(hash), let api else { return }
        sealedMediaPending.insert(hash)
        // The manager the answer belongs to. An image opened under one actor's
        // per-post key must never land in the next actor's cache, so a fetch that
        // outlives its manager is dropped (windows keys its bitmap cache to the
        // manager instance for the same reason).
        let generation = managerGeneration
        Task { @MainActor [weak self] in
            defer { self?.sealedMediaPending.remove(hash) }
            do {
                let bytes = try await api.get(url: api.blobUrl(hash: hash))
                guard let self, generation == self.managerGeneration, let manager = self.manager else { return }
                // `nil` means the item did not open — leave it uncached and paint
                // the placeholder, exactly as for bytes that will not decode.
                // Never hand AEAD ciphertext to an image decoder.
                guard let opened = manager.openMediaBytes(blobHash: hash, fetched: bytes),
                      let image = FaunaImage.decode(opened) else { return }
                self.sealedMediaImages[hash] = image
            } catch {
                // The card stays on its placeholder; a later render retries.
            }
        }
    }

    /// The hardened temp files opened sealed videos play from, keyed by blob hash,
    /// so a second tap does not fetch and open the blob again. Unobserved —
    /// nothing paints from it. Deleted with the manager, like the images.
    @ObservationIgnored private var sealedPlaybackFiles: [String: URL] = [:]

    /// What a tapped `video-thumbnail` plays (`render-model.md` § D6c → *Inline
    /// playback*), as a URL `AVPlayer` opens — `nil` when nothing plays. The
    /// decision is the shared manager's `playbackSource`; this only turns its
    /// answer into a URL, as web's `playbackUrl` does: a nest-relative path gets
    /// the nest origin (the blob route is unauthenticated by design, so the
    /// player needs no bearer), and a sealed item goes through the same fetch +
    /// `openMediaBytes` a sealed image does — the raw opened bytes, written to an
    /// owner-only temp file, since no URL can serve its plaintext.
    public func videoPlaybackURL(_ hash: String) async -> URL? {
        guard let manager, let api else { return nil }
        // The answer belongs to the manager that gave it (see
        // ``startSealedMediaFetch(_:)``): one outliving its actor is dropped.
        let generation = managerGeneration
        let source = await manager.playbackSource(block: .video(hash: hash, alt: ""))
        guard generation == managerGeneration else { return nil }
        switch VideoPlaybackPlan(source) {
        case .stream(let path):
            return api.contentUrl(path: path)?.absoluteURL
        case .openSealed(let sealedHash):
            if let file = sealedPlaybackFiles[sealedHash] { return file }
            // `nil` from the open means the item did not open: never hand AEAD
            // ciphertext to a decoder.
            guard let bytes = try? await api.get(url: api.blobUrl(hash: sealedHash)),
                  generation == managerGeneration,
                  let opened = manager.openMediaBytes(blobHash: sealedHash, fetched: bytes),
                  let file = try? SealedPlaybackFile.write(opened) else { return nil }
            sealedPlaybackFiles[sealedHash] = file
            return file
        case .unplayable:
            return nil
        }
    }

    /// Drop every opened sealed image and every remembered `c2pa-badge` verdict.
    /// Called wherever the manager is rebuilt — the keys that opened these bytes
    /// belong to the actor that just left, and a verdict is derived from the bytes
    /// they opened.
    private func dropMediaCaches() {
        sealedPlaybackFiles.values.forEach(SealedPlaybackFile.remove)
        sealedPlaybackFiles.removeAll()
        sealedMediaImages.removeAll()
        sealedMediaPending.removeAll()
        proxiedMediaImages.removeAll()
        proxiedMediaPending.removeAll()
        c2paVerdicts.removeAll()
    }

    /// The `c2pa-badge` gate ``PostImageC2paBadge`` calls: the VIEWER's verdict
    /// over the blob's bytes, never the uploader's word (`ui/media.md` § C2PA
    /// provenance — the badge-correction rule). Two stages, as tui's
    /// `Op::FetchC2pa`:
    ///
    /// 1. ``APIClient/hasC2paAssertion(hash:)`` — a HEAD reading `x-c2pa`, which
    ///    is only what the uploader claimed (the nest never inspects the bytes).
    ///    `false` ends the check with nothing fetched, which is the answer for
    ///    essentially every post.
    /// 2. For the few that claim provenance, GET the blob and ask
    ///    ``C2paBadgeVerdict/over(_:open:detect:)``, opening the bytes through the
    ///    shared manager first. This is the extra full-image fetch `media.md`
    ///    accepts for a shape-2 app, made solely for the badge — the image itself
    ///    still renders from its URL (``postImage(_:)``), so the two-shape rule
    ///    is untouched.
    ///
    /// Every failure — no `api` (pre-auth), a HEAD or GET that fails, a body the
    /// manager will not open, a manager rebuilt mid-flight — is "no badge", the
    /// same as a resolved `false`. Only a completed verdict is remembered, so a
    /// failure is retried by the next render rather than pinned.
    public func hasC2pa(_ hash: String) async -> Bool {
        if let known = c2paVerdicts[hash] { return known }
        guard let api, await api.hasC2paAssertion(hash: hash) else { return false }
        // The manager the answer belongs to — an actor change rebuilds it, and the
        // previous actor's keys must not decide the next one's badge (the same
        // guard `startSealedMediaFetch` keeps).
        let generation = managerGeneration
        guard let bytes = try? await api.get(url: api.blobUrl(hash: hash)),
              generation == managerGeneration, let manager
        else { return false }
        // Off the main actor: the parse walks the whole image, and it runs once
        // per claimed-provenance card (`FfiFeedManager` is `Sendable`).
        let verdict = await Task.detached(priority: .utility) {
            C2paBadgeVerdict.over(bytes, open: { manager.openMediaBytes(blobHash: hash, fetched: $0) })
        }.value
        guard generation == managerGeneration, let verdict else { return false }
        c2paVerdicts[hash] = verdict
        return verdict
    }

    /// The user's trained-topic-factor registry rows (topic-factors.md §
    /// Authoring surface & picker) — feeds both the post-card train-target
    /// sheet (`FeedPostActionsButton`) and the create-feed `feed-factor-select`
    /// picker's third source. Best-effort: an empty result on failure (mirrors
    /// windows' `PopulateTrainTargetComboAsync`/`AppendTrainedTopicFactorsAsync`
    /// — a fetch fault just leaves the picker/sheet short, never blocks the page).
    public func trainedFactorRows() async -> [FfiTrainedTopicRow] {
        guard let api else { return [] }
        return (try? await api.trainedTopicsList()) ?? []
    }

    // ── Interaction bar (client glue — NOT a manager action) ────────────────
    /// The last failure a feed verb (like / repost / reply / quote) landed on
    /// the page's `error-message`, so the verb's next success clears exactly
    /// that and never a sibling writer's error.
    @ObservationIgnored private var verbErrorMessage: String?

    /// Run one interaction-bar verb, and land its failure on the page's
    /// `error-message` (web's `verbErrorCopy`, linux's `FailedLocalized`). A
    /// **stated refusal** — words under a restricted post (`ui/feed.md` §
    /// Encryption at rest → *A reply, quote or repost of a restricted post*,
    /// ruling 6) — reads in the user's language, recognized by the one shared
    /// `feedRefusalI18nKey`; every other failure keeps its own text. The
    /// account-switch guard is `landClientErrorMessage`'s.
    private func runVerb(_ verb: (FfiFeedManager) async throws -> Void) async {
        guard let manager else { return }
        let generation = managerGeneration
        do {
            try await verb(manager)
            if generation == managerGeneration, let shown = verbErrorMessage,
               clientErrorMessage == shown
            {
                setClientErrorMessage(nil)
            }
            verbErrorMessage = nil
        } catch {
            let text: String
            if let ffi = error as? FfiError, case let .General(msg) = ffi {
                text = msg
            } else {
                text = String(describing: error)
            }
            let shown = feedRefusalI18nKey(err: text).map(L.lookup) ?? text
            if landClientErrorMessage(generation: generation, message: shown) {
                verbErrorMessage = shown
            }
        }
    }

    /// Like / un-like — one verb, toggle semantics off the target row's
    /// `viewer_liked` (`feed.md` § Interaction bar). **Call this, not
    /// `interact(id, "like", nil)`** — that call is one-way: the nest's like
    /// arm is idempotent per (actor, post), so a second tap moved nothing and
    /// a like could never be taken back. A failure reaches `error-message`
    /// (`runVerb`).
    public func likePost(postId: String) async {
        await runVerb { try await $0.like(postId: postId) }
    }
    /// Repost / un-repost — one verb, toggle semantics off the target row's
    /// `viewer_repost_id` (`feed.md` § Interaction bar → Repost, ratified
    /// 2026-08-10). **Call this, not `interact(id, "repost", nil)`** — that
    /// call only folds the nest's post-act counters back into the snapshot,
    /// it never composes the caller's `Reference::Repost` post, so the
    /// toggle never had anything to turn off (the count could move but no
    /// repost ever existed). Mirrors `likePost` above.
    public func repostPost(postId: String) async {
        await runVerb { try await $0.repost(postId: postId) }
    }

    // Reply/quote are COMPOSED posts, not one-way interact calls: the nest's
    // native `interact` arm discards `body` entirely for a native post (it
    // only returns target info so the client can compose — `FeedManager::
    // reply`'s own doc), so `interact(id, "reply"/"quote", body)` accepted a
    // typed reply/quote and silently dropped it (feed.md § Implementation
    // status today). Call the manager's dedicated `reply`/`quote` doors
    // instead, which actually create the referencing post — matching
    // tui/linux/web/android's shape. Never call `interact` to compose on a
    // native post; a bridged post still routes through it internally (the
    // manager decides, so the app leg doesn't need to know the source), and
    // the feed is deliberately NOT reloaded here (that would re-rank the
    // timeline under the user's finger — feed.md § Implementation status
    // today). A refused reply (words under a restricted post) says so on
    // `error-message` (`runVerb`).
    public func replyToPost(postId: String, body: String) async {
        await runVerb { try await $0.reply(postId: postId, body: body) }
    }
    /// Quote — a repost *with* commentary (`Reference::Quote`), composed via
    /// `FeedManager::quote`. `body` may be empty (§ Interaction bar's ratified
    /// direct quote-repost — the commentary composer is a deferred fleet-wide
    /// follow-on).
    public func quotePost(postId: String) async {
        await runVerb { try await $0.quote(postId: postId, body: "") }
    }

    /// Destroy the caller's own post (`feed-post-delete-confirm-button`; feed.md
    /// § State & data shape → Post deletion). Routes through the FeedManager-level
    /// call (`FfiFeedManager.deletePost`, not the raw `PostsClient` wrapper) — it
    /// signs the `Tombstone` and drops the post from the loaded window on success,
    /// re-emitting the snapshot. Silent swallow on failure, matching
    /// `FeedPostActionsButton.dispatchTrain`'s existing per-card gestures —
    /// there is no dedicated per-card error slot to surface it in.
    public func deletePost(_ postId: String) async {
        try? await manager?.deletePost(postId: postId)
    }

    /// Through the manager (`FfiFeedManager.interact`), not the raw
    /// `APIClient.interactWithPost` — same rule as `deletePost` above. The
    /// interaction bar's four counts render from the manager snapshot
    /// (`PostSummary.{like,reply,repost,quote}_count`, feed.md § Interaction
    /// bar) and only the manager writes the nest's post-act counters back into
    /// it, so the tapped count moves at once instead of waiting for an unrelated
    /// reload. Silent swallow on failure, matching `deletePost`.
    private func interact(_ postId: String, _ action: String, _ body: String?) async {
        do { try await manager?.interact(postId: postId, action: action, body: body) }
        catch { return }
        // A reply/quote creates a new post; re-query the current feed so it shows.
        // (The counts are already folded in above — this is about the NEW post
        // appearing, which is a different fact.) `refreshCurrentFeed` rather than
        // `selectFeed(selectedFeedId)`: the latter is nil for both Local and
        // Trending, so it silently drops a Trending viewer into Local
        // (trending.md § The Trending feed) and did nothing at all on Local.
        if action == "reply" || action == "quote" {
            await manager?.refreshCurrentFeed()
        }
    }
}

/// Trampoline conforming to UniFFI's `FeedSnapshotObserver` (distinct from
/// conversations' `SnapshotObserver`). The manager takes the observer via
/// `addObserver` before `FeedVM`'s `self` is fully initialized — late-binding via
/// `target` lets us avoid that ordering, the same way `ConversationsObserverBox` does.
final class FeedObserverBox: FeedSnapshotObserver, @unchecked Sendable {
    weak var target: FeedVM?
    func onChanged() {
        notifyOnMainActor(target) { $0.onManagerChanged() }
    }
}

/// Re-reads the feed's room options on every conversations-plane tick — the
/// list is a projection of that plane (`ui/feed.md` § Encryption at rest →
/// *Room-restricted — the app half*, "The rooms offered"), so a room joined,
/// bound or left reaches `compose-gate-tier-select` without re-entering the
/// Feed page. Registered on the CONVERSATIONS manager (a second, independent
/// observer alongside `ConversationsObserverBox`, which it notifies — the
/// conversations manager's `add_observer` takes a `Vec`) by
/// `FeedVM.installRoomPostKeys`. Mirrors linux's
/// `conv_backend::attach_own_rooms_refresh` / web's `syncFeedRoomPosts`.
final class FeedRoomPostObserverBox: SnapshotObserver, @unchecked Sendable {
    weak var target: FeedVM?
    func onChanged() {
        notifyOnMainActor(target) { $0.refreshOwnRooms() }
    }
}

// Snapshot row types are UniFFI-generated structs; give them stable identities so
// SwiftUI `ForEach` / `.sheet(item:)` can key on them. `BridgeFeedView` already
// carries an `id`, so it only needs the conformance declaration.
extension PostSummary: @retroactive Identifiable { public var id: String { postId } }
extension FeedSummaryView: @retroactive Identifiable { public var id: String { feedId } }
extension BridgeFeedView: @retroactive Identifiable {}
