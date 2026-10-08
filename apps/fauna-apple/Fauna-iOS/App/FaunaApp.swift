import SwiftUI
import SwiftData
import FaunaDeepLink
import FaunaKit

#if os(iOS)
import UserNotifications

// MARK: - Push notification tapped

public extension Notification.Name {
    static let pushNotificationTapped = Notification.Name("pushNotificationTapped")
}

// MARK: - AppDelegate

class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    var pushManager: PushManager?

    /// Set alongside `pushManager` at authenticated launch — the leave-flush
    /// door (`applicationDidEnterBackground` below) reaches all three drafts
    /// rails' VMs through it (`reserved-folders.md` § The leave-flush promise),
    /// the same reachability shape `pushManager` already needed for APNs.
    var appState: AppState?

    /// The leave-flush promise's iOS door: "moving to the background on a
    /// phone (the OS may kill a backgrounded app at any moment, so the flush
    /// rides the background transition, never the kill)". `beginBackgroundTask`
    /// buys the process real wall-clock time to finish the network write
    /// after the app is no longer in the foreground — without it, the OS is
    /// free to suspend the process the instant this method returns, and the
    /// detached `Task` below would never get to run.
    func applicationDidEnterBackground(_ application: UIApplication) {
        // All THREE drafts rails ride this door. Each `flushDraftsNow()`
        // no-ops fast when its rail has nothing pending (no attached sync, or
        // an unchanged snapshot), so a quiet background transition costs three
        // cheap compares and no round trip.
        guard let appState else { return }
        let eventsVM = appState.eventsVM
        let conversationsVM = appState.conversationsVM
        let feedVM = appState.feedVM
        guard eventsVM != nil || conversationsVM != nil || feedVM != nil else { return }
        var task: UIBackgroundTaskIdentifier = .invalid
        task = application.beginBackgroundTask(withName: "fauna.drafts.flush") {
            application.endBackgroundTask(task)
            task = .invalid
        }
        // A REFUSED extension is not a reason to skip the flush. It means "you
        // may be suspended sooner", not "do not save" — and the save is the
        // whole promise. So the flush runs either way; the extension only buys
        // wall-clock time to finish the network write. (Guarding on it would
        // also make this door's witness silently vacuous whenever the OS said
        // no.) `endBackgroundTask` is called only for an extension we actually
        // got — ending `.invalid` is an API misuse.
        Task {
            await eventsVM?.flushDraftsNow()
            await conversationsVM?.flushDraftsNow()
            await feedVM?.flushDraftsNow()
            if task != .invalid {
                application.endBackgroundTask(task)
                task = .invalid
            }
        }
    }

    func application(
        _ application: UIApplication,
        didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
    ) {
        pushManager?.didRegisterForRemoteNotifications(deviceToken: deviceToken)
    }

    func application(
        _ application: UIApplication,
        didFailToRegisterForRemoteNotificationsWithError error: Error
    ) {
        pushManager?.didFailToRegisterForRemoteNotifications(error: error)
    }

    // MARK: - UNUserNotificationCenterDelegate

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        let userInfo = response.notification.request.content.userInfo
        if let url = userInfo["url"] as? String {
            NotificationCenter.default.post(
                name: .pushNotificationTapped,
                object: nil,
                userInfo: ["url": url]
            )
        } else if let senderId = userInfo[NotificationManager.knockSenderIdKey] as? String {
            // Same destination `NotificationsView.open`'s `.knock` arm sends a
            // tapped notification-feed row to — the OS toast is a second door
            // into the identical `pendingKnockSenderId` hand-off.
            await MainActor.run {
                appState?.pendingKnockSenderId = senderId
                appState?.selectedTab = "contacts"
            }
        }
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .badge, .sound]
    }
}
#endif

public struct FaunaApp: App {
    /// The photo-backup dedup/upload-state store, actor-scoped
    /// (`account-scoping.md` § The scoping taxonomy, class 4 — re-derivable,
    /// wipe-not-adopt). `@State`, not `let`: rebuilt for the resolved actor
    /// at `completeAuthenticatedLaunch` and reset to the flat/no-actor
    /// container at every session-teardown path (`tearDownSessionForSwitch`,
    /// `resetToFactory`), the same lifecycle `client` already has. Mirrors
    /// `FaunaMacApp`.
    @State private var modelContainer: ModelContainer
    /// **The live photo-backup container, resolved the ONE way every
    /// callback-reached site must resolve it** — `AppState.liveModelContainer`
    /// carries the why, and it is `liveFaunaClient`'s below in miniature: the
    /// observed slot first, the `@State` only as the pre-auth fallback. Mirrors
    /// `FaunaMacApp`, which carries the identical pair for the identical reason.
    private var liveModelContainer: ModelContainer { appState.liveModelContainer ?? modelContainer }

    @State private var appState = AppState()
    /// **The live `FaunaClient`, resolved the ONE way every callback-reached site
    /// must resolve it** — the observed slot first, the `@State` only as fallback.
    ///
    /// This `App` is a **struct**, and the closures the test agent is started with
    /// (`TestAgentBootstrap`, `commandHandler: { [self] … }`) capture `self` BY
    /// VALUE at init. That copy's `client` stays the nil it was born with for the
    /// whole process lifetime, however many times a later sign-in writes the
    /// `@State`. `appState` is a **class**, so the very same stale copy still
    /// reaches the live `AppState` object — which is why `serializeState()` and
    /// every `handleTestCommand` arm already read `appState.liveClient` and
    /// nothing else.
    ///
    /// ⚠ **`resetToFactory` did not, and the cost was SILENT** (measured
    /// 2026-09-22, `--app macos`): its `stopAccountRuntimeForSignOut()`,
    /// `releaseAccountScopedStores()` and `shutdown()` all hung off `client?`, so
    /// on the harness's per-test reset — the most-executed path in the whole
    /// macOS suite — **the optional chain skipped all three**. Nothing logged,
    /// because a skipped call reaches no code that logs, and the erase either
    /// side of it (static `KeychainStore` / `AccountStateDir` calls, which need
    /// no captured state) went on working — so the reset looked like it worked.
    /// The account runtime survived every sign-out holding the writer key the
    /// erase had just taken, and was stopped only by the NEXT sign-in's
    /// supersession — a plain shutdown, which by `StopReason`'s rule retires
    /// nothing nest-side. One stranded `fauna` placeholder row per reset, for
    /// ever (`sync-agent-credentials.md` § Credential model → *The signed-out
    /// reconcile*; the roster witness is `test_sign_out_device_roster.py`).
    ///
    /// Same ordering rationale as the `.environment` injection's, which this
    /// property is now the single definition of: in the ordinary case both slots
    /// hold the same object and the order cannot matter; it matters only where
    /// they differ, and there the observed one is the live one.
    ///
    /// `tearDownSessionForSwitch` deliberately keeps its own pair of shutdowns —
    /// there the point is to reclaim BOTH objects when they differ, not to pick
    /// the live one.
    private var liveFaunaClient: FaunaClient? { appState.liveClient ?? client }
    @State private var onboardingVM = OnboardingVM()
    /// One shared `ConversationsVM` (thin observer over `fauna-conversations`'s
    /// `ConversationsManager`) for the unified conversations page. Held here
    /// (a class reference, like `onboardingVM`) so the TestAgent's conversations
    /// commands can reach the same manager the views render off. `init()` swaps
    /// in deterministic mock backends under e2e (`FaunaE2E.isActive` — the
    /// XCUITest bridge *or* the in-process server). Mirrors `FaunaMacApp`.
    @State private var conversationsVM = ConversationsVM()
    /// One shared `FeedVM` for the feed page, held here (like `conversationsVM`)
    /// so the TestAgent's `feed_inject_posts` injects into the SAME manager
    /// `FeedListView` renders off. `FfiNestClient.feed_manager` builds a fresh
    /// manager per call, so a handler calling `api.feedManager` again would seed a
    /// throwaway the view never observes. Mirrors `FaunaMacApp`.
    @State private var feedVM = FeedVM()
    /// One shared `EventsVM` for the events page, held here (like `feedVM`) so a
    /// half-written event outlives leaving the page: a page-owned VM died with
    /// the page, and the next one's launch restore raced the dead one's pending
    /// debounced save, so New Event came back blank (`events.md` § Persistence).
    /// Dropped at an identity change by `ActorScope.dropAppOwnedState`.
    @State private var eventsVM = EventsVM()
    /// The session's ONE `DevicesMachineVM`, shared by the Devices and Folders
    /// pages (like `feedVM`): the machine's followed-folders memory and refresh
    /// barrier must outlive a page visit (`ui/folders.md` § Implementation
    /// status today). Dropped at an identity change by
    /// `ActorScope.dropAppOwnedState`.
    @State private var devicesVM = DevicesMachineVM()
    /// One shared `ProfileEditVM` for the SELF profile edit form, held here (like
    /// `feedVM`/`conversationsVM`) so the TestAgent's `compose.file` avatar/banner
    /// staging reaches the SAME instance `ProfileEditFormView` renders off.
    /// Mirrors `FaunaMacApp`.
    @State private var profileEditVM = ProfileEditVM()
    /// Process-wide critical-alerts registry owner (`critical-alerts.md`
    /// § Mechanism), held here — like `feedVM`/`conversationsVM` — so the
    /// `CriticalAlertsBanner` and the session-start sweep loop share the SAME
    /// instance across a `FaunaClient` rebuild. Mirrors `FaunaMacApp`.
    @State private var criticalAlertsHost = CriticalAlertsHost()
    @State private var client: FaunaClient?
    /// The shared `LaunchMachine` for this launch, held so the
    /// `launch_identity_changed` trust button can call `trustNestIdentity()` on the
    /// instance that produced the verdict (it reads the secret + nest_url off that
    /// machine's own `IdentityChanged` state — a fresh one re-challenges from `Boot`
    /// and the button would silently no-op). Mirrors
    /// `FaunaMacApp.launchMachineBox`.
    ///
    /// A **reference-type box** (`LaunchMachineBox`), not a bare `@State
    /// LaunchMachine?` — see that type's doc comment for why: the post-auth
    /// escalation re-enters `runLaunch()` through the test bridge's
    /// init-time-captured `self`, where a plain `@State` assignment silently
    /// no-ops and leaves the live view holding the *boot* machine, which turns
    /// this button into a no-op instead of a missing one.
    @State private var launchMachineBox = LaunchMachineBox()
    /// Stash for the most recent value-returning `call_machine_method`
    /// dispatch (e.g. `provisioning_snapshot`, `provider_base_url`), read
    /// back by the E2E bridge via `machine_method_result` in
    /// `serializeState()`. `nil` after a setter-only call. Mirrors
    /// `FaunaMacApp.machineMethodResultBox`.
    /// A **reference-type box** (`MachineMethodResultBox`), not a plain
    /// `@State` value — see the type's own doc comment for why a bare `@State
    /// private var machineMethodResult: Any?` silently drops every write made
    /// through `startInProcessAgentIfNeeded()`'s init-time-captured `self`.
    @State private var machineMethodResultBox = MachineMethodResultBox()
    @Environment(\.scenePhase) private var scenePhase

    #if os(iOS)
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @State private var pushManager: PushManager?
    #endif

    public init() {
        // Install the in-process `fauna_log` ring + daily-rolling file
        // (observability.md § Shared capture) FIRST, so startup events are
        // captured. The ring drives the Settings → Logs view (`LogsView`).
        // Mirrors macOS `FaunaMacApp` + linux's `install_logging()`.
        let logDir = (try? FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask,
            appropriateFor: nil, create: true))?.path ?? NSTemporaryDirectory()
        installLogging(dataDir: logDir)

        // `SyncFile` + `SyncAnchor` retired with the B2 engine cutover — both were
        // derived projections of nest state (the `fauna.sync.files` / changes feed
        // and its cursor), re-derivable by the shared engine into its own per-set
        // `SyncDb`, so dropping them destroys nothing the user cannot recreate.
        // `PhotoBackupRecord` stays: it maps OS asset identity, not sync state.
        // `CachedAccount` and `Snapshot` were removed as dead code, 2026-08-10:
        // zero read/write sites beyond their own `@Model` declaration and this
        // schema registration.
        _modelContainer = State(initialValue: PhotoBackupRecord.buildModelContainer(actorIdHex: nil))
        // In E2E mode, clear persisted state so the app always starts from
        // onboarding. Without this, keychain credentials from previous test
        // sessions cause the app to skip onboarding. The wizard owns no
        // persistence post-cleanup (tracked internally), so the registry's
        // `clearAll` (every account + the index) is sufficient to return the
        // wizard to identity_choice on the next launch.
        // E2E mode = either the XCUITest bridge (FAUNA_E2E_BRIDGE) or the new
        // in-process automation server (FAUNA_E2E_AGENT_PORT) — both want the app
        // to start from a clean onboarding state (mirrors FaunaMacApp).
        if FaunaE2E.isActive {
            // Skip the wipe when the harness owns the store's lifecycle (it passed a
            // `FAUNA_E2E_CREDENTIAL_DIR`) — a fresh dir per launch already IS the clean
            // start, and a PINNED dir is a deliberate `preserve_state_across_relaunch()`
            // whose whole point is that the state survives. Mirrors FaunaMacApp.
            if !KeychainStore.e2eHarnessOwnsStore {
                _ = FaunaAccounts.registry(keychain: KeychainStore()).clearAll()
            }
            // Explicitly mirror macOS parity (FaunaMacApp.swift): set
            // these flags eagerly so the initial render always shows onboarding,
            // even before runLaunch() fires in the .task modifier.
            appState.session.clearAuthenticatedOverride()
            appState.isOnboarding = true
        }

        // Install the disk-backed nest-identity pin store before the first
        // authenticated connect, so TOFU pins (self-signed / LAN nests) survive
        // restarts (security.md § Transport trust). Mirrors macOS + linux.
        NestTrust.installPinStore()

        // Lend the Keychain to shared Rust's credential slots before the first
        // sign-in: iOS has no Rust keyring arm, so without this the W3 (account-data-plane.md § Workstreams) account
        // runtime's writer key can never persist and the account plane never
        // assembles on a real device (see the method). Mirrors macOS + android.
        FaunaAccounts.installPlatformCredentialStore()

        // Start the in-process automation server in init() (before any render)
        // so its /health is up the moment the e2e driver polls it. Gated on
        // FAUNA_E2E_AGENT_PORT; mirrors FaunaMacApp.startInProcessAgentIfNeeded.
        // (The legacy XCUITest TestAgent stays started from the `.task` modifier
        // under FAUNA_E2E_BRIDGE — it polls a remote bridge, the in-process
        // server hosts its own.)
        // Compiled out of release artifacts (testing.md convention 15).
        #if DEBUG
        TestAgentBootstrap.startInProcessAgentIfNeeded(
            stateProvider: { [self] in self.serializeState() },
            commandHandler: { [self] command in await self.handleTestCommand(command) }
        )
        #endif
    }

    public var body: some Scene {
        WindowGroup {
            ContentView(onboardingVM: onboardingVM)
                .environment(appState)
                // The shared FaunaKit `ThreadDetailView` reads
                // `@Environment(ContentPolicyStore.self)` to gate a flagged
                // bubble (family-safety.md § Content policy); the iOS feed card
                // reads the same store off `AppState` directly.
                .environment(appState.contentPolicy)
                // …and composes the region content plane into the same verdict
                // (`region-blocking.md` § Where it composes).
                .environment(appState.region)
                // The shared FaunaKit `ProfileView` reads
                // `@Environment(FamilyStatusStore.self)` for the ward's durable
                // "asked — waiting for your guardian" state, and the two Bridges
                // hosts hand it to `BridgeManagerVM` (family-safety.md §§
                // Child-initiated contact requests / Feed-source approvals).
                .environment(appState.familyStatus)
                // Inject the FaunaClient every `@Environment(FaunaClient.self)`
                // view (admin shell, mail-settings, …) reads. **The observed slot
                // is asked FIRST, the `@State` is only the fallback** — every path
                // that builds a client writes both, so the order matters only when
                // they differ, and when they differ `client` is the one holding an
                // object this app has already replaced (a `@State` write from the
                // test agent's `applySessionPatch` callback does not reach this
                // closure; `appState` is `@Observable` and does). The iOS peer of
                // FaunaMacApp's `appState.liveClient ?? client`, which carries the
                // measurement that ordered them this way.
                .environment(liveFaunaClient)
                // The shared FaunaKit `PhotoBackupControlsView` (Media tab + the
                // Settings "Photo Backup" link) reads `@Environment(PhotoBackupEngine.self)`.
                // On iOS the engine is `FaunaClient.photoBackup` (configured at init);
                // `#if os(iOS)`-only because that property is iOS-gated on FaunaClient
                // (the `swift build --target FaunaiOS` host typecheck excludes it).
                #if os(iOS)
                .environment(liveFaunaClient?.photoBackup)
                #endif
                .environment(conversationsVM)
                .environment(feedVM)
                .environment(eventsVM)
                .environment(devicesVM)
                .environment(profileEditVM)
                .environment(criticalAlertsHost)
                .modelContainer(liveModelContainer)
                // A mid-session supersession, suspension or nest-identity change
                // the connection supervisor stopped on → the launch surface
                // (`escalateSessionEnding`; a supersession this device's own
                // ceremony caused is held back until the user leaves Account).
                .onSessionEnding { verdict in
                    await escalateSessionEnding(verdict) { await escalateToLaunchSurface() }
                }
                .task {
                    runLaunch()
                    // Compiled out of release artifacts (testing.md convention 15).
                    #if DEBUG
                    TestAgentBootstrap.startTestAgentIfNeeded(
                        stateProvider: { [self] in self.serializeState() },
                        commandHandler: { [self] command in await self.handleTestCommand(command) }
                    )
                    #endif
                }
                // `fauna://` deep links — the same-device handoff's consent route
                // (`ConsentHandoff`) and the FP context actions (Files app
                // long-press → Share / Version history; `FaunaDeepLink`).
                // Identity/peer URI forms are pasted/scanned into their own
                // onboarding surfaces, never OS-opened, so any other link is
                // ignored here. A consent route held while signed out applies on
                // the authenticated flip.
                .onOpenURL { url in handleDeepLink(url) }
                .onChange(of: appState.session.isAuthenticated) { _, authenticated in
                    if authenticated { applyHeldRoute() }
                }
                .onAppear {
                    // The leave-flush door's reachability, wired ONCE PER LAUNCH
                    // and on no particular launch path — the macOS twin's exact
                    // placement and the reason for it (`reserved-folders.md`
                    // § The leave-flush promise). `applicationDidEnterBackground`
                    // reaches the three drafts rails only through `appState`,
                    // and both VMs are `@State` here on the App, so nothing but
                    // this handoff can give the delegate either.
                    //
                    // ⚠ It used to live in `enterOptimistically`, which is a
                    // BRANCH, not the launch: it is withheld for a
                    // pending-factory-reset slot, and the `.authenticated` arm
                    // re-runs it only `if client == nil`. A launch that already
                    // held a client therefore left `appDelegate.appState` nil
                    // and the door's own `guard let appState else { return }`
                    // silently took the early exit — the leave-flush promise
                    // off for that whole session, with nothing to see. Every
                    // e2e session-patch login took exactly that path, which is
                    // how the iOS witness found it.
                    //
                    // `#if os(iOS)`: `appDelegate` is declared under the same
                    // guard above (`@UIApplicationDelegateAdaptor` is UIKit-only),
                    // and this file also compiles as `FaunaiOSLib` for the host
                    // under `swift test`, where the symbol does not exist.
                    #if os(iOS)
                    appDelegate.appState = appState
                    #endif
                    appState.conversationsVM = conversationsVM
                    appState.feedVM = feedVM
                    // Wire the admin-nest Factory Reset re-onboard hook. Set here
                    // (not threaded through the view tree) so the shared FaunaKit
                    // `AdminNestView` stays platform-agnostic — it just calls the
                    // closure with the post-reset claim code (mirrors FaunaMacApp).
                    appState.onFactoryReset = { claimCode in
                        Task { @MainActor in await factoryResetReonboard(claimCode: claimCode) }
                    }
                    // "Use a different nest" (`launch-fallthrough-button`) — shared by
                    // BOTH blocking launch surfaces: the non-retry needs-update one
                    // (version-compatibility.md Dim 4) and `launch_identity_changed`
                    // (security.md names this fallthrough as one of its two
                    // ways out, the other being the trust button below).
                    appState.onUseDifferentNest = {
                        Task { @MainActor in useADifferentNest() }
                    }
                    // "Retry" on `launch_sign_in_refused` (`launch-retry-button`) — the
                    // way back in after the admin restores the account, on the HELD
                    // machine (`LaunchMachine::retry_silent_challenge`).
                    appState.onRetrySignIn = {
                        Task { @MainActor in retrySignIn() }
                    }
                    // "Trust this nest" (`nest-identity-changed-trust-button`) — forget
                    // the TOFU pin, re-TOFU, re-challenge, on the HELD machine.
                    appState.onTrustNestIdentity = {
                        Task { @MainActor in trustNestIdentity() }
                    }
                    // "Start over on this device" (`account-index-reset-button`) —
                    // reveals the confirm; and its confirm
                    // (`account-index-reset-confirm-button`) — the documented
                    // client-side floor.
                    appState.onAccountIndexStartOver = {
                        Task { @MainActor in revealAccountIndexStartOver() }
                    }
                    appState.onAccountIndexConfirmStartOver = {
                        Task { @MainActor in confirmAccountIndexStartOver() }
                    }
                    // Post-onboarding launch hook: the fresh-onboarding `.feed`
                    // exit (`WelcomeView`) calls this to build the authed
                    // FaunaClient via the same launch path a returning user hits
                    // (the identity is already recorded in the registry by
                    // `performLoggedInHandoff`). Mirrors `onFactoryReset`.
                    // The wizard's `LoggedIn` exit means two different things now. First
                    // identity → boot the session. **Appended** identity → register it and
                    // switch to it; the append wrote nothing to the store, so a
                    // `runLaunch()` here would boot the still-active old account and drop
                    // the identity the user just added (see `completeAppendedAccount`).
                    appState.onLaunchAuthenticated = {
                        Task { @MainActor in
                            if appState.isAddingAccount {
                                await completeAppendedAccount()
                            } else {
                                runLaunch()
                            }
                        }
                    }
                    // Append-mode pending-invite adoption (`onboarding.md` § Multi-account: "the
                    // append glue adopts on the submit return"). That journey has no wizard exit,
                    // so `onLaunchAuthenticated` above never fires for it; the VM reports each
                    // slot write here instead, and an "Add account" wizard answers by leaving
                    // append mode and switching to the identity the shared writer just registered
                    // and activated. `isAddingAccount` is cleared HERE, synchronously, before the
                    // switch task starts — the one-shot latch, since the poll reports every tick.
                    // (`completeAppendedAccount` is not reusable: it bails without the
                    // `LoggedIn` home nest a pending identity does not have yet.) Mirrors
                    // `FaunaMacApp`.
                    onboardingVM.onPendingInvitePersisted = { actorId in
                        guard appState.isAddingAccount else { return }
                        appState.isAddingAccount = false
                        Task { @MainActor in try? await switchAccount(to: actorId, confirmed: false) }
                    }
                    // Account switcher (Account settings) — same seam as `onFactoryReset`,
                    // so the shared FaunaKit `AccountSwitcherSection` stays platform-agnostic.
                    appState.onSwitchAccount = { actorId, confirmed in
                        try await switchAccount(to: actorId, confirmed: confirmed)
                    }
                    appState.onAddAccount = {
                        Task { @MainActor in beginAddAccount() }
                    }
                    // Sign-out / delete-account (`SignOutSection`, `AccountSettingsView`'s
                    // `onAccountReset`). Set here for the same reason as `onFactoryReset` —
                    // and, unlike those two call sites' own `{ appState.isOnboarding = true }`
                    // closures, this one is NOT cosmetic: without it the outgoing session's
                    // `client`/DNS+subscription cadences kept running in memory after a real
                    // Sign Out tap — only the test-only `logoutKeepData()` / the live
                    // `switchAccount()` ever ran this checklist. Mirrors `FaunaMacApp`.
                    //
                    // ⚠ `isOnboarding = true` is set HERE, as this Task's own LAST line, not
                    // only by the caller. `tearDownSessionForSwitch()` (below) sets
                    // `isOnboarding = false` internally (its ordinary teardown default), and
                    // that call always runs on the MainActor AFTER the synchronous
                    // `onAccountReset` closure that invoked `onSignOut?()` has already
                    // returned — so a caller-only `isOnboarding = true` is deterministically
                    // clobbered back to `false` moments later, stranding the just-signed-out
                    // user on the Settings root list. macOS's mirror closure never hit this:
                    // its caller and its `tearDownSessionForSwitch()` both drive
                    // `isOnboarded` toward the SAME value (`false`), so the ordering never
                    // mattered there. Owning the flip here, after the teardown, makes it
                    // correct regardless of scheduling order.
                    appState.onSignOut = {
                        // Synchronous, before the Task below: the residue must
                        // already be on `onboardingVM` by the time
                        // `isOnboarding = true` mounts the wizard, and nothing
                        // between here and there triggers a machine transition
                        // that would clear it (`OnboardingVM.signOutResidue`'s doc).
                        onboardingVM.takeSignOutResidue(from: appState.session)
                        Task { @MainActor in
                            await tearDownSessionForSwitch()
                            if !FaunaE2E.isActive {
                                await FileProviderCoordinator.signOut()
                            }
                            appState.isOnboarding = true
                        }
                    }
                }
                // Append-mode onboarding: a sheet over the LIVE session (the client keeps
                // running underneath). Dismissing only closes the sheet — it must never
                // strand the running session; an identity a cancelled wizard left in the
                // registry is read by the next launch like any other.
                .sheet(isPresented: $appState.isAddingAccount) {
                    WelcomeView(vm: onboardingVM)
                        .environment(appState)
                }
                .onChange(of: scenePhase) { _, newPhase in
                    switch newPhase {
                    case .active:
                        Task { await client?.resume() }
                        #if os(iOS)
                        Task { try? await UNUserNotificationCenter.current().setBadgeCount(0) }
                        #endif
                    case .background:
                        // Seal the engagement-cue rollup before the process can be
                        // suspended (engagement-cues.md § At rest: put on batch or
                        // on background/close) — iOS's analogue of linux's
                        // flush-on-close-request. Best-effort, exactly as linux
                        // treats its own flush: a failed put leaves the rollup
                        // marked dirty, so the next session's debounce retries it,
                        // and there is no UI left to surface an error on.
                        Task { await CueViewportObserver.flushForAppLifecycle() }
                        client?.suspend()
                    default:
                        break
                    }
                }
        }
    }

    /// Drive the launch through the **shared** `LaunchMachine` and render its verdict.
    ///
    /// Every routing decision lives in `libs/fauna-launch-machine` and is shared with
    /// linux / web / windows / android / tui: the four-case hydration table, the
    /// pending-factory-reset **boot reconcile** (gap CR-2), the silent challenge and its
    /// claimed/unclaimed fallback table, and the nest-identity-pin (TOFU) check. This
    /// file only *renders* the resulting `LaunchPhase`. Mirrors `FaunaMacApp.runLaunch()`
    /// — see its doc for what apple's old hand-rolled Swift table missed.
    ///
    /// **iOS keeps its optimistic entry.** Unlike macOS (which gates behind a launch
    /// spinner), iOS renders the home tabs straight off the cached identity and reconciles
    /// in the background — its documented posture (`AppState.needsUpdateMessage`;
    /// `onboarding.md` § Implementation status today). That is why the machine is *not*
    /// awaited before deciding what to show: `isOnboarding` defaults to `true`, so a
    /// network round-trip in front of the first render would flash the **wizard** on every
    /// launch. Instead we enter optimistically and let the machine's verdict correct
    /// course — including backing out of the session entirely when it must.
    @MainActor
    private func runLaunch() {
        let keychain = KeychainStore()

        // Bring credential rows in line with the iCloud-backup preference (default:
        // device-bound) — a self-healing re-converge of any row a crash mid-toggle left
        // behind; idempotent. apps/ios.md § Credential Storage.
        keychain.reconcileCredentialAccessibility()

        // The optimistic read below and the machine both resolve through the registry's
        // ACTIVE account, so a same-process account SWITCH (`switchAccount(to:)` moves the
        // active pointer) builds the incoming identity's session on both halves — there is
        // no single-slot row that could still name the outgoing account.
        let machine = LaunchMachine(
            observer: NullLaunchObserver(),
            persistence: FaunaAccounts.bootLaunchPersistence(keychain: keychain)
        )
        // Hold it — `trustNestIdentity()` must run on the machine that produced the
        // verdict (it reads the secret + nest_url off that machine's own
        // `IdentityChanged` state). See `FaunaMacApp`'s holder for the full note.
        launchMachineBox.machine = machine

        // Optimistic entry is withheld in exactly one case: a pending-factory-reset slot.
        // The box may have been wiped, so entering the home tabs first would flash a
        // session against a nest that is not there — and whether the slot is even REAL is
        // the machine's CR-2 boot reconcile to decide (a slot left by a permanently-failed
        // dispatch is stale, and the box is healthy and still claimed), not ours to assume.
        // The slot is the active account's per-actor row, read through the same launch
        // persistence the machine routes on.
        if FaunaAccounts.launchPersistence(keychain: keychain).loadPendingFactoryReset() == nil {
            enterOptimistically(keychain: keychain)
        }

        Task { @MainActor in
            await machine.start()
            // A verdict renders only while its own launch is still the held one — the
            // macOS twin carries the same guard, and `LaunchMachineBox.machine`'s doc
            // owns the rule. iOS needs it for the same reason and one more: it has
            // already entered optimistically above, so a superseded verdict here does
            // not merely misroute a wizard, it can tear down a shell the patch (or the
            // next launch) just built.
            guard launchMachineBox.machine === machine else {
                logMessage(
                    level: .info, target: "fauna.app",
                    message: "[launch] verdict \(machine.snapshot().phase.diagnosticName) "
                           + "superseded before it rendered — dropping it")
                return
            }
            await dispatchLaunch(machine.snapshot(), keychain: keychain)
        }
    }

    /// Render the home tabs from the cached identity without waiting for the network.
    /// No-op unless the served account's session material holds a complete, well-formed
    /// (secret, nest_url, device_id) — the registry's one session-identity read
    /// (`FaunaAccounts.sessionMaterial`).
    ///
    /// This is a *bet* that the machine will land on `.online` — overwhelmingly the common
    /// case for a returning user. `dispatchLaunch` settles it either way.
    @MainActor
    private func enterOptimistically(keychain: KeychainStore) {
        let material = FaunaAccounts.sessionMaterial(keychain: keychain)
        guard let material,
              let nodeUrl = material.nestUrl,
              let url = URL(string: nodeUrl),
              let deviceId = material.deviceId else {
            // No usable session to bet on — stay in the wizard and let the machine say
            // which page it opens on.
            //
            // Logged under `[admin-gate]` because this is the admin chain's ZEROTH
            // link: `enterOptimistically` is the only launch path that builds a
            // `FaunaClient` from stored credentials, so bailing here is what makes
            // every later `[admin-gate] no client → isAdmin=false` inevitable rather
            // than a race. Without this line the chain's reader can see the probe
            // fail-close but not whether a client ever existed to find.
            logMessage(level: .warn, target: "fauna.app",
                       message: "[admin-gate] launch: no usable stored session (secret/nest_url/device_id) → NO FaunaClient built")
            appState.isOnboarding = true
            return
        }

        let secret = material.secretHex
        let actorId = material.actorId
        appState.session.secretHex = secret
        appState.session.actorId = actorId
        appState.session.nodeUrl = nodeUrl
        appState.session.deviceId = deviceId
        // Pre-populate the handle from the account's server-data cache so settings/status
        // views render instantly on relaunch. The authoritative refresh is the machine's
        // own `save_authenticated` write, picked up in `completeAuthenticatedGlue`.
        appState.session.handle = material.handle
        // `isOnboarding = false` IS the authenticated flip now: `session.isAuthenticated`
        // derives from it live (`AppState.init`'s mounted-shell probe), so there is no
        // second flag to keep in step here or on any teardown path.
        appState.isOnboarding = false

        // Re-scope the photo-backup store to THIS actor before anything reads
        // `modelContainer.mainContext` below (`account-scoping.md` § Serialized
        // switching) — unconditionally, mirroring `FaunaMacApp`: every path
        // through here (cold boot, post-switch `runLaunch()`) needs the swap,
        // and a same-actor re-dispatch rebuilding against the identical
        // scoped file is harmless.
        // Both halves from ONE container, always together — `AppState`'s
        // `liveModelContainer` is what the reads resolve to, and this is one of
        // only two sites that writes the pair. One build, not two: a second
        // container over the same store file is a second open handle to it.
        let scoped = PhotoBackupRecord.buildModelContainer(actorIdHex: actorId)
        modelContainer = scoped
        appState.liveModelContainer = scoped

        // Dial the harness's override (e2e only; the identity function in a
        // release build) rather than the literal typed URL — the store read
        // is where every app but tui begins, so this is the one dial call
        // site a cold-boot relaunch needs (`fauna_launch_machine::dial`
        // module docs). The stored/rendered `nodeUrl`
        // above is untouched — only the socket target changes.
        let dialUrl = URL(string: resolvedDialUrl(nestUrl: nodeUrl)) ?? url
        let faunaClient = FaunaClient(
            nodeUrl: dialUrl, secretHex: secret,
            deviceId: deviceId, modelContext: scoped.mainContext
        )
        self.client = faunaClient
        // ALSO store on the observed AppState — the same reason `applySessionPatch`
        // does it, and for the same class of caller. `enterOptimistically` runs
        // synchronously inside the App's own `.task { runLaunch() }`, i.e. during the
        // first view-update pass, and a `@State` write from there does NOT reliably
        // re-invoke the `WindowGroup` content closure — so `.environment(client ??
        // appState.liveClient)` keeps handing out the nil it was born with, for the
        // whole process lifetime. `appState` is `@Observable` and `liveClient` IS read
        // by that closure (`??` evaluates its RHS while `client` is nil), so writing it
        // here is what actually invalidates the injection.
        //
        // Concretely, this is the iOS admin-gate bug: `SettingsView`'s
        // `.task(id: client != nil)` fired 447 ms AFTER the client was built, still saw
        // nil, fail-closed `isAdmin=false`, and never re-fired because the id never
        // flipped — so the admin auto-default never ran and `admin-tab` never revealed.
        // The launch path reached the App's `@State` and nothing else.
        appState.liveClient = faunaClient
        // The chain's zeroth link, positive half (see the guard above). The
        // TIMESTAMP is the payload: ordered against the `[admin-gate]` probe line
        // it separates the only two remaining explanations for a nil-client probe
        // on a launch that IS authenticated — built BEFORE the probe ⇒ the
        // environment injection never reached the view; built AFTER ⇒ the view's
        // `.task(id: client != nil)` never re-fired on the transition. Those need
        // opposite fixes, and no other witness distinguishes them.
        logMessage(level: .info, target: "fauna.app",
                   message: "[admin-gate] launch: FaunaClient built + assigned to @State client (actor \(actorId))")
        Task {
            await faunaClient.start()
            // Critical-alerts session-start + periodic re-sweep loop
            // (critical-alerts.md § Mechanism → *Who runs the detector* +
            // *How often*) — mirrors macOS's `completeAuthenticatedLaunch`.
            criticalAlertsHost.startSweepLoop(api: faunaClient.api)
        }

        #if os(iOS)
        // Initialize push notification manager
        let pm = PushManager(api: faunaClient.api, deviceId: deviceId)
        self.pushManager = pm
        appDelegate.pushManager = pm
        // `appDelegate.appState` and the two drafts-rail VM slots are NOT set
        // here: this function is a branch (withheld for a pending-factory-reset
        // slot, re-entered only `if client == nil`), and the leave-flush door
        // must be reachable on every launch. They are wired in the scene's
        // `.onAppear`, where macOS wires its own — see the note there.
        UNUserNotificationCenter.current().delegate = appDelegate

        // The shared session start — the macOS twin of this line sits in
        // `FaunaMacApp.completeAuthenticatedLaunch`: announce which device this
        // connection serves, and re-arm APNs only for an install that already
        // opted in (`PushManager.shouldRegisterAtLaunch` carries the reasoning:
        // the OS permission is not the opt-in, it survives a switch-off
        // untouched, so gating on it alone silently restored the subscription
        // the user just deleted). One call, so the two targets cannot drift.
        Task {
            await pm.onSessionStart()
        }
        #endif

        // ── The succession's closing act, on the SUCCESSOR's first
        // authenticated session (`identity-succession.md` § The RecoveryKey →
        // *At succession*). The macOS twin sits at the same point in
        // `completeAuthenticatedLaunch`, and its comment carries the full
        // reasoning: only the navigation happens here — synchronously, and only
        // after the client above exists — while the mint itself is
        // `RecoveryKitSection`'s own hydrate, because the secret has to land in
        // the view model that renders it.
        //
        // ⚠ **This navigation re-pushes a page the teardown just popped, and for
        // ~100 ms BOTH are mounted.** `tearDownSessionForSwitch` nils
        // `selectedSettingsPage` (`:2164`) and `switchAccount` hops to a fresh
        // `Task` before calling `runLaunch()` (`:2087-2100`), so the outgoing
        // `RecoveryKitSection` outlives the incoming one's mount. That is not the
        // "iOS admin-gate bug" class above — it was investigated as such and is
        // NOT (that one is an environment injection that never invalidates; this
        // is two live instances of one view) — it is its own class: **a one-shot
        // obligation raced by two mounted copies of the view that discharges it.**
        // Measured 2026-08-26 , three times over, and the
        // third is the one that stayed hidden longest:
        //   1. the dying copy won the one-shot claim and minted the successor's
        //      only RecoveryKey 147 ms after its own `onDisappear`;
        //   2. a FAILED mint spent the obligation instead of re-arming it — not
        //      iOS-specific, since the ceremony revokes every session of the
        //      account in the nest's own transaction, so the successor's first
        //      mint races its own reconnect everywhere and macOS passed on luck;
        //   3. the surviving copy hydrated as the successor while the environment
        //      still held the PREDECESSOR's `FaunaClient`, so it minted five times
        //      over a seat the nest had already revoked. Its `.task(id:)` key
        //      tracked whether a client existed, not WHICH — so the successor's
        //      client arriving changed nothing and the dead seat was kept.
        // **The standing rule this leaves:** anything that fires once per switch
        // from a view keyed to identity must claim only while ON SCREEN and only
        // over a seat that belongs to the claiming identity — `onScreen` plus
        // `APIClient.boundActorIdHex`. macOS needs neither (its `launchGate`
        // keeps the shell unmounted until the nav is already correct), which is
        // exactly why every one of these three was invisible there.
        if SuccessionHandoff.kitOwed {
            appState.selectedTab = "more"
            appState.moreSelectedView = "settings"
            appState.selectedSettingsPage = .account
        }
    }

    /// Back out of an optimistic entry: the machine says this identity does not belong on
    /// the saved nest (a wizard row), or the nest can no longer prove the identity we
    /// pinned. Drops the client so nothing keeps talking over a connection we've just been
    /// told not to trust (`security.md` § Connection-teardown rule).
    /// Route one `fauna://` deep link (`FaunaDeepLink` — the FP context-action
    /// vocabulary). Share lands on Settings → Folders (the share affordance's
    /// home); Version history stages the `(set, rel)` target for the Media
    /// explorer (`MediaDeepOpen`) and navigates there — the explorer opens the
    /// item's `media-item-detail` once its snapshot holds it. Pre-auth links are
    /// dropped: an FP domain only exists for a signed-in session, so a link
    /// arriving before auth is stale by construction.
    @MainActor
    private func handleDeepLink(_ url: URL) {
        // The same-device handoff's consent route (`ios.md` § App Entry →
        // *In-app routes*) takes the one shared FaunaKit door first; signed out,
        // the door holds it and `applyHeldRoute` applies it after sign-in.
        if ConsentHandoff.shared.receive(url, authenticated: appState.session.isAuthenticated) {
            if appState.session.isAuthenticated { showConnectedApps() }
            return
        }
        guard let link = FaunaDeepLink.parse(url), appState.session.isAuthenticated else {
            return
        }
        appState.isOnboarding = false
        appState.inAdmin = false
        switch link {
        case .folderShare:
            appState.selectedTab = "more"
            appState.moreSelectedView = "settings"
            appState.selectedSettingsPage = .folders
        case .fileVersions(let set, let rel):
            MediaDeepOpen.shared.stage(set: set, rel: rel)
            appState.selectedTab = "more"
            appState.moreSelectedView = "media"
        }
    }

    /// Land on Settings → Connected apps for a consent route. The bumped
    /// `navGeneration` restarts the page's visit, which opens the staged request
    /// through the shared machine whether or not the page was already on screen.
    @MainActor
    private func showConnectedApps() {
        appState.isOnboarding = false
        appState.inAdmin = false
        appState.selectedTab = "more"
        appState.moreSelectedView = "settings"
        appState.selectedSettingsPage = .connectedApps
        appState.navGeneration += 1
    }

    /// Apply a consent route that arrived signed out, now the session is
    /// authenticated (`ConsentHandoff.takeHeld`).
    @MainActor
    private func applyHeldRoute() {
        if ConsentHandoff.shared.takeHeld() { showConnectedApps() }
    }

    @MainActor
    private func leaveAuthenticatedSession() {
        // Convention 14's teardown counter, and iOS's FIFTH arm — macOS has no
        // twin (`e2e-latency-independent-assertions.md` § convention 14, slice
        // D5). It exists only because iOS enters OPTIMISTICALLY: the client is
        // built before the launch verdict lands, so three verdicts
        // (`.identityChanged`, `.wizardAt`, an unreadable account index) have a
        // live authenticated session to drop here, where macOS simply never
        // entered and raises a `launchGate` instead.
        //
        // ⚠ Without this bump the arm tore a session down without counting
        // itself — the exact silent undercount the "bump inside the teardown
        // FUNCTION" rule exists to prevent. It is reached by the whole
        // *connection* half of `security.md` § Post-auth surfacing (channels 1
        // and 2: a bearer re-mint or an SPKI re-handshake meeting a pin the nest
        // can no longer prove re-runs the launch machine, which projects
        // `.identityChanged` straight to here) — a path that never touches
        // `tearDownSessionForSwitch`, so every `assert_no_relaunch` reading this
        // counter across a gesture that can end in one of those verdicts would
        // have reported "no relaunch" over a session that plainly went away.
        //
        // Guarded on the client it drops, so the RE-ENTRANT path counts one
        // teardown once: `performPostAuthSilentSignIn` →
        // `tearDownSessionForSwitch` (which bumps, and nils `client`) →
        // `runLaunch()` → `.identityChanged` → here. Same shape and same reason
        // as `factoryResetReonboard`'s own guard — `SessionGeneration` counts
        // teardowns, not attempts. Read through `liveFaunaClient` — this
        // function is `handleTestCommand`-reachable (`dispatchLaunch`'s
        // `.wizardAt`/`.identityChanged`/`.offline` arms), and a raw `client`
        // read off the test agent's captured self is the row 376 bug class.
        if liveFaunaClient != nil {
            SessionGeneration.recordTeardown()
        }
        // Third `[admin-gate]` zeroth-link case: the client was built and then
        // TORN DOWN (a `.wizardAt` / `.identityChanged` launch verdict). A probe
        // firing after this sees nil for a reason that is neither a propagation
        // bug nor a missed re-fire, so it must be distinguishable from both.
        logMessage(level: .warn, target: "fauna.app",
                   message: "[admin-gate] client DROPPED (leaveAuthenticatedSession) — session torn down")
        liveFaunaClient?.suspend()
        // Both halves, like every OTHER client-drop site in this file
        // (resetToFactory/logoutKeepData/tearDownSessionForSwitch/
        // factoryResetReonboard/useADifferentNest) — this one was the lone
        // unpaired drop, leaving `appState.liveClient` pointing at a suspended
        // client for any later handleTestCommand-reachable read to trust as
        // live.
        client = nil
        appState.liveClient = nil
        appState.session.clearAuthenticatedOverride()
        appState.isOnboarding = true
        appState.fpReconcile = nil
        // The nest-scoped FP capability + domains must not outlive the session
        // (mirrors FaunaMacApp's teardown; best-effort, no-op without an appex).
        Task { await FileProviderCoordinator.signOut() }
    }

    /// Project one `LaunchSnapshot`. The machine has already decided; nothing here
    /// re-classifies an error or re-reads a routing input.
    @MainActor
    private func dispatchLaunch(_ snap: LaunchSnapshot, keychain: KeychainStore) async {
        switch snap.phase {
        case .online:
            // Normally we are already in — the optimistic bet paid off. The one case where
            // we are not: entry was withheld for a pending-factory-reset slot that the CR-2
            // reconcile then found STALE and cleared (the box was never reset, or was
            // already re-claimed — it is healthy and still claimed, and the ordinary rows
            // just log the admin back in). That is precisely the launch CR-2 used to trap
            // on a claim page forever. Through `liveFaunaClient`, not the raw
            // `client` — this function is handleTestCommand-reachable, and a
            // raw read is the row 376 bug class.
            if liveFaunaClient == nil {
                enterOptimistically(keychain: keychain)
            }
            await completeAuthenticatedGlue(keychain: keychain)

        case .wizardAt(let entry):
            // The machine says this identity does not belong on the saved nest (or there is
            // no saved nest). Back out of any optimistic entry, then seed the wizard.
            //
            // iOS previously had NO silent-challenge fallback at all: a `nil` (not-registered)
            // result simply fell out of the `if let`, leaving the user sitting in the app on a
            // nest that had never heard of them (`onboarding.md`'s status table said so
            // outright — "silent-challenge fallbacks not yet surfaced"). This closes that.
            leaveAuthenticatedSession()
            // `sync-agent.md` § Credential model → *The signed-out reconcile*,
            // shape (a): structural no-op on iOS — no agent runs
            // here, so `AgentEndpoint::default_for_user()` errors and the FFI
            // face returns immediately — wired anyway because it costs
            // nothing and keeps macOS + iOS on one call site
            // (`sync_agent_provisioning.rs` module doc: present on every
            // apple slice for flat-binding consistency).
            Task { await signedOutOnboardingReconcile() }
            seedWizard(at: entry, keychain: keychain)

        case .identityChanged(let pinnedHex, let seenHex):
            // BLOCKING: no auto-entry, no retry CTA (security.md). Tear the
            // session down — iOS got in optimistically, and this is the one verdict that
            // says it should not have.
            leaveAuthenticatedSession()
            appState.identityChanged = IdentityChangedInfo(pinnedHex: pinnedHex, seenHex: seenHex)

        case .offline(let transient):
            // The saved account index is present and unusable
            // (`onboarding.md` § App-launch routing — the row checked before
            // every other). Checked BEFORE the transient/non-transient split
            // below, for the same reason tui's `route()` checks it before
            // both `Offline` arms: the machine deliberately projects this to
            // `Offline { transient: false }` and carries the verdict on a
            // side channel, so ignoring the channel would either stay
            // optimistically on stale/no content (the transient branch) or
            // paint "use a different nest" (the non-transient branch) —
            // both misstate a problem that has nothing to do with the nest.
            if let refusal = snap.accountIndexRefusal {
                leaveAuthenticatedSession()
                appState.accountIndexRefusal = refusal
                appState.accountIndexConfirming = false
            } else if let claimed = snap.supersededSuccessor {
                // The identity was succeeded: tear down the optimistic entry and
                // route to the import flow (`SupersededLaunchRoute` owns the
                // why). Ahead of the transient/terminal split for the account-
                // index row's side-channel reason — left to the terminal arm,
                // it would paint "update your nest" over an identity problem.
                leaveAuthenticatedSession()
                SupersededLaunchRoute.route(
                    machine: onboardingVM.machine,
                    claimedSuccessor: claimed,
                    keychain: keychain,
                    // A held verified successor: an ordinary switch to it, as
                    // the ceremony's `onSucceeded` ends in — no re-auth prompt
                    // ran, so `confirmed: false`. Its relaunch clears the
                    // onboarding flag set just below.
                    adopt: { successor in try? await switchAccount(to: successor, confirmed: false) })
                appState.isOnboarding = true
            } else if snap.signInRefused {
                // A nest this app signed in to before refused the identity
                // (`onboarding.md` § App-launch routing — the previously-signed-in
                // row). Ahead of the transient/terminal split for the side-channel
                // reason above: left to the terminal arm it would paint "update your
                // nest" with no way back in. iOS got in optimistically, and this
                // verdict says it should not have — tear the session down, then show
                // `launch_sign_in_refused` (the one terminal surface WITH Retry).
                leaveAuthenticatedSession()
                logMessage(
                    level: .error, target: "fauna.app",
                    message: "[launch] the saved nest no longer signs this identity in: \(snap.lastError ?? "")")
                appState.signInRefusedMessage = snap.lastError ?? ""
            } else if transient {
                // Stay optimistically entered. A reachability blip is exactly what iOS's
                // posture is for: cached content still renders and the client's own
                // reconnect loop owns recovery. (macOS, which gates, shows a retry surface
                // here instead — it has no session to fall back on.)
                logMessage(
                    level: .error, target: "fauna.app",
                    message: "[launch] transient offline: \(snap.lastError ?? "unknown") — staying on cached session")
            } else {
                // Terminal: the nest reported it is outdated (`fauna.nest.outdated`), the
                // secret is invalid, or the account is locked. Non-retry surface, overlaid
                // on the main UI (version-compatibility.md Dim 4).
                appState.needsUpdateMessage = snap.lastError ?? ""
            }

        case .boot, .hydrating, .silentChallenge, .refreshing:
            // Unreachable once `start()` has returned — it always lands on a terminal phase.
            break
        }
    }

    /// The post-authentication glue, run once the machine has reported `.online`.
    ///
    /// No silent sign-in happens here any more: the machine already ran the challenge, and
    /// its `save_authenticated` already wrote (nest_url, handle, domain, tier) into the
    /// **active account's** slots via the shared `FaunaAccounts` registry. Read those slots
    /// back directly — the freshly-verified values for THIS run — rather than the boot-time launch snapshot
    /// (`bootLaunchPersistence`), which is read once at boot and so
    /// can still name a previous run's (or no) domain the moment a fresh login or a
    /// mid-session rename resolves (conversations.md § State & data shape → *Self-address:
    /// live, never baked*; mirrors macOS `completeAuthenticatedLaunch`, the richest existing
    /// pattern here).
    /// Wire the freshly-built conversations session into the two consumers that
    /// meet it outside the Conversations page — ONE install, called by the
    /// production login path and by the e2e `real_conversations` path alike, so
    /// an e2e run can never exercise a session the production wiring would have
    /// left unconnected.
    ///
    /// * The feed's room-post key seam (`ui/feed.md` § Encryption at rest →
    ///   *Room-restricted — the app half*) — the one place the feed manager and
    ///   the conversations session meet on apple, mirroring linux's
    ///   `conv_backend.rs`/web's `syncFeedRoomPosts`. Safe even before the Feed
    ///   tab has built its own manager: `FeedVM` holds the session and
    ///   re-installs it the moment `configure` builds one.
    /// * The home-screen widget's background leg (`apps/ios.md` § Home-screen
    ///   widget): while the app is suspended, the widget-refresh
    ///   `BGAppRefreshTask` runs one receive pass, whose ingest republishes the
    ///   count through `ConversationsVM`'s observer.
    @MainActor
    private func installConversationsSessionSeams(_ faunaClient: FaunaClient) {
        if let session = conversationsVM.session {
            feedVM.installRoomPostKeys(session: session, conversationsManager: conversationsVM.manager)
        }
        #if os(iOS)
        let conversations = conversationsVM
        faunaClient.backgroundScheduler.configure(widgetRefresh: { await conversations.receivePass() })
        #endif
    }

    @MainActor
    private func completeAuthenticatedGlue(keychain: KeychainStore) async {
        // Through `liveFaunaClient`, not the raw `client` — handleTestCommand-
        // reachable (via dispatchLaunch), the row 376 bug class.
        guard let faunaClient = liveFaunaClient else { return }

        let registry = FaunaAccounts.registry(keychain: keychain)
        let sessionActor = appState.session.actorId ?? registry.active()
        let material = sessionActor.flatMap { registry.sessionMaterial(actorId: $0) }
        let handle = material?.handle ?? ""
        let domain = material?.domain ?? ""
        appState.session.handle = handle
        // Guards both the heal below AND the full rebuild's `selfAddress:` — a
        // handle-less account must not compose the forbidden `"@domain"` shape
        // (an unresolved half stays empty, same sentinel tui's
        // `resolve_self_address(…).unwrap_or_default()` uses).
        let selfAddress = (!handle.isEmpty && !domain.isEmpty) ? "\(handle)@\(domain)" : ""

        // Heal an already-built session (e.g. re-entering `.online` after
        // `trustNestIdentity()` resolves a changed handle/domain) — a no-op when no session
        // is active yet; the build below then carries the same fresh value. An unresolved
        // half is dropped rather than composed into the forbidden `"@nest.example"` shape.
        if !selfAddress.isEmpty {
            conversationsVM.session?.setSelfAddress(selfAddress: selfAddress)
        }

        do {
            // Activate the dual-rail conversations session so the Conversations page Send
            // (`dm-send-button`) issues a real RPC — mail via the SMTP rail, DMs/groups via
            // FaunaMls. Same FaunaKit wiring macOS uses; best-effort.
            try await conversationsVM.rebuild(
                api: faunaClient.api,
                selfAddress: selfAddress, selfSecretHex: appState.session.secretHex ?? "",
                deviceIdHex: faunaClient.deviceId,
                predecessorBackupKeys: faunaClient.resolvedPredecessorBackupKeys()
            )
            installConversationsSessionSeams(faunaClient)
            // Hands-off TLS-cert auto-renew for managed/delegated domains (admin-dns,
            // tls-certificates.md § C.3) — secret now primed for the credentialed Dns
            // machine. Idempotent.
            DnsAutoRenewCadence.shared.start(api: faunaClient.api)
            // Author-side subscription reconcile (monetization.md § Pillar 1): heal
            // crash-staged subscriber removals + auto-approve queued follows (encrypted mode
            // enqueues them — the nest cannot mint the KeyBlob). Headless, best-effort; twin
            // of linux `subscriptions_author`.
            SubscriptionsAuthorCadence.shared.start(api: faunaClient.api)
            // Guardian Notify's flush cadence (family-safety.md § Guardian Notify) — its `content_notify`/enabled state is set
            // separately by `ContentPolicyStore.refresh`, but the due-check loop
            // itself needs the live api the same way the two cadences above do.
            GuardianNotifyCadence.shared.start(api: faunaClient.api)
            // Screen-time's flush cadence (family-safety.md § Screen time) — its policy/guardian state is set
            // separately by `ScreenTimeStore.refresh`, but the one-minute tick
            // itself needs the live api the same way the cadences above do.
            appState.screenTime.start(api: faunaClient.api)
        } catch {
            logMessage(level: .error, target: "fauna.app", message: "[launch] conversations session activation failed (non-fatal): \(error)")
        }

        // Post-auth identity re-check (security.md § Post-auth surfacing): one-shot,
        // best-effort, decoupled — see `performPostAuthSilentSignIn` below. A separate
        // Task rather than an awaited step in this chain: if it DOES escalate, it tears
        // this session down and starts a fresh one, which must not race the rest of
        // THIS glue continuing to mutate state through the now-torn-down `faunaClient`.
        Task { @MainActor in await performPostAuthSilentSignIn() }

        // The post-claim serving-enablement call (one shared-Rust step,
        // replacing the mail/CalDAV/CardDAV/WebDAV per-step firing) + the trust-prompt grant + the three
        // universal post-auth hooks (deployment-seed capture, host-address
        // report, seed-map fan-out, seed-custody self-heal) — the fixed
        // six-call sequence `MailEnableGlue.runPostAuthGlue` documents,
        // extracted because it was byte-identical to macOS's own call site
        // (`completeAuthenticatedLaunch`'s `Task { @MainActor in ... }` block)
        // despite this function as a whole genuinely diverging from its macOS
        // twin.
        await MailEnableGlue.runPostAuthGlue(api: faunaClient.api, session: appState.session)

        // File Provider auto-appear for the Files app (file-sync.md § On-Demand
        // Files → Apple File Provider binding): the same FaunaKit reconcile macOS
        // runs — domains for the user's own sets this device's place accepts in and
        // the sets shared with the account, behind the default-ON
        // per-device toggle, app-dead capability provisioned first. iOS has no
        // local-folder binding surface, so the one-local-presence rule reduces to
        // `boundSets: []` (FP domain or RemoteOnly; the resident-engine arm can't
        // arise). Best-effort: only the appex-embedding `.xcodeproj`-built app
        // serves domains. Gated off e2e — FP domains + the shared app-group
        // Keychain are machine-global (testing.md § conventions point 10).
        // `os(iOS)`-gated (not just canImport) — this file also typechecks for
        // the macOS host via the CrossPlatformUI shims, where UIDevice is absent.
        #if os(iOS)
        if !FaunaE2E.isActive,
           let secret = appState.session.secretHex,
           let nodeUrl = appState.session.nodeUrl,
           let deviceId = appState.session.deviceId {
            let fpReconcile: @Sendable () async -> Void = {
                do {
                    // Every set the account holds — own and shared with it —
                    // mapped in shared Rust (the plan applies this device's
                    // place and the toggle), keyed by its `FolderRef` wire
                    // string; the name is the Files display label.
                    let api = await faunaClient.api
                    let sets = try await api.folderPresenceSets(deviceIdHex: deviceId)
                    await FileProviderCoordinator.reconcile(
                        sets: sets,
                        provisioning: FileProviderProvisioningContext(
                            nestUrl: nodeUrl,
                            secretHex: secret,
                            deviceIdHex: deviceId,
                            deviceLabel: UIDevice.current.name
                        )
                    )
                } catch {
                    logMessage(
                        level: .warn, target: "fauna.fileprovider",
                        message: "[fp] reconcile skipped — folder list or roster read failed: \(error)")
                }
            }
            // The `folder-on-demand-toggle` re-drives the same reconcile a
            // launch runs (AppState.fpReconcile — FoldersView consumes it).
            appState.fpReconcile = fpReconcile
            await fpReconcile()
        }
        #endif
    }

    /// Seed the onboarding wizard for the entry **the machine chose**, then show it.
    ///
    /// The machine decides *which* entry; the long-term-store slots only supply what that
    /// page pre-fills with. We re-read the store here only to hydrate, never to re-decide —
    /// an `if slot != nil → claim page` that does not consult the machine's verdict is
    /// precisely the CR-2 bug. Mirrors `FaunaMacApp.seedWizard`.
    @MainActor
    private func seedWizard(at entry: LaunchWizardEntry, keychain: KeychainStore) {
        // The served account's session material — the registry's one session-identity
        // read; the resume slots come through the SAME `LaunchPersistence` the machine
        // branched on.
        let material = FaunaAccounts.sessionMaterial(keychain: keychain)
        let secret = material?.secretHex
        let persistence = FaunaAccounts.launchPersistence(keychain: keychain)

        switch entry {
        case .identityChoice:
            // No identity in the store — cold start, nothing to seed. The one
            // thing a signed-out launch still owes: re-sweep a residue record a
            // previous sign-out left, silently, and paint the
            // `sign-out-residue` view only if something is still left
            // (`account-scoping.md` § Erasure follows scope → *the residue
            // surface*). Shared Rust leaves the record alone unless the
            // registry is empty.
            Task { await onboardingVM.recheckSignOutResidueAtLaunch() }

        case .handleEntry:
            if let secret { onboardingVM.machine.seedIdentity(secret: secret) }

        case .inviteRequest:
            if let secret { onboardingVM.machine.seedIdentity(secret: secret) }
            if let pending = persistence.loadPendingInvite() {
                onboardingVM.machine.seedPendingInvite(
                    nestUrl: pending.nestUrl,
                    handle: pending.handle,
                    requestId: pending.requestId,
                    statusJson: pending.statusJson
                )
            } else if let nodeUrl = material?.nestUrl,
                      let cachedHandle = material?.handle, !cachedHandle.isEmpty {
                // The machine's other road here: the silent challenge reported the secret is
                // not registered on the saved nest (and the box answered "claimed", or did
                // not answer at all — the safe default). Land on invite_request pre-filled.
                onboardingVM.machine.seedAtInviteRequestUnregistered(
                    handle: cachedHandle, nestUrl: nodeUrl
                )
            }

        case .claimCode:
            // The saved nest is up but UNCLAIMED (verify 404 + setup-status claimed=false),
            // so no admin exists yet and the user must claim it themselves.
            if let secret { onboardingVM.machine.seedIdentity(secret: secret) }
            if let nodeUrl = material?.nestUrl {
                onboardingVM.machine.navigateToClaimCodeForKnownNest(
                    nestUrl: nodeUrl,
                    handle: material?.handle ?? ""
                )
            }

        case .pendingFactoryReset:
            // Factory-reset resume (gap CR-1). The code survives ONLY because it was minted
            // and persisted before the reset was dispatched, so seed the claim page from the
            // slot — pre-filled, sourced from disk, so it survives a crash.
            //
            // Re-read the slot AFTER `start()`: the CR-2 boot reconcile probes the box and
            // DELETES a stale slot. Reaching this arm means the probe said *unclaimed* (or
            // the box was unreachable, where the slot is deliberately kept), so the row
            // should still be here; if it is not, the store changed underneath us — fall
            // back to the wizard rather than open a claim page with no code.
            //
            // Through the SAME `LaunchPersistence` the machine branched on (matching
            // `.awaitingManualDns` below).
            guard let pending = persistence.loadPendingFactoryReset() else {
                logMessage(
                    level: .error, target: "fauna.app",
                    message: "[launch] pendingFactoryReset row with no slot; falling back to the wizard.")
                if let secret { onboardingVM.machine.seedIdentity(secret: secret) }
                break
            }
            onboardingVM.machine.reset()
            if let secret { onboardingVM.machine.seedIdentity(secret: secret) }
            onboardingVM.machine.navigateToClaimCodeForKnownNestWithCode(
                nestUrl: pending.nestUrl,
                handle: pending.handle,
                code: pending.claimCode
            )

        case .awaitingManualDns:
            // Deferred-DNS relaunch (onboarding.md § App-launch routing): the user
            // provisioned their own nest, chose "Set up later" for DNS, and force-quit.
            // Seed identity (so the eventual claim can sign) then hydrate the wizard at
            // the deferred-DNS exit from the saved slot — the container renders the
            // "Almost ready" surface off `wizardOutcome() == AwaitingManualDns`, which
            // `seedAwaitingManualDnsJson` sets. The record comes back through the SAME
            // `LaunchPersistence` the machine branched on to pick this entry.
            if let secret { onboardingVM.machine.seedIdentity(secret: secret) }
            if let rec = persistence.loadAwaitingDns() {
                // The whole record, verbatim: the machine re-holds the box's
                // built-with identity (`rec.nestActorId`) as the first-contact
                // root before the surface's first poll.
                onboardingVM.machine.seedAwaitingManualDnsRecord(record: rec)
            } else {
                // The machine picked this entry from the slot, so it should be present;
                // if the store changed underneath us, fall back to the plain seeded
                // wizard rather than a blank "Almost ready".
                logMessage(
                    level: .error, target: "fauna.app",
                    message: "[launch] awaitingManualDns entry with no slot; falling back to the wizard.")
            }
        }

        appState.isOnboarding = true
    }

    /// "Trust this nest" from the blocking `launch_identity_changed` surface
    /// (`nest-identity-changed-trust-button`). Forgets the TOFU pin through the machine's
    /// connector trust seam, re-TOFUs, re-runs the silent challenge, and re-dispatches the
    /// resulting verdict.
    ///
    /// Called on the HELD machine, never a fresh one: `trustNestIdentity()` reads the secret
    /// + nest_url off the `IdentityChanged` state itself, so a fresh machine would
    /// re-challenge from `Boot`, never reach the forget-the-pin branch, and leave the button
    /// silently doing nothing.
    @MainActor
    private func trustNestIdentity() {
        guard let machine = launchMachineBox.machine else {
            // Not merely defensive: this button is only ever rendered while
            // `identityChanged` is set, so a missing holder means a launch
            // installed its machine somewhere the live view cannot see it.
            logMessage(level: .error, target: "fauna.app",
                       message: "[trust-nest] no held launch machine — the button cannot act")
            return
        }
        // Which machine the button got. `trust_nest_identity()` is a deliberate
        // no-op off `IdentityChanged`, so acting on the wrong (e.g. still-`Online`
        // boot) machine forgets no pin and re-dispatches a stale verdict — a
        // *silent* substitution, not a visible failure, and the one this log
        // exists to make visible.
        logMessage(level: .warn, target: "fauna.app",
                   message: "[trust-nest] acting on launch phase \(machine.snapshot().phase.diagnosticName)")
        appState.identityChanged = nil
        Task { @MainActor in
            await machine.trustNestIdentity()
            await dispatchLaunch(machine.snapshot(), keychain: KeychainStore())
        }
    }

    /// `launch-retry-button` on `launch_sign_in_refused`: re-run the silent
    /// challenge on the machine that produced the verdict (`retry_silent_challenge`
    /// is valid from the sign-in-refused state; a fresh machine would also work but
    /// would re-read the store for no gain), then re-dispatch its snapshot — back
    /// into the app on success, the same page again if the nest still refuses.
    @MainActor
    private func retrySignIn() {
        guard let machine = launchMachineBox.machine else {
            // Only rendered while `signInRefusedMessage` is set, so a missing holder
            // means a launch installed its machine somewhere the view cannot see it.
            logMessage(level: .error, target: "fauna.app",
                       message: "[sign-in-refused] no held launch machine — Retry cannot act")
            return
        }
        appState.signInRefusedMessage = nil
        Task { @MainActor in
            await machine.retrySilentChallenge()
            await dispatchLaunch(machine.snapshot(), keychain: KeychainStore())
        }
    }

    /// "Start over on this device" from the malformed verdict's
    /// `account-index-reset-button`: reveal the confirm, which states the
    /// residual before anything is erased. A no-op unless the malformed
    /// verdict is actually showing — the version verdict paints no such
    /// button, so this must never become a second way to reach the reset
    /// from it.
    @MainActor
    private func revealAccountIndexStartOver() {
        guard appState.accountIndexRefusal == .malformed else { return }
        appState.accountIndexConfirming = true
    }

    /// The confirm: the documented client-side floor
    /// (`long-term-store.md` § Cleanup contract), which is exactly the app's
    /// existing factory reset (`StatusVM.signOut(sessionState:)` — reused
    /// verbatim, never a second clearing path) — then land on fresh
    /// onboarding. Guarded the same way as the reveal, plus on the confirm
    /// having actually been shown.
    @MainActor
    private func confirmAccountIndexStartOver() {
        guard appState.accountIndexRefusal == .malformed, appState.accountIndexConfirming else {
            return
        }
        Task { @MainActor in
            await StatusVM().signOut(sessionState: appState.session)
            // The floor ran sign-out's erase, so it owes the same residue
            // statement on the wizard it lands on (`account-scoping.md`
            // § Erasure follows scope → *the residue surface*).
            onboardingVM.takeSignOutResidue(from: appState.session)
            appState.accountIndexRefusal = nil
            appState.accountIndexConfirming = false
            appState.isOnboarding = true
        }
    }

    // Compiled out of release artifacts (testing.md convention 15) — neither
    // start function, nor the `TestAgent`/`InProcessAutomationServer` types they
    // reach, exist in a non-DEBUG build. Both start functions live in
    // FaunaKit's `TestAgentBootstrap` (shared with macOS); only the
    // `serializeState`/`handleTestCommand` closures passed to them are
    // per-target.

    @MainActor
    private func serializeState() -> [String: Any] {
        var state = AppStateObservables.commonState(
            session: appState.session,
            isAdmin: appState.isAdmin,
            liveClient: appState.liveClient,
            conversationsSession: conversationsVM.session,
            feedManager: feedVM.manager,
            devicesMachine: devicesVM.machine,
            inboxMode: appState.inboxMode
        )

        // The photo-backup pass funnel (`PhotoBackupEngine.lastPass*`) — the
        // observable that makes "backup ran and uploaded nothing" name WHICH
        // nothing, instead of leaving four silent causes indistinguishable from
        // outside the app. Convention 11's corollary holds: every value is a plain
        // field read off the engine, no round trip. Same key, same depth, the same
        // shared `PhotoBackupEngine` on both shells (priority #1). No engine (never
        // authenticated on iOS) publishes no key at all — convention 11.
        // `FaunaClient.photoBackup` is `#if os(iOS)` (FaunaClient.swift:86), and
        // `swift build --target FaunaiOS` typechecks this file for the macOS HOST
        // — so the read needs the same gate the property has. macOS publishes the
        // identical key from its own shell, off its own engine.
        #if os(iOS)
        if let pb = appState.liveClient?.photoBackup {
            state["photo_backup"] = [
                "authorization": pb.lastPassAuthorization,
                "assets_seen": pb.lastPassAssetsSeen,
                "already_synced": pb.lastPassAlreadySynced,
                "export_failed": pb.lastPassExportFailed,
                "ingest_failed": pb.lastPassIngestFailed,
                "uploaded": pb.lastPassUploaded,
                "pending": pb.pendingCount,
                "passes_started": pb.passesStarted,
                "passes_completed": pb.passesCompleted,
                "completed_total": pb.completedCount,
                "last_backup_at": pb.lastBackupDate?.timeIntervalSince1970 as Any,
                "enabled": UserDefaults.standard.bool(forKey: PhotoBackupControlsView.enabledKey),
            ]
        }
        // The widget-refresh pass counters (`fauna_e2e_agent::WIDGET_REFRESH_KEY`)
        // — plain field reads off the scheduler (convention 11). iOS-only like the
        // scheduler itself: macOS keeps the app resident instead and publishes no
        // key, which the consumer refuses loudly rather than reading as "no pass".
        if let scheduler = appState.liveClient?.backgroundScheduler {
            state["widget_refresh"] = [
                "passes_started": scheduler.widgetRefreshPassesStarted,
                "passes_completed": scheduler.widgetRefreshPassesCompleted,
                "last_pass_count": scheduler.widgetRefreshLastPassCount as Any,
            ]
        }
        #endif

        // Navigation — report the canonical view name, not the tab name.
        // Admin is a sub-paged shell (a top-level peer), so reflect the active
        // admin page in the second stack entry (the inverse of applyNavPatch) for
        // round-trip parity (mirrors macOS). Else: if on the "more" tab, report
        // which sub-view is selected.
        // The terminal needs-update surface (`dispatchLaunch`'s non-transient
        // offline arm) overlays the optimistic tab shell without leaving it, so
        // `isOnboarding` alone would name the hidden tab. Report the launch
        // surface instead — what macOS publishes for the same verdict
        // (`!isOnboarded` under `launchGate = .needsUpdate`), and linux/tui too.
        if appState.isOnboarding || appState.needsUpdateMessage != nil
            || appState.signInRefusedMessage != nil {
            state["nav"] = ["stack": [["view": "welcome"]], "modal": NSNull()]
        } else if appState.inAdmin {
            state["nav"] = ["stack": [
                ["view": "admin"],
                ["view": "admin", "id": appState.selectedAdminPage.navId],
            ], "modal": NSNull()]
        } else if appState.selectedTab == "more", let moreView = appState.moreSelectedView {
            // Settings carries its active sub-page in the second stack entry (the
            // inverse of applyNavPatch's `settings` case), for round-trip parity
            // with the admin shell above.
            if moreView == "settings", let page = appState.selectedSettingsPage {
                state["nav"] = ["stack": [
                    ["view": page.canonicalTopLevelNavView ?? "settings"],
                    ["view": "settings", "id": page.navId],
                ], "modal": NSNull()]
            } else {
                state["nav"] = ["stack": [["view": moreView]], "modal": NSNull()]
            }
        } else {
            state["nav"] = ["stack": [["view": appState.selectedTab]], "modal": NSNull()]
        }

        // Data
        let context = liveModelContainer.mainContext
        state["data"] = serializeData(context: context)

        // Cross-app E2E bridge — the most recent value-returning
        // `call_machine_method` reader result (`provisioning_snapshot`,
        // `provider_base_url`, …), decoded (not the raw JSON string — this
        // dict is itself re-encoded to JSON at the wire boundary, so a raw
        // string would double-encode). `nil`/absent after a setter-only call.
        state["machine_method_result"] = machineMethodResultBox.value ?? NSNull()

        return state
    }

    @MainActor
    private func serializeData(context: ModelContext) -> [String: Any] {
        // The succession_sweep/conversation_threads/selected_thread_id/contacts/
        // knocks/events/notifications core — byte-identical on both shells,
        // extracted so macOS+iOS cannot drift on it (priority #1/#2). iOS's `selected_thread_id` note: lets
        // an e2e opener recognize a thread as already-open by id instead of only
        // by clicking its `conversation-item` row — needed because
        // `send_new_thread` selects the new thread and iOS's single-pane
        // `NavigationStack` pushes straight to its detail
        // (`ConversationsListView.swift`'s `.onChange(of: vm.selectedThreadId)`)
        // before the freshly-inserted row ever registers, so `count(
        // "conversation-item")` stays load-independently stuck at the pre-insert
        // count. macOS's two-pane list is never covered so doesn't need this to
        // pass its own leg, but exposes it too for shape parity.
        var data = AppStateObservables.commonData(
            conversationsVM: conversationsVM,
            contacts: appState.lastContacts,
            knocks: appState.lastKnocks,
            events: appState.lastEvents,
            notificationsUnreadCount: appState.notificationsUnreadCount,
            liveClient: appState.liveClient
        )

        // Which photo-backup store this context is actually ON
        // (`account-scoping.md` § Serialized switching). The `context`
        // parameter existed but was never read, so nothing outside the process
        // could tell the ACTOR-SCOPED container apart from the pre-auth flat
        // one — and the e2e login path built the scoped file, then lost the
        // reference to it in a `@State` write through the captured `self`, so a
        // file-existence check would have reported the scoping working while
        // every read in the app went to the flat store. Reading the container's own configuration is the honest
        // answer (convention 11) and costs no I/O: it is an in-memory property
        // of the already-open container. Witness:
        // `test_photo_backup_store_scoping_apple.py`.
        data["photo_backup_store"] =
            context.container.configurations.first?.url.path as Any? ?? NSNull()

        // Feed — serialized from the shared FeedManager snapshot (FeedVM mirrors
        // the latest posts into the static on every observer tick; same source
        // macOS serializes from).
        //
        // ⚠ `lastLoadedPosts` is ALSO written by the TestAgent's `compose.post`
        // from a throwaway manager, so this read can mask a dead app-level
        // manager (the search / interaction paths silently no-op while `posts`
        // still serializes). The `diag` block below un-masks that — same as
        // macOS: it reads the LIVE app-level `feedVM` + its manager snapshot,
        // so one failing run distinguishes "field text never reached the VM" /
        // "manager nil" / "query committed but reload never happened".
        let feedSnap = feedVM.manager?.snapshot()
        data["feed"] = [
            "diag": [
                "vm_has_manager": feedVM.manager != nil,
                "vm_search_text": feedVM.searchText,
                "committed_search_query": feedSnap?.searchQuery as Any? ?? NSNull(),
                "manager_posts_count": feedSnap?.posts.count as Any? ?? NSNull(),
            ] as [String: Any],
            "posts": FeedVM.lastLoadedPosts.map { post in
                [
                    "post_id": post.postId,
                    "author": post.author,
                    // What the card PAINTS on `post-author` — the same door the
                    // card calls (mirrors macOS) — so a harness read of the
                    // card's author compares with the painted
                    // `feed-post-detail-author`.
                    "author_label": conversationsVM.postAuthorLabel(post),
                    // Painted document plaintext (markdown markers stripped), NOT
                    // the raw `body` — same as macOS: the in-process `feed-post-text`
                    // element doesn't register in the iOS lazy feed list, so the
                    // harness reads the feed from this state field; serialize the
                    // SAME painted text the element read returns (PostCardView's
                    // `renderDocumentToPlaintext`) so the state-read
                    // fallback matches every other app (render-model.md § D6).
                    // `""` for a post the viewer reported (same as macOS).
                    "body": appState.contentPolicy.inputs.paintedBody(of: post),
                    "timestamp": post.timestamp,
                    "tags": post.tags,
                    "has_media": post.hasMedia,
                    // Resolved lazily by `resolveMedia` (post-card `.task`) once the
                    // card renders; `""` until then — the harness's
                    // `post_image_blob_hash_by_text` polls this field (same as macOS).
                    "media_hash": post.mediaHash ?? "",
                    "is_reply": post.isReply,
                    // Muted-collapse state (topic-factors.md § Scoring), for the
                    // state-read fallback's "must not leak the body" / post-reveal
                    // assertions — mirrors the SAME check `PostCardView` renders
                    // from (`manager.isMuted`), with the reveal exception applied
                    // (same as macOS).
                    "is_muted": (feedVM.manager?.isMuted(postId: post.postId) ?? false)
                        && !FeedVM.revealedMutedPostIds.contains(post.postId),
                    // Interaction-bar counts (ratified 2026-06-27) — same as macOS:
                    // the shared PostSummary counts the card binds, read from snapshot
                    // state because the lazy feed List doesn't register the
                    // feed-*-button elements in-process (feed.md § Interaction bar).
                    "like_count": post.likeCount,
                    "reply_count": post.replyCount,
                    "repost_count": post.repostCount,
                    "quote_count": post.quoteCount,
                    // The like TOGGLE's routing state (feed.md § Interaction bar) —
                    // the harness's id-keyed state reads need this to confirm the
                    // toggle actually flipped, not just the count (same as macOS).
                    "viewer_liked": post.viewerLiked,
                    // Repost carrier + the repost TOGGLE's routing state (feed.md §
                    // Interaction bar → Repost, ratified 2026-08-10) — same as
                    // macOS, mirrors linux's identical state keys.
                    "reposted_post_id": post.repostedPostId ?? "",
                    "viewer_repost_id": post.viewerRepostId ?? "",
                    // Every link preview with its state (render-model.md § D4)
                    // — same key as macOS, tui and linux.
                    "link_previews": AppStateObservables.feedPostLinkPreviews(post.document),
                ] as [String: Any]
            }
        ] as [String: Any]

        // Sync — not yet populated on iOS
        data["sync"] = NSNull()

        return data
    }

    // Compiled out of release artifacts (testing.md convention 15) — the whole
    // command-handler surface `handleTestCommand` dispatches to. Its only
    // callers are the DEBUG-gated `startTestAgentIfNeeded`/
    // `startInProcessAgentIfNeeded`, so this is unreachable dead code in
    // release; gating it removes the `strings`-grep hits too.
    #if DEBUG

    // MARK: - Conversations TestAgent commands
    //
    // Flat-shape commands (rail / sender / subject / body at the top level of
    // the command dict) — mirrors apps/fauna-linux/src/main.rs's
    // handle_conversations_* and Windows TestAgent.cs (and FaunaMacApp.swift).
    // The string forms for `rail` are the Rust enum Debug spellings the
    // cross-app action layer (tests/e2e-unified/actions/conversations.py)
    // expects.

    /// Surface a test-agent failure on the app's error surface (`error-message`) —
    /// the iOS twin of `FaunaMacApp.testAgentFailure`. `POST /app/commands` acks 200
    /// before the handler runs, so this is the only channel the harness can observe;
    /// a `.debug` log is not one.
    @MainActor
    private func testAgentFailure(_ message: String) {
        // Delegates to the ONE FaunaKit implementation both targets share
        // (priority #2) — this was a byte-identical twin per target until
        // 2026-08-28, and it wrote to the WRONG SLOT: `AppMessages.error` is the
        // page's banner mirror, assigned by `ErrorBanner.onAppear` and cleared
        // by its `onDisappear`, so a refusal parked there is clobbered by the
        // next banner to render. `reportRefusedAgentCommand` stamps the
        // dedicated nav-independent slot instead, which `errorForDisplay`
        // publishes ahead of the page's own banner and only `reset` clears.
        AppMessages.reportRefusedAgentCommand(message)
    }

    @MainActor
    private func handleConversationsInjectInbound(_ command: [String: Any]) {
        ConversationsTestInject.injectInbound(command, into: conversationsVM.manager)
    }

    @MainActor
    private func handleTestCommand(_ command: [String: Any]) async {
        if let action = command["__action"] as? String {
            switch action {
            case "reset":
                // Clear the barrier probe slots at the same point the macOS twin
                // (and tui/linux) do — a leaked token would make the next test's
                // precondition assertion fire.
                BarrierTestCommand.clear()
                // Convention 11: the refusal slot is cleared HERE and nowhere
                // else — `reset` is the per-test boundary every `app` fixture
                // drives, and a nav must never reach it (that is the whole
                // reason it is not `AppMessages.error`).
                AppMessages.clearRefusedAgentCommand()
                await resetToFactory()
            case "logout":
                BarrierTestCommand.clear()
                await logoutKeepData()
            case "call_machine_method":
                // Convention 11's bad-payload clause — see the twin guard in
                // `TestAgent.processCommand`. Silently dropping this left
                // `machineMethodResultBox` stale, so the caller read the PREVIOUS
                // call's result and believed it.
                guard let method = command["method"] as? String,
                      let jsonArg = command["json_arg"] as? String else {
                    testAgentFailure(
                        "call_machine_method: needs both `method` and `json_arg` strings")
                    return
                }
                machineMethodResultBox.value = await callMachineMethod(name: method, jsonArg: jsonArg)
            case "device_set_state":
                // The fleet-removal convergence reader — whether `device_id_hex`'s
                // `fauna.state.device-set` row reads Removed/Enrolled from THIS
                // app's own account runtime (`test_crash_recovery_journeys.py`'s
                // kill-between-the-legs journey). Shared FaunaKit handler (macOS +
                // iOS, one implementation); the answer rides the same result slot
                // `call_machine_method` uses, which `call_command` reads back.
                //
                // Cleared FIRST and written only on success, as
                // `custodian_pull_run_now` below: a refusal must leave the slot
                // empty, or the caller reads the PREVIOUS command's result and
                // believes it. Convention 11: a refusal is LOUD.
                machineMethodResultBox.value = nil
                switch await DeviceSetStateTestCommand.apply(command, api: appState.liveClient?.api) {
                case .report(let json):
                    machineMethodResultBox.value = json
                case .refused(let reason):
                    testAgentFailure(reason)
                }
            case "conversations_inject_inbound":
                handleConversationsInjectInbound(command)
            case "conversations_evict_attachment":
                ConversationsTestInject.evictAttachment(command, in: conversationsVM.manager)
            case "conversations_seed_resolved_link_preview":
                ConversationsTestInject.seedResolvedLinkPreview(
                    command, into: conversationsVM.manager)
            case "conversations_create_mls_group":
                ConversationsSendTestCommand.createMlsGroup(command, vm: conversationsVM)
            case "conversations_inject_send_failure":
                ConversationsSendTestCommand.injectSendFailure(command, vm: conversationsVM)
            case "conversations_select_message":
                ConversationsSendTestCommand.selectMessage(command, vm: conversationsVM)
            case "conversations_accept_recipient":
                // The manager decides which recipient picker is active
                // (add-participant overlay > new-thread compose) and whether
                // its current text parses. Mirrors Windows'
                // AcceptVisibleRecipientPicker / Linux's accept_current.
                //
                // PROBE, then commit — the order the GUI's own on-Enter handler
                // drives and every sibling app already had (web
                // `+page.svelte::onRecipientKeydown`, tui, linux
                // `conv_backend::e2e_accept_recipient`). Apple committed with no
                // probe at all until 2026-08-28, and that is not a style
                // difference: without the probe the commit can only ever use the
                // format-only `try_parse_typed_address`, which **cannot produce
                // `TypedAddress::Fauna` by design** (`libs/fauna-conversations/
                // src/address.rs` says so in its own doc comment), so a typed
                // Fauna handle or 64-hex actor id committed NO chip over the
                // agent while working fine for a real user. `resolveRecipient`
                // is a documented no-op when no picker is open or the input is
                // empty, so it is safe unconditionally.
                await conversationsVM.manager.resolveRecipient()
                // Convention 11's declining-arm clause. The boolean was
                // discarded with a literal `_ =` until 2026-08-28, so an accept
                // against an untouched picker acked green and surfaced ~5 s
                // later as the action layer's generic "chip not added", naming
                // neither the command nor the reason. web, tui and linux all hit
                // the same gap; the sentence is shared so the cross-app pin can
                // assert text every app agrees on.
                if !conversationsVM.manager.acceptCurrentRecipientChip() {
                    testAgentFailure(
                        "conversations_accept_recipient: \(acceptRecipientNoChipReason)")
                }
            case "conversations_real_resolve_send_new":
                if let reason = await ConversationsSendTestCommand.resolveSendNew(command, vm: conversationsVM) {
                    testAgentFailure(reason)
                }
            case "conversations_real_send":
                if let reason = await ConversationsSendTestCommand.realSend(command, vm: conversationsVM) {
                    testAgentFailure(reason)
                }
            case "conversations_real_add", "conversations_real_remove", "conversations_real_rename":
                // The membership/rename half of the real-wire arms. Shared
                // FaunaKit handler (macOS + iOS, one implementation) over the
                // same `uniffi::export`ed ConversationsManager methods android,
                // windows, linux and tui drive. Convention 11: a refusal is LOUD.
                if let reason = await ConversationsRealMembershipTestCommand.apply(
                    action, command, vm: conversationsVM)
                {
                    testAgentFailure(reason)
                }
            case "feed_inject_posts":
                FeedInjectPostsTestCommand.apply(command, feedVM: feedVM, client: appState.liveClient, session: appState.session)
            case "feed_inject_error":
                FeedInjectErrorTestCommand.apply(command, feedVM: feedVM, client: appState.liveClient, session: appState.session)
            case "alert_sweep_wake", "reconnect_backoff":
                // The loud surfaces' e2e seams (`fauna_e2e_agent::ALERT_SWEEP_WAKE`
                // / `RECONNECT_BACKOFF`). Shared FaunaKit handler (macOS + iOS, one
                // implementation) over fauna-ffi's test-helpers exports.
                // Convention 11: a refusal is LOUD.
                if let reason = E2eLoudSurfaces.apply(action, command, api: appState.liveClient?.api) {
                    testAgentFailure(reason)
                }
            case "feed_hold_next_reload", "feed_release_held_reload":
                // The feed manager's one-shot reload hold (`ui/feed.md` § The read
                // model). Shared FaunaKit handler (macOS + iOS, one
                // implementation). Convention 11: a refusal is LOUD.
                if let reason = FeedReloadHoldTestCommand.apply(action, feedVM: feedVM) {
                    testAgentFailure(reason)
                }
            case "feed_seed_cue_rollup_for_test":
                // Seeds a real `cues:v1` nest row for "Clear activity data" to
                // delete. Shared FaunaKit handler (macOS + iOS, one
                // implementation) over the same Rust seam tui/linux drive.
                // AWAITED — the caller reads the nest row with no poll around
                // it. Convention 11: a refusal is LOUD.
                if let reason = await FeedSeedCueRollupTestCommand.apply(
                    command, feedVM: feedVM, client: appState.liveClient, session: appState.session)
                {
                    testAgentFailure(reason)
                }
            case "atproto_delegation_advance_clock":
                // The D10 lapse journey's fake render clock. Shared FaunaKit
                // handler (macOS + iOS, one implementation) over the same Rust
                // seam tui drives. Convention 11: a refusal is LOUD.
                if let reason = await DelegationClockTestCommand.apply(command) {
                    testAgentFailure(reason)
                }
            case "open_route":
                // The `fauna://` in-app routes' e2e seam (`ios.md` § App Entry →
                // *In-app routes*): feeds a URI to the same door `onOpenURL`
                // takes, awaiting the page's open so the card is painted on ack.
                // Shared FaunaKit handler (macOS + iOS, one implementation).
                // Convention 11: a refusal is LOUD.
                if let reason = await OpenRouteTestCommand.apply(
                    command, authenticated: appState.session.isAuthenticated,
                    navigate: { showConnectedApps() })
                {
                    testAgentFailure(reason)
                }
            #if !FAUNA_EXCISE_P2P_SHARE
            case "offline_share_advance_clock":
                // The co-present ceremony's fake admission clock (outcome 6,
                // `p2p.md` § Offline share initiation). Shared FaunaKit
                // handler (macOS + iOS, one implementation) over the same
                // Rust seam tui/linux drive. Never refuses.
                OfflineShareTestCommand.applyAdvanceClock(command)
            case "offline_share_drop_connections":
                // Drop every connection a counterpart has open to this
                // session's bound ceremony seat (outcome 5). Shared FaunaKit
                // handler (macOS + iOS, one implementation) over the FFI's
                // process-wide session-seat slot. Convention 11: a refusal is
                // LOUD; cleared FIRST so a refusal never leaves the PREVIOUS
                // command's count behind.
                machineMethodResultBox.value = nil
                switch OfflineShareTestCommand.applyDropConnections(api: appState.liveClient?.api) {
                case .report(let count):
                    machineMethodResultBox.value = count
                case .refused(let reason):
                    testAgentFailure(reason)
                }
            #endif
            case "trust_facet_advance_clock":
                // The Nests trust facet's fake render clock (grant liveness,
                // auto-renew due decision, custody receipt freshness — never the
                // mint clock). Shared FaunaKit handler (macOS + iOS, one
                // implementation) over the same Rust seam tui/linux/android
                // drive. Convention 11: a refusal is LOUD.
                if let reason = await TrustFacetClockTestCommand.apply(command) {
                    testAgentFailure(reason)
                }
            case "backup_audit_run_now":
                // The backup-audit page's fake clock + re-run poke. Shared
                // FaunaKit handler (macOS + iOS, one implementation) over the
                // same Rust seam linux/tui drive. Convention 11: a refusal is LOUD.
                if let reason = await BackupAuditTestCommand.apply(command) {
                    testAgentFailure(reason)
                }
            case "custodian_pull_run_now":
                // One real custodian pull pass — convention 14's causal barrier
                // for the enroll → pull → check-in proof and the orphaned-store
                // witness. Shared FaunaKit handler (macOS + iOS, one
                // implementation); the platform choice (agent vs in-app host) is
                // `FaunaClient`'s — here no provisioner exists, so it runs
                // this process's own host. The report rides the same result slot
                // `call_machine_method` uses, which `call_command` reads back.
                //
                // Cleared FIRST and written only on success: a refusal must leave
                // the slot empty, or the caller reads the PREVIOUS command's
                // result and believes it (the trap the `call_machine_method` guard
                // above records). Convention 11: a refusal is LOUD.
                machineMethodResultBox.value = nil
                switch await CustodianPullTestCommand.apply(command, client: appState.liveClient) {
                case .report(let json):
                    machineMethodResultBox.value = json
                case .refused(let reason):
                    testAgentFailure(reason)
                }
            case "enable_caldav_mailbox":
                await CaldavMailboxTestCommand.apply(command, client: appState.liveClient)
            case "serve_enable_folder":
                // Shared FaunaKit handler (macOS + iOS, one implementation) over the
                // same `FoldersAuthor::serve_set` seam linux drives. The production
                // control it stands in for — the shared `FolderWebdavToggle` in
                // `FoldersContent` — has been on both apple targets since 2026-07-12;
                // only this test seam was macOS-only, which is the whole reason iOS
                // could not witness `files-in-standard-apps` outcome 2.
                await ServeEnableFolderTestCommand.apply(command, client: appState.liveClient)
            case "silent_sign_in":
                // `test_nest_identity_pin_post_auth.py`: trigger the same
                // production post-auth refresh on demand rather than waiting
                // for the one-shot boot-time call.
                await performPostAuthSilentSignIn()
            case "launch_refresh_token":
                // Force the held session bearer's refresh NOW, awaited so the ack
                // lands after the outcome — the wrong-clock refresh witness's
                // ceremony leg (`fauna_e2e_agent::LAUNCH_REFRESH_TOKEN`, case M).
                // Shared FaunaKit handler (macOS + iOS, one implementation).
                // Convention 11: a refusal is LOUD.
                if let reason = await LaunchRefreshTokenTestCommand.apply(client: appState.liveClient) {
                    testAgentFailure(reason)
                }
            case "photo_backup_scheduled_pass_now":
                // Convention 14's run_now poke for the ONE photo-backup trigger a
                // test can never wait for: the OS owns the schedule
                // (`fauna_e2e_agent::PHOTO_BACKUP_SCHEDULED_PASS_NOW`). It drives
                // `BackgroundScheduler.runScheduledUploadPass` — the production
                // body of the `social.fauna.sync.upload` handler, so the poke and
                // the OS take the identical path.
                //
                // Spawned, not awaited: the barrier is the funnel's
                // `passes_completed`, not this ack (an awaited pass would hold the
                // ack for the whole upload). Pre-auth is a quiet, honoured no-op —
                // there is no engine to run yet, and the consumer's own deadline
                // poll on the funnel is what fails, loudly (convention 11).
                //
                // `#if os(iOS)`-gated because `FaunaClient.backgroundScheduler`
                // itself is (`FaunaClient.swift:89`) and `swift build --target
                // FaunaiOS` typechecks this file for the macOS HOST — the same
                // gate the `photo_backup` funnel read above carries, for the same
                // reason. macOS has no scheduler at all (its engine watches the
                // library directly), so there the command is REFUSED loudly rather
                // than dropped.
                #if os(iOS)
                if let scheduler = appState.liveClient?.backgroundScheduler {
                    Task {
                        do {
                            try await scheduler.runScheduledUploadPass()
                        } catch {
                            logMessage(
                                level: .info, target: "fauna.app",
                                message: "[e2e] photo_backup_scheduled_pass_now: the pass threw: \(error.localizedDescription)")
                        }
                    }
                } else {
                    logMessage(
                        level: .info, target: "fauna.app",
                        message: "[e2e] photo_backup_scheduled_pass_now: no session yet (pre-auth)")
                }
                #else
                testAgentFailure(
                    "photo_backup_scheduled_pass_now: this platform has no BackgroundScheduler "
                    + "(macOS watches the photo library directly — ui/folders.md § Photo backup)")
                #endif
            case "widget_refresh_scheduled_pass_now":
                // Convention 14's run_now poke for the home-screen widget's
                // background refresh, whose schedule the OS alone owns
                // (`fauna_e2e_agent::WIDGET_REFRESH_SCHEDULED_PASS_NOW`). It drives
                // `BackgroundScheduler.runWidgetRefreshPass` — the production body
                // of the `social.fauna.widget.refresh` handler — so the poke and
                // the OS take the identical path. Spawned, not awaited: the barrier
                // is the `widget_refresh` pass counters, never this ack. Gated and
                // refused on macOS for the photo-backup poke's reasons above: macOS
                // has no scheduler — its widget stays current because the app stays
                // resident (`apps/ios.md` § Home-screen widget).
                #if os(iOS)
                if let scheduler = appState.liveClient?.backgroundScheduler {
                    Task { @MainActor in await scheduler.runWidgetRefreshPass() }
                } else {
                    logMessage(
                        level: .info, target: "fauna.app",
                        message: "[e2e] widget_refresh_scheduled_pass_now: no session yet (pre-auth)")
                }
                #else
                testAgentFailure(
                    "widget_refresh_scheduled_pass_now: this platform has no BackgroundScheduler "
                    + "(the macOS app stays resident — apps/ios.md § Home-screen widget)")
                #endif
            case "conv_receive_now":
                // Convention 14's run_now poke for the client receive loop (row
                // 92, `fauna_e2e_agent::CONV_RECEIVE_NOW`) — see the macOS twin.
                // No session yet is a legitimate quiet ack.
                conversationsVM.session?.convReceiveNow()
            case "family_notify_check_now":
                // Guardian Notify's run_now poke (convention 14,
                // `fauna_e2e_agent::FAMILY_NOTIFY_CHECK_NOW`) — see the macOS
                // twin for the full rationale, including why every refusal
                // (including "nothing due") is surfaced loudly here.
                if let reason = await GuardianNotifyCadence.shared.flushIfDue() {
                    testAgentFailure("family_notify_check_now: \(reason)")
                }
            case "screen_time_heartbeat":
                // Screen time's fake-clock run_now poke — see the macOS twin
                // for the full rationale.
                if let minutes = (command["minutes"] as? NSNumber)?.intValue ?? (command["minutes"] as? Int) {
                    await appState.screenTime.advanceTestClockAndTick(minutes: minutes)
                } else {
                    testAgentFailure("screen_time_heartbeat: payload needs an integer `minutes`")
                }
            case "account_pump_now":
                // The account plane's run-one-pass-now poke — see the macOS twin
                // for the full rationale (spawned not awaited; pre-auth is a
                // quiet, honoured no-op rather than a dropped command).
                if let api = appState.liveClient?.api {
                    Task {
                        if await api.accountPumpNow() == false {
                            logMessage(
                                level: .info, target: "fauna.app",
                                message: "[e2e] account_pump_now: no account runtime yet (pre-auth)")
                        }
                    }
                } else {
                    logMessage(
                        level: .info, target: "fauna.app",
                        message: "[e2e] account_pump_now: no client yet (pre-auth)")
                }
            case "nav_back":
                // The phone's back gesture (nav-bar back / edge swipe): pop the top
                // screen off the visible tab's `NavigationStack`. iOS-only — macOS
                // has no plain back (`conversations.md` § Persistence). Only the
                // conversations tab's stack answers it today
                // (`ConversationsListView`'s `navBackGeneration` observer); any other
                // tab is refused loudly rather than silently not popping.
                if appState.selectedTab == "conversations" {
                    appState.navBackGeneration += 1
                } else {
                    testAgentFailure("nav_back: only the conversations tab's stack pops today (selected tab: \(appState.selectedTab))")
                }
            case BarrierTestCommand.barrierAction, BarrierTestCommand.probeAction:
                // Convention 14's causal anchor + its self-test probe — see the
                // macOS twin for why the barrier runs inside the handler here
                // (this app's ack fires only after `handleTestCommand` returns).
                // Shared FaunaKit handler; convention 11: a refusal is LOUD.
                if let reason = await BarrierTestCommand.apply(action: action, command: command) {
                    testAgentFailure(reason)
                }
            case FocusWalkTestCommand.focusMoveAction, FocusWalkTestCommand.switchPaneAction:
                // Convention 17 layer (c)'s walk vocabulary — see the macOS
                // twin. Shared FaunaKit handler; convention 11: a refusal is
                // LOUD (and on iOS it always refuses — a declared platform
                // absence, apple-e2e-automation.md § Declared platform absences).
                if let reason = await FocusWalkTestCommand.apply(action: action, command: command) {
                    testAgentFailure(reason)
                }
            default:
                // NEVER silently ignore a command we do not implement — see the macOS
                // twin. A silently-dropped command reads as data loss downstream.
                testAgentFailure("unknown command: \(action)")
            }
            return
        }

        if let session = command["session"] as? [String: Any] {
            applySessionPatch(session)
        }
        if let nav = command["nav"] as? [String: Any] {
            applyNavPatch(nav)
        }
        if let messages = command["messages"] as? [String: Any] {
            AppMessages.applyPatch(messages)
        }
        if let compose = command["compose"] as? [String: Any] {
            await applyComposePatch(compose)
        }
    }

    @MainActor
    private func applySessionPatch(_ session: [String: Any]) {
        let keychain = KeychainStore()
        let s = appState.session

        SessionPatchAccounts.applyPatchedFields(
            session, into: s, keychain: keychain,
            dropActorScopedState: dropActorScopedState)

        if let authenticated = session["authenticated"] as? Bool {
            s.authenticatedOverride = authenticated
            appState.isOnboarding = !authenticated
            // This patch IS the session now, so retire any launch still in flight —
            // the macOS twin's clear, for the reason `LaunchMachineBox.machine`'s doc
            // states.
            launchMachineBox.machine = nil

            // An authenticating patch MUST yield a live `FaunaClient` — see the twin
            // comment in `FaunaMacApp.applySessionPatch`. Authenticating without building
            // one leaves a zombie session: the shell renders off `isOnboarding` alone
            // (which the line above just flipped),
            // while every nest-backed view's `.task` guard-returns on the nil client and
            // each dispatch silently no-ops behind an empty error banner. So default the
            // one field a patch may fairly omit (the device id), exactly as the real
            // onboarding handoff does — keychain, else generate + persist (`OnboardingVM`).
            if authenticated {
                let deviceId: String
                if let injected = s.deviceId, hex_to_data(injected).count == 32 {
                    // The forced-id path (`sync-agent-credentials.md` §
                    // Implementation status today: "the e2e session door's
                    // forced id keeps working unchanged") — kept, and never
                    // persisted, exactly as before this row.
                    deviceId = injected
                } else if let secretHex = s.secretHex,
                          let actorId = try? actor_id_from_secret(secretHex),
                          let resolved = try? FaunaAccounts.deviceId(forActorId: actorId, keychain: keychain) {
                    // The un-forced path: the named `sync_devices` row this
                    // actor registers under, resolved (never minted here) by
                    // the same FaunaKit call site `OnboardingVM` uses — apple
                    // makes no Keychain `.deviceId` write of its own
                    // (`sync-agent-credentials.md` § Credential model, the
                    // RULED 2026-09-20 block, the secret-store face
                    // paragraph). An injected id that is NOT 32-byte hex
                    // (the e2e un-forced case, `UNFORCED_DEVICE_ID`) falls
                    // through to here rather than being adopted verbatim.
                    deviceId = resolved
                } else {
                    // Last resort — should not happen for a well-formed
                    // patch: no valid injected id and no resolvable actor
                    // (secret_hex not in the patch yet, or the derivation's
                    // own install-secret mint failed to read back). Keep the
                    // old random-mint behaviour rather than block the patch
                    // (`adoptPatchedActor` below persists it per-actor).
                    deviceId = generate_device_id()
                    logMessage(level: .error, target: "fauna.app", message: "[applySessionPatch] could not resolve a per-actor device id — minting a fresh random id")
                }
                s.deviceId = deviceId

                guard let nodeUrl = s.nodeUrl, let url = URL(string: nodeUrl),
                      let secret = s.secretHex else {
                    // node_url + secret_hex are the caller's to supply; nothing sane to
                    // default them to. Say so loudly — the shell already rendered as
                    // authenticated, so a silent skip reads as "logged in, nothing works".
                    logMessage(
                        level: .error, target: "fauna.app",
                        message: "[applySessionPatch] authenticated=true but the session lacks a usable node_url/secret_hex — NO FaunaClient built; every nest-backed surface will silently no-op. Fix the caller's session patch.")
                    return
                }

                // The ACCOUNT REGISTRY half of "become this actor", before the
                // rebuild below: the launch resolves the session identity from the
                // registry's active account, so the patched actor must be enrolled
                // (with its resolved nest + device id) and active before any client
                // is built for it. `SessionPatchAccounts` carries the full reasoning
                // and is shared with macOS.
                SessionPatchAccounts.adoptPatchedActor(
                    secretHex: secret, nestUrl: nodeUrl, deviceId: deviceId,
                    keychain: keychain)

                // Same re-scope `enterOptimistically` does — the e2e agent's
                // injection path builds a session without ever going through
                // it, so it needs its own actor-scoped rebuild or an
                // agent-driven account switch would silently share the
                // previous actor's photo-backup store.
                // ⚠ The `@State` write below is LOST here — this runs on the
                // `self` the App captured by value at `init()` — so the live
                // slot beside it is the load-bearing half, and the context comes
                // off the local rather than either slot. Both are still written, from one container: a pair kept
                // in step at both build sites is one fewer thing to reason about.
                let scoped = PhotoBackupRecord.buildModelContainer(actorIdHex: s.actorId)
                modelContainer = scoped
                appState.liveModelContainer = scoped
                let context = scoped.mainContext
                // Same dial swap as `enterOptimistically` — `s.nodeUrl` stays
                // the caller-supplied literal; only the socket target
                // resolves through the harness override (identity in
                // release).
                let dialUrl = URL(string: resolvedDialUrl(nestUrl: nodeUrl)) ?? url
                let faunaClient = FaunaClient(
                    nodeUrl: dialUrl, secretHex: secret,
                    deviceId: deviceId, modelContext: context
                )
                // Store on the observed AppState immediately so the
                // `appState.liveClient ?? client` env injection reaches admin /
                // mail views right away (reassigning the App's `@State client`
                // from this callback may not propagate; mirrors FaunaMacApp).
                // This write is also why the injection asks the observed slot
                // first: `client` may still hold the object a launch put there.
                appState.liveClient = faunaClient
                Task {
                    // Authenticate BEFORE setting self.client so that
                    // FeedListView's onChange(of: client) fires with a
                    // valid API token — avoids the auth timing race.
                    try? await faunaClient.api.authenticate(secret: secret)
                    self.client = faunaClient
                    await faunaClient.start()
                    // Critical-alerts session-start + periodic re-sweep loop —
                    // the e2e session-patch path calls the full `start()`
                    // (unlike macOS's, which skips it), so this fires here too
                    // (critical-alerts.md § Mechanism; `test_session_start_alert_sweep.py`).
                    criticalAlertsHost.startSweepLoop(api: faunaClient.api)
                    // Guardian Notify's flush cadence (family-safety.md §
                    // Guardian Notify), same reasoning as the sweep loop
                    // above: the production `onLaunchAuthenticated` closure
                    // starts it, but the e2e session-patch path never runs
                    // that closure — without this, `GuardianNotifyCadence.
                    // shared`'s `api` stays nil for every e2e-logged-in ward.
                    GuardianNotifyCadence.shared.start(api: faunaClient.api)
                    // Same reasoning for screen-time's flush cadence
                    // (family-safety.md § Screen time): without this,
                    // `appState.screenTime`'s `api` stays nil for every
                    // e2e-logged-in ward, so `screen_time_heartbeat` finds a
                    // due report but no live api to send it through.
                    appState.screenTime.start(api: faunaClient.api)
                    // Same reasoning for the author-side subscription reconcile
                    // (monetization.md § Pillar 1): the production
                    // `onLaunchAuthenticated` closure starts it, but the e2e
                    // session-patch path never runs that closure — without this,
                    // an e2e-logged-in author's client never mints/uploads the
                    // KeyBlob a queued follow/subscribe needs, so
                    // `drain_auto_approvals` never runs and every subscriber-side
                    // assertion times out
                    // (`test_subscriptions.py::test_follow_auto_grants_in_encrypted_mode`).
                    SubscriptionsAuthorCadence.shared.start(api: faunaClient.api)
                    // Two e2e sub-modes (the e2e session-patch path never runs the
                    // production `activate(...)`, which keeps the deterministic mock
                    // conversation backends):
                    //  • default — attach the conversations drafts autosync directly,
                    //    else the autosave gate stays closed and no draft reaches the
                    //    nest `__drafts` plane (the SAVE-side e2e gap, file-sync.md
                    //    § Drafts Sync).
                    //  • `FAUNA_E2E_REAL_CONVERSATIONS` (the `real_conversations`
                    //    marker) — build the REAL dual-rail `ConversationsSession` and
                    //    `activate` it so `startReceiveLoop` drains real-decrypted
                    //    inbound mail/DMs into the snapshot (the tier_3
                    //    test_mail_client_{receive,send,spam_receive} harness); the
                    //    apple twin of windows `StartE2eRealConversationsAsync` + linux
                    //    `conv_backend::start_conversations_session`. `activate` wires
                    //    drafts too, so this branch subsumes the default one.
                    // Both lazily open the WS via `ensureNestConnected` (auth above
                    // primed `api.secret`); best-effort. Mirrors macOS.
                    // Stamp the verdict on BOTH arms before branching — a bare `if`
                    // here is silent when it is false, which is how a mock-backend
                    // session came to look identical to a real one in the log. The
                    // wording is shared FaunaKit's; only the call is per-shell.
                    if let verdict = FaunaE2E.realConversationsGateVerdict {
                        logMessage(level: .info, target: "fauna.app", message: verdict)
                    }
                    if FaunaE2E.realConversations {
                        do {
                            // self_address = "<handle>@<domain>", composed ONCE in shared
                            // FaunaKit (`APIClient.e2eSelfAddress`) — both shells used to
                            // carry their own copy of this, which is how the same
                            // wrong-domain defect came to exist twice. Its doc owns why the
                            // domain must be the nest's canonical identity domain and never
                            // the URL host.
                            let selfAddress = await faunaClient.api.e2eSelfAddress()
                            try await conversationsVM.rebuild(
                                api: faunaClient.api,
                                selfAddress: selfAddress, selfSecretHex: secret,
                                deviceIdHex: faunaClient.deviceId,
                                predecessorBackupKeys: faunaClient.resolvedPredecessorBackupKeys()
                            )
                            // Same seam install as the production login path
                            // above — the e2e `real_conversations` path is how
                            // the room-restricted-post journey drives a real
                            // MLS session on apple, and how the widget's
                            // background-refresh witness reaches a real poll.
                            installConversationsSessionSeams(faunaClient)
                        } catch {
                            logMessage(level: .error, target: "fauna.app", message: "[e2e] real conversations session activation failed (non-fatal): \(error)")
                        }
                    } else if let drafts = try? await faunaClient.api.draftsSync(rail: "conversations") {
                        conversationsVM.attachDraftsSync(drafts)
                    }
                }
            }
        }
    }

    private static let topLevelTabs: Set<String> = ["conversations", "feed", "contacts"]
    // `files` + `devices` moved into the Settings shell (Settings → Folders +
    // Settings → Devices) at the 2026-06-28 sync/folder UI unification — reached
    // via `{"view":"settings","id":"folders"|"devices"}`, not a "More" entry.
    // `family` is reached from the gated in-Settings entry / the global
    // supervised-indicator rather than a More-list row (ui.yaml gated_tabs:
    // "mobile: in-Settings entry"), but its DESTINATION lives in the More stack —
    // so the cross-app `{"view":"family"}` state-protocol nav lands the page.
    private static let moreTabs: Set<String> = ["profile", "events", "settings", "media", "backups", "bridges", "moderation", "notifications", "family"]

    @MainActor
    private func applyNavPatch(_ nav: [String: Any]) {
        guard let stack = nav["stack"] as? [[String: Any]],
              let first = stack.first,
              let view = first["view"] as? String else { return }

        // Clear messages on navigation — errors belong to the page that raised them
        AppMessages.error = nil
        AppMessages.warning = nil
        AppMessages.info = nil

        // Bump on every patch so admin pages keyed on `navGeneration` re-fetch on
        // re-navigation (the e2e poll-by-re-navigate pattern), not just first mount.
        appState.navGeneration += 1

        // Any navigation clears a stale OTHER-profile target; the profile case
        // below re-sets it when the nav stack carries an `actor_id`.
        appState.profileActorId = nil

        switch view {
        case "welcome":
            appState.isOnboarding = true
            appState.inAdmin = false
            appState.moreSelectedView = nil
            appState.selectedSettingsPage = nil
        case "settings":
            // Settings is a "more"-tab sub-view that ALSO carries a sub-page in
            // the second stack entry's `id` (settings.md:22 — the same admin-style
            // two-element nav the `admin` case below honors). iOS keeps idiomatic
            // settings nav (settings.md:28), so the sub-id drives the `SettingsView`
            // `NavigationStack` via `selectedSettingsPage` (mirrors `selectedAdminPage`).
            // Absent ⇒ the settings root list. This is what lets the cross-app
            // `{"view":"settings"},{"view":"settings","id":"mail-settings"}` land the
            // mail-settings sub-page instead of stopping at the root list.
            appState.isOnboarding = false
            appState.inAdmin = false
            appState.selectedTab = "more"
            appState.moreSelectedView = "settings"
            let subId = stack.count > 1 ? (stack[1]["id"] as? String) : nil
            appState.selectedSettingsPage = SettingsPage(navId: subId)
        case "admin":
            // Admin is a sub-paged top-level shell: the cross-app state protocol
            // carries the sub-page in the second stack entry's `id` (admin.md
            // § Navigation model; mirrors macOS + linux `nav.stack[1].id`). Absent
            // ⇒ dashboard. (`navigate_users` → "users", `navigate_nest` →
            // "admin-nest", `navigate_settings` → "settings", etc.)
            appState.isOnboarding = false
            appState.inAdmin = true
            let subId = stack.count > 1 ? (stack[1]["id"] as? String) : nil
            appState.selectedAdminPage = AdminPage(navId: subId) ?? .dashboard
        case "devices", "folders":
            // Compat shim (2026-06-28 sync/folder UI unification): the device
            // roster + folder control plane moved out of the "More" menu into the
            // Settings shell (devices.md / folders.md). A bare
            // `{"view":"devices"|"folders"}` nav — the pre-unification cross-app
            // shape still used by the shared e2e action layer until every app
            // migrates — lands on the matching Settings sub-page so iOS stays
            // drivable during the rollout. (The canonical post-migration form
            // `{"view":"settings","id":"devices"|"folders"}` is the `settings` case.)
            appState.isOnboarding = false
            appState.inAdmin = false
            appState.selectedTab = "more"
            appState.moreSelectedView = "settings"
            appState.selectedSettingsPage = SettingsPage(navId: view)
        case "search":
            // Search is an overlay above the main tabs, not a tab/page of its
            // own (`ContentView.MainTabView`'s `showSearchBar`) — macOS has no
            // equivalent case because its `search-query-field` is always
            // present via `.searchable(placement: .sidebar)`. Leaves the
            // current tab as-is (unlike topLevelTabs/moreTabs below), matching
            // the real search-toggle-button gesture, which never changes tabs.
            appState.isOnboarding = false
            appState.inAdmin = false
            appState.showSearchBar = true
        case _ where Self.topLevelTabs.contains(view):
            appState.isOnboarding = false
            appState.inAdmin = false
            appState.selectedTab = view
            appState.moreSelectedView = nil
            // Leaving More (to a top-level tab) drops any stale Settings
            // sub-page the same way a real `more-back-button` tap does
            // (`MoreView.exitMoreSubview`) — a state-protocol nav jump skips
            // that cleanup entirely, so `selectedSettingsPage` could otherwise
            // sit on `.status` (still pushed on `SettingsView`'s OWN nested
            // `NavigationStack`, kept alive off-screen by the outer `TabView`)
            // while a LATER patch swaps `MoreView`'s content straight to a
            // different moreTab — the stale nested stack state is the leading
            // suspect for `profile-view` intermittently never rendering on
            // re-entry.
            appState.selectedSettingsPage = nil
        case _ where Self.moreTabs.contains(view):
            appState.isOnboarding = false
            appState.inAdmin = false
            appState.selectedTab = "more"
            appState.moreSelectedView = view
            // Same staleness guard as the topLevelTabs case above — "settings"
            // never reaches this catch-all (it has its own case), so every
            // value that DOES land here is leaving Settings, if it was ever
            // entered.
            appState.selectedSettingsPage = nil
            // Another actor's profile carries its hex actor_id in the nav stack
            // (profile.md § Layout & flow → Another's profile); SELF profile has
            // none → the reset above leaves it nil. profileNavTarget normalizes
            // the viewer's OWN id back to nil (shared fauna-core, priority #2 —
            // was three hand-rolled per-app copies) — a raw pass-through would
            // render the viewer's own profile in OTHER shape.
            if view == "profile" {
                appState.profileActorId = (first["actor_id"] as? String).flatMap {
                    profileNavTarget(entryActorId: $0, selfActorId: appState.session.actorId)
                }
            }
        default:
            logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] Unknown nav view: \(view)")
        }
    }

    /// compose.file — attach a file via the shared `FeedVM.attachComposeFile`
    /// seam, which HOLDS the bytes on the LIVE composer's VM; a subsequent real
    /// post-submit-button click seals them for the staged audience and uploads
    /// them (`ui/media.md` § Encryption at rest — the seal is resolved before
    /// the attachment is uploaded, so the pick cannot upload).
    /// Mirrors `FaunaMacApp.applyComposePatch`; iOS has no `compose.post`
    /// branch — unlike macOS, iOS's `create_post`/`create_post_with_tags` never
    /// use compose-state injection (`_use_compose_state()` is macOS-only), only
    /// `create_post_with_image` does, via this file-only patch.
    @MainActor
    private func applyComposePatch(_ compose: [String: Any]) async {
        if let file = compose["file"] as? String {
            // See `FaunaMacApp.applyComposePatch` for the full rationale — the
            // `target` field disambiguates feed's `compose-file` from
            // conversations' `attachment-button`, both of which are OS panels no
            // in-process agent can drive.
            if compose["target"] as? String == "attachment-button" {
                await conversationsVM.attachComposerFile(atPath: file)
                return
            }
            // profile-edit-avatar / profile-edit-banner — see
            // `FaunaMacApp.applyComposePatch` for the full rationale.
            if let target = compose["target"] as? String,
               target == "profile-edit-avatar" || target == "profile-edit-banner" {
                guard let data = try? Data(contentsOf: URL(fileURLWithPath: file)) else {
                    logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] compose.file (\(target)): could not read \(file)")
                    AppMessages.error = "compose.file (\(target)): could not read staged file"
                    return
                }
                if target == "profile-edit-avatar" {
                    profileEditVM.stageAvatar(path: file, data: data)
                } else {
                    profileEditVM.stageBanner(path: file, data: data)
                }
                return
            }
            guard appState.liveClient != nil else {
                logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] compose.file: no client available")
                AppMessages.error = "compose.file: not authenticated"
                return
            }
            do {
                try await feedVM.attachComposeFile(atPath: file)
                logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] compose.file: attached \(file)")
            } catch {
                logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] compose.file failed: \(String(describing: error))")
                AppMessages.error = "compose.file failed: \(error.localizedDescription)"
            }
        }
    }

    #endif

    // MARK: - Multi-account switching (`long-term-store.md` § Multi-account evolution)
    //
    // The iOS twin of `FaunaMacApp`'s switcher glue — same sequence, same reasons; see
    // there for the full rationale. Only the app-owned state differs (`isOnboarding`
    // rather than `isOnboarded`, no menu-bar controller).

    /// See `FaunaMacApp.switchInFlight` — first trigger wins, cleared on every exit path.
    @MainActor private static var switchInFlight = false

    /// `set_active` → teardown → rebuild (`long-term-store.md`:268, switch-first). The
    /// registry mutation runs FIRST so a target that is unknown or has no resolvable secret
    /// fails while the current session is still whole. The rebuild is `runLaunch()` itself —
    /// it builds the machine over `FaunaAccounts.bootLaunchPersistence()`, which reads the
    /// now-active account from the registry for launch routing.
    /// `confirmed` is the Stage-2 re-auth bit — see `FaunaMacApp.switchAccount`.
    /// Throws the registry's refusal for the Account page to paint — see
    /// `FaunaMacApp.switchAccount`.
    @MainActor
    private func switchAccount(to actorId: String, confirmed: Bool) async throws {
        guard !Self.switchInFlight else { return }
        Self.switchInFlight = true

        do {
            if confirmed {
                try FaunaAccounts.registry().setActiveConfirmed(actorId: actorId)
            } else {
                try FaunaAccounts.registry().setActive(actorId: actorId)
            }
        } catch {
            logMessage(level: .error, target: "fauna.accounts",
                       message: "[account-switch] setActive(\(actorId)) failed: \(error); keeping the current session")
            Self.switchInFlight = false
            throw error
        }

        // The leave-gesture push drop (`common.md` § Registration, ruled
        // 2026-08-30): the OUTGOING actor's row falls while its authority is
        // still in hand (the live session is the outgoing one until the
        // teardown below). AFTER the activation, never before, as web's
        // `performSwitch` and the macOS twin do: a refused switch keeps the
        // user's notifications (`multiple-accounts` outcome 11). Best-effort
        // inside `dropActorRow()`; the incoming actor re-arms at its own
        // session start (`PushManager.onSessionStart`), while the bit is set.
        // `#if os(iOS)`-gated because `pushManager` itself is (the `swift
        // build --target FaunaiOS` host typecheck excludes it). Through
        // `appDelegate.pushManager` — the same observed-slot door
        // `enterOptimistically` writes alongside the raw `@State` — not the
        // raw property: this function is `appState.onSwitchAccount`-reachable,
        // and a raw read is the row 376 bug class.
        #if os(iOS)
        await appDelegate.pushManager?.dropActorRow()
        #endif

        await tearDownSessionForSwitch()
        Task { @MainActor in
            // FP teardown must COMPLETE before the incoming session starts
            // (file-sync.md § Apple File Provider binding, multi-account: the
            // capability store is single-slot, so a stale async revoke landing
            // after the new account's provision would wipe the fresh slot; and
            // the outgoing account's domains must not keep serving mid-switch).
            // Removal preserves dirty data (`.preserveDirtyUserData`); mirrors
            // FaunaMacApp.switchAccount. E2e-gated like the launch reconcile.
            if !FaunaE2E.isActive {
                await FileProviderCoordinator.signOut()
            }
            runLaunch()
            Self.switchInFlight = false
        }
    }

    /// Drop every piece of in-memory actor-scoped state this target owns. **The**
    /// canonical list — every teardown path calls it and hand-lists none of it.
    /// The macOS twin is `FaunaMacApp.dropActorScopedState`, sharing this same
    /// FaunaKit code for both halves: `ActorScope.resetSharedState()` for the
    /// two cadences + `FeedVM`'s statics, `ActorScope.dropAppOwnedState(...)`
    /// for `criticalAlertsHost`/`conversationsVM`/`screenTime`/`modelContainer`/
    /// `appState`'s e2e caches (identically typed on both targets, so no
    /// drift risk despite being per-target-owned). See `ActorScope` for the
    /// ruling (one explicit function, not a registry) and `account-scoping.md`
    /// § The scoping taxonomy for the contract.
    @MainActor
    private func dropActorScopedState() {
        ActorScope.resetSharedState()
        ActorScope.dropAppOwnedState(
            criticalAlertsHost: criticalAlertsHost,
            conversationsVM: conversationsVM,
            feedVM: feedVM,
            eventsVM: eventsVM,
            devicesVM: devicesVM,
            screenTime: appState.screenTime,
            modelContainer: &modelContainer,
            appState: appState
        )
    }

    /// Drop everything the OUTGOING identity owns, keeping its credentials (a switch, not a
    /// sign-out). Anything left running here would keep acting as — or showing — the previous
    /// identity, which on a switch means another person's data.
    @MainActor
    private func tearDownSessionForSwitch() async {
        // Convention 14's teardown counter — see the macOS twin for why the bump
        // lives in the function rather than at its three call sites, and why
        // this line is the commit point (`SessionGeneration`).
        SessionGeneration.recordTeardown()
        await client?.shutdown()
        await appState.liveClient?.shutdown()

        // Every in-memory surface keyed to the outgoing identity, in one call.
        dropActorScopedState()

        launchMachineBox.machine = nil
        appState.identityChanged = nil
        appState.needsUpdateMessage = nil
        appState.signInRefusedMessage = nil
        appState.accountIndexRefusal = nil
        appState.accountIndexConfirming = false
        appState.isOnboarding = false
        appState.inAdmin = false
        appState.selectedSettingsPage = nil
        // ⚠ What this does NOT unmount: `selectedTab` and `moreSelectedView` are
        // deliberately left alone (a switch keeps the user in More → Settings, where
        // the switcher lives), and `isOnboarding` stays false, so `MainTabView` — and
        // every page-owned view model on a tab or a More destination — outlives the
        // switch. Those pages own their own identity-keyed seam; the two lines above
        // are the guarantee for admin and Settings pages. `ActorScope` § *State a view
        // owns* is the one place that says which is which — extend it, don't restate.
        // The FP reconcile hook captured the OUTGOING session's secret/nest —
        // the incoming launch re-sets it.
        appState.fpReconcile = nil

        appState.session.clearAuthenticatedOverride()
        appState.session.secretHex = nil
        appState.session.actorId = nil
        appState.session.nodeUrl = nil
        appState.session.deviceId = nil
        appState.session.handle = nil
        // Onboarding-only provisioning intents — an appended identity must not re-provision
        // the nest it is joining, and a plain switch must not replay the previous identity's
        // claim-time intents.
        appState.session.pendingFirstSetupMail = nil
        appState.session.pendingCaldavEnable = false
        appState.session.pendingCarddavEnable = false
        appState.session.pendingWebdavEnable = false
        appState.session.pendingTrustPromptGranted = false
        appState.session.pendingRecoveryKitHex = nil

        client = nil
        appState.liveClient = nil
    }

    /// Forwards to the shared `FaunaKit.performPostAuthSilentSignIn` (`security.md`
    /// § Post-auth surfacing) — see there for the full rationale. Also the target
    /// of the `#if DEBUG` `silent_sign_in` test command, which awaits this
    /// directly rather than waiting for the one-shot boot-time call.
    ///
    /// Deliberately reads `appState.session` — a stable class instance — rather
    /// than the `@State private var client`: the test-command dispatch runs
    /// through its init-time-captured `self`, and `@State` vars *reassigned*
    /// after init (like `client`) read back stale/nil through that detached copy
    /// (the same trap `MachineMethodResultBox`'s doc comment records). The shared
    /// function's `tearDown`/`runLaunch` closures still work from the detached
    /// self because their observable effect is entirely through `appState`
    /// mutations (also stable), matching `resetToFactory()`'s existing precedent
    /// for this same test-command path.
    @MainActor
    private func performPostAuthSilentSignIn() async {
        await FaunaKit.performPostAuthSilentSignIn(
            session: appState.session,
            escalate: { await self.escalateToLaunchSurface() }
        )
    }

    /// Tear the session down and re-enter launch — the shared
    /// `FaunaKit.escalateToLaunchSurface` over this app's own teardown and
    /// relaunch, guarded by the switch's `switchInFlight`. Credentials kept.
    @MainActor
    private func escalateToLaunchSurface() async {
        await FaunaKit.escalateToLaunchSurface(
            switchInFlight: &Self.switchInFlight,
            tearDown: { await self.tearDownSessionForSwitch() },
            runLaunch: { self.runLaunch() }
        )
    }

    /// "Add account" → onboarding in **append** mode (a sheet over the live session).
    @MainActor
    private func beginAddAccount() {
        onboardingVM.machine.reset()
        appState.isAddingAccount = true
    }

    /// Forwards to the shared `FaunaKit.completeAppendedAccount` — see there for
    /// the full rationale.
    @MainActor
    private func completeAppendedAccount() async {
        await FaunaKit.completeAppendedAccount(
            wizard: onboardingVM.machine,
            dismissWizard: { appState.isAddingAccount = false },
            switchAccount: { actorId, confirmed in
                try? await self.switchAccount(to: actorId, confirmed: confirmed)
            }
        )
    }

    // Compiled out of release artifacts (testing.md convention 15) — both
    // `handleTestCommand`'s "reset"/"logout" actions, its only callers.
    #if DEBUG

    @MainActor
    private func resetToFactory() async {
        // A reset tears the authenticated session down, so it counts
        // (`SessionGeneration` — deliberately not cleared here either).
        SessionGeneration.recordTeardown()
        // Sign-out-shaped stop, BEFORE the erase below and before the rest of this
        // method's teardown — same order and same reason as `StatusVM.signOut`
        // (`sync-agent-credentials.md` § Credential model → *The signed-out
        // reconcile*). iOS has no native Keychain arm, so the account-store
        // namespace (the writer key `deleteAll()` is about to sweep) rides the
        // SAME `KeychainStore` service as everything else here (`KeychainStore.swift`'s
        // `installPlatformCredentialStore` doc) — unlike macOS's `resetToFactory`,
        // whose separate native account-store Keychain service `deleteAll()`
        // cannot reach, this reset erases the slot and so counts as a sign-out.
        // `client?.shutdown()` below still runs its own switch-shaped stop; that's
        // a safe no-op once this one has already torn the runtime down.
        await liveFaunaClient?.api.stopAccountRuntimeForSignOut()
        // EVERY key, not the three identity ones — a surviving pending-factory-reset slot
        // would leak into the next test in the module (the app process is session-scoped)
        // and launch it onto a claim page for a nest that no longer exists. Mirrors
        // FaunaMacApp; see `KeychainStore.deleteAll()`.
        KeychainStore().deleteAll()
        // Reset the wizard machine so the next test (which doesn't relaunch
        // the app) starts at identity_choice. `machine.reset()` clears the
        // in-memory state and fires an observer notification so SwiftUI
        // re-renders the active WelcomeView with the new step. Post the
        // onboarding-persistence cleanup the wizard owns no durable state,
        // so deleting the long-term `KeychainStore` keys above is enough.
        onboardingVM.machine.reset()
        // The launch surfaces are launch-scoped, not app-scoped: a test that left the app
        // on the blocking identity-changed / needs-update / account-index-unreadable
        // surface must not hand it to the next one.
        appState.identityChanged = nil
        appState.needsUpdateMessage = nil
        appState.signInRefusedMessage = nil
        appState.accountIndexRefusal = nil
        appState.accountIndexConfirming = false
        launchMachineBox.machine = nil

        appState.session.clearAuthenticatedOverride()
        appState.session.secretHex = nil
        appState.session.actorId = nil
        appState.session.nodeUrl = nil
        appState.session.deviceId = nil
        appState.session.handle = nil
        appState.isOnboarding = true
        appState.inAdmin = false
        appState.selectedSettingsPage = nil
        // Not overwritten by ordinary settings nav (only the mail-lists Members
        // button sets it) — must be cleared explicitly or it leaks into the next
        // test's direct mail-list-members navigation.
        appState.selectedMailListId = nil
        appState.selectedMailListName = nil

        // Shut the client down BEFORE releasing it: ARC-nulling `client` reclaims
        // none of what it owns (WS, observer loops, sync host), and this path — the
        // iOS twin of `FaunaMacApp.resetToFactory`, which has always done this —
        // was the one site that never did, so every test in the module left a live
        // client behind.
        await liveFaunaClient?.shutdown()
        self.client = nil
        appState.liveClient = nil

        // Same canonical drop the production paths run — the next test in the
        // module does not relaunch the app, so anything left here leaks into it.
        // (This site used to reach the `_for_test` conversations seam; it is a thin
        // alias for the production wipe the canonical drop calls, so nothing is
        // lost.)
        dropActorScopedState()
        // The succession hand-off's ONE clear point — the macOS twin carries the
        // reasoning: those four fields exist to survive a *switch* teardown, and a
        // reset is the case where there is no successor, predecessor or sweep left
        // to describe.
        SuccessionHandoff.clearOnFactoryReset()
        // Per-process dial budget: the macOS twin carries the reasoning.
        dialBudgetClearForTest()
    }

    /// Dispatch named `OnboardingMachine` method by string name. Used by
    /// the cross-app `call_machine_method` E2E bridge (see
    /// `tests/e2e-unified/drivers/machine_test_setter.py`). Mirrors the
    /// macOS implementation in `Fauna-macOS/App/FaunaMacApp.swift`.
    /// Requires the FFI to be built with `--features test-helpers`
    /// (always enabled in the `apple-ffi` recipe — methods are inert
    /// unless called). Returns the reader's decoded JSON value (`nil` for
    /// every setter name, typed cases included) for the caller to stash as
    /// `machine_method_result`.
    ///
    /// The fallback below routes through the **async** dispatcher
    /// (`callMachineMethodAsync`), which additionally runs the machine's
    /// async methods (`verify_dns`, `wizard_submit_claim_code`,
    /// `submit_nat_mode_choice`, …) to completion. Through the sync one
    /// they fall into its silent `_` arm and ack green having done nothing
    /// — which is exactly how the live Hetzner provisioning drive failed
    /// on macos before its own fix (`verify_dns` never ran, so
    /// provisioning sat at `overall: 'Idle'` for the full timeout).
    /// Delegating rather than hand-listing the async names here keeps ONE
    /// name table, in shared Rust.
    @MainActor
    @discardableResult
    private func callMachineMethod(name: String, jsonArg: String) async -> Any? {
        let machine = onboardingVM.machine
        let argData = Data(jsonArg.utf8)
        let decoder = JSONDecoder()

        // The one arm the shared dispatcher deliberately leaves to the
        // client: provisioning spawns a task that outlives the call
        // (minutes — the driver polls `provisioning_snapshot` instead of
        // blocking on the ack), so *which* runtime owns the spawn is
        // platform-divergent.
        //
        // ⚠ Both map to `runProvisioning()`, NOT to the same-named
        // `startProvisioning()` / `retryProvisioning()`. Those two have
        // bare `tokio::spawn` Rust bodies needing an ambient tokio runtime
        // on the CALLING thread, which SwiftUI's main thread does not have.
        // This mirrors the macOS fix and the production buttons, which
        // call `runProvisioning()` for the same reason, and
        // `run_provisioning_inner` resets the snapshot + cancel flag at
        // entry, so it doubles as the retry entry point.
        //
        // The detached `Task` is what preserves the bridge's
        // return-immediately contract: `runProvisioning()` drives to
        // completion (minutes), so awaiting it here would stall the ack
        // the driver is waiting on.
        switch name {
        case "start_provisioning", "retry_provisioning":
            Task { await machine.runProvisioning() }
            return nil
        default:
            break
        }

        // The **registry** arms next — they need this app's own account
        // registry, which no machine dispatcher can reach (`RegistryTestBridge`).
        if case .handled(let resultJson) = RegistryTestBridge.call(name: name, jsonArg: jsonArg) {
            return resultJson.flatMap(machineMethodResultValue(from:))
        }

        switch name {
        case "set_handle_check_snapshot_for_test":
            do {
                let snap = try decoder.decode(HandleCheckSnapshotPayload.self, from: argData).intoSnapshot()
                machine.setHandleCheckSnapshotForTest(snap: snap)
            } catch {
                logMessage(level: .debug, target: "fauna.testagent", message: "[callMachineMethod] decode HandleCheckSnapshot failed: \(String(describing: error))")
            }
            return nil
        case "set_invite_request_snapshot_for_test":
            do {
                let snap = try decoder.decode(InviteRequestSnapshotPayload.self, from: argData).intoSnapshot()
                machine.setInviteRequestSnapshotForTest(snap: snap)
            } catch {
                logMessage(level: .debug, target: "fauna.testagent", message: "[callMachineMethod] decode InviteRequestSnapshot failed: \(String(describing: error))")
            }
            return nil
        case "set_current_handle":
            // Tests fixture the input ahead of a snapshot setter to put
            // the page in a state the user could legitimately reach
            // (Check is gated on input non-empty per target-state §2).
            if let s = try? decoder.decode(String.self, from: argData) {
                machine.setCurrentHandle(h: s)
            }
            return nil
        case "set_nest_url":
            if let s = try? decoder.decode(String.self, from: argData) {
                machine.setNestUrl(url: s)
            }
            return nil
        case "reset":
            machine.reset()
            return nil
        default:
            // Everything else — including "set_step_for_test" (delegated to
            // Rust's serde dispatch rather than a typed Swift step mapper: a
            // local switch silently no-op'd any variant it didn't list,
            // which hid the admin-path steps (ClaimCode / NatModeChoice)
            // in-process) and value-returning reader names
            // (`provisioning_snapshot`, `provider_base_url`, …, per the
            // fake-cloud native-bridge follow-ons) — routes through the
            // ASYNC value-returning Rust dispatch, which additionally
            // drives the machine's async methods to completion (see the
            // function doc comment). Setter names return `nil` here
            // exactly as the sync `callMachineMethodWithResult` did.
            guard let json = await machine.callMachineMethodAsync(name: name, jsonArg: jsonArg) else {
                return nil
            }
            return machineMethodResultValue(from: json)
        }
    }

    @MainActor
    private func logoutKeepData() async {
        // A logout is a teardown (`SessionGeneration`).
        SessionGeneration.recordTeardown()
        let keychain = KeychainStore()
        // Route the logout through the registry (`account-scoping.md`
        // § Concurrent instances, the delete corollary): `remove` deletes the
        // active account's per-actor slots + index row. Mirrors
        // `FaunaMacApp.logoutKeepData`.
        //
        // Unlike this file's `resetToFactory` (`KeychainStore().deleteAll()`, a
        // whole-SERVICE sweep by `kSecAttrService`), `remove` only ever touches
        // the registry's own `fauna/…` rows, never the separately-namespaced
        // `fauna-account-store/…` ones the writer key rides under, even though
        // both share this same physical Keychain service on iOS. So this path
        // does not erase the account-store slot, and `client?.shutdown()` below
        // keeps the switch-shaped stop, per `sync-agent-credentials.md`
        // § Credential model → *The signed-out reconcile*'s rule.
        let registry = FaunaAccounts.registry(keychain: keychain)
        if let active = registry.active() {
            do {
                try registry.remove(actorId: active)
            } catch {
                logMessage(level: .error, target: "fauna.app",
                           message: "[logout] registry remove(\(active)) failed: \(error)")
            }
        }

        // Stop everything the session owns before releasing it — ARC-nulling
        // `client` reclaims none of it, and a logout that left the WS and observer
        // loops running would keep acting as the signed-out identity. Mirrors
        // `FaunaMacApp.logoutKeepData`, which has always done this.
        await liveFaunaClient?.shutdown()

        appState.session.clearAuthenticatedOverride()
        appState.session.secretHex = nil
        appState.session.actorId = nil
        appState.session.nodeUrl = nil
        appState.session.deviceId = nil
        appState.session.handle = nil
        appState.inAdmin = false
        self.client = nil
        appState.liveClient = nil

        // This path `runLaunch()`s straight into the promoted account on a
        // multi-account install, so every surface below would otherwise be read by
        // the INCOMING account.
        dropActorScopedState()

        // If the removal promoted another account (multi-account install),
        // boot into it — `remove` made it active, so a logout lands on the
        // next signed-in account exactly as the switcher's remove does. With
        // no account left, the classic signed-out end state (the wizard).
        if registry.active() != nil {
            appState.isOnboarding = false
            runLaunch()
        } else {
            appState.isOnboarding = true
        }
    }

    #endif

    /// Factory-reset re-onboard (admin-nest Danger Zone → shared `AdminNestView` →
    /// `appState.onFactoryReset`). After `fauna.admin.factory_reset` wipes the box
    /// and returns the post-reset `claimCode` (the human never sees it), drop the
    /// authed session but **keep local creds** — the box was wiped, not the client,
    /// so the SAME identity re-claims the fresh nest — and re-seed the wizard at the
    /// claim-code step with the code pre-filled, so the just-reset nest is
    /// immediately re-claimable. Mirrors `FaunaMacApp.factoryResetReonboard` /
    /// linux `main.rs::register_factory_reset_handler`; the re-seed lives here (not
    /// the shared FaunaKit view) because it touches the App-owned `onboardingVM` +
    /// `isOnboarding`. Per `mail-bridge-lifecycle.md` § Factory reset +
    /// `architecture/nest/common.md` § Client-state recoverability.
    @MainActor
    private func factoryResetReonboard(claimCode: String) async {
        // Capture identity material BEFORE dropping the session (kept in the
        // registry — only the in-memory session is torn down).
        let material = FaunaAccounts.sessionMaterial()
        let secretHex = appState.session.secretHex ?? material?.secretHex
        let nestUrl = appState.session.nodeUrl ?? material?.nestUrl ?? ""
        let handle = appState.session.handle ?? material?.handle ?? ""
        guard let secretHex, !nestUrl.isEmpty else {
            logMessage(level: .error, target: "fauna.app", message: "[FaunaApp] factoryResetReonboard: missing identity material; cannot re-onboard")
            return
        }

        // Drop the authed session, KEEPING local creds (the account's secret /
        // nest_url / device_id stay in the registry — the identity re-claims the
        // fresh nest).
        //
        // Counted here and not above the guard: a missing-material bail drops
        // nothing (`SessionGeneration` counts teardowns, not attempts).
        SessionGeneration.recordTeardown()
        // Shut the client down before releasing it — the box was just wiped, so its
        // WS and observer loops are talking to a nest that no longer knows this
        // session, and ARC-nulling `client` reclaims none of them. The macOS twin
        // has always done this; iOS's omission was more of the same drift.
        // (No `unprovisionSyncAgent()` counterpart here — iOS runs no sync agent.)
        // Through `liveFaunaClient`, not the raw `client` — handleTestCommand-
        // reachable, the row 376 bug class.
        await liveFaunaClient?.shutdown()
        appState.session.clearAuthenticatedOverride()
        appState.inAdmin = false
        self.client = nil
        appState.liveClient = nil
        // The box was wiped, so every in-memory surface built against it is stale —
        // including the two cadences, which this path used to leave running. See the
        // macOS twin for why that was a production defect and not merely untidy
        // (`DnsAutoRenewCadence.start` is latched, so the re-claim's own `start`
        // silently no-ops and the cadence stays bound to the pre-reset `APIClient`).
        dropActorScopedState()

        // Re-seed the wizard at claim-code with the returned code pre-filled
        // (`claim_code_prefill()` feeds the input; the human never types it).
        onboardingVM.machine.reset()
        onboardingVM.machine.seedIdentity(secret: secretHex)
        onboardingVM.machine.navigateToClaimCodeForKnownNestWithCode(
            nestUrl: nestUrl, handle: handle, code: claimCode)

        appState.isOnboarding = true
    }

    /// The `launch-fallthrough-button` escape ("Use a different nest"), shared by BOTH
    /// blocking launch surfaces: the non-retry needs-update one (version-compatibility.md
    /// Dim 4 — the nest is outdated / the account is terminal) and
    /// `launch_identity_changed` (security.md — the nest's pinned identity
    /// changed). Neither offers a retry, so this is the escape.
    ///
    /// Drops the saved (nodeUrl, deviceId), KEEPS the identity, and re-seeds the wizard at
    /// handle_entry so the user can point at a different nest. Clears both overlay
    /// triggers so `ContentView` leaves the surface. Mirrors `FaunaMacApp.useADifferentNest`.
    ///
    /// Note it does NOT forget the TOFU pin: walking away from a nest whose identity you do
    /// not trust must not un-pin it. Only the explicit `trustNestIdentity()` does that.
    @MainActor
    private func useADifferentNest() {
        let keychain = KeychainStore()
        appState.needsUpdateMessage = nil
        appState.signInRefusedMessage = nil
        appState.identityChanged = nil
        let material = FaunaAccounts.sessionMaterial(keychain: keychain)
        guard let secret = appState.session.secretHex ?? material?.secretHex else {
            // No identity to re-seed — fall through to the wizard root.
            appState.session.clearAuthenticatedOverride()
            appState.isOnboarding = true
            return
        }
        // Drop the stale (nodeUrl, deviceId) so the user picks a fresh nest;
        // identity stays, so the wizard resumes at handle_entry pre-populated.
        // Routed through the registry (the delete corollary,
        // `account-scoping.md` § Concurrent instances): `clearNestBinding`
        // clears the account's per-actor slots. Mirrors
        // `FaunaMacApp.useADifferentNest`.
        if let active = FaunaAccounts.registry(keychain: keychain).active() {
            do {
                try FaunaAccounts.registry(keychain: keychain).clearNestBinding(actorId: active)
            } catch {
                logMessage(level: .error, target: "fauna.app",
                           message: "[launch] use-a-different-nest: clearNestBinding failed: \(error)")
            }
        }
        // Walking away from the saved nest: its sets' FP domains + the capability
        // minted against it must not linger (the bearer/BackupKey are nest-scoped;
        // mirrors FaunaMacApp.useADifferentNest).
        Task { await FileProviderCoordinator.signOut() }
        appState.session.clearAuthenticatedOverride()
        self.client = nil
        appState.liveClient = nil
        onboardingVM.machine.reset()
        onboardingVM.machine.seedIdentity(secret: secret)
        if let cachedHandle = material?.handle, !cachedHandle.isEmpty {
            onboardingVM.machine.setCurrentHandle(h: cachedHandle)
        }
        appState.isOnboarding = true
    }
}
