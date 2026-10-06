import SwiftUI
import SwiftData

/// The two fingerprints the shared machine's `LaunchPhase::IdentityChanged` carries:
/// the identity this client previously TOFU-pinned for the nest, and the one the nest
/// proved on this launch (`nil` = it could prove none at all — the withdrawn /
/// downgrade case, which the pin must still refuse; security.md § Transport trust).
///
/// A plain value type rather than two loose optionals on `AppState`, so "identity
/// changed" cannot be half-set: the surface renders iff this is non-`nil`.
public struct IdentityChangedInfo: Equatable, Sendable {
    public let pinnedHex: String
    public let seenHex: String?

    public init(pinnedHex: String, seenHex: String?) {
        self.pinnedHex = pinnedHex
        self.seenHex = seenHex
    }
}

@Observable
public class AppState {
    public let session = SessionState()
    public var isOnboarding: Bool = true
    public var selectedTab: String = "conversations"
    /// For views behind the "More" tab on mobile (events, files, devices, settings, etc.)
    public var moreSelectedView: String?
    /// The hex actor_id of the profile to show when `moreSelectedView == "profile"`;
    /// `nil` ⇒ the viewer's own (SELF) profile. Set by a contact-row tap-through or
    /// the nav-stack `actor_id` (profile.md § Layout & flow → Another's profile);
    /// cleared on any other navigation.
    public var profileActorId: String?
    /// A `search-result-item` Contact-arm deep link's target — the CardDAV
    /// card's `uid_hash` to locate and open in the Address Book segment
    /// (`ui/search.md` § Where logic lives → Result navigation (deep link)).
    /// Set by `SearchResultsView` alongside switching `selectedTab`; consumed
    /// and cleared by `ContactsView`/`AddressBookView` once located. `nil` in
    /// the ordinary (non-deep-link) case. Mirrors `MacAppState`.
    public var pendingContactUidHash: String?
    /// A `notification-item` Knock-arm deep link's target — the knocker's hex
    /// actor id, set alongside switching `selectedTab` to `contacts`
    /// (`behavior/notifications.md` § Deep-link destinations). Consumed and
    /// cleared by `ContactsView`, which selects the **People** segment: the
    /// segment is sticky and may be sitting on the Address Book, where no
    /// `knock-request-item` renders. The mirror image of
    /// `pendingContactUidHash`, which forces the same toggle the other way.
    /// The id rides along so a future apple build can highlight the matching
    /// row, exactly as the shared enum intends; today both apple apps list
    /// knocks flat and simply open the page. Mirrors `MacAppState`.
    public var pendingKnockSenderId: String?
    /// The user's inbox-privacy mode, cached here so the e2e state serializer
    /// can expose `settings.inbox_mode` (the cross-app snapshot key the
    /// inbox-mode test reads). `nil` until a real fetch resolves — never a
    /// guessed default (settings.md § Privacy sub-page item 6): the state
    /// snapshot must not report a mode nobody fetched any more than the
    /// selector itself may paint one.
    public var inboxMode: String?
    /// Whether the search bar overlay is shown above the main tabs
    /// (`search-query-field` etc., `ContentView.MainTabView`). Owned here
    /// (rather than `MainTabView`'s own `@State`) so a `{"view":"search"}` nav
    /// patch (`applyNavPatch`'s `"search"` case) can reveal it — mirrors macOS,
    /// where `.searchable(placement: .sidebar)` is unconditionally present so no
    /// nav-driven toggle is needed there.
    public var showSearchBar: Bool = false

    // MARK: - Admin shell (admin.md § Navigation model)
    //
    // Admin is a **top-level nav peer** (not nested under Settings): entering it
    // shows the `AdminShellView` in place of the main `TabView`, and
    // `admin-nav-back` exits to the primary view (Conversations), the same
    // landing as `settings-nav-back` (admin.md:30, corrected 2026-06-07). The
    // shared FaunaKit `AdminShellView`/page set is driven by `selectedAdminPage`;
    // the e2e state protocol carries it as `nav.stack[1].id` (mirrors macOS
    // `MacAppState.selectedAdminPage` + linux `nav.stack[1].id`).

    /// True while the admin shell is shown (a top-level peer of the main tabs).
    public var inAdmin: Bool = false
    /// The active page inside the admin shell.
    public var selectedAdminPage: AdminPage = .dashboard
    /// Whether the current identity is a nest admin — the `am-i-admin` gate
    /// (admin.md § Navigation model: "non-admins do not see admin entries").
    /// Fail-closed (default `false`): the gated `admin-tab` entry is shown only
    /// when this is `true`, refreshed from `APIClient.amIAdmin()` on login and
    /// reconnect. Mirrors web's `userIsAdmin` (`routes/+layout.svelte`) and
    /// `MacAppState.isAdmin`.
    public var isAdmin: Bool = false
    /// The caller's `fauna.family.status` projection — gates the two global family
    /// surfaces (family-safety.md § App surface): the gated `family-tab`
    /// in-Settings entry on any relationship (guardian OR supervised), and the
    /// global `supervised-indicator` on the supervised side alone. Fail-closed: a
    /// failed read hides both. Refreshed on login + reconnect from `MainTabView`
    /// (the app root, NOT `SettingsView` — the indicator must render on every
    /// tab, so its gate cannot depend on Settings having been opened).
    public let familyStatus = FamilyStatusStore()
    /// The app-scoped content-policy render cache (family-safety.md § Content
    /// policy) — the guardian floor + the viewer's own spam/phishing thresholds,
    /// composed strictest-wins in shared Rust. ONE cache for both social
    /// surfaces (feed post-card + conversation bubble) so they cannot drift on
    /// how a floor is enforced. Refreshed on the same login + reconnect triggers
    /// as `familyStatus` above.
    public let contentPolicy = ContentPolicyStore()
    /// The device's region content plane (`region-blocking.md` § The content
    /// plane) — the third strictest-wins source of the same render verdict,
    /// refreshed on the same login + reconnect triggers as `contentPolicy`.
    /// Device-scoped: kept across sign-out and account switch.
    public let region = RegionStore()
    /// The ward's screen-time lock state — the global `screen-time-lock` overlay's one
    /// source of truth, refreshed on the same login + reconnect triggers as
    /// `contentPolicy` above.
    public let screenTime = ScreenTimeStore()
    /// The shared `fauna.web.*` published-post surface cache — the `web-settings`
    /// Published-posts section and the feed ⋯-menu's web-publishing verbs both
    /// read/write through this ONE store, so they can never disagree about a
    /// creator's serving origin (`web-content-hosting.md` § Published-post
    /// management). Mirrors `contentPolicy` above.
    public let webPublish = WebPublishStore()
    /// The active Settings sub-page, or `nil` for the settings root (the page
    /// list itself). iOS keeps idiomatic settings nav (settings.md:28), so this
    /// drives the `SettingsView` `NavigationStack` path rather than a sidebar
    /// swap; the e2e state protocol carries it as the second stack entry's `id`
    /// (`{"view":"settings"},{"view":"settings","id":"<navId>"}`), mirroring
    /// `selectedAdminPage`/`nav.stack[1].id`. The shared `SettingsPage` enum is
    /// the one source of truth the macOS Settings rail consumes too.
    public var selectedSettingsPage: SettingsPage?
    /// The list a `mail-lists` row's Members button targeted — re-points the
    /// `mail-list-members` page at that specific list rather than a placeholder
    /// (mirrors linux's `on_navigate_to_members`, `mail_lists.rs`). `nil` means
    /// the page was reached directly (no list selected yet), which stays an
    /// honest empty page.
    public var selectedMailListId: String?
    public var selectedMailListName: String?
    /// Bumped on every nav patch (incl. re-navigation to the same page) so admin
    /// pages keyed on it re-fetch — the cross-app poll-by-re-navigate pattern
    /// relies on each navigation reloading, not just first mount (mirrors macOS).
    public var navGeneration: Int = 0
    /// Bumped by the iOS test agent's `nav_back` command — the back gesture: the
    /// visible tab's `NavigationStack` pops its top screen (today only
    /// `ConversationsListView` observes it).
    public var navBackGeneration: Int = 0
    /// **The live `FaunaClient`, in the one slot that cannot go stale** — the iOS
    /// peer of `MacAppState.liveClient`. Named for the caller it was added for,
    /// but no longer only theirs: both shells' *launch* paths write it too, and
    /// the `@Environment(FaunaClient.self)` injection resolves it FIRST
    /// (`appState.liveClient ?? client`).
    ///
    /// Stored here, on a class, because that is the whole point: a `FaunaClient`
    /// built inside a callback — the test agent's `applySessionPatch` — writes the
    /// App's `@State client` without reaching the `WindowGroup` content closure,
    /// so `client` can hold an object the app has already replaced. `AppState` is
    /// `@Observable`, so this slot always reaches the views. Ordering them the
    /// other way round is what let a later login look applied while every
    /// env-configured view kept answering for the previous identity.
    public var liveClient: FaunaClient?

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
    public var liveModelContainer: ModelContainer?
    /// Factory-reset re-onboard hook (admin-nest Danger Zone). `FaunaApp` sets it;
    /// `AdminShellView` hands it to the shared `AdminNestView`, which calls it with
    /// the post-reset claim code. The closure drops the authed session keeping
    /// local creds and re-seeds onboarding at claim-code with the code pre-filled
    /// (touches App-owned onboarding state, so the glue lives in `FaunaApp`).
    /// Mirrors `MacAppState.onFactoryReset`.
    public var onFactoryReset: ((String) -> Void)?
    /// FP-domain re-converge hook (`FileProviderCoordinator.reconcile` with the
    /// live session's provisioning context). `FaunaApp` sets it on authenticated
    /// launch; the `folder-on-demand-toggle` calls it so a flip adds/removes
    /// the set's Files-app domain immediately (macOS routes the same flip
    /// through `LocationsModel.onBindingsChanged` instead — its reconcile
    /// also carries bound-set arbitration).
    public var fpReconcile: (@Sendable () async -> Void)?
    /// Account-switch hook (Account settings → the switcher). `FaunaApp` sets it; the
    /// shared `AccountSwitcherSection` calls it with the target actor id, and the closure
    /// makes that account active, tears the authenticated session down and re-runs the
    /// launch machine over it. App-owned state, so the glue lives in `FaunaApp`.
    /// Throws the registry's refusal (nothing torn down) for the switcher to paint.
    /// Mirrors `MacAppState.onSwitchAccount`.
    public var onSwitchAccount: (@MainActor (String, Bool) async throws -> Void)?
    /// Add-account hook (Account settings → "Add account") — opens onboarding in **append**
    /// mode. Mirrors `MacAppState.onAddAccount`.
    public var onAddAccount: (() -> Void)?
    /// Sign-out / delete-account teardown hook (`SignOutSection`, `AccountSettingsView`'s
    /// `onAccountReset`). `FaunaApp` sets it to the same "drop everything the outgoing
    /// identity owns" checklist `switchAccount()` runs before rebuilding — client shutdown,
    /// DNS/subscription cadence stop, File Provider sign-out — for the same reason as
    /// `onFactoryReset`: the shared view only names the intent, the App owns the live
    /// objects the teardown touches. The call site still sets `isOnboarding` itself
    /// afterward; this hook is pure teardown, not navigation. Mirrors `MacAppState.onSignOut`.
    public var onSignOut: (() -> Void)?
    /// True while the **append**-mode onboarding sheet is up. Makes the wizard's `LoggedIn`
    /// exit register the identity + switch to it, rather than boot a first session.
    /// Mirrors `MacAppState.isAddingAccount`.
    public var isAddingAccount = false
    /// Non-retry "update required" surface (version-compatibility.md Dim 4 /
    /// onboarding.md § App-launch routing). When the launch silent-challenge
    /// refresh raises `FfiError.NestOutdated` (the nest is degraded / outdated),
    /// `FaunaApp` sets this to the already-localized actionable message; non-`nil`
    /// makes `ContentView` render `LaunchNeedsUpdateView` ahead of the main UI.
    /// iOS has no `launchPhase` enum (it enters optimistically and refreshes in the
    /// background — there is no gate to extend, unlike macOS `MacAppState`), so the
    /// outdated signal is carried as this optional message instead.
    public var needsUpdateMessage: String?
    /// The `launch_sign_in_refused` surface (`onboarding.md` § App-launch routing,
    /// the previously-signed-in row): the saved nest no longer signs this identity
    /// in (`LaunchSnapshot.signInRefused`, carried on the terminal
    /// `Offline{transient:false}` snapshot's side channel). Holds the machine's
    /// `last_error` sentence; non-`nil` makes `ContentView` render the shared
    /// `LaunchSignInRefusedView` ahead of the main UI — terminal, but WITH Retry.
    /// Optional for the same reason `needsUpdateMessage` is: iOS has no launch-gate
    /// enum (macOS uses `MacAppState.LaunchGate.signInRefused`).
    public var signInRefusedMessage: String?
    /// Retry hook for `LaunchSignInRefusedView` (`launch-retry-button`). `FaunaApp`
    /// sets it to call `retrySilentChallenge()` on the HELD machine that produced
    /// the verdict, then re-dispatch its snapshot.
    public var onRetrySignIn: (() -> Void)?
    /// BLOCKING nest-identity-changed surface (security.md § Transport trust — the SSH `known_hosts` model). Set from `LaunchPhase::IdentityChanged`
    /// when the shared `LaunchMachine` finds the nest's pinned deployment identity
    /// changed, or a pinned nest can no longer prove any identity; non-`nil` makes
    /// `ContentView` render `LaunchIdentityChangedView` ahead of the main UI, with
    /// **no retry CTA** and no auto-entry.
    ///
    /// Carried as an optional for the same reason `needsUpdateMessage` is: iOS enters
    /// optimistically and has no launch-gate enum to extend (macOS uses
    /// `MacAppState.LaunchGate.identityChanged` instead). The rendered surface itself
    /// is the shared FaunaKit `LaunchIdentityChangedView` on both targets — priority #2.
    public var identityChanged: IdentityChangedInfo?
    /// The saved account index at `fauna/index` is present and this build
    /// cannot use it (`LaunchSnapshot.accountIndexRefusal`, carried on the
    /// terminal `Offline{transient:false}` snapshot's side channel;
    /// `version-compatibility.md` § 5 item 9; `onboarding.md` § App-launch
    /// routing — the row checked before every other). Non-`nil` makes
    /// `ContentView` render `LaunchAccountIndexUnreadableView` ahead of the
    /// main UI, at the SAME tier as `identityChanged` — both are terminal,
    /// blocking, retry-less verdicts with no cached-session fallback.
    ///
    /// Carried as an optional for the same reason `identityChanged` is: iOS
    /// has no launch-gate enum to extend (macOS uses
    /// `MacAppState.LaunchGate.accountIndexUnreadable` instead).
    public var accountIndexRefusal: AccountIndexRefusal?
    /// The malformed verdict's revealed-confirm toggle for
    /// `LaunchAccountIndexUnreadableView` — always `false` for
    /// `.newerBuild`, which offers no action at all. Kept as its own field
    /// (rather than folded into `accountIndexRefusal`, an FFI enum) because
    /// macOS's `LaunchGate.accountIndexUnreadable` carries it as sibling
    /// associated state; iOS mirrors that shape with a sibling field instead.
    public var accountIndexConfirming = false
    /// "Start over on this device" hook for `LaunchAccountIndexUnreadableView`
    /// (`account-index-reset-button`) — reveals the confirm. `FaunaApp` sets
    /// it.
    public var onAccountIndexStartOver: (() -> Void)?
    /// The confirm hook (`account-index-reset-confirm-button`) — the
    /// documented client-side floor (`long-term-store.md` § Cleanup
    /// contract), reusing the app's existing factory reset. `FaunaApp` sets
    /// it.
    public var onAccountIndexConfirmStartOver: (() -> Void)?
    /// "Trust this nest" hook for `LaunchIdentityChangedView`
    /// (`nest-identity-changed-trust-button`). `FaunaApp` sets it to call
    /// `trustNestIdentity()` **on the machine that produced the verdict** — a fresh
    /// machine re-challenges from `Boot` and never reaches the forget-the-pin branch,
    /// so the button would silently do nothing (the trap linux hit). Hence `FaunaApp`
    /// holds the machine and this closure captures it.
    public var onTrustNestIdentity: (() -> Void)?
    /// "Use a different nest" hook for `LaunchNeedsUpdateView` (`launch-fallthrough-button`).
    /// `FaunaApp` sets it; the closure drops the saved (nest_url, device_id),
    /// re-seeds onboarding at handle_entry with the identity, and clears
    /// `needsUpdateMessage` (touches App-owned keychain + onboarding glue, so it
    /// lives in `FaunaApp` — same pattern as `onFactoryReset`). Mirrors the macOS
    /// `fallbackToHandleEntry` the `.needsUpdate` phase routes to. Shared with
    /// `LaunchIdentityChangedView`, whose fallthrough is the same wizard exit.
    public var onUseDifferentNest: (() -> Void)?
    /// Post-onboarding launch hook. `FaunaApp` sets this to re-run
    /// `checkKeychainOnLaunch()`; the onboarding `.feed` exit (`WelcomeView`)
    /// calls it so the identity `performLoggedInHandoff` just persisted is picked
    /// up and the authed `FaunaClient` is built (which also fires the first-setup
    /// mail/caldav glue). Without it, fresh onboarding would enter the home tabs
    /// with a nil client. Mirrors `onFactoryReset` + macOS
    /// `MacAppState.onLaunchAuthenticated`.
    public var onLaunchAuthenticated: (() -> Void)?
    /// Contacts from the last loadContacts() call, for E2E state serialization.
    public var lastContacts: [Contact] = []
    /// Knocks from the last loadKnocks() call, for E2E state serialization.
    public var lastKnocks: [Knock] = []
    /// Events from the last loadEvents() call, for E2E state serialization.
    public var lastEvents: [EventSummary] = []
    /// Unread notification count, for E2E state serialization.
    public var notificationsUnreadCount: Int = 0
    /// The Events page's view model, published here by `CalendarListView` so
    /// the app delegate's background-transition leave-flush can reach it
    /// (`reserved-folders.md` § The leave-flush promise) — the VM otherwise
    /// lives entirely inside the view (`@State`). Mirrors macOS
    /// `MacAppState.eventsVM`.
    public var eventsVM: EventsVM?

    /// The Conversations and Feed page view models, published here by
    /// `FaunaApp` at launch so the app delegate's background-transition
    /// leave-flush can reach them (`reserved-folders.md` § The leave-flush
    /// promise). Unlike `eventsVM` above — which lives inside its view and is
    /// published by `CalendarListView` when that page first appears — these two
    /// are `@State` on the App itself and so exist from launch; publishing them
    /// beside `pushManager`/`appState` keeps one reachability shape rather than
    /// two. Mirrors macOS `MacAppState.conversationsVM`/`feedVM`.
    public var conversationsVM: ConversationsVM?
    public var feedVM: FeedVM?

    public init() {
        // Convention 11's MOUNTED-SHELL derivation for iOS, installed once and
        // read live (`SessionState.mainShellMountedProbe`). This is exactly the
        // expression `ContentView.body` branches on, in the same precedence
        // order — the blocking identity-changed surface first, then the terminal
        // needs-update surface, then the wizard — so "the flag is true" and "the
        // authenticated shell is what's rendered" cannot come apart.
        //
        // ⚠ Keep this in lockstep with `ContentView.body`. A new BLOCKING
        // pre-authenticated surface added there owes a clause here too, or the
        // agent reports an authenticated session while that surface is up.
        session.mainShellMountedProbe = { [weak self] in
            guard let self else { return false }
            return self.identityChanged == nil && self.needsUpdateMessage == nil
                && self.signInRefusedMessage == nil && !self.isOnboarding
        }
    }

    /// Exit the admin shell back to the non-admin app's **primary view**
    /// (Conversations) — the single "leave admin" affordance (`admin-nav-back`,
    /// admin.md:30). Not an inter-admin-page jump; switching admin pages is the
    /// shell's own switcher. Lands on the primary view, not the Settings shell.
    public func leaveAdmin() {
        inAdmin = false
        selectedTab = "conversations"
        moreSelectedView = nil
    }
}
