import Foundation
import SwiftData

/// The **FaunaKit-owned half** of apple's in-memory actor-scoped state — the
/// surfaces macOS and iOS share, dropped from one place so the two targets cannot
/// drift from each other.
///
/// `account-scoping.md` § The scoping taxonomy binds this: the switch/sign-out
/// isolation contract forbids account B rendering or modifying account A's local
/// state, and its in-memory corollary makes a live cache or a running timer
/// account-scoped by class 1/4 exactly as its on-disk twin would be. Neither apple
/// target exits the process on an actor change — `tearDownSessionForSwitch()` tears
/// down in place and every identity transition re-enters via `runLaunch()` — so
/// nothing here is dropped for us by a shell teardown.
///
/// ── Why one function rather than a list per teardown site ─────────────────────
///
/// Before this type the drop was **hand-listed at four sites per target**
/// (`tearDownSessionForSwitch`, `resetToFactory`, `logoutKeepData`,
/// `factoryResetReonboard`) — eight lists — and they had already drifted apart,
/// which is the failure mode rather than an accident of maintenance. Linux hit the
/// identical problem at six sites and ruled the same shape
/// (`apps/fauna-linux/src/actor_scope.rs`): **one explicit, statically greppable
/// function that every teardown site calls**, deliberately not a registration
/// registry — a registry only relocates the hand-list and adds a *silent* failure
/// mode when a registration is forgotten. Adding actor-scoped FaunaKit state means
/// adding one line to ``resetSharedState()`` and nowhere else.
///
/// ── Why apple needs no generation counter ─────────────────────────────────────
///
/// Linux pairs its drop with an actor-generation counter because a
/// `glib::timeout_add_local` tick captures its client and runs forever, so a
/// latched arm-flag would leave the outgoing actor's tick firing beside the
/// incoming one's. Apple's two cadences are *objects* that own their loop as a
/// `Task`, so ``stop()`` cancels the captured work outright and there is no
/// generation to retire.
///
/// That cancellation is not optional, though, and one of the two makes it
/// load-bearing: ``DnsAutoRenewCadence/start(api:)`` is latched
/// (`guard loop == nil`), so a teardown that skips ``stop()`` leaves the cadence
/// **permanently** bound to the outgoing `APIClient` — the next login's `start`
/// silently no-ops. That is the apple twin of linux's never-reset `AtomicBool`
/// finding.
///
/// The app-owned half of the drop (`criticalAlertsHost`, `conversationsVM.manager`,
/// `modelContainer` and the `AppState`/`MacAppState` caches) is held per-target
/// only because a SwiftUI `App` struct constructs its own state — every value is
/// already an identically-typed FaunaKit object or shared model type, so
/// ``dropAppOwnedState(criticalAlertsHost:conversationsVM:feedVM:eventsVM:devicesVM:screenTime:modelContainer:appState:)``
/// below shares the drop itself; each target's `dropActorScopedState()` calls
/// ``resetSharedState()`` then that. Site-specific work — `client?.shutdown()`,
/// agent unprovisioning, session-field nils, navigation resets, keychain and
/// wizard state — stays at its own site: this type drops *state*, exactly as
/// linux's `reset_actor_scoped_state` does.
///
/// ── State a *view* owns is not on this list — and where its guarantee comes from ──
///
/// Everything above is state the app shell or a FaunaKit static holds, which a
/// teardown function can reach. A **page-owned view model** (`@State private var vm =
/// …VM()` inside a view) cannot be reached by any list: it dies only when SwiftUI
/// unmounts its view. That makes its drop ride a teardown, which
/// `account-scoping.md` § The scoping taxonomy (the in-memory corollary) allows only
/// where the app *says where the guarantee comes from*. This is that statement, once —
/// the ~20 view models whose `configure(api:)` guards on `machine == nil` and/or
/// `configuredApi !== api` (`MailSpamVM`, `LinkedNestsVM`, `AdminMailVM`, …) carry no
/// copy of it.
///
/// * **macOS** — `tearDownSessionForSwitch()` sets `isOnboarded = false`, which
///   unmounts `MainWindowView` wholesale (`ContentView`'s `.ready && isOnboarded`
///   branch), so every page-owned view model below it dies with the window; the
///   teardown then lands the incoming identity on its default view besides. Nothing on
///   macOS needs a per-view seam.
/// * **iOS** — the shell is *reused*: `isOnboarding` stays false, so `MainTabView`
///   stays mounted, and only part of what hangs off it is unmounted.
///   - **Admin pages** — `inAdmin = false` swaps `AdminShellView` for the tab shell.
///     Unmounted.
///   - **Settings sub-pages** — `selectedSettingsPage = nil` pops the value-based
///     pages. Unmounted; this is the guarantee behind the `configure(api:)` guards of
///     every view model a Settings or admin page owns. (A closure-destination
///     `NavigationLink` is not on that path and is not popped — Settings → Backups.)
///   - **More destinations and the three tabs are NOT unmounted.**
///     `tearDownSessionForSwitch()` never clears `moreSelectedView` or `selectedTab` —
///     a switch keeps the user in More → Settings, where the account switcher lives
///     and where `test_account_switcher_apple.py` waits for the admin entry — and a
///     tab is never unmounted at all. A page-owned view model there outlives the
///     switch holding the outgoing account's data, so it owns the seam itself: an
///     identity-keyed `reset()` on the view model, called from `configure` on a
///     different `APIClient` **before** it rebuilds and from the page's nil-client
///     phase (`SessionKey`), plus an identity re-check after every `await` that lands
///     state.
///
///     **The whole surviving set, audited page by page** — the three tabs and every
///     More destination, which is every page this shell keeps mounted
///     . A view model
///     here is listed with the seam it owns, or with why it owes none; nothing on a
///     surviving page is left to inference:
///
///       * `SearchVM` — shell-level. Identity-keyed `reset()`.
///       * `MediaMachineVM`, `BackupsMachineVM`, `AddressBookVM` — More → Media /
///         Backups (also reachable from Settings through a closure-destination link
///         the `selectedSettingsPage` reset does not pop) and the Contacts tab
///        . Identity-keyed `reset()`.
///       * `ContactsVM`, `NotificationsVM`, `StatusVM`, `BridgeManagerVM`,
///         `ModerationQueueVM`, `FamilyVM`, `SubscriptionsVM`, `ProfileOffersVM`
///         — the Contacts tab and More → notifications / settings /
///         bridges / moderation / family / profile.
///         Identity-keyed `reset()`. Every one of them loaded from a bare one-shot
///         `.task {}` or a `.task(id:)` keyed on `navGeneration`/`viewedActorId`
///         rather than the client, so before this sweep none of them re-loaded at a
///         switch at all: the outgoing account's data simply stayed on screen, and
///         `configure`'s unconditional `self.api = api` paired the incoming
///         account's api with it — the finding, nine more times.
///       * `ConversationsListView`'s `path` — not a view model but page-owned
///         `@State` on a tab, and a `Route.thread(<id>)` names the OUTGOING
///         account's thread. Dropped on the same `SessionKey` phase the view models
///         use. (Its `ConversationsVM` is environment-held, so like `feedVM` below
///         it rides the app-owned drop, not a page seam.)
///       * `FeedVM` — **NOT on this list, and the correction is worth keeping:**
///         The sweep first filed it here as the Feed *tab*'s view model, but
///         `feedVM` is App-scene-level `@State` injected through `.environment`
///         (`FaunaApp`/`FaunaMacApp`), which `FeedListView` then reads with
///         `@Environment(FeedVM.self)`. That makes it *reachable* from a teardown
///         function, so by this section's own opening test it belongs on the
///         app-owned drop above — beside `conversationsVM`, which is the identical
///         shape — and its `reset()` is called from
///         ``dropAppOwnedState(criticalAlertsHost:conversationsVM:feedVM:eventsVM:devicesVM:screenTime:modelContainer:appState:)``.
///         linux reaches the same conclusion from the same premise
///         (`actor_scope.rs`'s `feed::host::clear()`, on its canonical list for the
///         same stated reason), so this is the cross-app shape. The lesson for the
///         next reader: *"which page renders it"* is the wrong question here — ask
///         *"can a teardown function reach it"*, which is what the opening
///         paragraph of this section actually says . ⚠ And note what that means for the macOS bullet
///         above: the wholesale `MainWindowView` unmount does **not** cover
///         `feedVM`, because the scene outlives the window shell — so macOS was
///         exposed here exactly as iOS was, and it is the app-owned drop, not the
///         unmount, that is its guarantee for this one.
///       * `EventsVM` — **moved off this list 2026-09-25**, the `FeedVM` shape
///         below it: App-scene-level `@State` read with `@Environment(EventsVM.self)`,
///         so a half-written event survives leaving the Events page (a page-owned VM
///         died with the page and the next one's launch restore raced its pending
///         save). Its `reset()` now rides the app-owned drop; `configure`'s
///         `!==` guard stays as the page-level seam it always was.
///       * `AccountSwitcherVM` — More → Settings, and owes NO seam: it reads the
///         install-scoped account index (`FaunaAccounts.registry()`), holds no
///         `APIClient` and no account-scoped datum, and listing every account on the
///         device is its whole job. Stated here so its one-shot `.task` is not
///         mistaken for the same omission as the nine above.
///
///     **The one seam that was weighed and REJECTED: keying the shell itself.**
///     `MainTabView`'s `TabView` could carry the session identity in its `.id(…)`
///     — it already carries `.id(appState.session.recoveryCustodyWarning != nil)`
///     for a layout re-measure, so one rebuild per session is already accepted —
///     which would unmount every page-owned view model at the identity change and
///     make macOS's wholesale guarantee true here by construction, in three lines
///     instead of the roster above. `account-scoping.md`'s in-memory corollary
///     permits that *form* (a teardown that genuinely IS guaranteed and IS keyed on
///     the identity). Two of the same corollary's rules refuse it, and they are why
///     it must not be re-proposed as a shortcut for the next page:
///       1. **Dropping state is only half of it — the background loops that WRITE
///          that state must be retired by the same drop** (corollary 2's second
///          rule). SwiftUI cancels a view's `.task`, but a view model that spawned
///          a detached `Task` capturing `self` outlives its own view and keeps
///          acting as the outgoing identity: `EventsVM.pollWhileVisible()` and the
///          drafts rails are exactly that shape, and the events rail is the sharp
///          case — a stale `FfiEventDraftsSync` makes the app delegate's
///          leave-flush write the INCOMING account's typed draft into the outgoing
///          account's rail. An unmount makes such state unrenderable; it does not
///          retire it. Only the view model's own `reset()` cancels the work.
///       2. **The failure is silent by construction — stale actor-scoped state
///          renders as empty or plausible, never as an error — so it needs a
///          red-first test at the identity change, not an inspection** (corollary
///          2's third rule). "SwiftUI unmounted this view" is not assertable in a
///          FaunaKit unit test; `reset()` and `configure`'s guard are, and each pin
///          reds when its own guard is removed. The shell key would trade a dozen
///          cheap mutation-verified pins for one untestable claim.
///     So the shell key would *appear* to close this section while leaving the loop
///     half open and the whole guarantee unpinned. Rejected on those two grounds —
///     which is also why the visible-rebuild cost never had to be judged: it is not
///     what decides this.
///
/// The test for a new page-owned view model is one question — *can its page stay
/// mounted through an identity change?* One under a Settings or admin page needs no
/// seam; one under a More destination or a tab owes it, and the answer is its own
/// `reset()`, never a key on the shell (the rejection above). Note what the boundary
/// is NOT: every view model in the roster above has a `configure(api:)`, so the
/// method's presence sorts nothing. What the twenty-one Settings/admin view models
/// carry and these did not is the `machine == nil` / `configuredApi !== api` *guard*
/// — and even that is only convenience for a same-page re-point, never the
/// guarantee. The guarantee is the page's mount lifetime, which is why this section
/// enumerates pages rather than methods.
@MainActor
public enum ActorScope {
    /// Drop every piece of in-memory actor-scoped state FaunaKit owns. **The**
    /// canonical list for the shared half — no teardown site hand-lists any of it.
    public static func resetSharedState() {
        // Process singletons bound to the OUTGOING `api`. Left running they would
        // keep renewing DNS and polling subscriptions as the previous identity;
        // `completeAuthenticatedLaunch` restarts them against the new `api`, which
        // for the DNS cadence only works because it was stopped here first.
        DnsAutoRenewCadence.shared.stop()
        SubscriptionsAuthorCadence.shared.stop()
        // Guardian Notify (family-safety.md § Guardian Notify) — drops the
        // pending count batch AND the per-item dedup set, not just the loop:
        // carrying the dedup set across a switch would make the incoming
        // ward's first enforcement on a reused item id silently UNcounted
        // (an under-report to their guardian indistinguishable from "nothing
        // happened"), the same sharp edge android's `FamilyNotifyStore.reset`
        // calls out.
        GuardianNotifyCadence.shared.stop()
        // The home-screen widget's count (apps/common.md § Home-screen widget):
        // CLEAR the snapshot, so the outgoing account's number never stays on
        // the home screen (android's `refoldForAccountSwitch`).
        WidgetUnreadPublisher.shared.clear()

        // Shared statics keyed to the outgoing identity. Read by the views and by
        // the test agent's state serialization, so a survivor reports the departed
        // account's posts as the current one's (e2e-conventions.md § point 11: a
        // wrong answer from the agent reads downstream as a product bug).
        FeedVM.lastLoadedPosts = []
        FeedVM.revealedMutedPostIds = []
    }

    /// The app-owned half of the drop — was a byte-identical per-target twin
    /// (both callers' own doc comments cross-referenced each other) until this
    /// harvest pass found it . Every
    /// value here is an identically-typed FaunaKit object or shared model
    /// type on both `AppState` and `MacAppState` (no common protocol between
    /// the two classes otherwise), so the drop itself needed only
    /// ``ActorScopedAppCaches`` below to name their four shared cache fields.
    /// Callers pass their own `dropActorScopedState()`'s `appState.screenTime`
    /// and `appState` directly. `newModelContainer` defaults to the real
    /// `PhotoBackupRecord.buildModelContainer(actorIdHex: nil)` both app
    /// shells actually want — overridden only by `ActorScopeTests`, which
    /// swaps in an in-memory throwaway so the test touches no real
    /// `AccountStateDir` file. That seam is what makes this function
    /// unit-testable at all: everything else here is a plain FaunaKit object
    /// or model type, unlike ``resetSharedState()``'s siblings' need for a
    /// full SwiftUI `App` — closing the gap `ActorScopeTests.swift`'s own
    /// scope note names for the app-owned half.
    @MainActor
    public static func dropAppOwnedState<Caches: ActorScopedAppCaches>(
        criticalAlertsHost: CriticalAlertsHost,
        conversationsVM: ConversationsVM,
        feedVM: FeedVM,
        eventsVM: EventsVM,
        devicesVM: DevicesMachineVM,
        screenTime: ScreenTimeStore,
        modelContainer: inout ModelContainer,
        appState: Caches,
        newModelContainer: () -> ModelContainer = { PhotoBackupRecord.buildModelContainer(actorIdHex: nil) }
    ) {
        // An alert keyed to the departing identity's DID must not outlive it
        // (critical-alerts.md § Mechanism → *Lifetime*; also stops the sweep loop,
        // `CriticalAlerts::teardown_epoch`).
        criticalAlertsHost.clearAll()

        // The PRODUCTION identity-scoped wipe, not the `_for_test` seam (a thin
        // alias for it — `manager.rs::clear_for_test`): the manager is an
        // identity-scoped singleton that outlives this teardown, so skipping it
        // renders the outgoing account's threads and drafts to the incoming one.
        conversationsVM.manager.clearForIdentityChange()

        // …and the session itself, which the wipe above deliberately does not
        // touch (it preserves registered rails). Dropping it ends the outgoing
        // actor's shared-Rust receive loop on the session-closed *event* —
        // corollary 2's "the background loops that WRITE that state must be
        // retired by the same drop" (`account-scoping.md` § The scoping
        // taxonomy). Apple had no conversations-session teardown at all until
        // this line; android's `stopConversationsSession` is the twin.
        conversationsVM.deactivate()

        // The Feed page's view model, app-scene-level `@State` injected through
        // `.environment` exactly as `conversationsVM` is — so it is reachable from
        // here, and belongs on this list rather than behind a page seam. Its
        // `reset()` names what it drops and why; the headline is that its
        // `configuredSecret` early-return otherwise let a same-actor re-claim keep a
        // manager built on the discarded `APIClient` .
        feedVM.reset()

        // The Events page's view model — app-scene-level `@State` like `feedVM`, so a
        // half-written event outlives leaving the page. Its `reset()` detaches the
        // drafts rail first (`attachEventDrafts(nil)`), which cancels the outgoing
        // actor's in-flight restore/save and the leave-flush's target with it.
        eventsVM.reset()

        // The session's one Devices/Folders view model — app-scene-level like
        // `feedVM`, so its machine (and the followed-folders memory and refresh
        // counts inside it) outlives a page visit. Dropping it here is what keeps
        // that memory from outliving the actor it was read for.
        devicesVM.reset()

        // Drop the outgoing ward's screen-time lock/accrual (family-safety.md §
        // Screen time) — without this a new account inherits the previous
        // ward's lock, naming a guardian the user does not have.
        screenTime.stop()

        // Drop the outgoing actor's serving origin, published-posts list and
        // `FfiWebClient` — without this the feed ⋯-menu's own-post copy-link
        // and Settings → Web both keep answering for the outgoing actor until
        // Settings → Web is opened again for the incoming one
        // (`web-content-hosting.md` § Published-post management). Reached through the protocol so this one
        // call drops BOTH shells' `webPublish` field, matching
        // `liveModelContainer` below.
        appState.webPublish.reset()

        // Drop the outgoing actor's photo-backup container. A switch's incoming
        // actor gets its own next (`completeAuthenticatedLaunch`); sign-out's
        // `AccountStateDir.eraseAll` has already erased the file this one pointed
        // at, so releasing the reference is what lets the next login open a fresh
        // container rather than silently reusing this handle.
        modelContainer = newModelContainer()
        // …and the live slot beside it. A teardown that replaced only the
        // `@State` half would leave the outgoing actor's container as the one
        // every callback-reached read resolves to — the slot exists precisely
        // because those reads cannot see the `@State` one.
        appState.liveModelContainer = nil

        // Read by the test agent's state serialization (`data.*` readers).
        // Survivors report the departed account's contacts, knocks and events
        // as the current one's.
        appState.lastContacts = []
        appState.lastKnocks = []
        appState.lastEvents = []
        appState.notificationsUnreadCount = 0
    }
}

/// The `AppState`/`MacAppState` slots ``ActorScope/dropAppOwnedState(criticalAlertsHost:conversationsVM:feedVM:eventsVM:devicesVM:screenTime:modelContainer:appState:)``
/// resets — the four e2e-serialization caches plus the live photo-backup
/// container — present by the same name/shape on both classes (no common
/// protocol between them otherwise). Conformed retroactively purely to name
/// the shape once; see ``MachineSnapshotWithError``/``MailboxToggleOption``
/// for the same idiom elsewhere in FaunaKit.
public protocol ActorScopedAppCaches: AnyObject {
    var lastContacts: [Contact] { get set }
    var lastKnocks: [Knock] { get set }
    var lastEvents: [EventSummary] { get set }
    var notificationsUnreadCount: Int { get set }
    /// The live photo-backup container slot (see either class's own
    /// `liveModelContainer`). Named on the protocol rather than passed as a
    /// second `inout`, so the drop reaches BOTH halves of the pair through one
    /// argument and neither shell can forget its own.
    var liveModelContainer: ModelContainer? { get set }
    /// The shared `fauna.web.*` published-post surface cache (`AppState`/
    /// `MacAppState`'s `webPublish`) — named here so `dropAppOwnedState`
    /// reaches both shells' `WebPublishStore.reset()` through this ONE
    /// protocol, exactly as `liveModelContainer` above does for the
    /// photo-backup slot.
    var webPublish: WebPublishStore { get }
}

extension AppState: ActorScopedAppCaches {}
