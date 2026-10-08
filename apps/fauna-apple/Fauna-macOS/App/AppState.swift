import SwiftUI
import SwiftData
import FaunaKit

/// Single door for the dock-icon-visibility default, read by `AppDelegate`
/// (before `MacAppState` exists — `applicationDidFinishLaunching` sets
/// `NSApp.activationPolicy` first) and by `MacAppState.dockIconVisible`
/// (the persisted door once the app is up), so the key string and its
/// `true` fallback live in exactly one place.
enum DockIconPreference {
    static let key = "fauna.showDockIcon"
    static var current: Bool {
        get { UserDefaults.standard.object(forKey: key) as? Bool ?? true }
        set { UserDefaults.standard.set(newValue, forKey: key) }
    }
}

enum SidebarItem: String, CaseIterable, Identifiable {
    case conversations
    case contacts
    case events
    case feed
    case profile
    case media
    case backups
    case bridges
    case notifications
    case moderation
    case settings
    case admin
    case family
    // `devices` (roster) + `conflicts` are no longer top-level nav items — they
    // moved into the Settings shell (Settings → Devices roster + Settings → File
    // sets, which absorbs the conflict surface) at the 2026-06-28 sync/folder UI
    // unification (devices.md / folders.md). Reached via `{"view":"settings",
    // "id":"devices"|"folders"}`, not a sidebar row.

    var id: String { rawValue }

    /// The always-visible sidebar rows. `admin` and `family` are handled
    /// separately — both are *gated* rows appended only when their gate passes
    /// (`am-i-admin` for admin, admin.md § Navigation model; any
    /// `fauna.family.status` relationship for family, family-safety.md § Client
    /// surface). Mirrors linux `views/sidebar.rs`, where the standard items are
    /// always visible and the two gated rows are appended after them.
    static var standardCases: [SidebarItem] {
        allCases.filter { $0 != .admin && $0 != .family }
    }

    var label: String {
        switch self {
        case .conversations: L.conversations.list.title
        case .contacts: L.common.contacts
        case .events: L.events.title
        case .feed: L.common.feed
        case .profile: L.profile.title
        case .media: L.media.title
        case .backups: L.backups.title
        case .bridges: L.common.bridges
        case .notifications: L.common.notifications
        case .moderation: L.common.moderation
        case .settings: L.common.settings
        case .admin: L.common.admin
        case .family: L.family.title
        }
    }

    var systemImage: String {
        switch self {
        case .conversations: "envelope"
        case .contacts: "person.crop.circle"
        case .events: "calendar"
        case .feed: "antenna.radiowaves.left.and.right"
        case .profile: "person.text.rectangle"
        case .media: "doc"
        case .backups: "clock.arrow.circlepath"
        case .bridges: "link"
        case .notifications: "bell"
        case .moderation: "shield.checkered"
        case .settings: "gear"
        case .admin: "shield"
        case .family: "figure.2.and.child.holdinghands"
        }
    }
}

/// `AdminPage` (the admin shell's page set) moved to shared FaunaKit
/// (`FaunaKit/Sources/FaunaKit/Core/AdminPage.swift`) 2026-06-13 so both Apple
/// targets share one enum: macOS swaps it via `AdminNavRail`, iOS
/// switches it via the mobile `AdminShellView`. `MacAppState.selectedAdminPage`
/// below uses that shared type.

/// What the macOS launch **screen** should render, once the shared `LaunchMachine`
/// has routed (`FaunaMacApp.runLaunch()` → `dispatchLaunch`).
///
/// **Not** the machine's `LaunchPhase` — that type is the UniFFI-generated one
/// (`fauna_launch_machine.swift`), and it is the *input* to this. This is the output:
/// a render gate. It is deliberately coarser (the machine's `Boot`/`Hydrating`/
/// `SilentChallenge`/`Refreshing` all collapse into `.launching`, and every
/// `WizardAt` entry collapses into `.ready` + `isOnboarded == false`, because the
/// wizard's own seeded state — not this enum — decides which page it opens on).
///
/// Renamed from `LaunchPhase` when apple adopted the shared machine (2026-07-13):
/// two same-named types in one file, one meaning "where the launch flow is" and the
/// other "what to draw", is exactly the confusion a cold reader doesn't need.
enum LaunchGate: Equatable {
    /// First moments of launch — the machine is hydrating / challenging.
    case launching
    /// Reachability failure the client can't reliably classify
    /// (`LaunchPhase.offline(transient: true)`); show the retry button.
    case retrying(error: String)
    /// A **terminal** failure (`LaunchPhase.offline(transient: false)`): the nest
    /// authoritatively reported it is outdated (`fauna.nest.outdated`), the secret is
    /// invalid, or the account is locked. NON-retry surface: the message, no retry
    /// button, only "Use a different nest" (version-compatibility.md Dim 4 /
    /// onboarding.md § App-launch routing — retrying any of these is futile).
    /// Reusing one surface for all three terminal causes matches windows
    /// (`App.xaml.cs`: "the terminal case … reuses the same surface for v1").
    case needsUpdate(message: String)
    /// The saved nest no longer signs this identity in
    /// (`LaunchSnapshot.signInRefused`, carried on the terminal
    /// `Offline{transient:false}` snapshot's side channel; `onboarding.md`
    /// § App-launch routing — the previously-signed-in row). Terminal, but the
    /// one terminal surface WITH Retry — see `LaunchSignInRefusedView`. `message`
    /// is the machine's `last_error` sentence. The iOS peer is
    /// `AppState.signInRefusedMessage`.
    case signInRefused(message: String)
    /// The nest's pinned deployment identity changed, or a pinned nest can no longer
    /// prove any identity (`LaunchPhase.identityChanged`; security.md § Transport
    /// trust). BLOCKING, and deliberately **retry-less** — see
    /// `LaunchIdentityChangedView`. The iOS peer is `AppState.identityChanged`.
    case identityChanged(pinnedHex: String, seenHex: String?)
    /// The saved account index at `fauna/index` is present and this build
    /// cannot use it (`LaunchSnapshot.accountIndexRefusal`, carried on the
    /// terminal `Offline{transient:false}` snapshot's side channel;
    /// `version-compatibility.md` § 5 item 9; `onboarding.md` § App-launch
    /// routing — the row checked before every other). Terminal,
    /// non-retryable, no fallthrough — see `LaunchAccountIndexUnreadableView`.
    /// `confirming` is the malformed verdict's revealed-confirm toggle,
    /// always `false` for `.newerBuild`, which offers no action at all.
    /// `error` is the confirm's refusal line while another instance serves an
    /// account the start-over would erase (`AccountIndexStartOver`), painted
    /// as the page's `error-message` with the confirm still showing.
    case accountIndexUnreadable(refusal: AccountIndexRefusal, confirming: Bool, error: String? = nil)
    /// Launch resolved (either authenticated or wizard); read
    /// `isOnboarded` to pick the rendered scene.
    case ready
}

@Observable
class MacAppState {
    let session = SessionState()

    /// The page a fresh launch mounts the authenticated shell on — and, through
    /// `OnboardingContainerView`'s `LoggedIn` arm, the page a wizard that just
    /// finished lands on too. A completed onboarding starts a new session, so it
    /// must not inherit the page a previous identity's session left selected
    /// (after a sign-out from Settings → Account, the new identity mounted on
    /// that identity-scoped page instead of its own landing).
    static let landingSidebar: SidebarItem = .conversations

    init() {
        // Convention 11's MOUNTED-SHELL derivation for macOS, installed once and
        // read live (`SessionState.mainShellMountedProbe`). This is exactly the
        // condition under which `ContentView.body` renders `MainWindowView()` —
        // the launch gate resolved AND the authenticated scene picked — so the
        // agent's `session.authenticated` and the rendered surface cannot come
        // apart. Every other `LaunchGate` case draws a pre-authenticated
        // surface, and every teardown path already flips `isOnboarded` back.
        //
        // ⚠ Keep this in lockstep with `ContentView.body`'s `.ready` branch.
        session.mainShellMountedProbe = { [weak self] in
            guard let self else { return false }
            return self.launchGate == .ready && self.isOnboarded
        }
    }

    var launchGate: LaunchGate = .ready {
        // The hidden auto-start launch settles on the launch's first settled
        // route (`AutoStartWindow`). Deferred one main-actor turn so a write
        // paired with this one (`isOnboarded`) has landed before it is read.
        didSet {
            Task { @MainActor [weak self] in
                guard let self else { return }
                AutoStartWindow.settle(gate: self.launchGate, isOnboarded: self.isOnboarded)
            }
        }
    }
    var isOnboarded: Bool = false
    /// Crossing the boundary *into* the Settings/Admin shell resets that shell's
    /// sub-page to its canonical entry (`docs/goal/ui/README.md` § Navigation
    /// model → *Entering a shell lands on its canonical entry*) — a shell's
    /// sub-page is shell state, not session state. The `oldValue` guard is
    /// qualification 1: a *reload* of the shell you're already in (re-selecting
    /// the same rail row, a redundant nav patch) must not clobber the sub-page a
    /// visit is mid-way through. This is the one edge every writer of
    /// `selectedSidebar` — the rail binding, keyboard shortcuts, the quick
    /// switcher, the state-protocol nav patch — passes through, so none of them
    /// need their own reset (mirrors iOS's `ContentView.selectedSettingsPage =
    /// nil` on its More-hub entry edge).
    var selectedSidebar: SidebarItem = MacAppState.landingSidebar {
        didSet {
            guard oldValue != selectedSidebar else { return }
            switch selectedSidebar {
            case .admin: selectedAdminPage = .dashboard
            case .settings: selectedSettingsPage = .status
            default: break
            }
        }
    }
    /// The hex actor_id of the profile shown when `selectedSidebar == .profile`;
    /// `nil` ⇒ the viewer's own (SELF) profile. Set by a contact-row tap-through
    /// (`openProfile`) or the nav-stack `actor_id` (profile.md § Layout & flow →
    /// Another's profile); cleared when the sidebar navigates anywhere else.
    var profileActorId: String?
    /// A `search-result-item` Contact-arm deep link's target — the CardDAV
    /// card's `uid_hash` to locate and open in the Address Book segment
    /// (`ui/search.md` § Where logic lives → Result navigation (deep link)).
    /// Set by `SearchResultsView` alongside switching `selectedSidebar`;
    /// consumed and cleared by `ContactSplitView`/`AddressBookView` once
    /// located. `nil` in the ordinary (non-deep-link) case.
    var pendingContactUidHash: String?
    /// A `notification-item` Knock-arm deep link's target — the knocker's hex
    /// actor id, set alongside switching `selectedSidebar` to `.contacts`
    /// (`behavior/notifications.md` § Deep-link destinations). Consumed and
    /// cleared by `ContactSplitView`, which selects the **People** segment: the
    /// segment is sticky and may be sitting on the Address Book, where no
    /// `knock-request-item` renders. The mirror image of
    /// `pendingContactUidHash`, which forces the same toggle the other way.
    /// The id rides along so a future build can highlight the matching row,
    /// exactly as the shared enum intends; today both apple apps list knocks
    /// flat and simply open the page. Mirrors the shared iOS `AppState`.
    var pendingKnockSenderId: String?
    /// Whether the current identity is a nest admin — the `am-i-admin` gate
    /// (admin.md § Navigation model: "non-admins do not see admin entries").
    /// Fail-closed (default `false`): the gated `admin-tab` sidebar row (and the
    /// admin quick-switcher shortcut) appear only when this is `true`, refreshed
    /// from `APIClient.amIAdmin()` on login and reconnect. Mirrors web's
    /// `userIsAdmin` and the shared iOS `AppState.isAdmin`.
    var isAdmin: Bool = false
    /// The caller's `fauna.family.status` projection — gates the two global
    /// family surfaces (family-safety.md § App surface): the `family-tab`
    /// sidebar row on any relationship (guardian OR supervised), and the global
    /// `supervised-indicator` on the supervised side alone. Fail-closed: a failed
    /// read hides both. Refreshed on login + reconnect from `MainWindowView`.
    let familyStatus = FamilyStatusStore()
    /// The app-scoped content-policy render cache (family-safety.md § Content
    /// policy) — the guardian floor + the viewer's own spam/phishing thresholds,
    /// composed strictest-wins in shared Rust. ONE cache for both social
    /// surfaces (feed post-card + conversation bubble) so they cannot drift on
    /// how a floor is enforced. Refreshed on the same login + reconnect triggers
    /// as `familyStatus` above.
    let contentPolicy = ContentPolicyStore()
    /// The device's region content plane (`region-blocking.md` § The content
    /// plane) — the third strictest-wins source of the same render verdict,
    /// refreshed on the same login + reconnect triggers as `contentPolicy`.
    /// Device-scoped: kept across sign-out and account switch.
    let region = RegionStore()
    /// The ward's screen-time lock state — the global `screen-time-lock` overlay's one
    /// source of truth, refreshed on the same login + reconnect triggers as
    /// `contentPolicy` above.
    let screenTime = ScreenTimeStore()
    /// The shared `fauna.web.*` published-post surface cache — the `web-settings`
    /// Published-posts section and the feed ⋯-menu's web-publishing verbs both
    /// read/write through this ONE store, so they can never disagree about a
    /// creator's serving origin (`web-content-hosting.md` § Published-post
    /// management). Mirrors `contentPolicy` above; the iOS peer is `AppState.webPublish`.
    let webPublish = WebPublishStore()
    /// The active page inside the admin shell (when `selectedSidebar == .admin`).
    var selectedAdminPage: AdminPage = .dashboard
    /// The active page inside the Settings shell (when `selectedSidebar ==
    /// .settings`). Default `.status` — the folded-in former standalone status
    /// page carrying the live nest-data (settings.md § Navigation model).
    var selectedSettingsPage: SettingsPage = .status
    /// The list a `mail-lists` row's Members button targeted — re-points the
    /// `mail-list-members` page at that specific list rather than a placeholder
    /// (mirrors linux's `on_navigate_to_members`, `mail_lists.rs`). `nil` means
    /// the page was reached directly (no list selected yet), which stays an
    /// honest empty page.
    var selectedMailListId: String?
    var selectedMailListName: String?
    /// Bumped on every nav patch (incl. re-navigation to the same page) so admin
    /// pages re-fetch when the e2e re-navigates to refresh — the cross-app
    /// poll pattern (re-`navigate_users()` until a seeded row appears) relies on
    /// each navigation reloading, not just the first mount.
    var navGeneration: Int = 0
    /// Bumped by a nav patch that lands on a PAGE (a sidebar destination), never
    /// by one naming the toolbar search. `ContentView` dismisses the search
    /// overlay on it, because a patch to the page already selected under the
    /// overlay changes no `selectedSidebar` and would otherwise leave the search
    /// results covering the page it asked for.
    var pageNavRequest: Int = 0
    /// The user's inbox-privacy mode, cached here so the e2e state serializer can
    /// expose `settings.inbox_mode` (the cross-app snapshot key the inbox-mode
    /// test reads). The shared `PrivacySettingsView` writes the picked mode back
    /// here via its `onInboxModeChanged` closure — the macOS peer of iOS's
    /// `AppState.inboxMode` and linux's `settings::set_inbox_mode` local cache.
    /// `nil` until a real fetch resolves — never a guessed default
    /// (settings.md § Privacy sub-page item 6): the state snapshot must not
    /// report a mode nobody fetched any more than the selector itself may
    /// paint one.
    var inboxMode: String?
    var dockIconVisible: Bool = DockIconPreference.current {
        didSet { DockIconPreference.current = dockIconVisible }
    }
    // Tri-state (never chosen / explicitly on / explicitly off, common.md §
    // Desktop Residency — default ON): `.bool(forKey:)` collapses "never
    // chosen" into `false`, so read the raw object like `DockIconPreference`
    // above and default to `true` only when the key has never been written.
    var launchAtLogin: Bool = AutoStart.storedChoice ?? true
    /// System Settings → General → Login Items has turned Fauna off — the
    /// user's choice too, so the toggle reads off over it (`AutoStart`).
    /// Refreshed whenever General settings shows and whenever the app
    /// reactivates (the user may come back from that System Settings page).
    var autostartOSOptedOut: Bool = false
    var isMainWindowFocused: Bool = false
    var activeConversationId: String?

    // Sheet state
    var showingQuickSwitcher: Bool = false

    // Search
    var searchText = ""
    var isSearchActive = false

    // The live `FaunaClient`, in the one slot that cannot go stale — both
    // shells' launch paths write it, and the `@Environment(FaunaClient.self)`
    // injection resolves it FIRST (`appState.liveClient ?? client`), so it is
    // the live-client slot in release builds too, not an e2e-only fallback.
    // Stored here (a class) rather than in @State on FaunaMacApp (a struct),
    // because closures captured during struct init() can't see later @State
    // mutations — the FaunaKit peer's own doc comment (`AppState.swift`)
    // carries the full reasoning.
    var liveClient: FaunaClient?

    /// The live, ACTOR-SCOPED photo-backup container, in the one slot a `@State`
    /// write from a callback cannot lose — the peer of `liveClient`, and held
    /// here for the same reason (see that property).
    ///
    /// `nil` before the first login; the App struct resolves
    /// `appState.liveModelContainer ?? modelContainer`, so the pre-auth flat
    /// container stays the fallback exactly as `client` is `liveClient`'s.
    ///
    /// ⚠ **Not a nicety — the login that needs it most is the one that cannot
    /// use `@State`.** The e2e `set_state` login runs on a `self` the App
    /// captured by value at `init()`, so its `modelContainer = buildModelContainer(
    /// actorIdHex:)` was dropped on the floor: the scoped store was created on
    /// disk (so a file check saw scoping working) while every read in the app —
    /// the view injection, the `FaunaClient`'s `modelContext`, the agent's own
    /// state — stayed on the flat, account-shared container for the whole
    /// session. `ActorScope.dropAppOwnedState` clears this slot with the `@State`
    /// one, so a teardown cannot leave the outgoing actor's container live.
    var liveModelContainer: ModelContainer?

    /// The session's sync-agent provisioner and its agent event listener
    /// (`sync-agent.md` § Control plane split) — held HERE for `liveClient`'s
    /// reason above, one degree sharper: **every** site that touches these two
    /// is a callback site. They are written post-auth
    /// (`startSyncAgentProvisioner`, the listener spawn beside it) and read by
    /// `unprovisionSyncAgent()`, which all four session-teardown paths call —
    /// and two of those, `resetToFactory()` and `logoutKeepData()`, are reached
    /// ONLY through `handleTestCommand`'s init-time-captured `self`, where a
    /// `@State` write no-ops and a `@State` read answers the nil it was born
    /// with. That is exactly what happened: the harness reset logged
    /// `[sync-agent] unprovision skipped` with a real agent running, which reads
    /// truthfully on the far commoner launch that provisions nothing.
    ///
    /// **Unlike `liveClient` there is no `@State` twin to fall back to, by
    /// design.** No view renders either object (the Sync pages read
    /// `LocationsModel` / `SyncAgentHealthModel`), so nothing wants a
    /// `@State`-shaped home — and a second slot is what made this a bug rather
    /// than a curiosity: the DEBUG-only static this pair used to keep for the
    /// e2e retire-before-rebuild guard served that ONE read while every
    /// teardown still asked the stale `@State`. One home, one answer.
    var syncAgentProvisioner: FfiSyncAgentProvisioner?
    var syncEventListener: FfiSyncAgentEventListener?

    /// The macOS instance's **launch binding** and the (OS login, account)
    /// single-instance lock it holds — the fourth and fifth slots of the
    /// captured-`self` trap, moved here 2026-09-22.
    ///
    /// Held HERE rather than in `@State` on `FaunaMacApp` (a struct) for
    /// `syncAgentProvisioner`'s reason above, and by its own test: **no view
    /// renders any of the three**, so nothing wants a `@State`-shaped home,
    /// and a second slot is what would make this a bug rather than a
    /// curiosity. All eleven use sites are `@MainActor private func`s on the
    /// App struct — `runLaunch`, `dispatchLaunch`,
    /// `completeAuthenticatedLaunch`, `useADifferentNest` and (DEBUG-only)
    /// `logoutKeepData` — and five of them are reached ONLY through
    /// `handleTestCommand`'s init-time-captured `self`. So the rule applies
    /// verbatim (`sync-agent-credentials.md` § Implementation status today):
    /// a value a callback-reached site must read does not live in `@State`
    /// alone. No `@State` twin and no `live*` mirror, by design — one home,
    /// one answer.
    ///
    /// ⚠ **What the old `@State` home cost, stated because it was not
    /// hypothetical.** `logoutKeepData` is `#if DEBUG` with
    /// `handleTestCommand`'s `"logout"` case as its ONLY caller, so its
    /// cross-chain shape was unconditional rather than narrow: it read
    /// `boundActorId` as the `nil` its own copy was born with whatever the
    /// launch binding — so a bound secondary took the primary's full logout
    /// path instead of its own terminal refusal — then nilled THAT copy's
    /// lock pair and re-entered `runLaunch()` while the live instance went on
    /// holding the real lock for the account it had just removed, for the rest
    /// of the process. And `completeAuthenticatedLaunch` is re-entrant (the
    /// `instanceLock` rule below), so a re-entrant call reached through a
    /// differently-captured closure — `handleTestCommand`'s `silent_sign_in`
    /// → `performPostAuthSilentSignIn` → `runLaunch()` is the live route —
    /// read `instanceLockActorId` as `nil` and re-acquired a lock this process
    /// already held. Harmless under the SHARED acquire macOS retired to in
    /// W5.6; a terminal false refusal against a pre-retirement (exclusive)
    /// binary of this app.
    ///
    /// The account this process is **bound** to, when launched as a secondary
    /// instance (`FAUNA_BOUND_ACCOUNT` → `FaunaAccounts.resolveLaunchBinding`);
    /// `nil` for the ordinary primary launch. Set once per launch resolution
    /// and held for the process lifetime (`account-scoping.md` § Concurrent
    /// instances): the session account is `boundActorId ?? registry.active()`,
    /// and every session-path identity read resolves through the registry for
    /// that account — never the *active* account's launch snapshot (which a bound instance
    /// must not read, since it names the *active* account).
    /// Deliberately NOT on `ActorScope.dropAppOwnedState`'s canonical list: it
    /// is process-scoped, not actor-scoped — a switch changes the account, not
    /// the binding.
    var boundActorId: String?

    /// The held (OS login, account) single-instance lock for the session
    /// account, plus which account it keys (`account-scoping.md` § Concurrent
    /// instances — **macOS RETIRED, W5.6 (account-data-plane.md § Workstreams),
    /// 2026-08-16**: a SHARED lock, so same-account instances coexist).
    /// Acquired in `completeAuthenticatedLaunch` — the one point every session
    /// build funnels through (primary, bound, switch rebuild, post-onboarding)
    /// — and replaced on a cross-account switch. The actor is tracked because
    /// a same-account rebuild (e.g. the switcher's re-auth retry) must REUSE
    /// the held lock: the kernel treats a second open of the lock file as a
    /// competing owner, so re-acquiring against ourselves would read as
    /// "another instance" and falsely refuse. Swapped by
    /// `completeAuthenticatedLaunch` itself and released by `logoutKeepData`,
    /// so like `boundActorId` it stays off the canonical actor-scoped drop.
    var instanceLock: FfiAccountInstanceLock?
    var instanceLockActorId: String?

    /// The session's push-notification manager (`settings.md` § Push
    /// notifications). Lives here for the same reason `liveClient` does: the
    /// launch path builds it so `AppDelegate`'s APNs callbacks have somewhere to
    /// deliver a device token, and the General sub-page has to read *that*
    /// instance rather than build a second one that would never hear back.
    var pushManager: PushManager?

    /// The newer-version check (`UpdateCheck`): one per process, shared by
    /// Settings → General's About block, the app menu item and the
    /// once-per-sign-in look, so all three paint the same notice.
    let updates = UpdateCheck()

    /// The Events page's view model, published here by `EventSplitView` so
    /// `AppDelegate.applicationShouldTerminate` can flush the events-drafts
    /// rail on quit (`reserved-folders.md` § The leave-flush promise) — the
    /// VM otherwise lives entirely inside the view (`@State`), unreachable
    /// from the app-lifecycle delegate. Same reachability shape as
    /// `pushManager` above.
    var eventsVM: EventsVM?

    /// The Conversations and Feed page view models, published here by
    /// `FaunaMacApp` at launch so `AppDelegate.applicationShouldTerminate` can
    /// flush the conversations and feed drafts rails on quit
    /// (`reserved-folders.md` § The leave-flush promise). Unlike `eventsVM`
    /// above — which lives inside its view and is published by `EventSplitView`
    /// when that page first appears — these two are `@State` on the App itself
    /// and so exist from launch; publishing them beside `appDelegate.appState`
    /// keeps one reachability shape rather than two. Mirrors iOS
    /// `AppState.conversationsVM`/`feedVM`.
    var conversationsVM: ConversationsVM?
    var feedVM: FeedVM?

    /// Factory-reset re-onboard hook (admin-nest Danger Zone). `FaunaMacApp` sets
    /// this; `AdminShellView` hands it to `AdminNestView`, which calls it with the
    /// post-reset claim code. The closure drops the authed session keeping local
    /// creds and re-seeds onboarding at claim-code with the code pre-filled (the
    /// re-seed touches `onboardingVM` + `isOnboarded`, which the App owns — so the
    /// platform glue lives there, not in the shared FaunaKit view). Stored here
    /// (a class) for the same reason as `liveClient`.
    var onFactoryReset: ((String) -> Void)?

    /// Account-switch hook (Account settings → the switcher). `FaunaMacApp` sets it;
    /// the shared `AccountSwitcherSection` calls it with the target actor id. The closure
    /// makes the account active, tears the authenticated session down and re-runs the
    /// launch machine over the now-active identity — all App-owned state (the launch
    /// machine, the menu-bar controller, onboarding), which is why the shared FaunaKit
    /// view hands the decision out here rather than doing it itself. Stored on this class
    /// for the same reason as `onFactoryReset`. Throws the registry's refusal (nothing
    /// torn down) for the switcher to paint on the page's `error-message`.
    var onSwitchAccount: (@MainActor (String, Bool) async throws -> Void)?

    /// Add-account hook (Account settings → "Add account"). `FaunaMacApp` sets it to open
    /// onboarding in **append** mode: same wizard, but cancelling dismisses instead of
    /// quitting the live session, and success registers the new identity and switches to it.
    var onAddAccount: (() -> Void)?

    /// Sign-out / delete-account teardown hook (`SignOutSection`, `AccountSettingsView`'s
    /// `onAccountReset`). `FaunaMacApp` sets it to the same "drop everything the outgoing
    /// identity owns" checklist `switchAccount()` runs before rebuilding — client shutdown,
    /// sync-agent unprovision, DNS/subscription cadence stop, File Provider sign-out — for
    /// the same reason as `onFactoryReset`: the shared FaunaKit view only names the intent,
    /// the App owns the live objects the teardown touches. The call site still sets
    /// `isOnboarded` itself afterward; this hook is pure teardown, not navigation.
    var onSignOut: (() -> Void)?

    /// True while the **append**-mode onboarding sheet is up (`onAddAccount`). It is what
    /// makes the wizard's `LoggedIn` exit mean "register this identity and switch to it"
    /// rather than "boot the first session" — the two paths are otherwise the same wizard.
    var isAddingAccount = false

    /// Post-onboarding launch hook. `FaunaMacApp` sets this to re-run `runLaunch()`;
    /// the onboarding `.feed` exit
    /// (`OnboardingContainerView`) calls it so the identity
    /// `performLoggedInHandoff` just persisted is picked up and the authed
    /// `FaunaClient` is built via the same shared-`LaunchMachine` gate a returning-user
    /// launch uses (landing on `completeAuthenticatedLaunch`, which also fires the
    /// first-setup mail/caldav glue). Without it, fresh onboarding would enter
    /// `MainWindowView` with a nil client. Stored here (a class) for the same
    /// reason as `onFactoryReset` / `liveClient`.
    var onLaunchAuthenticated: (() -> Void)?

    // E2E state protocol data — populated by views after loading.
    var lastContacts: [Contact] = []
    var lastKnocks: [Knock] = []
    var lastEvents: [EventSummary] = []
    var notificationsUnreadCount: Int = 0

    /// Navigate to a profile: `actorId` ⇒ another actor's (subscriber browse),
    /// `nil` ⇒ the viewer's own. Sets the target BEFORE selecting the sidebar so
    /// the profile detail view mounts with it. Used by the contact-row tap-through
    /// (mirrors linux `open_profile`).
    func openProfile(_ actorId: String?) {
        profileActorId = actorId
        selectedSidebar = .profile
    }

    func updateDockVisibility() {
        NSApp.setActivationPolicy(dockIconVisible ? .regular : .accessory)
    }

    /// What the start-at-login toggle shows (`AutoStart.displayedChoice`).
    var autostartShown: Bool {
        AutoStart.displayedChoice(choice: launchAtLogin, osOptedOut: autostartOSOptedOut)
    }

    func refreshAutostartOSStatus() {
        autostartOSOptedOut = AutoStart.osOptedOut
    }

    /// An explicit start-at-login choice: persisted, and the registration made
    /// to match. E2E gates the OS registration off (mirrors linux's `autostart`
    /// module and windows' `AutoStartGate.ShouldRegister`) — the toggle's
    /// witness is the persisted CHOICE surviving a relaunch.
    func setLaunchAtLogin(_ on: Bool) {
        launchAtLogin = on
        AutoStart.applyUserChoice(on)
        refreshAutostartOSStatus()
    }
}

/// `MacAppState` already exposes the same `session`/`contentPolicy`/
/// `webPublish` FaunaKit types the shared iOS `AppState` does — this just
/// names that shared shape so `FaunaKit.PostCardBody` can read either
/// platform's state through one protocol. `AppState`'s own conformance lives in FaunaKit itself
/// (`PostCardBody.swift`), since it's declared there.
extension MacAppState: PostCardAppStateProviding {}

/// `MacAppState`'s `lastContacts`/`lastKnocks`/`lastEvents`/
/// `notificationsUnreadCount` e2e-serialization caches are the same shape as
/// iOS `AppState`'s — this names it so `ActorScope.dropAppOwnedState(...)`
/// can drop both platforms' caches through one shared function.
/// `AppState`'s own conformance lives in FaunaKit's `ActorScope.swift`, since
/// it's declared there.
extension MacAppState: ActorScopedAppCaches {}
