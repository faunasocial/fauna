import SwiftUI
import SwiftData
import FaunaDeepLink
import FaunaKit

/// The macOS app root. `public` (not `@main`): the entry point is the thin
/// `FaunaMacOSMain` shell in `Fauna-macOS-Main/`, compiled by both the SPM
/// executable and the `.xcodeproj` app target, which calls `FaunaMacApp.main()`.
public struct FaunaMacApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate

    /// The photo-backup dedup/upload-state store, actor-scoped
    /// (`account-scoping.md` § The scoping taxonomy, class 4 — re-derivable,
    /// wipe-not-adopt). `@State`, not `let`: rebuilt for the resolved actor
    /// at `completeAuthenticatedLaunch` and reset to the flat/no-actor
    /// container at every session-teardown path (`tearDownSessionForSwitch`,
    /// `resetToFactory`), the same lifecycle `client` already has. The
    /// initial `init()` value is the flat container — pre-auth, no actor is
    /// known yet, matching `AccountStateDir`'s own no-actor fallback.
    @State private var modelContainer: ModelContainer
    /// **The live photo-backup container, resolved the ONE way every
    /// callback-reached site must resolve it** — `MacAppState.liveModelContainer`
    /// carries the why, and it is `liveFaunaClient`'s below in miniature: the
    /// observed slot first, the `@State` only as the pre-auth fallback. Every
    /// read goes through here; only the two build sites touch the raw pair.
    private var liveModelContainer: ModelContainer { appState.liveModelContainer ?? modelContainer }

    @State private var appState = MacAppState()
    @State private var client: FaunaClient?
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
    /// The session's sync-agent provisioner + agent event listener live on
    /// `appState`, NOT in `@State` here — `MacAppState.syncAgentProvisioner`
    /// carries the reasoning, which is `liveFaunaClient`'s above taken to its
    /// conclusion: both of their lifecycle ends are macOS-app-specific (the
    /// spawner does launchctl / `.dmg` self-install; all four session-teardown
    /// paths live in this file and must `unprovision()`), and two of those ends
    /// are reached only through the captured `self` a `@State` cannot serve.
    /// The shared `LaunchMachine` for this launch, held so the
    /// `launch_identity_changed` trust button can call `trustNestIdentity()` on the
    /// instance that produced the verdict (it reads the secret + nest_url off that
    /// machine's own `IdentityChanged` state — a fresh one re-challenges from `Boot`
    /// and the button would silently no-op). The linux `machine_holder` analogue.
    ///
    /// A **reference-type box** (`LaunchMachineBox`), not a bare `@State
    /// LaunchMachine?` — see that type's doc comment for why: the post-auth
    /// escalation re-enters `runLaunch()` through the test bridge's
    /// init-time-captured `self`, where a plain `@State` assignment silently
    /// no-ops and leaves the live view holding the *boot* machine, which turns
    /// this button into a no-op instead of a missing one.
    @State private var launchMachineBox = LaunchMachineBox()
    /// The launch binding (`boundActorId`) and the (OS login, account)
    /// single-instance lock pair (`instanceLock`, `instanceLockActorId`) live
    /// on `MacAppState`, NOT in `@State` here — `MacAppState.boundActorId`
    /// carries the reasoning, which is `syncAgentProvisioner`'s: every site
    /// that touches the three is an app-lifecycle callback site, no view
    /// renders any of them, and five of the eleven are reached ONLY through
    /// `handleTestCommand`'s init-time-captured `self`, where a `@State` read
    /// answers the nil it was born with and a `@State` write is dropped.
    @State private var photoBackupEngine = PhotoBackupEngine()
    @State private var onboardingVM = OnboardingVM()
    /// One shared `ConversationsVM` (thin observer over `fauna-conversations`'s
    /// `ConversationsManager`) for the unified conversations page. Held here
    /// (a class reference, like `onboardingVM`) so the TestAgent's
    /// conversations commands can reach the same manager the views render off.
    @State private var conversationsVM = ConversationsVM()
    /// One shared `FeedVM` for the feed page, held here (like `conversationsVM`)
    /// so the TestAgent's `feed_inject_posts` command injects into the SAME
    /// manager `FeedSplitView` renders off. `FfiNestClient.feed_manager` builds a
    /// *fresh* manager per call, so a handler that called `api.feedManager` again
    /// would seed a throwaway the view never observes (the injected posts would
    /// reach `lastLoadedPosts` — masking the bug in the state read — but never the
    /// rendered cards). Sharing one app-level VM is the conversations pattern.
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
    /// One shared `ProfileEditVM` for the SELF profile edit form, held here
    /// (like `feedVM`/`conversationsVM`) so the TestAgent's `compose.file`
    /// avatar/banner staging (no OS file-chooser can be driven headlessly)
    /// reaches the SAME instance `ProfileEditFormView` renders off — `ProfileView`
    /// reads it via `@Environment` instead of a page-local `@State` for exactly
    /// this reason.
    @State private var profileEditVM = ProfileEditVM()
    /// Device-local folder ↔ folder bindings for the Sync settings page.
    /// Held here so the TestAgent's `sync_inject_locations` command can seed the
    /// same model `SyncSettingsView` renders (mirrors `conversationsVM`).
    @State private var locationsModel = LocationsModel()
    /// The global `sync-agent-status` shell indicator's poll state
    /// (`sync-agent.md` § Local agent health). Held beside `locationsModel`
    /// (same environment-injection pattern) and started/stopped alongside the
    /// provisioner — see the post-auth `Task` and `unprovisionSyncAgent()`.
    /// Not gated on e2e itself: it simply never gets a channel to poll there
    /// (the provisioner build is what e2e skips), so it renders "Not running"
    /// like a real box with no agent.
    @State private var syncAgentHealthModel = SyncAgentHealthModel()
    /// Process-wide critical-alerts registry owner (`critical-alerts.md`
    /// § Mechanism), held here — like `feedVM`/`conversationsVM` — so the
    /// `CriticalAlertsBanner` and the session-start sweep loop share the SAME
    /// instance across a `FaunaClient` rebuild (account switch).
    @State private var criticalAlertsHost = CriticalAlertsHost()
    /// Stash for the most recent value-returning `call_machine_method`
    /// dispatch (e.g. `provisioning_snapshot`, `provider_base_url`), read
    /// back by the E2E bridge via `machine_method_result` in
    /// `serializeState()`. `nil` after a setter-only call. Mirrors linux's
    /// `machine_method_result` field / android's `TestAgent.machineMethodResult`.
    /// A **reference-type box** (`MachineMethodResultBox`), not a plain
    /// `@State` value — see the type's own doc comment for why a bare `@State
    /// private var machineMethodResult: Any?` silently drops every write made
    /// through `startInProcessAgentIfNeeded()`'s init-time-captured `self`.
    @State private var machineMethodResultBox = MachineMethodResultBox()

    public init() {
        // Install the in-process `fauna_log` ring + daily-rolling file +
        // os_log-via-stderr (observability.md § Shared capture) FIRST, so startup
        // events are captured. The ring drives the Settings → Logs sub-page
        // (`LogsView`). Mirrors linux's `install_logging()` at `main()` start.
        let logDir = (try? FileManager.default.url(
            for: .applicationSupportDirectory, in: .userDomainMask,
            appropriateFor: nil, create: true))?.path ?? NSTemporaryDirectory()
        installLogging(dataDir: logDir)

        // `SyncFile` + `SyncAnchor` retired with the B2 engine cutover. Both were
        // pure *derived projections* of nest state — `SyncFile` mirrored the
        // `fauna.sync.files` / changes feed, `SyncAnchor` was that feed's cursor —
        // so dropping them destroys nothing a user cannot recreate: the shared
        // engine re-derives per-file state into its own per-set `SyncDb` on the next
        // pass, and the nest remains the source of truth for both. (The no-data-loss
        // invariant's recreatable-table carve-out; `PhotoBackupRecord` is NOT such a
        // table — it maps OS asset identity and stays. `CachedAccount` and `Snapshot`
        // were removed as dead code, 2026-08-10: zero read/write sites beyond their
        // own `@Model` declaration and this schema registration.)
        _modelContainer = State(initialValue: PhotoBackupRecord.buildModelContainer(actorIdHex: nil))
        // In E2E mode, clear persisted state so the app starts from onboarding.
        // Without this, keychain credentials from previous test sessions cause
        // the app to go straight to MainWindowView — which can render a crashing
        // page (backups/devices/settings) and crash before the test agent starts.
        // The wizard owns no persistence post-cleanup (design tracked internally),
        // so the registry's `clearAll` (every account + the index) is sufficient
        // to return the wizard to identity_choice on the next launch.
        // E2E mode = either the XCUITest bridge (FAUNA_E2E_BRIDGE) or the new
        // in-process automation server (FAUNA_E2E_AGENT_PORT) — both want the app
        // to start from a clean onboarding state.
        let isE2ELaunch = FaunaE2E.isActive
        if isE2ELaunch {
            // Skip the wipe when the harness owns the store's lifecycle (it passed a
            // `FAUNA_E2E_CREDENTIAL_DIR`). A fresh dir per launch already IS the clean
            // start this wipe exists to give; a PINNED dir is a deliberate
            // `preserve_state_across_relaunch()`, and wiping there would destroy exactly
            // the state the crash-recovery journeys assert survives a crash (gaps CR-1 /
            // CR-2 — the pending-factory-reset slot). Without a dir the store is a
            // process-static dict, so there is nothing to preserve and the wipe still
            // earns its keep.
            if !KeychainStore.e2eHarnessOwnsStore {
                _ = FaunaAccounts.registry(keychain: KeychainStore()).clearAll()
            }
            appState.session.clearAuthenticatedOverride()
            appState.isOnboarded = false

            // Clear persisted NSSplitView frames. macOS saves these in UserDefaults
            // and restores them on launch. If the view hierarchy changed (e.g.
            // NavigationSplitView → HSplitView), stale frames cause EXC_BREAKPOINT
            // in ___NSViewLayout_block_invoke when XCUITest probes the accessibility
            // tree. This is the #1 cause of "app crashes under XCUITest but not
            // from terminal" — terminal launch swallows the exception, XCUITest doesn't.
            let defaults = UserDefaults.standard
            for key in defaults.dictionaryRepresentation().keys {
                if key.hasPrefix("NSSplitView") || key.hasPrefix("NSWindow Frame") {
                    defaults.removeObject(forKey: key)
                }
            }
        }

        // Install the disk-backed nest-identity pin store before the first
        // authenticated connect, so TOFU pins (self-signed / LAN nests) survive
        // restarts (security.md § Transport trust). Mirrors linux startup.
        NestTrust.installPinStore()

        // Lend the Keychain to shared Rust's credential slots. Inert on macOS
        // (the Rust crate's own login-Keychain arm is the slot the sync agent
        // shares, and it outranks the lent store); called here so the shared
        // FaunaKit startup stays one code path with iOS, where it is
        // load-bearing (see the method). Mirrors iOS + android.
        FaunaAccounts.installPlatformCredentialStore()

        // Start test agent in init() so it runs before XCUITest's "idle" wait.
        // If started in a .task modifier, the task may never fire because
        // XCUITest considers the app "not idle" during SwiftUI state restoration.
        // Compiled out of release artifacts (testing.md convention 15).
        #if DEBUG
        TestAgentBootstrap.startTestAgentIfNeeded(
            stateProvider: { [self] in self.serializeState() },
            commandHandler: { [self] command in await self.handleTestCommand(command) }
        )
        TestAgentBootstrap.startInProcessAgentIfNeeded(
            stateProvider: { [self] in self.serializeState() },
            commandHandler: { [self] command in await self.handleTestCommand(command) }
        )
        #endif
    }

    public var body: some Scene {
        WindowGroup(id: "main") {
            ContentView(
                onboardingVM: onboardingVM,
                // Retry re-runs the WHOLE launch over a fresh machine rather than
                // calling `retrySilentChallenge()`. Both are valid from
                // `Offline{transient:true}`, but a full re-run also re-reads the store
                // and re-runs the CR-2 reconcile, so a retry can't be stranded by state
                // that changed while the retry surface was up.
                onRetryLaunch: { Task { @MainActor in runLaunch() } },
                loadLaunchRecoverableBoxes: { await loadLaunchRecoverableBoxes() },
                onRecoverFromLaunch: { boxes in Task { @MainActor in recoverFromLaunch(boxes: boxes) } },
                onUseDifferentNest: { Task { @MainActor in useADifferentNest() } },
                onRetrySignIn: { Task { @MainActor in retrySignIn() } },
                onTrustNestIdentity: { Task { @MainActor in trustNestIdentity() } },
                onAccountIndexStartOver: { Task { @MainActor in revealAccountIndexStartOver() } },
                onAccountIndexConfirmStartOver: { Task { @MainActor in confirmAccountIndexStartOver() } }
            )
                .environment(appState)
                // The shared FaunaKit `ThreadDetailView` reads
                // `@Environment(ContentPolicyStore.self)` to gate a flagged
                // bubble (family-safety.md § Content policy); the macOS feed card
                // reads the same store off `MacAppState` directly.
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
                .environment(appDelegate.menuBarController)
                // Inject the FaunaClient every `@Environment(FaunaClient.self)`
                // view (admin shell, mail-settings, …) reads.
                //
                // **The observed slot is asked FIRST, and the `@State` is only the
                // fallback.** Every path that builds a client writes both, so in
                // the ordinary case they are the same object and the order cannot
                // matter. It matters in the one case where they differ: a `@State`
                // write from a callback (the test agent's `applySessionPatch`)
                // does not reach this closure, while `appState` is `@Observable`
                // and does — so `client` can hold an object this app has already
                // replaced. The old order (`client ?? appState.liveClient`) was
                // written when `client` could only be *nil* here, and a real
                // in-process account switch broke that premise: `completeAuthenticatedLaunch`
                // fills `client` with the successor's session, and from then on the
                // `??` served that stale object to every view for the rest of the
                // process — the agent's own state reads (which go straight to
                // `appState.liveClient`) reported the new actor while the views
                // read the old one, so a later login looked applied and every
                // owner-scoped surface answered for the previous identity
                // (a fresh actor's Recovery Kit section
                // reported the predecessor's registered kit and refused `create`).
                .environment(liveFaunaClient)
                .environment(conversationsVM)
                .environment(feedVM)
                .environment(eventsVM)
                .environment(devicesVM)
                .environment(profileEditVM)
                .environment(locationsModel)
                .environment(syncAgentHealthModel)
                .environment(criticalAlertsHost)
                // The shared FaunaKit `PhotoBackupControlsView` (the Photo Backup
                // Settings rail sub-page) reads `@Environment(PhotoBackupEngine.self)`.
                .environment(photoBackupEngine)
                .modelContainer(liveModelContainer)
                .frame(minWidth: 800, minHeight: 500)
                .task {
                    // An `--autostart` login launch holds this window out of
                    // sight until the launch settles (`AutoStartWindow`).
                    AutoStartWindow.armIfAutostartLaunch()
                    runLaunch()
                }
                // A mid-session supersession, suspension or nest-identity change
                // the connection supervisor stopped on → the launch surface
                // (`escalateSessionEnding`; a supersession this device's own
                // ceremony caused is held back until the user leaves Account).
                .onSessionEnding { verdict in
                    await escalateSessionEnding(verdict) { await escalateToLaunchSurface() }
                }
                // `fauna://` deep links — the same-device handoff's consent route
                // (`ConsentHandoff`) and the FP context actions (Finder
                // right-click → Share / Version history on a Fauna-domain item;
                // `FaunaDeepLink`). Identity/peer URI forms are pasted into their
                // own onboarding surfaces, never OS-opened, so any other link is
                // ignored here. A consent route held while signed out applies on
                // the authenticated flip.
                .onOpenURL { url in handleDeepLink(url) }
                .onChange(of: appState.isOnboarded) { _, onboarded in
                    if onboarded { applyHeldRoute() }
                }
                .onAppear {
                    appState.isMainWindowFocused = true
                    appDelegate.appState = appState
                    // The conversations/feed drafts rails' leave-flush
                    // reachability (`reserved-folders.md` § The leave-flush
                    // promise): both VMs are `@State` here on the App, so the
                    // quit-time `applicationShouldTerminate` gate can only
                    // reach them through `appState`. Published beside the
                    // delegate's own `appState` handoff, for one shape.
                    appState.conversationsVM = conversationsVM
                    appState.feedVM = feedVM
                    // Wire the admin-nest Factory Reset re-onboard hook. Set here
                    // (not threaded through the view tree) so the shared FaunaKit
                    // `AdminNestView` stays platform-agnostic — it just calls the
                    // closure with the post-reset claim code.
                    appState.onFactoryReset = { claimCode in
                        Task { @MainActor in await factoryResetReonboard(claimCode: claimCode) }
                    }
                    // Post-onboarding launch hook: the fresh-onboarding `.feed`
                    // exit calls this to build the authed FaunaClient via the same
                    // launch gate a returning user hits (the identity is already
                    // recorded in the registry by `performLoggedInHandoff`). Set
                    // here for the same reason as `onFactoryReset`.
                    // The wizard's `LoggedIn` exit means two different things now. First
                    // identity → boot the session (`runLaunch()`). **Appended** identity →
                    // register it in the registry and switch to it; the append wrote
                    // nothing to the store, so a `runLaunch()` here would boot the
                    // still-active old account and drop the identity the user just added
                    // (see `completeAppendedAccount`).
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
                    // `LoggedIn` home nest a pending identity does not have yet.)
                    onboardingVM.onPendingInvitePersisted = { actorId in
                        guard appState.isAddingAccount else { return }
                        appState.isAddingAccount = false
                        Task { @MainActor in try? await switchAccount(to: actorId, confirmed: false) }
                    }
                    // Account switcher (Account settings). Set here, not threaded through the
                    // view tree, so the shared FaunaKit `AccountSwitcherSection` stays
                    // platform-agnostic — same seam as `onFactoryReset`.
                    appState.onSwitchAccount = { actorId, confirmed in
                        try await switchAccount(to: actorId, confirmed: confirmed)
                    }
                    appState.onAddAccount = {
                        Task { @MainActor in beginAddAccount() }
                    }
                    // Sign-out / delete-account (`SignOutSection`, `AccountSettingsView`'s
                    // `onAccountReset`). Set here for the same reason as `onFactoryReset` —
                    // and, unlike those two call sites' own `{ appState.isOnboarded = false }`
                    // closures, this one is NOT cosmetic: without it the outgoing session's
                    // `client`/`syncAgentProvisioner`/DNS+subscription cadences kept running
                    // in memory (and a real sync agent kept syncing under the old identity)
                    // after a real Sign Out tap — only the test-only `logoutKeepData()` /
                    // `resetToFactory()` / the live `switchAccount()` ever ran this checklist.
                    appState.onSignOut = {
                        // Synchronous, before `isOnboarded = false` (this closure's
                        // caller flips it right after this call returns): the residue
                        // must already be on `onboardingVM` by the time the wizard
                        // mounts, and nothing between here and there triggers a
                        // machine transition that would clear it
                        // (`OnboardingVM.signOutResidue`'s doc).
                        onboardingVM.takeSignOutResidue(from: appState.session)
                        Task { @MainActor in
                            await tearDownSessionForSwitch()
                            if !FaunaE2E.isActive {
                                await FileProviderCoordinator.signOut()
                            }
                        }
                    }
                }
                // Append-mode onboarding. A sheet over the LIVE session (the client keeps
                // running underneath) — the apple analogue of linux's separate wizard window.
                // Dismissing it just closes the sheet: it must never quit the app or touch
                // the running session, and the identity a cancelled wizard stranded in the
                // registry is read by the next launch like any other.
                .sheet(isPresented: $appState.isAddingAccount) {
                    OnboardingContainerView(vm: onboardingVM)
                        .environment(appState)
                        .frame(minWidth: 700, minHeight: 500)
                }
                .onDisappear { appState.isMainWindowFocused = false }
        }
        // ⚠ Do NOT shrink this back to 1100x700 without re-measuring. The
        // densest page (Backups: a 3-pane `HSplitView` above a fixed 420pt
        // destinations/restore section) lays out at ~1146pt wide, and
        // `HSplitView` does NOT compress below the sum of its panes' intrinsic
        // widths — it silently OVERHANGS the window instead, without ever
        // reporting the larger minimum up to SwiftUI (so `.windowResizability(
        // .contentSize)` never widens the window either). At 1100 that parked
        // `snapshot-file-download-button` ~15pt past the right edge, where no
        // user could click it and the automation registry reported it
        // `HIDDEN(geo-parked)` — the real defect behind three failed
        // pane-level fix attempts (per-row Text width, capping snapshotContent,
        // and a layoutPriority pass), each of which only redistributed width
        // WITHIN a total that stayed pinned at 1146.
        .defaultSize(width: 1280, height: 800)
        .commands {
            CommandGroup(replacing: .newItem) {
                Button(L.conversations.list.newConversation) {
                    appState.selectedSidebar = .conversations
                    conversationsVM.startNewConversation()
                }
                .keyboardShortcut("n")
            }

            CommandGroup(after: .appInfo) {
                CheckForUpdatesMenuItem(updates: appState.updates)
            }

            CommandGroup(replacing: .appSettings) {
                Button("\(L.common.settings)…") {
                    appState.selectedSidebar = .settings
                }
                .keyboardShortcut(",")
            }

            CommandGroup(after: .toolbar) {
                Button(L.navigation.quickSwitcher) {
                    appState.showingQuickSwitcher = true
                }
                .keyboardShortcut("k")
            }

            SidebarCommands()
        }

        Window("Welcome to Fauna", id: "onboarding") {
            OnboardingContainerView(vm: onboardingVM)
                .environment(appState)
                .modelContainer(liveModelContainer)
                .frame(width: 500, height: 400)
        }
        .windowResizability(.contentSize)

        // Settings is in the sidebar (SidebarItem.settings), not a separate window.
        // Cmd+, shortcut navigates there via the keyboard shortcut below.
    }

    /// Drive the launch through the **shared** `LaunchMachine` and render its verdict.
    ///
    /// Every routing decision lives in `libs/fauna-launch-machine` and is shared with
    /// linux / web / windows / android / tui: the four-case hydration table, the
    /// pending-factory-reset **boot reconcile** (gap CR-2), the silent challenge and
    /// its claimed/unclaimed fallback table, and the nest-identity-pin (TOFU) check.
    /// This method only *renders* the resulting `LaunchPhase`.
    ///
    /// Apple used to hand-roll that whole table here in Swift, mirroring the machine's
    /// precedence *by convention* rather than calling it — which is exactly how it
    /// missed two things the other five apps got for free: CR-2's reconcile (a stale
    /// pending-factory-reset slot trapped a healthy, still-claimed box on a claim page
    /// on **every** launch, with no in-app exit — `common.md` § Client-state
    /// recoverability) and `LaunchPhase::IdentityChanged` (a changed nest identity fell
    /// into the catch-all and rendered a **Retry CTA on a possible-MITM signal**, which
    /// `security.md` forbids). Do not re-derive either one here; grow the
    /// machine instead.
    @MainActor
    private func runLaunch() {
        let keychain = KeychainStore()

        // Bring credential rows in line with the iCloud-backup preference (default:
        // device-bound) — a self-healing re-converge of any row a crash mid-toggle left
        // behind; idempotent. apps/ios.md § Credential Storage.
        keychain.reconcileCredentialAccessibility()

        appState.launchGate = .launching
        // Claimed before the first `await` below, so a session patch landing while
        // the binding resolves retires this launch (`LaunchMachineBox.beginLaunch`).
        let launch = launchMachineBox.beginLaunch()

        Task { @MainActor in
            // Primary or bound? (`account-scoping.md` § Concurrent instances.)
            // Primary resolves the active account inside `resolveLaunchBinding`; a
            // bound launch resolves the named account's slots, never consults
            // or moves `active`. A
            // refused binding is terminal: falling back to a plain launch would
            // put a second window on the ACTIVE account and walk past a flagged
            // account's re-auth.
            switch await FaunaAccounts.resolveLaunchBinding(keychain: keychain) {
            case .primary(let persistence):
                appState.boundActorId = nil
                await runLaunchMachine(launch: launch, persistence: persistence, keychain: keychain)
            case .bound(let actorId, let persistence):
                appState.boundActorId = actorId
                await runLaunchMachine(launch: launch, persistence: persistence, keychain: keychain)
            case .refused(let actorId, let reason):
                refuseLaunch(actorId: actorId, reason: reason)
            }
        }
    }

    /// Start the shared `LaunchMachine` over the given (active-or-bound)
    /// persistence and render its verdict.
    @MainActor
    private func runLaunchMachine(
        launch: UInt64, persistence: LaunchPersistence, keychain: KeychainStore
    ) async {
        // Superseded while the binding resolved — a session patch or a teardown
        // cleared the box before there was a machine in it to retire.
        guard launchMachineBox.isCurrent(launch: launch) else {
            logMessage(
                level: .info, target: "fauna.app",
                message: "[launch] superseded before its machine was built — dropping it")
            return
        }
        let machine = LaunchMachine(observer: NullLaunchObserver(), persistence: persistence)
        // Hold it. `trustNestIdentity()` reads the secret + nest_url off the machine's
        // OWN `IdentityChanged` state, so the trust button must call the instance that
        // produced the verdict — a fresh machine re-challenges from `Boot`, never
        // reaches the forget-the-pin branch, and the button silently does nothing.
        // (The trap that cost linux an extra step; it needed the same holder.)
        launchMachineBox.machine = machine
        await machine.start()
        // A verdict renders only while its own launch is still the held one
        // (`LaunchMachineBox.machine`'s doc owns the rule). `start()` decided from a
        // store read taken before this suspension; anything that superseded the
        // session while it ran — a re-entered launch, a teardown, the e2e session
        // patch — cleared or replaced the box, and this snapshot is answering a
        // question nobody is asking any more.
        guard launchMachineBox.machine === machine else {
            logMessage(
                level: .info, target: "fauna.app",
                message: "[launch] verdict \(machine.snapshot().phase.diagnosticName) "
                       + "superseded before it rendered — dropping it")
            return
        }
        await dispatchLaunch(machine.snapshot(), keychain: keychain)
    }

    /// Terminal refusal of a launch (`account-scoping.md` § Concurrent
    /// instances): the instance must NOT run as `actorId` — because the bind
    /// gate refused it, or because another live instance already serves it
    /// (the single-instance guard) — and must not fall back onto a different
    /// account either (for a bound launch, that fallback would put a second
    /// window on the ACTIVE account and walk past a flagged account's
    /// re-auth).
    @MainActor
    private func refuseLaunch(actorId: String, reason: String) {
        logMessage(
            level: .error, target: "fauna.app",
            message: "[launch-refused] for \(actorId): \(reason) — exiting")
        exit(1)
    }

    /// Project one `LaunchSnapshot` onto the macOS launch gate. The machine has already
    /// decided; nothing here re-classifies an error or re-reads a routing input.
    @MainActor
    private func dispatchLaunch(_ snap: LaunchSnapshot, keychain: KeychainStore) async {
        switch snap.phase {
        case .online:
            await completeAuthenticatedLaunch(keychain: keychain)

        case .wizardAt(let entry):
            // A secondary instance never enters the onboarding wizard
            // (`account-scoping.md` § Concurrent instances): the wizard reads
            // and writes the shared onboarding scratchpad, which belongs to the primary.
            // An account that needs re-onboarding is the primary's to fix —
            // refuse under the same terminal contract as a refused binding.
            if let bound = appState.boundActorId {
                refuseLaunch(
                    actorId: bound,
                    reason: "launch routed to wizard entry \(entry) — onboarding belongs to the primary instance")
                return
            }
            // `sync-agent.md` § Credential model → *The signed-out reconcile*,
            // shape (a): this instance has no account and is
            // about to render onboarding, so nudge a reachable agent still
            // serving THIS machine's just-signed-out account to drop its
            // capability now rather than wait on its own renewal-loop
            // cadence. Never reached from an append launch — `beginAddAccount`
            // is a separate sheet, not routed through `dispatchLaunch` — so no
            // extra gate is needed here the way linux's shared wizard builder
            // needs one.
            Task { await signedOutOnboardingReconcile() }
            seedWizard(at: entry, keychain: keychain)

        case .identityChanged(let pinnedHex, let seenHex):
            // BLOCKING: no auto-entry, and no retry CTA — a retry cannot change the
            // verdict and must never silently re-pin (security.md). The
            // bearer the machine minted is already dropped.
            appState.isOnboarded = false
            appState.launchGate = .identityChanged(pinnedHex: pinnedHex, seenHex: seenHex)

        case .offline(let transient):
            appState.isOnboarded = false
            // The saved account index is present and unusable
            // (`onboarding.md` § App-launch routing — the row checked before
            // every other). Checked BEFORE the transient/non-transient split
            // below, for the same reason tui's `route()` checks it before
            // both `Offline` arms: the machine deliberately projects this to
            // `Offline { transient: false }` and carries the verdict on a
            // side channel, so an app that ignores the channel still falls
            // into `.needsUpdate` — whose "use a different nest" CTA
            // misstates the problem, since the nest is fine.
            if let refusal = snap.accountIndexRefusal {
                appState.launchGate = .accountIndexUnreadable(refusal: refusal, confirming: false)
            } else if let claimed = snap.supersededSuccessor {
                // The identity was succeeded: route to the import flow
                // (`SupersededLaunchRoute` owns the why). Also ahead of the
                // generic split, for the same side-channel reason as the
                // account-index row above. That flow is onboarding, which
                // belongs to the primary instance — so a bound launch refuses
                // under the same contract as the `.wizardAt` arm.
                if let bound = appState.boundActorId {
                    refuseLaunch(
                        actorId: bound,
                        reason: "launch refused as superseded — importing the successor belongs to the primary instance")
                    return
                }
                SupersededLaunchRoute.route(
                    machine: onboardingVM.machine,
                    claimedSuccessor: claimed,
                    keychain: keychain,
                    // A held verified successor: an ordinary switch to it, as
                    // the ceremony's `onSucceeded` ends in — no re-auth prompt
                    // ran, so `confirmed: false`.
                    adopt: { successor in try? await switchAccount(to: successor, confirmed: false) })
                appState.launchGate = .ready
            } else if snap.signInRefused {
                // A nest this app signed in to before refused the identity
                // (`onboarding.md` § App-launch routing — the previously-signed-in
                // row). Ahead of the generic split for the side-channel reason
                // above: left to `.needsUpdate` it paints "update your nest" with
                // no way back in. `launch_sign_in_refused`, WITH Retry.
                logMessage(
                    level: .error, target: "fauna.app",
                    message: "[launch] the saved nest no longer signs this identity in: \(snap.lastError ?? "")")
                appState.launchGate = .signInRefused(message: snap.lastError ?? "")
            } else {
                appState.launchGate = transient
                    ? .retrying(error: snap.lastError ?? "")
                    : .needsUpdate(message: snap.lastError ?? "")
            }

        case .boot, .hydrating, .silentChallenge, .refreshing:
            // Unreachable once `start()` has returned — it always lands on a terminal
            // phase. Surface it as transient rather than hanging on the spinner forever
            // (the same defensive choice windows makes).
            appState.isOnboarded = false
            appState.launchGate = .retrying(error: snap.lastError ?? "")
        }
    }

    /// The launch-retry surface's box read (`box-recovery.md` § Recovery UI,
    /// surviving-device entry): the served account's custodied boxes, off the saved
    /// nest when it answers and off the device's own store when it does not — so a
    /// dead saved nest still offers recovery. Empty with no served account.
    @MainActor
    private func loadLaunchRecoverableBoxes() async -> [String] {
        guard let material = FaunaAccounts.sessionMaterial() else { return [] }
        return await RecoverableBoxes.load(
            nestUrl: material.nestUrl ?? "", ownerSecret: hex_to_data(material.secretHex))
    }

    /// `launch-recover-button`: seed the wizard straight into `nest_recovery` with
    /// the already-read box list (the shared machine entry, as tui/linux/windows).
    @MainActor
    private func recoverFromLaunch(boxes: [String]) {
        guard let secret = FaunaAccounts.sessionMaterial()?.secretHex else { return }
        onboardingVM.machine.reset()
        onboardingVM.machine.seedIdentityForRecovery(secret: secret)
        onboardingVM.machine.setRecoveryBoxes(boxes: boxes)
        appState.isOnboarded = false
        appState.launchGate = .ready
    }

    /// Seed the onboarding wizard for the entry **the machine chose**, then show it.
    ///
    /// The machine decides *which* entry; the long-term-store slots only supply what
    /// that page pre-fills with. Note the direction of trust: we re-read the store here
    /// only to hydrate, never to re-decide — an `if slot != nil → claim page` that does
    /// not consult the machine's verdict is precisely the CR-2 bug (web had it too).
    @MainActor
    private func seedWizard(at entry: LaunchWizardEntry, keychain: KeychainStore) {
        // The served account's session material — the registry's one session-identity
        // read; the resume slots come through the SAME `LaunchPersistence` the machine
        // branched on. Mirrors `FaunaApp.seedWizard`.
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
                // The machine's other road to this entry: the silent challenge reported
                // the secret isn't registered on the saved nest (and the box answered
                // "claimed", or didn't answer at all — the safe default). Land on
                // invite_request pre-filled rather than making the user re-type a handle
                // they have already used.
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
            // Factory-reset resume (gap CR-1). The admin dispatched a reset and this
            // client died before the re-claim completed — possibly before the reply
            // carrying the claim code ever rendered. The code survives ONLY because it
            // was minted and persisted before dispatch, so seed the claim page from the
            // slot: the same surface `factoryResetReonboard` lands on, but pre-filled
            // from disk so it survives a crash.
            //
            // Re-read the slot AFTER `start()` — the machine's CR-2 boot reconcile
            // probes the box and DELETES a stale slot (one left by a permanently-failed
            // dispatch against a box that is still claimed and healthy). Reaching this
            // arm means the probe said *unclaimed* (or the box was unreachable, where
            // the slot is deliberately kept), so the row should still be there; if it
            // is not, the store changed underneath us — fall back to the wizard rather
            // than open a claim page with no code.
            //
            // Through the SAME `LaunchPersistence` the machine branched on (matching
            // `.awaitingManualDns` below).
            guard let pending = persistence.loadPendingFactoryReset() else {
                logMessage(
                    level: .error, target: "fauna.app",
                    message: "[launch] pendingFactoryReset row with no slot; falling back to the wizard."
                )
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
                    message: "[launch] awaitingManualDns entry with no slot; falling back to the wizard."
                )
            }
        }

        appState.launchGate = .ready
        appState.isOnboarded = false
    }

    /// "Trust this nest" from the blocking `launch_identity_changed` surface
    /// (`nest-identity-changed-trust-button`). Forgets the TOFU pin through the
    /// machine's connector trust seam, re-TOFUs, and re-runs the silent challenge —
    /// then re-dispatches whatever verdict that produces (`Online` on success; back to
    /// `IdentityChanged` if the nest still can't prove the identity).
    ///
    /// Called on the HELD machine, never a fresh one: `trustNestIdentity()` reads the
    /// secret + nest_url off the `IdentityChanged` state itself.
    @MainActor
    private func trustNestIdentity() {
        guard let machine = launchMachineBox.machine else {
            // Not merely defensive: this button is only ever rendered while the
            // gate says `identityChanged`, so a missing holder means a launch
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
        appState.launchGate = .launching
        Task { @MainActor in
            await machine.trustNestIdentity()
            await dispatchLaunch(machine.snapshot(), keychain: KeychainStore())
        }
    }

    /// `launch-retry-button` on `launch_sign_in_refused`: re-run the silent
    /// challenge on the HELD machine that produced the verdict
    /// (`retry_silent_challenge`, valid from the sign-in-refused state), then
    /// re-dispatch its snapshot — into the app on success, the same page again if
    /// the nest still refuses (the admin's restore is what changes the answer).
    @MainActor
    private func retrySignIn() {
        guard let machine = launchMachineBox.machine else {
            // Only rendered while the gate says `signInRefused`, so a missing
            // holder means a launch installed its machine somewhere the live
            // view cannot see it.
            logMessage(level: .error, target: "fauna.app",
                       message: "[sign-in-refused] no held launch machine — Retry cannot act")
            return
        }
        appState.launchGate = .launching
        Task { @MainActor in
            await machine.retrySilentChallenge()
            await dispatchLaunch(machine.snapshot(), keychain: KeychainStore())
        }
    }

    /// "Start over on this device" from the malformed verdict's
    /// `account-index-reset-button`: reveal the confirm, which states the
    /// residual before anything is erased. A no-op on any other gate — the
    /// version verdict paints no such button, so this must never become a
    /// second way to reach the reset from it.
    @MainActor
    private func revealAccountIndexStartOver() {
        if case .accountIndexUnreadable(let refusal, _, _) = appState.launchGate {
            appState.launchGate = .accountIndexUnreadable(refusal: refusal, confirming: true)
        }
    }

    /// The confirm: the documented client-side floor
    /// (`long-term-store.md` § Cleanup contract), which is exactly the app's
    /// existing factory reset (`StatusVM.signOut(sessionState:)` — reused
    /// verbatim, never a second clearing path) — then land on fresh
    /// onboarding, the same "nothing to re-seed" landing
    /// `useADifferentNest()` uses. Guarded the same way as the reveal, plus
    /// on the confirm having actually been shown.
    ///
    /// ⚠ **Asked first** (`AccountIndexStartOver`): that erase sweeps the
    /// platform store root a tui or the sync agent on this Mac serves from, so
    /// while another instance serves an account it reaches, nothing runs — no
    /// keychain clear, no scope erase — and the line paints on this gate with
    /// the confirm still showing.
    @MainActor
    private func confirmAccountIndexStartOver() {
        guard case .accountIndexUnreadable(let refusal, true, _) = appState.launchGate else { return }
        appState.launchGate = .accountIndexUnreadable(refusal: refusal, confirming: true)
        Task { @MainActor in
            let refused = await AccountIndexStartOver.confirm(ownLock: appState.instanceLock) {
                await StatusVM().signOut(sessionState: appState.session)
            }
            if let refused {
                appState.launchGate = .accountIndexUnreadable(
                    refusal: refusal, confirming: true, error: refused)
                return
            }
            // The floor ran sign-out's erase, so it owes the same residue
            // statement on the wizard it lands on (`account-scoping.md`
            // § Erasure follows scope → *the residue surface*).
            onboardingVM.takeSignOutResidue(from: appState.session)
            appState.launchGate = .ready
            appState.isOnboarded = false
        }
    }

    #if DEBUG
    /// Unprovision + drop the previous e2e provisioner, if any, so at most one
    /// convergence loop is ever live. Awaited (not fire-and-forget) so the next
    /// `ProvisionCapability` cannot race the outgoing loop's last push.
    ///
    /// Reads `appState`, like every other site: the e2e command dispatch runs
    /// through an init-time-captured `self`, so a guard reading a `@State` var
    /// reassigned after init would see the nil it was born with and silently do
    /// nothing — exactly the failure it exists to prevent. This guard used to
    /// hold its own DEBUG-only static for that reason; `MacAppState` now serves
    /// it and the teardown read alike, so there is
    /// no second slot for the two to disagree about.
    @MainActor
    private func retireE2eSyncAgentProvisioner() async {
        guard let previous = appState.syncAgentProvisioner else { return }
        appState.syncAgentProvisioner = nil
        syncAgentHealthModel.stop()
        do {
            try await previous.unprovision()
        } catch {
            logMessage(level: .warn, target: "fauna.sync",
                       message: "[sync-agent] e2e provisioner retire failed: \(error)")
        }
    }
    #endif

    /// Build the sync-agent provisioner over `spawner` and start its convergence
    /// loop (`sync-agent.md` § Control plane split, milestone A4).
    ///
    /// Extracted so the **two** authenticated entry points share one construction
    /// rather than growing a second, drifting copy (priority #4): the production
    /// launch (`completeAuthenticatedLaunch`) and the e2e session patch
    /// (`applySessionPatch`, real-agent runs only — see its call site for why that
    /// path exists at all).
    @MainActor
    private func startSyncAgentProvisioner(
        faunaClient: FaunaClient, spawner: FfiAgentSpawner
    ) async {
        do {
            // Wire the reachability observer so the folder reconcile re-fires the
            // moment the just-spawned agent first answers (closing the
            // race where a bind beats the agent's socket — sync-agent.md § Control
            // plane split). Attach BEFORE start() so the model holds the control
            // channel before the loop can report a reachable edge.
            let provisioner = try await faunaClient.makeSyncAgentProvisioner(
                spawner: spawner,
                reachabilityObserver: SyncAgentReachabilityObserver(model: locationsModel))
            // One slot, reachable from the boot machine's live `self` AND from
            // the e2e session patch's captured one (`MacAppState`'s own note).
            appState.syncAgentProvisioner = provisioner
            // No content keys are pushed from here: the agent resolves every
            // set's keys from this account's custody itself and re-reads it at
            // its own edges (`on-demand-files.md` § Shared sets on a capability
            // host → *One mechanism*).
            locationsModel.attach(provisioner: provisioner)
            // Health polling doesn't depend on the convergence loop actually
            // converging — start it even if `start()` below throws, since a
            // reachable-but-unprovisioned agent is still a meaningful "Running"
            // to show the user.
            // The mass-delete floor's per-set hold rides the SAME 10 s tick
            // (`delete-propagation.md` § A wholesale-vanished folder
            // is infrastructure failure) — folded here, never in
            // `LocationsModel.reconcile()`, since no user gesture or
            // reachability edge ever produces it.
            let locationsModel = self.locationsModel
            syncAgentHealthModel.onEngineHoldsTick = { holds in
                await locationsModel.foldEngineHolds(holds)
            }
            // The binding park (D4 revocation's watch) rides the same tick.
            syncAgentHealthModel.onLocationsTick = { locations in
                await locationsModel.foldParks(locations)
            }
            syncAgentHealthModel.start(channel: provisioner)

            try await provisioner.start()

            // The peer-transfer plane rides this provisioner (its two agent verbs), so
            // it starts HERE — the one construction point both authenticated entry
            // points share (the production launch and the e2e session patch, which
            // skips `FaunaClient.start()`). Detached: shared Rust waits for the
            // account runtime's store and the conversations session (each lands on
            // its own task), and nothing after this should wait on that.
            #if !FAUNA_EXCISE_P2P_SHARE
            Task { await faunaClient.startSharePlane(provisioner: provisioner) }
            #endif
        } catch {
            logMessage(level: .warn, target: "fauna.sync",
                       message: "[sync-agent] provisioner start failed (sync degraded until relaunch): \(error)")
        }
    }

    /// Forwards to the shared `FaunaKit.performPostAuthSilentSignIn` (`security.md`
    /// § Post-auth surfacing) — see there for the full rationale. Also the target
    /// of the `#if DEBUG` `silent_sign_in` test command, which awaits this
    /// directly rather than waiting for the one-shot boot-time call.
    ///
    /// Deliberately reads `appState.session` — a stable class instance, `let
    /// session = SessionState()` on `AppState` — rather than the `@State private
    /// var client`: `startInProcessAgentIfNeeded()`'s test-command dispatch runs
    /// through its **init-time-captured `self`**, and `@State` vars *reassigned*
    /// after init (like `client`) read back stale/nil through that detached copy
    /// (the same trap `MachineMethodResultBox`'s doc comment records — a bare
    /// `@State` silently drops what a live `self` wrote). The shared function's
    /// `tearDown`/`runLaunch` closures still work from the detached self because
    /// their OBSERVABLE effect is entirely through `appState` mutations (also
    /// stable), matching `resetToFactory()`'s existing precedent for this same
    /// test-command path.
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

    /// Enter the authenticated UI after the machine reported `LaunchPhase.online`.
    ///
    /// No second sign-in happens here. The machine already ran the silent challenge, and
    /// its `save_authenticated` already wrote (nest_url, handle, domain, tier) into the
    /// **session account's** slots through the shared `RegistryLaunchPersistence`
    /// (`FaunaAccounts`), so this reads them back — exactly
    /// the seam windows uses ("SaveAuthenticated already wrote nest_url + handle/domain/
    /// tier to the vault — StartMainAppAsync doesn't repeat the silent-sign-in refresh").
    /// Re-challenging here would double every launch's round-trip and, worse, re-derive a
    /// verdict the machine already owns.
    ///
    /// The read is `sessionMaterial(sessionActor)` — the account-resolved
    /// session-identity read (`account-scoping.md` § Concurrent instances →
    /// *Session identity resolves through the session's account*), never the
    /// *active* account's launch snapshot:
    /// a bound secondary reading it would authenticate as the wrong
    /// account (the exact live-tested failure that ratified the seam).
    @MainActor
    private func completeAuthenticatedLaunch(keychain: KeychainStore) async {
        let registry = FaunaAccounts.registry(keychain: keychain)
        let sessionActor = appState.boundActorId ?? registry.active()
        let material = sessionActor.flatMap { registry.sessionMaterial(actorId: $0) }
        guard let material,
              let nodeUrl = material.nestUrl,
              let url = URL(string: nodeUrl),
              let deviceId = material.deviceId else {
            // Defensive. `Online` means the machine authenticated against a saved identity
            // + nest_url, but we still cannot build a client without a device_id and a
            // well-formed URL. For a bound instance there is no wizard to fall into
            // (onboarding belongs to the primary) — refuse instead. For the primary,
            // enter the wizard rather than a half-wired session. (`deviceId` is written
            // at the onboarding handoff and is only ever deleted together with
            // `nodeUrl`, so this should be unreachable — windows keeps the same guard
            // for the same reason.)
            if let bound = appState.boundActorId {
                refuseLaunch(
                    actorId: bound,
                    reason: "phase=online but the bound account's material no longer resolves")
                return
            }
            logMessage(
                level: .error, target: "fauna.app",
                message: "[launch] phase=online but the store lacks a usable secret/nest_url/device_id — falling back to the wizard.")
            if let secret = material?.secretHex {
                onboardingVM.machine.seedIdentity(secret: secret)
            }
            appState.launchGate = .ready
            appState.isOnboarded = false
            return
        }

        let secret = material.secretHex
        let actorId = material.actorId

        // The (OS login, account) single-instance guard (`account-scoping.md`
        // § Concurrent instances — macOS RETIRED, W5.6
        // (account-data-plane.md § Workstreams), 2026-08-16): become
        // one of the account's instances BEFORE opening any of its scoped
        // state (the FaunaClient below opens the account's mls.db). A
        // same-account rebuild reuses the held lock (see `MacAppState.instanceLock`); a
        // cross-account switch acquires the new account's lock and releases
        // the old by replacement. The SHARED acquire lets same-account
        // instances coexist (the conversations-engine role lock is the
        // critical section that still refuses, surfaced honestly via
        // `ConversationsVM.pageError`); a `nil` refusal fires only
        // when the lock acquire itself fails, and stays
        // terminal for primary and bound alike — LaunchServices only guards
        // the .app bundle per (OS login, bundle id), so this is the guard
        // that holds for bare-binary launches and for same-account bound
        // launches.
        if appState.instanceLockActorId != actorId {
            guard let lock = FaunaAccounts.acquireInstanceLock(actorIdHex: actorId) else {
                refuseLaunch(
                    actorId: actorId,
                    reason: "another instance already serves this account")
                return
            }
            if !lock.isHeld() {
                logMessage(
                    level: .warn, target: "fauna.app",
                    message:
                        "[instance-lock] degraded acquire for \(actorId) — proceeding unguarded")
            }
            appState.instanceLock = lock
            appState.instanceLockActorId = actorId
        }

        // Written by the machine's `save_authenticated` moments ago (and by every prior
        // successful launch), so these are the freshly-verified values, not a stale cache.
        let handle = material.handle ?? ""
        let domain = material.domain ?? ""
        // Guards both the heal below AND the full rebuild's `selfAddress:` — a
        // handle-less account must not compose the forbidden `"@domain"` shape
        // (an unresolved half stays empty, same sentinel tui's
        // `resolve_self_address(…).unwrap_or_default()` uses).
        let selfAddress = (!handle.isEmpty && !domain.isEmpty) ? "\(handle)@\(domain)" : ""

        // Heal an already-built session (a re-dispatch after `trustNestIdentity()` resolves
        // a changed handle/domain) before the full rebuild below — a no-op when no session
        // is active yet (conversations.md § State & data shape → *Self-address: live, never
        // baked*). An unresolved half is dropped rather than composed into the forbidden
        // `"@nest.example"` shape.
        if !selfAddress.isEmpty {
            conversationsVM.session?.setSelfAddress(selfAddress: selfAddress)
        }

        // Re-scope the photo-backup store to THIS actor before anything reads
        // `modelContainer.mainContext` below (`account-scoping.md` § Serialized
        // switching) — unconditionally, like `client` itself just below: a
        // same-actor re-dispatch (e.g. post-`trustNestIdentity()`) rebuilding
        // against the identical scoped file is harmless, and every other path
        // through here (primary, bound, switch, post-onboarding) needs the swap.
        // Both halves from ONE container, always together — `MacAppState`'s
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
        // below is untouched — only the socket target changes.
        let dialUrl = URL(string: resolvedDialUrl(nestUrl: nodeUrl)) ?? url
        let faunaClient = FaunaClient(
            nodeUrl: dialUrl, secretHex: secret,
            deviceId: deviceId, modelContext: scoped.mainContext
        )

        appState.session.secretHex = secret
        appState.session.actorId = actorId
        appState.session.nodeUrl = nodeUrl
        appState.session.deviceId = deviceId
        appState.session.handle = handle
        // `isOnboarded = true` IS the authenticated flip now: `session.isAuthenticated`
        // derives from it live (`MacAppState.init`'s mounted-shell probe), so there is
        // no second flag to keep in step here or on any teardown path.
        appState.isOnboarded = true

        BackupNotificationManager.requestPermission()
        logMessage(level: .info, target: "fauna.app",
                   message: "[launch] client built by completeAuthenticatedLaunch: material=\(actorId) "
                            + "seat=\(faunaClient.api.boundActorIdHex ?? "<no-secret>")")
        self.client = faunaClient
        // ALSO store on the observed AppState, which iOS's launch path has done
        // since its admin-gate bug (`enterOptimistically`, and see the env
        // injection's own comment). macOS not doing it was the last place the two
        // shells disagreed about where the live client lives, and it was not
        // cosmetic: `appState.liveClient` is the ONLY slot a `@State` write from a
        // callback cannot lose, so it is what the injection resolves — leaving a
        // launch-built client out of it meant a session this path created was
        // invisible to every `@Environment(FaunaClient.self)` view and to the test
        // agent's own reads, until some later patch happened to fill the slot in.
        appState.liveClient = faunaClient
        appDelegate.menuBarController.client = faunaClient

        // Auto-start at sign-in (`apps/macos.md` § App Lifecycle): the first
        // successful sign-in registers every later login — by default, never
        // over an explicit or a Login Items opt-out, never under e2e.
        AutoStart.registerAtPostAuth()

        // The once-per-sign-in newer-version look (`installers/README.md`
        // § Knowing a newer version is out): notify-only, silent unless a newer
        // release is out. `applySessionPatch` makes the same call.
        appState.updates.lookOnceAtSignIn()

        // Push notifications (settings.md § Push notifications; common.md
        // § Push Notifications → Transports). The macOS twin of iOS's
        // `FaunaApp.swift` block: build the shared `PushManager`, hand it to the
        // AppDelegate so a device token has somewhere to land, and run its
        // session start — announce which device this connection serves, and
        // re-ask APNs for a token only when this install already opted in: the
        // OS prompt itself belongs to the toggle, never to launch, and the OS
        // permission alone is NOT the opt-in (`PushManager.shouldRegisterAtLaunch`
        // carries the reasoning). `PushManager` no-ops its OS half on a host
        // that cannot serve a notification centre (the bare-binary debug
        // build), so this is safe on every launch path.
        let pushManager = PushManager(api: faunaClient.api, deviceId: deviceId)
        appState.pushManager = pushManager
        appDelegate.pushManager = pushManager
        Task { @MainActor in
            await pushManager.onSessionStart()
        }

        // Post-auth identity re-check (security.md § Post-auth surfacing): one-shot,
        // best-effort, non-blocking — see `performPostAuthSilentSignIn` below.
        Task { @MainActor in await performPostAuthSilentSignIn() }

        // Always configure photo backup so its sub-page can drive it (Sync Now, live
        // status) even before the first observe pass; the engine host it uploads
        // through arrives with `start()` below, and observing + the initial pass only
        // begin when the user has it enabled.
        photoBackupEngine.configure(
            api: faunaClient.api,
            networkMonitor: faunaClient.networkMonitor,
            modelContext: liveModelContainer.mainContext,
            deviceId: faunaClient.deviceId
        )

        // Activate the dual-rail conversations session so the Conversations page
        // Send (`dm-send-button`) issues a real RPC — mail via the SMTP rail
        // (`fauna.email.send`), DMs/groups via FaunaMls. Best-effort: a failure
        // leaves Send disabled (NotSupported), never crashes the launch.
        Task { @MainActor in
            do {
                try await conversationsVM.rebuild(
                    api: faunaClient.api,
                    selfAddress: selfAddress, selfSecretHex: secret,
                    deviceIdHex: faunaClient.deviceId,
                    predecessorBackupKeys: faunaClient.resolvedPredecessorBackupKeys()
                )
                // Install the conversations session as the feed's room-post key
                // seam (`ui/feed.md` § Encryption at rest → *Room-restricted —
                // the app half*) — the one place the feed manager and the
                // conversations session meet on apple, mirroring linux's
                // `conv_backend.rs`/web's `syncFeedRoomPosts`. Safe even before
                // the Feed tab has built its own manager: `FeedVM` holds the
                // session and re-installs it the moment `configure` builds one.
                if let session = conversationsVM.session {
                    feedVM.installRoomPostKeys(session: session, conversationsManager: conversationsVM.manager)
                }
                // Hands-off TLS-cert auto-renew for managed/delegated domains
                // (admin-dns, tls-certificates.md § C.3) — the actor secret is now
                // primed for the credentialed Dns machine. Native-only loop, idempotent.
                DnsAutoRenewCadence.shared.start(api: faunaClient.api)
                // Author-side subscription reconcile (monetization.md § Pillar 1):
                // heal crash-staged subscriber removals + auto-approve queued follows
                // (encrypted mode enqueues them — the nest cannot mint the KeyBlob).
                // Headless, best-effort; the native twin of linux `subscriptions_author`.
                SubscriptionsAuthorCadence.shared.start(api: faunaClient.api)
                // Guardian Notify's flush cadence (family-safety.md § Guardian
                // Notify) — its `content_notify`/enabled
                // state is set separately by `ContentPolicyStore.refresh`, but the
                // due-check loop itself needs the live api the same way the two
                // cadences above do.
                GuardianNotifyCadence.shared.start(api: faunaClient.api)
                // Screen-time's flush cadence (family-safety.md § Screen
                // time) — its policy/guardian state
                // is set separately by `ScreenTimeStore.refresh`, but the
                // one-minute tick itself needs the live api the same way the
                // cadences above do.
                appState.screenTime.start(api: faunaClient.api)
            } catch {
                logMessage(level: .error, target: "fauna.app", message: "[launch] conversations session activation failed (non-fatal): \(error)")
            }
        }

        Task {
            await faunaClient.start()

            // Critical-alerts session-start + periodic re-sweep loop
            // (critical-alerts.md § Mechanism → *Who runs the detector* +
            // *How often*) — the universal post-auth hook this app's leg was
            // missing (tui/linux/web/android/windows already call it). Held
            // at App-root (`criticalAlertsHost`), not inside `FaunaClient`,
            // so the loop's own scope survives a `FaunaClient` rebuild across
            // account switch and the banner/TestAgent reach the same instance.
            criticalAlertsHost.startSweepLoop(api: faunaClient.api)

            // Sync-agent provisioning (`sync-agent.md` § Control plane split,
            // milestone A4): start the convergence loop that keeps the external
            // `fauna-sync-agent` provisioned (spawn → RefreshBearer → full
            // ProvisionCapability), and hand the bindings UI its control channel
            // so add/remove bind and unbind folders on the agent.
            //
            // WHICH spawner is the whole e2e story here, mirroring linux's
            // `SystemdUserUnitSpawner::new(e2e)` production/e2e branch:
            //   * production      → `LaunchdSyncAgentSpawner` (installs + kicks
            //     the `social.fauna.sync-agent` LaunchAgent).
            //   * e2e + opt-in    → `FfiChildAgentSpawner`, a private child on
            //     THIS launch's socket. The plist + launchctl domain stay
            //     untouched (testing.md § point 10) because the child inherits
            //     the launch's isolated HOME/CFFIXED_USER_HOME and the socket is
            //     home-derived (`~/Library/Application Support/Fauna/
            //     sync-agent.sock`), so isolation holds by construction.
            //   * e2e, no opt-in  → nil: no agent, folder rows seeded by
            //     `sync_inject_locations`, as before.
            //
            // This block used to be `if !FaunaE2E.isActive` outright, which left
            // EVERY e2e launch with folder binds that had nothing behind them —
            // the macOS multiseat seat bound its folder and then synced in
            // neither direction (run 20260724-02), while the injected rows made
            // it look bound. Skipping machine-global state was right; skipping
            // the agent entirely was not.
            // `FfiChildAgentSpawner` is `test-helpers`-gated in `libs/fauna-ffi`
            // (its `spawn_agent()` execs a path read verbatim from
            // `FAUNA_E2E_SYNC_AGENT_BIN` — a process-execution redirect), so it
            // is absent from the production FFI flavor and the reference must be
            // compile-time gated, not just runtime-gated behind `FaunaE2E`
            // (a plain `public enum` with no `#if DEBUG`). testing.md
            // § convention 15 — the automation surface is compiled out of
            // release artifacts.
            #if DEBUG
            let agentSpawner: FfiAgentSpawner? =
                FaunaE2E.isActive
                    ? (FaunaE2E.realSyncAgent ? FfiChildAgentSpawner() : nil)
                    : LaunchdSyncAgentSpawner()
            #else
            let agentSpawner: FfiAgentSpawner? = LaunchdSyncAgentSpawner()
            #endif
            if let agentSpawner {
                await startSyncAgentProvisioner(faunaClient: faunaClient, spawner: agentSpawner)

                // Subscribe to the agent's pushed events for per-file
                // completed-sync notifications (self-healing: retries while
                // the agent is down, reconnects across restarts — independent
                // of provisioner success above). Same e2e gate: the default
                // socket is the REAL user's agent, which a test launch must
                // never read (testing.md § point 10).
                do {
                    appState.syncEventListener = try spawnSyncEventListener(
                        observer: SyncCompleteEventObserver(notifier: SyncNotificationManager()))
                } catch {
                    logMessage(level: .warn, target: "fauna.sync",
                               message: "[sync-agent] event listener spawn failed (no sync notifications): \(error)")
                }
            }

            // `start()` builds the in-process **one-shot-only** engine host (photo
            // ingress, restore, state reads — no resident engines; those live in
            // the agent). Photo backup's ingress uploads through it.
            guard let host = faunaClient.syncHost else { return }
            photoBackupEngine.host = host
            if UserDefaults.standard.bool(forKey: "fauna.photoBackupEnabled") {
                photoBackupEngine.startObserving()
                _ = try? await photoBackupEngine.syncNewPhotos()
            }
        }

        // File Provider auto-appear (file-sync.md § On-Demand Files → Apple File
        // Provider binding): converge the per-set FP domains onto the user's own
        // sets this device's place accepts in and the sets shared with the account
        // (on-demand-files.md § Shared sets on a capability host, decision 3;
        // per-device toggle store, default ON) **minus the
        // bound-folder sets** — the one-local-presence rule; a binding's resident
        // engine outranks the ambient toggle — and provision the app-dead
        // capability (BackupKey + minted bearer — never the seed) before the
        // first add. Re-runs whenever the binding surface changes
        // (`onBindingsChanged`), so unbinding lets a set's domain reappear.
        // Best-effort: from the bare SPM binary (no appex in the bundle) domain
        // calls fail and are logged; only the `.xcodeproj`-built bundle serves
        // domains. Gated off e2e — FP domains and the shared app-group Keychain
        // are machine-global state a test launch must never touch (testing.md
        // § Cross-app e2e conventions point 10).
        if !FaunaE2E.isActive {
            let locationsModel = self.locationsModel
            let fpReconcile: @Sendable () async -> Void = {
                do {
                    // Every set the account holds — own and shared with it —
                    // mapped in shared Rust (the plan applies this device's
                    // place and the toggle), keyed by its `FolderRef` wire
                    // string — the binding's key; the name is the label.
                    let api = await faunaClient.api
                    let sets = try await api.folderPresenceSets(deviceIdHex: deviceId)
                    let boundSets = await Set(locationsModel.mappings.map(\.folderId))
                    await FileProviderCoordinator.reconcile(
                        sets: sets,
                        boundSets: boundSets,
                        provisioning: FileProviderProvisioningContext(
                            nestUrl: nodeUrl,
                            secretHex: secret,
                            deviceIdHex: deviceId,
                            deviceLabel: Host.current().localizedName ?? "Mac"
                        )
                    )
                } catch {
                    logMessage(
                        level: .warn, target: "fauna.fileprovider",
                        message: "[fp] reconcile skipped — folder list or roster read failed: \(error)")
                }
            }
            locationsModel.onBindingsChanged = fpReconcile
            Task { await fpReconcile() }
        }

        // The post-claim serving-enablement call (one shared-Rust step,
        // replacing the mail/CalDAV/CardDAV/WebDAV per-step firing) + the trust-prompt grant + the three
        // universal post-auth hooks (deployment-seed capture, host-address
        // report, seed-map fan-out, seed-custody self-heal) — the fixed
        // six-call sequence `MailEnableGlue.runPostAuthGlue` documents,
        // extracted because it was byte-identical to iOS's own call site despite
        // this function as a whole genuinely diverging from its iOS twin
        // (`completeAuthenticatedGlue`). Detached, same as before extraction — best-effort, never
        // gates the rest of launch.
        Task { @MainActor in
            await MailEnableGlue.runPostAuthGlue(api: faunaClient.api, session: appState.session)
        }

        // ── The succession's closing act, on the SUCCESSOR's first
        // authenticated session (`identity-succession.md` § The RecoveryKey →
        // *At succession*). Only the navigation happens here; the mint is
        // `RecoveryKitSection`'s own hydrate, because the secret has to land in
        // the view model that renders it.
        //
        // **Both halves of the order are load-bearing.** Synchronously, and
        // BEFORE `launchGate = .ready`, so the shell mounts straight onto the
        // section that shows the kit — a navigation applied after the mint
        // landed would enter Account and clear the very secret it exists to
        // show (the shown-once custody rule). And only after the client above is
        // built, because the mint authenticates as the successor and reads its
        // handle to build the `fauna://recovery` payload — minting earlier would
        // produce a kit whose URI names no account.
        //
        // A launch with no reachable nest leaves the flag SET (see
        // `RecoveryKitVM.dischargeOwedSuccessionKit`), so the next one offers the
        // kit instead of the step being silently lost.
        if SuccessionHandoff.kitOwed {
            appState.selectedSidebar = .settings
            appState.selectedSettingsPage = .account
        }

        appState.launchGate = .ready
    }

    /// The `launch-fallthrough-button` escape ("Use a different nest"), shared by BOTH
    /// blocking launch surfaces: the non-retry `.needsUpdate` one (the nest is outdated
    /// / the account is terminal) and `.identityChanged` (the nest's pinned identity
    /// changed — `security.md` names this exact fallthrough as one of the two
    /// ways out, the other being the trust button).
    ///
    /// Drops the saved (nodeUrl, deviceId) and re-seeds onboarding at handle_entry,
    /// keeping the identity. Note it does NOT forget the TOFU pin — walking away from a
    /// nest whose identity you don't trust must not un-pin it; only the explicit
    /// `trustNestIdentity()` does that.
    @MainActor
    private func useADifferentNest() {
        // Re-onboarding is the primary's job: a bound secondary that walks away
        // from its nest has nowhere to go but the wizard, which it must never
        // enter (`account-scoping.md` § Concurrent instances).
        if let bound = appState.boundActorId {
            refuseLaunch(
                actorId: bound,
                reason: "use-a-different-nest requested — re-onboarding belongs to the primary instance")
            return
        }
        let keychain = KeychainStore()
        guard let material = FaunaAccounts.sessionMaterial(keychain: keychain) else {
            // No identity to re-seed — fall through to the wizard root.
            appState.launchGate = .ready
            appState.isOnboarded = false
            return
        }
        // Route the walk-away through the registry (`account-scoping.md`
        // § Concurrent instances, the delete corollary): `clearNestBinding`
        // clears the account's per-actor (nest_url, device_id) slots, keeping
        // the identity.
        do {
            try FaunaAccounts.registry(keychain: keychain)
                .clearNestBinding(actorId: material.actorId)
        } catch {
            logMessage(level: .error, target: "fauna.app",
                       message: "[launch] use-a-different-nest: clearNestBinding failed: \(error)")
        }
        // Walking away from the saved nest: its sets' FP domains + the capability
        // minted against it must not linger (the bearer/BackupKey are nest-scoped).
        if !FaunaE2E.isActive {
            Task { await FileProviderCoordinator.signOut() }
        }
        onboardingVM.machine.seedIdentity(secret: material.secretHex)
        if let cachedHandle = material.handle, !cachedHandle.isEmpty {
            onboardingVM.machine.setCurrentHandle(h: cachedHandle)
        }
        appState.launchGate = .ready
        appState.isOnboarded = false
    }

    // The `migrateDirectoryWatcherToDaemon` UserDefaults→daemon migration retired
    // with the B2 cutover, along with the daemon it fed. It could never have
    // migrated anything: its source key `"fauna.syncFolders"` was written by exactly
    // one function (`DirectoryWatcher.saveLocationConfig`), reachable only from
    // `addFolder`/`removeFolder` — which had **zero call sites**. Folder bindings
    // have always lived in `folder-map.json`, which the shared engine host now reads
    // and writes directly.

    // MARK: - Test Agent (state protocol)
    //
    // Compiled out of release artifacts (testing.md convention 15) — neither
    // start function, nor the `TestAgent`/`InProcessAutomationServer` types they
    // reach, exist in a non-DEBUG build. Both start functions live in
    // FaunaKit's `TestAgentBootstrap` (shared with iOS); only the
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

        // macOS's plain-launch raise observable (`account-scoping.md`
        // § Concurrent instances): LaunchServices answers a second launch of a
        // running bundle with a reopen event, and this counts the handled ones
        // — linux's `raises_served` twin, macOS-only (iOS has no reopen
        // concept). Witnessed by the artifact suite's relaunch test.
        state["reopens_handled"] = ReopensHandled.count

        // The photo-backup pass funnel (`PhotoBackupEngine.lastPass*`) — the
        // observable that makes "backup ran and uploaded nothing" name WHICH
        // nothing, instead of leaving four silent causes indistinguishable from
        // outside the app. Convention 11's corollary holds: every value is a plain
        // field read off the engine, no round trip. Same key, same depth, the same
        // shared `PhotoBackupEngine` as the iOS twin (priority #1). macOS runs the
        // engine against the HOST Photos library, so this is a diagnostic here
        // rather than an e2e observable — an e2e must not touch that library
        // (convention 10); the iOS simulator's per-device library is where the
        // walk runs.
        state["photo_backup"] = [
            "authorization": photoBackupEngine.lastPassAuthorization,
            "assets_seen": photoBackupEngine.lastPassAssetsSeen,
            "already_synced": photoBackupEngine.lastPassAlreadySynced,
            "export_failed": photoBackupEngine.lastPassExportFailed,
            "ingest_failed": photoBackupEngine.lastPassIngestFailed,
            "uploaded": photoBackupEngine.lastPassUploaded,
            "pending": photoBackupEngine.pendingCount,
            "passes_started": photoBackupEngine.passesStarted,
            "passes_completed": photoBackupEngine.passesCompleted,
            "completed_total": photoBackupEngine.completedCount,
            "last_backup_at": photoBackupEngine.lastBackupDate?.timeIntervalSince1970 as Any,
            "enabled": UserDefaults.standard.bool(forKey: PhotoBackupControlsView.enabledKey),
        ]

        // Concurrent instances: what this instance has spawned (each record
        // carries the child's own agent_port under e2e, so the harness can
        // observe the child directly — the parent's port is never shared).
        state["spawned_instances"] = InstanceSpawner.stateRecords()

        // Navigation — macOS uses sidebar directly, no "more" tab indirection.
        // Admin and Settings are sub-paged sidebar-swap shells, so reflect the
        // active sub-page in the second stack entry (the inverse of applyNavPatch)
        // for round-trip parity.
        if !appState.isOnboarded {
            state["nav"] = ["stack": [["view": "welcome"]], "modal": NSNull()]
        } else if appState.selectedSidebar == .admin {
            state["nav"] = ["stack": [
                ["view": "admin"],
                ["view": "admin", "id": appState.selectedAdminPage.navId],
            ], "modal": NSNull()]
        } else if appState.selectedSidebar == .settings {
            state["nav"] = ["stack": [
                ["view": appState.selectedSettingsPage.canonicalTopLevelNavView ?? "settings"],
                ["view": "settings", "id": appState.selectedSettingsPage.navId],
            ], "modal": NSNull()]
        } else {
            state["nav"] = ["stack": [["view": appState.selectedSidebar.rawValue]], "modal": NSNull()]
        }

        // Test-agent command reply slots (see `TestAgentReplies`) — macOS's own
        // `serve_enable_folder`/webdav-serve command mirror; `caldav_mailbox_reply`
        // is already covered by the shared `AppStateObservables.commonState` call.
        if let reply = TestAgentReplies.webdavServeReply {
            state["webdav_serve_reply"] = reply
        }

        // Data
        let context = liveModelContainer.mainContext
        state["data"] = serializeData(context: context)

        // Cross-app E2E bridge — the most recent value-returning
        // `call_machine_method` reader result (`provisioning_snapshot`,
        // `provider_base_url`, …), decoded (not the raw JSON string — this
        // dict is itself re-encoded to JSON at the wire boundary, so a raw
        // string would double-encode). `nil`/absent after a setter-only call.
        // Top-level under `state` (a sibling of `data`, `nav`, `session`, …),
        // matching linux's `state_json`'s top-level `machine_method_result`
        // key and the bridge contract `tests/e2e-unified/drivers/http_bridge.py`
        // documents — NOT nested under `data` (where it lived until this fix;
        // the mis-placement is exactly why the fake-cloud provisioning e2e's
        // `provider_base_url` read-back always reported `None`, even once the
        // override itself applied correctly).
        state["machine_method_result"] = machineMethodResultBox.value ?? NSNull()

        return state
    }

    @MainActor
    private func serializeData(context: ModelContext) -> [String: Any] {
        // The succession_sweep/conversation_threads/selected_thread_id/contacts/
        // knocks/events/notifications core — byte-identical on both shells,
        // extracted so macOS+iOS cannot drift on it (priority #1/#2). `selected_thread_id`'s rationale
        // (iOS's single-pane push-before-registration race) lives on iOS
        // `FaunaApp.swift`'s twin call site — macOS's two-pane list is never
        // covered by it, but shares the field for shape parity.
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

        // Feed — real posts from the shared FeedManager snapshot (FeedVM mirrors
        // the latest `[PostSummary]` into the static on every observer tick).
        //
        // ⚠ `lastLoadedPosts` is ALSO written by the TestAgent's `compose.post`
        // from a throwaway manager, so this read can mask a dead app-level
        // manager (the search / interaction paths silently no-op while `posts`
        // still serializes). The `diag` block below un-masks that: it reads the
        // LIVE app-level `feedVM` + its manager snapshot, so one failing run
        // can tell "field text never reached the VM" from "manager nil" from
        // "query committed but reload never happened" without instrumented
        // reruns (e2e conventions point 6 — failures diagnose themselves).
        let feedPosts = FeedVM.lastLoadedPosts
        let feedSnap = feedVM.manager?.snapshot()
        data["feed"] = [
            "diag": [
                "vm_has_manager": feedVM.manager != nil,
                "vm_search_text": feedVM.searchText,
                "committed_search_query": feedSnap?.searchQuery as Any? ?? NSNull(),
                "manager_posts_count": feedSnap?.posts.count as Any? ?? NSNull(),
                "selected_feed": feedSnap?.selectedFeed as Any? ?? NSNull(),
                "feeds_count": feedSnap?.feeds.count as Any? ?? NSNull(),
                "status": feedSnap.map { String(describing: $0.status) } as Any? ?? NSNull(),
                "error": feedSnap?.error.map { String(describing: $0) } as Any? ?? NSNull(),
            ] as [String: Any],
            "posts": feedPosts.map { p in
            [
                "post_id": p.postId,
                "author": p.author,
                // What the card PAINTS on `post-author` — the same door the
                // card calls — so a harness read of the card's author compares
                // with the painted `feed-post-detail-author`.
                "author_label": conversationsVM.postAuthorLabel(p),
                // Painted document plaintext (markdown markers stripped), NOT the
                // raw `body` — the in-process `feed-post-text` element doesn't
                // register in the macOS lazy feed list, so the harness reads the
                // feed from this state field; serialize the SAME painted text the
                // element read returns (MacPostCardView's `renderDocumentToPlaintext`) so the state-read fallback matches every other app's
                // element read (render-model.md § D6 / read-uniformity residual).
                // `""` for a post the viewer reported (the card paints "You
                // reported this" and no body — `paintedBody` keeps this state
                // read equal to every other app's element read).
                "body": appState.contentPolicy.inputs.paintedBody(of: p),
                "timestamp": p.timestamp,
                "tags": p.tags,
                "has_media": p.hasMedia,
                // Resolved lazily by `resolveMedia` (post-card `.task`, `resolve_media.md`
                // analogue: `libs/fauna-feed::FeedManager::resolve_media`) once the card
                // renders; `""` until then — the harness's `post_image_blob_hash_by_text`
                // polls this field (mirrors windows' `AppDataSnapshot.cs` `media_hash`).
                "media_hash": p.mediaHash ?? "",
                "is_reply": p.isReply,
                // Muted-collapse state (topic-factors.md § Scoring), for the
                // state-read fallback's "must not leak the body" / post-reveal
                // assertions — mirrors the SAME check `MacPostCardView` renders
                // from (`manager.isMuted`), with the reveal exception applied.
                "is_muted": (feedVM.manager?.isMuted(postId: p.postId) ?? false)
                    && !FeedVM.revealedMutedPostIds.contains(p.postId),
                // Interaction-bar counts (ratified 2026-06-27) — the shared
                // PostSummary counts the card binds; the lazy feed List doesn't
                // register the feed-*-button elements in-process, so the harness
                // reads them from this snapshot state (feed.md § Interaction bar).
                "like_count": p.likeCount,
                "reply_count": p.replyCount,
                "repost_count": p.repostCount,
                "quote_count": p.quoteCount,
                // The like TOGGLE's routing state (feed.md § Interaction bar) —
                // the harness's id-keyed state reads need this to confirm the
                // toggle actually flipped, not just the count.
                "viewer_liked": p.viewerLiked,
                // Repost carrier + the repost TOGGLE's routing state (feed.md §
                // Interaction bar → Repost, ratified 2026-08-10) — mirrors
                // linux's identical `reposted_post_id`/`viewer_repost_id` state
                // keys (`main.rs::sync_state_json`). `reposted_post_id` present
                // marks THIS row a REPOST ROW (naming the original it carries);
                // `viewer_repost_id` on the original names the caller's own live
                // repost post — the harness's id-keyed reads need both to confirm
                // the toggle actually created/removed a post, not just moved a count.
                "reposted_post_id": p.repostedPostId ?? "",
                "viewer_repost_id": p.viewerRepostId ?? "",
                // Every link preview with its state, in body order (render-model.md
                // § D4) — lets a test tell a FAILED preview from one still
                // resolving, both of which paint no card. tui's and linux's key.
                "link_previews": AppStateObservables.feedPostLinkPreviews(p.document),
            ] as [String: Any]
        }]

        // Sync — the same three keys linux reports (`main.rs::sync_state_json`)
        // and windows mirrors (`AppDataSnapshot.GetSyncForState`), so a
        // real-agent assertion is platform-agnostic: `files` (never populated
        // on macOS — no sync-files list is plumbed to this state block yet),
        // the agent's live `running` flag, and the rendered folder map.
        //
        // `running` comes from the shared, non-blocking `any_engine_serving`
        // cache read (`syncAgentAnyEngineServingCached()`) — a synchronous
        // UniFFI face, deliberately not the `async` one every other call site
        // awaits. `serializeData` is `@MainActor` but reached from a raw
        // blocking-socket accept thread bridged onto the main thread
        // SYNCHRONOUSLY (`InProcessAutomationServer.onMainActor`'s
        // `DispatchQueue.main.sync`) — no Swift Concurrency in scope here, so
        // an `await` would require threading `async` through that whole
        // bridge. The shared reader itself does no I/O on the caller (a
        // last-observed value plus a one-at-a-time background refresh,
        // `fauna_client_sync::agent::any_engine_serving_cached`), which is what
        // makes a synchronous FFI face over it safe to call directly with no
        // local cache of our own (apple leg; the previous
        // `SyncEngineServingProbe` shim is deleted). `folders` reports **agent
        // bindings only**: an injected model is
        // `sync_inject_locations`' pure render fixture — the Folders page
        // driving without a native folder picker — and reporting those rows here
        // would let a test assert a binding on a box where no agent exists.
        // Linux draws the identical line by construction (its injection lives in
        // the Folders *view*, never in `sync_agent::current_locations()`), and
        // windows states it outright. `running` cannot false-green either way:
        // it is a real socket read, and every consuming wait requires both keys.
        #if DEBUG
        data["sync"] = [
            "files": NSNull(),
            "running": syncAgentAnyEngineServingCached(),
            "locations": locationsModel.isInjected ? [] : locationsModel.mappings.map {
                ["path": $0.path, "folder": $0.folder]
            },
        ] as [String: Any]
        #else
        data["sync"] = NSNull()
        #endif

        return data
    }

    // Compiled out of release artifacts (testing.md convention 15) — the whole
    // command-handler surface `handleTestCommand` dispatches to. Its only
    // callers are the DEBUG-gated `startTestAgentIfNeeded`/
    // `startInProcessAgentIfNeeded`, so this is unreachable dead code in
    // release; gating it removes the `strings`-grep hits too.
    #if DEBUG

    @MainActor
    private func handleTestCommand(_ command: [String: Any]) async {
        if let action = command["__action"] as? String {
            switch action {
            case "reset":
                // Clear the barrier probe slots at the same point tui's
                // `App::barrier_probe` and linux's `clear_barrier_probe` do, so a
                // token cannot leak into the next test of this reused process.
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
                machineMethodResultBox.value = await callMachineMethod(
                    name: method, jsonArg: jsonArg)
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
            case "sync_add_location":
                handleSyncAddLocation(command)
            case "sync_remove_location":
                handleSyncRemoveLocation(command)
            case "sync_inject_locations":
                handleSyncInjectLocations(command)
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
                    command, authenticated: appState.isOnboarded, navigate: { showConnectedApps() })
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
                // `FaunaClient`'s. The report rides the same result slot
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
            case "photo_backup_seed_library", "photo_backup_request_access":
                // The macOS photo-backup venue's fixture seams over the REAL
                // System Photo Library (`fauna_e2e_agent::PHOTO_BACKUP_SEED_LIBRARY`
                // / `PHOTO_BACKUP_REQUEST_ACCESS`) — driven only by the
                // real-session photo-library launch (`drivers/macos.py`), which
                // refuses them in every other launch mode (convention 10). Shared
                // FaunaKit handler; the same slot rule as `custodian_pull_run_now`
                // above: cleared FIRST, written only on success, a refusal LOUD.
                machineMethodResultBox.value = nil
                let outcome = action == "photo_backup_seed_library"
                    ? await PhotoLibraryTestCommand.seed(command)
                    : await PhotoLibraryTestCommand.requestAccess()
                switch outcome {
                case .report(let json):
                    machineMethodResultBox.value = json
                case .refused(let reason):
                    testAgentFailure(reason)
                }
            case "enable_caldav_mailbox":
                await CaldavMailboxTestCommand.apply(command, client: appState.liveClient)
            case "serve_enable_folder":
                // Shared FaunaKit handler (macOS + iOS, one implementation) over the
                // same `FoldersAuthor::serve_set` seam linux drives.
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
            case "conv_receive_now":
                // Convention 14's run_now poke for the client receive loop (row
                // 92, `fauna_e2e_agent::CONV_RECEIVE_NOW`). No session yet (never
                // logged in) is a legitimate quiet ack — the counters simply do
                // not move, and the consumer's own deadline poll on
                // `conv_receive_cycles` names the app + key rather than hanging
                // on a reply nobody would send.
                conversationsVM.session?.convReceiveNow()
            case "family_notify_check_now":
                // Guardian Notify's run_now poke (convention 14,
                // `fauna_e2e_agent::FAMILY_NOTIFY_CHECK_NOW`) — forces
                // `GuardianNotifyCadence`'s due-check now instead of waiting out
                // its real 5s tick; the real
                // `notifyReportMinIntervalSecs`/batch-interval gate still
                // applies inside it. Mirrors linux `flush_notify_report` /
                // windows `GuardianNotifyCache.CheckNowAsync`. Convention 11: a
                // refusal is LOUD — including "nothing due", since this
                // command's one caller (test_family.py's
                // guardian-notify journey) always expects a batch to be
                // pending at this exact point.
                if let reason = await GuardianNotifyCadence.shared.flushIfDue() {
                    testAgentFailure("family_notify_check_now: \(reason)")
                }
            case "screen_time_heartbeat":
                // Screen time's fake-clock run_now poke (convention 14): advance
                // the ward's screen-time clock by `minutes` of foreground use and
                // run one production heartbeat step — mirrors linux `main.rs` /
                // android `advanceTestClockAndTick`. Convention 11: honour the
                // command or refuse audibly, never silently drop it.
                if let minutes = (command["minutes"] as? NSNumber)?.intValue ?? (command["minutes"] as? Int) {
                    await appState.screenTime.advanceTestClockAndTick(minutes: minutes)
                } else {
                    testAgentFailure("screen_time_heartbeat: payload needs an integer `minutes`")
                }
            case "account_pump_now":
                // The account plane's run-one-pass-now poke (convention 14's
                // `run_now`), apple's twin of android's / tui's / linux's arm
                // (`fauna_e2e_agent::ACCOUNT_PUMP_NOW`). `reconcile_now` under the
                // FFI is the ticker's OWN work on demand, never a bypass, so a
                // test that pokes and then waits on `account_pump_cycles`
                // observes exactly the production pass.
                //
                // Spawned, not awaited — matching android and the `conv_receive_now`
                // family: the barrier is the counters in `serializeState()`, not
                // this ack, and awaiting would put a whole pump pass (network
                // included) on the agent's dispatch path, where one wedged pass
                // would surface as an unrelated command timeout.
                //
                // No runtime yet (pre-auth) is a legitimate quiet no-op, honoured
                // rather than dropped (convention 11): `accountPumpNow` reports
                // `false` and the consumer's own deadline poll on the counters is
                // what fails, naming the app.
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
            case BarrierTestCommand.barrierAction, BarrierTestCommand.probeAction:
                // Convention 14's causal anchor + its self-test probe. Shared
                // FaunaKit handler (macOS + iOS, one implementation) — the
                // contract lives in `fauna_e2e_agent::BARRIER`. Convention 11:
                // a refusal is LOUD.
                //
                // ⚠ The barrier runs INSIDE the handler on purpose: this app's
                // ack fires only after `handleTestCommand` returns (see
                // `InProcessAutomationServer.dispatchCommand`), so ordering the
                // work here orders it before the ack. linux had to defer its ack
                // into an idle instead, because its ack site sits on a
                // higher-priority glib source than the work it must follow.
                if let reason = await BarrierTestCommand.apply(action: action, command: command) {
                    testAgentFailure(reason)
                }
            case FocusWalkTestCommand.focusMoveAction, FocusWalkTestCommand.switchPaneAction:
                // Convention 17 layer (c)'s walk vocabulary. Shared FaunaKit
                // handler (macOS + iOS, one implementation) — the contract
                // lives in `fauna_e2e_agent::{FOCUS_MOVE, SWITCH_PANE}`.
                // Convention 11: a refusal is LOUD.
                if let reason = await FocusWalkTestCommand.apply(action: action, command: command) {
                    testAgentFailure(reason)
                }
            default:
                // NEVER silently ignore a command we do not implement. The agent used
                // to `break` here, so a harness driving a command apple lacks saw a
                // 200/ok and then… nothing — the symptom was an empty `list_threads()`,
                // which reads exactly like data loss and cost sibling sessions two
                // cycles chasing MLS at-rest bugs that did not exist. A test agent that
                // discards a command it cannot honour is a trap for every future
                // session (testing.md § Cross-app e2e conventions).
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
            appState.isOnboarded = authenticated
            // This patch IS the session now, so retire any launch still in flight:
            // its verdict was computed from a store read taken before these lines ran
            // (`LaunchMachineBox.machine`'s doc owns the rule and the incident). The
            // same clear every teardown path already does — `resetToFactory`,
            // `logoutKeepData`, `tearDownSessionForSwitch` — reached from the one
            // other direction that makes a held launch stale.
            //
            // ⚠ **And the gate must be resolved in the same breath, because on macOS
            // the dropped verdict was the only thing that would have resolved it.**
            // `ContentView.body` switches on `launchGate` BEFORE it reads
            // `isOnboarded`, so a patch that retires an in-flight launch without this
            // line leaves the shell on `LaunchProgressView` for ever — the session is
            // live, `nav` reads `admin/dashboard`, and the app publishes zero elements
            // (measured: three green crash-recovery journeys went red on exactly that,
            // with `registry ids (0 records)`, the moment the guard above landed
            // alone). Same clear, and the same reason, as the reset path's: launch
            // surfaces are launch-scoped, and a patched-in session is a new scope.
            // iOS needs no twin — it enters optimistically and has no launching gate.
            launchMachineBox.machine = nil
            appState.launchGate = .ready

            // An authenticating patch MUST yield a live `FaunaClient`. The shell renders
            // off `isOnboarded` alone (which the line above just flipped), so
            // authenticating *without* building one leaves
            // a zombie session: every nest-backed view's `.task` guard-returns on the nil
            // client, so its VM is never configured and every dispatch silently no-ops
            // behind an empty error banner. The screen looks logged in and nothing works.
            //
            // That is not hypothetical — it cost two sessions. `login_as_nest_admin`
            // omitted `device_id`, this `if` skipped the client, and apple's mail machine
            // went quiet with `error: ''`. linux never had the bug because it *defaults*
            // the field (`main.rs`: `.unwrap_or("test-device")`). So default it here too:
            // the device id is the one field a patch may fairly omit, and the real
            // onboarding handoff already derives it (keychain, else generate + persist —
            // `OnboardingVM`), which is exactly what we mirror.
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
                    // node_url + secret_hex are the caller's to supply; there is nothing
                    // sane to default them to. Refuse quietly-succeeding: say so loudly,
                    // because the shell above has already rendered as authenticated.
                    logMessage(
                        level: .error, target: "fauna.app",
                        message: "[applySessionPatch] authenticated=true but the session lacks a usable node_url/secret_hex — NO FaunaClient built; every nest-backed surface will silently no-op. Fix the caller's session patch.")
                    return
                }

                // The ACCOUNT REGISTRY half of "become this actor", before the
                // rebuild below: `completeAuthenticatedLaunch` resolves the session
                // identity from `appState.boundActorId ?? registry.active()`, so the
                // patched actor must be enrolled (with its resolved nest + device id)
                // and active before any client is built for it.
                // `SessionPatchAccounts` carries the full reasoning and is shared
                // with iOS.
                SessionPatchAccounts.adoptPatchedActor(
                    secretHex: secret, nestUrl: nodeUrl, deviceId: deviceId,
                    keychain: keychain)

                // Same re-scope `completeAuthenticatedLaunch` does — the e2e
                // agent's injection path builds a session without ever going
                // through it, so it needs its own actor-scoped rebuild or an
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
                // Same dial swap as `completeAuthenticatedLaunch` — `s.nodeUrl`
                // stays the caller-supplied literal; only the socket target
                // resolves through the harness override (identity in
                // release).
                let dialUrl = URL(string: resolvedDialUrl(nestUrl: nodeUrl)) ?? url
                let faunaClient = FaunaClient(
                    nodeUrl: dialUrl, secretHex: secret,
                    deviceId: deviceId, modelContext: context
                )
                logMessage(level: .info, target: "fauna.app",
                           message: "[launch] client built by applySessionPatch: "
                                    + "seat=\(faunaClient.api.boundActorIdHex ?? "<no-secret>")")
                appDelegate.menuBarController.client = faunaClient
                self.client = faunaClient
                appState.liveClient = faunaClient
                // This patch is a sign-in too: the same once-per-sign-in
                // newer-version look `completeAuthenticatedLaunch` makes.
                appState.updates.lookOnceAtSignIn()
                let isE2E = FaunaE2E.isActive
                if isE2E {
                    // Authenticate only — no WebSocket/sync/backup (too heavy for
                    // MainActor). Two e2e sub-modes after auth (which primes
                    // `api.secret` for `ensureNestConnected`):
                    //  • default — attach the conversations drafts autosync directly.
                    //    The e2e path never runs the production `activate(...)` (which
                    //    would swap in the dual-rail manager and lose the deterministic
                    //    mock conversation backends), so without this the autosave gate
                    //    stays closed and no draft ever reaches the nest `__drafts`
                    //    plane (the SAVE-side e2e gap, file-sync.md § Drafts Sync).
                    //  • `FAUNA_E2E_REAL_CONVERSATIONS` (the `real_conversations`
                    //    marker) — build the REAL dual-rail `ConversationsSession` and
                    //    `activate` it, so `startReceiveLoop` drains real-decrypted
                    //    inbound mail/DMs into the snapshot (the tier_3
                    //    test_mail_client_{receive,send,spam_receive} harness). The
                    //    apple twin of windows `StartE2eRealConversationsAsync` + linux
                    //    `conv_backend::start_conversations_session`; `activate` wires
                    //    drafts too, so this branch subsumes the default one.
                    // Both the session and the drafts handle lazily open the WS via
                    // `ensureNestConnected` (the same lazy path every e2e API read
                    // uses), so neither needs `start()`; best-effort throughout.
                    Task { @MainActor in
                        try? await faunaClient.api.authenticate(secret: secret)
                        // Same reasoning as the observers below, and it is the leg
                        // `test_account_runtime_pump.py --app macos` asserts: this
                        // e2e path skips `start()`, so the W3 account-store runtime
                        // would never be hosted for exactly the runs that check that
                        // it is (android hit the identical trap by considering its
                        // conversations-session call site, which also returns early
                        // under e2e — `account-data-plane.md` § Implementation
                        // status today → *Built — W3 the android host*). Placed
                        // inside this Task, after `authenticate`, so the assembly
                        // reads the primed secret rather than racing it.
                        await faunaClient.startAccountRuntime()
                        // Same reasoning again for the in-process sync engine
                        // host: `start()` is what builds it, so without this an
                        // e2e launch's Media badges (`SyncStatesStore`) have no
                        // host to read and every item renders `RemoteOnly`,
                        // whatever the agent's `fsid-<ref>.db` holds — the leg
                        // `test_media_sync_state_badge.py --app macos` asserts
                        // for an agent-bound set. It starts no engine.
                        await faunaClient.startSyncHost()
                        // The A4 "live-e2e harness carve-out" (`sync-agent.md`
                        // § Implementation status today → A4 remainder). The
                        // spawner branch in `completeAuthenticatedLaunch` has
                        // been able to build `FfiChildAgentSpawner` since
                        // 2026-07-24 — but an e2e login never reaches that
                        // function: it arrives as a session PATCH, and the
                        // branch above deliberately skips `faunaClient.start()`
                        // as too heavy for the MainActor. So the real agent was
                        // reachable in production and unreachable from any test,
                        // which is why the macOS multiseat seat could bind a
                        // folder and sync in neither direction (run 20260724-02)
                        // and why `test_media_delete_removes_the_file_from_disk`
                        // could not join here until now.
                        //
                        // Gated on `realSyncAgent` (the `real_sync_agent`
                        // marker), so it is opt-in per test and every other e2e
                        // launch keeps today's no-agent behaviour exactly.
                        // Isolation holds by construction: the child inherits
                        // this launch's HOME/CFFIXED_USER_HOME and the socket is
                        // home-derived, so launchctl and the machine-global
                        // `social.fauna.sync-agent` LaunchAgent stay untouched
                        // (e2e-conventions.md § point 10). It runs AFTER
                        // `authenticate` above because the provisioner's first
                        // act is registering this machine's named row
                        // (`register_this_machine`), which needs the primed
                        // secret.
                        //
                        // ⚠ **One provisioner at a time, and this is where that
                        // invariant is easy to lose.** Production reaches
                        // `startSyncAgentProvisioner` exactly once per launch
                        // (`completeAuthenticatedLaunch`); a session PATCH can
                        // arrive many times in one app instance, because the e2e
                        // `app` fixture logs a fresh actor in per test without
                        // relaunching. Left unguarded, each login starts another
                        // convergence loop against the SAME agent, and the loops
                        // then fight: each re-pushes its own capability on its own
                        // cadence, so the agent can flip back to a previous actor
                        // *after* the newest one provisioned. The agent reports the
                        // folder bound and `Syncing` throughout while the current
                        // actor's uploads land nowhere — indistinguishable from the
                        // R2 (account-data-plane.md § The ratified decisions) defect (`sync-agent.md` → R2), whose fix rebuilds the
                        // host across a *sequence* of identities and cannot help
                        // against two concurrent loops. Measured: the ordered
                        // two-login run passed while a whole-file run (many logins)
                        // stalled with `running: True` and zero uploaded items.
                        // So retire the previous one first.
                        #if DEBUG
                        if FaunaE2E.realSyncAgent {
                            await retireE2eSyncAgentProvisioner()
                            await startSyncAgentProvisioner(
                                faunaClient: faunaClient, spawner: FfiChildAgentSpawner())
                        }
                        #endif
                        // Stamp the verdict on BOTH arms before branching — a bare
                        // `if` here is silent when it is false, which is how a
                        // mock-backend session came to look identical to a real one
                        // in the log. The wording is shared FaunaKit's; only the
                        // call is per-shell.
                        if let verdict = FaunaE2E.realConversationsGateVerdict {
                            logMessage(level: .info, target: "fauna.app", message: verdict)
                        }
                        if FaunaE2E.realConversations {
                            do {
                                // self_address = "<handle>@<domain>", composed ONCE in
                                // shared FaunaKit (`APIClient.e2eSelfAddress`) — both shells
                                // used to carry their own copy of this, which is how the
                                // same wrong-domain defect came to exist twice. Its doc
                                // owns why the domain must be the nest's canonical identity
                                // domain and never the URL host.
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
                                // MLS session on apple.
                                if let session = conversationsVM.session {
                                    feedVM.installRoomPostKeys(session: session, conversationsManager: conversationsVM.manager)
                                }
                            } catch {
                                logMessage(level: .error, target: "fauna.app", message: "[e2e] real conversations session activation failed (non-fatal): \(error)")
                            }
                        } else if let drafts = try? await faunaClient.api.draftsSync(rail: "conversations") {
                            conversationsVM.attachDraftsSync(drafts)
                        }
                    }
                    // …but the global `connection-status` indicator must still be
                    // driven by the live transport state under e2e (the
                    // forced-disconnect regression guard reads it). This observer is
                    // lightweight — one subscription loop over the FfiNestClient the
                    // agent's reads open lazily — so it's safe outside `start()`.
                    faunaClient.startConnectionStateObserver()
                    // Same reasoning for the push pump: the macOS e2e agent skips
                    // `start()`, so the notifications-page live-refresh test
                    // (`test_push_live_refresh.py`) needs the push observer wired up
                    // explicitly here (the iOS agent calls `start()`, which does it).
                    faunaClient.startPushObserver()
                    // Same reasoning for the reconnect pump: the feed's reconnect-
                    // triggered re-hydrate test (`test_nest_flip_feed_rehydrate`,
                    // row 165) needs `.faunaReconnected` actually posting, which
                    // only `startReconnectObserver()` does — this was the one
                    // sibling observer left un-wired here (found running row 165).
                    faunaClient.startReconnectObserver()
                    // Same reasoning for the dedicated knock pump: the mounted-contacts
                    // live-refresh test (`test_knock_live_refresh.py`) needs it wired
                    // explicitly here off the same skipped `start()`.
                    faunaClient.startKnockObserver()
                    // Same reasoning for the Nests auto-renew loop
                    // (`test_a_blessed_nests_trust_renews_itself…`).
                    faunaClient.startNestsAutoRenewLoop()
                    // Same reasoning for the critical-alerts sweep loop:
                    // `test_session_start_alert_sweep.py --app macos` logs in
                    // through this e2e path, which skips `start()` entirely.
                    criticalAlertsHost.startSweepLoop(api: faunaClient.api)
                    // Same reasoning for Guardian Notify's flush cadence
                    // (family-safety.md § Guardian Notify): the production
                    // `conversations.activate(...)` call site starts it, but
                    // this e2e path never reaches that — without this,
                    // `GuardianNotifyCadence.shared`'s `api` stays nil for
                    // every e2e-logged-in ward, so `family_notify_check_now`
                    // finds a batch due but no live api to send it through.
                    GuardianNotifyCadence.shared.start(api: faunaClient.api)
                    // Same reasoning for screen-time's flush cadence
                    // (family-safety.md § Screen time): without this,
                    // `appState.screenTime`'s `api` stays nil for every
                    // e2e-logged-in ward, so `screen_time_heartbeat` finds a
                    // due report but no live api to send it through.
                    appState.screenTime.start(api: faunaClient.api)
                    // Same reasoning for the author-side subscription reconcile
                    // (monetization.md § Pillar 1): the production
                    // `conversations.activate(...)` call site starts it, but this
                    // e2e path never reaches that — without this, an e2e-logged-in
                    // author's client never mints/uploads the KeyBlob a queued
                    // follow/subscribe needs, so `drain_auto_approvals` never runs
                    // and every subscriber-side assertion times out
                    // (`test_subscriptions.py::test_follow_auto_grants_in_encrypted_mode`).
                    SubscriptionsAuthorCadence.shared.start(api: faunaClient.api)
                } else {
                    Task {
                        await faunaClient.start()
                        criticalAlertsHost.startSweepLoop(api: faunaClient.api)
                    }
                }
            }
        }
    }

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

        if view == "welcome" {
            appState.isOnboarded = false
        } else if let item = SidebarItem(rawValue: view) {
            appState.isOnboarded = true
            appState.selectedSidebar = item
            appState.pageNavRequest += 1
            // Another actor's profile carries its hex actor_id (profile.md
            // § Layout & flow → Another's profile); SELF profile + every other
            // sidebar destination clears it. profileNavTarget normalizes the
            // viewer's OWN id back to nil (shared fauna-core, priority #2 —
            // was three hand-rolled per-app copies) — a raw pass-through would
            // render the viewer's own profile in OTHER shape.
            appState.profileActorId = (item == .profile)
                ? (first["actor_id"] as? String).flatMap {
                    profileNavTarget(entryActorId: $0, selfActorId: appState.session.actorId)
                }
                : nil
            // Admin is a sub-paged shell: the cross-app state protocol carries
            // the sub-page in the second stack entry's `id` (admin.md
            // § Navigation model; mirrors linux `nav.stack[1].id`). Absent ⇒
            // dashboard. (`navigate_users` → "users", `navigate_nest` →
            // "admin-nest", `navigate_settings` → "settings", etc.)
            if item == .admin {
                let subId = stack.count > 1 ? (stack[1]["id"] as? String) : nil
                appState.selectedAdminPage = AdminPage(navId: subId) ?? .dashboard
            }
            // Settings is a sub-paged shell too: the second stack entry's `id`
            // selects the rail sub-page (settings.md § Navigation model). Absent ⇒
            // Status (the default, folded-in live-data page).
            if item == .settings {
                let subId = stack.count > 1 ? (stack[1]["id"] as? String) : nil
                appState.selectedSettingsPage = SettingsPage(navId: subId) ?? .status
            }
        } else if view == "devices" || view == "folders" {
            // Compat shim (2026-06-28 sync/folder UI unification): the device
            // roster + folder control plane moved out of the top-level sidebar
            // into the Settings shell (devices.md / folders.md). A bare
            // `{"view":"devices"|"folders"}` nav — the pre-unification
            // cross-app shape still used by the shared e2e action layer until
            // every app migrates — lands on the matching Settings sub-page, so
            // apple stays drivable during the rollout. (The canonical post-migration
            // form `{"view":"settings","id":"devices"|"folders"}` is handled by the
            // `.settings` branch above.)
            appState.isOnboarded = true
            appState.selectedSidebar = .settings
            appState.pageNavRequest += 1
            appState.selectedSettingsPage = SettingsPage(navId: view) ?? .status
        } else {
            logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] Unknown nav view: \(view)")
        }
    }

    @MainActor
    private func applyComposePatch(_ compose: [String: Any]) async {
        // compose.post — create a feed post via API, bypassing compose UI
        if let post = compose["post"] as? [String: Any] {
            guard let api = appState.liveClient?.api, let secret = appState.session.secretHex else {
                logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] compose.post: no client or secret available")
                AppMessages.error = "compose.post: not authenticated"
                return
            }
            let body = post["body"] as? String ?? ""
            let tags = post["tags"] as? [String] ?? []
            do {
                // Ensure API is authenticated (the session patch fires auth in
                // a detached Task, which may not have completed yet)
                if api.currentToken == nil {
                    try await api.authenticate(secret: secret)
                }
                let secretBytes = Data(hexString: secret) ?? Data()
                let payload = tags.isEmpty
                    ? try build_post(secretBytes, body)
                    : try build_post_tagged(secretBytes, body, tags)
                try await api.createPost(payload: payload)
                // Refresh FeedVM.lastLoadedPosts via a throwaway FeedManager so
                // state serialization reflects the new post (the snapshot is the
                // unified `[PostSummary]` source; nil ⇒ the nest's local feed).
                // `feedManager(secret:)` mints a NEW `FfiFeedManager` on every call
                // (`nest_client.rs::feed_manager` — no caching/singleton), so this
                // instance is NOT the one `feedVM`/the rendered view is bound to.
                let mgr = try await api.feedManager(secret: secret)
                await mgr.selectFeed(feedId: nil)
                FeedVM.lastLoadedPosts = mgr.snapshot().posts
                // Also refresh the LIVE manager the real UI observes — re-run its
                // current query (a genuine reload, not a same-id no-op guard) so a
                // post created via this state-injection shortcut actually appears on
                // screen, not just in the state-read fallback. Without this,
                // `post-card`/`feed-post-text` never mount even though `post_count()`
                // (state-backed) reports the post landed — the real UI and the state
                // serialization silently diverge (found 2026-07-18 diagnosing
                // `test_feed_post_delete.py`, which needs real `post-card` elements,
                // not the state-read fallback `_use_state_for_feed_reads` covers).
                // `refreshCurrentFeed()`, not `selectFeed(feedId: selectedFeed)`: the
                // latter is nil for both Local and Trending, so it silently drops a
                // Trending viewer into Local (trending.md § Implementation status
                // today) — the same bug FeedVM's reply/quote re-query already avoids
                // by calling this same shared seam.
                if let liveManager = feedVM.manager {
                    await liveManager.refreshCurrentFeed()
                }
                logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] compose.post: created post (\(mgr.snapshot().posts.count) posts in feed)")
            } catch {
                logMessage(level: .debug, target: "fauna.testagent", message: "[TestAgent] compose.post failed: \(String(describing: error))")
                AppMessages.error = "compose.post failed: \(error.localizedDescription)"
            }
        }
        // compose.file — attach a file via the shared FeedVM.attachComposeFile
        // seam, which HOLDS the bytes on the LIVE composer's VM; a subsequent
        // real post-submit-button click seals them for the staged audience and
        // uploads them (ui/media.md § Encryption at rest — the seal is resolved
        // before the attachment is uploaded, so the pick cannot upload).
        if let file = compose["file"] as? String {
            // `target` disambiguates which composer's picker is being stood in
            // for — feed's `compose-file` (the default, historical behaviour) vs
            // conversations' `attachment-button`. Mirrors the linux seam
            // (`drivers/http_bridge.py::set_input_files` sends
            // `{"file": path, "target": element_id}`); an OS file panel can't be
            // driven by an in-process agent, so this injection *is* the e2e path
            // for both buttons, and it calls the same seams the real pickers do.
            if compose["target"] as? String == "attachment-button" {
                await conversationsVM.attachComposerFile(atPath: file)
                return
            }
            // profile-edit-avatar / profile-edit-banner: stage (not upload) the
            // picked file onto the SELF profile edit form (profile.md § Where
            // logic lives → Field ownership). Real picker leaf would read the
            // bytes immediately too (mirrors `ComposeAttachButton`'s
            // security-scoped read) — the upload itself is deferred to Save,
            // so `ProfileEditVM` stores the bytes, not just the path.
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

    /// Cross-app E2E bridge — `driver.call_machine_method(name, json_arg)`.
    /// Dispatch lives in Rust (`OnboardingMachine.callMachineMethodAsync`)
    /// so all 7 apps share the same name → typed-setter/reader table.
    /// Requires the FFI to be built with `--features test-helpers` (always
    /// enabled in the `apple-ffi` recipe — the methods are inert unless
    /// called). Returns the reader's decoded JSON value (`nil` for a
    /// setter-only name) for the caller to stash as `machine_method_result`.
    ///
    /// We route through the **async** dispatcher, which additionally runs the
    /// machine's async methods (`verify_dns`, `wizard_submit_claim_code`,
    /// `submit_nat_mode_choice`, …) to completion. Through the sync one they
    /// fall into its silent `_` arm and ack green having done nothing — which
    /// is exactly how the live Hetzner provisioning drive failed on macos
    /// (2026-08-29: `verify_dns` never ran, so provisioning sat at
    /// `overall: 'Idle'` for the full 1200 s timeout). Delegating rather than
    /// hand-listing the async names here keeps ONE name table, in shared Rust —
    /// the same reasoning `apps/fauna-tui/src/automation.rs` records.
    @MainActor
    @discardableResult
    private func callMachineMethod(name: String, jsonArg: String) async -> Any? {
        // The two arms the shared dispatcher deliberately leaves to the client:
        // provisioning spawns a task that outlives the call (minutes — the
        // driver polls `provisioning_snapshot` instead of blocking on the ack),
        // so *which* runtime owns the spawn is platform-divergent.
        //
        // ⚠ Both map to `runProvisioning()`, NOT to the same-named
        // `startProvisioning()` / `retryProvisioning()`. Those two have bare
        // `tokio::spawn` Rust bodies needing an ambient tokio runtime on the
        // CALLING thread, which SwiftUI's main thread does not have — the panic
        // class GTK/Compose UI threads hit for this exact orchestrator. This
        // mirrors the production buttons, which call `runProvisioning()` for
        // the same reason (`MacNestProvisioningView.swift`), and
        // `run_provisioning_inner` resets the snapshot + cancel flag at entry,
        // so it doubles as the retry entry point.
        //
        // The detached `Task` is what preserves the bridge's return-immediately
        // contract: `runProvisioning()` drives to completion (minutes), so
        // awaiting it here would stall the ack the driver is waiting on.
        switch name {
        case "start_provisioning", "retry_provisioning":
            Task { await onboardingVM.machine.runProvisioning() }
            return nil
        default:
            break
        }
        // The **registry** arms next — they need this app's own account
        // registry, which no machine dispatcher can reach (`RegistryTestBridge`).
        if case .handled(let resultJson) = RegistryTestBridge.call(name: name, jsonArg: jsonArg) {
            return resultJson.flatMap(machineMethodResultValue(from:))
        }
        guard let json = await onboardingVM.machine.callMachineMethodAsync(
            name: name, jsonArg: jsonArg
        ) else {
            return nil
        }
        return machineMethodResultValue(from: json)
    }

    // MARK: - Conversations TestAgent commands
    //
    // Flat-shape commands (rail / sender / subject / body at the top level of
    // the command dict) — mirrors apps/fauna-linux/src/main.rs's
    // handle_conversations_* and Windows TestAgent.cs. The string forms for
    // `rail` / `flavor` are the Rust enum Debug spellings the cross-app
    // action layer (tests/e2e-unified/actions/conversations.py) expects.

    /// Surface a test-agent failure where the harness can actually see it.
    ///
    /// `POST /app/commands` acks 200 before the handler runs, so the HTTP reply can
    /// never carry the outcome — the only honest channel is the app's own error
    /// surface (`error-message`, which every driver reads via `app.error_text()`;
    /// testing.md § Cross-app e2e conventions, rule 2). Anything the agent cannot
    /// honour lands here loudly instead of vanishing into a `.debug` log.
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

    /// `sync_add_location` — bind a real local folder to a named folder through the
    /// **real** agent, so the engine starts on the agent's side and bytes actually
    /// move. Payload: `{"path": str, "folder": str, "folder_id": str}` — `folder_id`
    /// is the set's `FolderRef` wire string, the binding's key (the name-keyed bind
    /// is retired), exactly what the Folders UI's gesture takes from `folderRef`.
    ///
    /// This drives `LocationsModel.add` — the *same* optimistic model the
    /// `folder-location-*` Folders UI writes, which pushes `bind_location`
    /// (`AddLocation` + the ref-keyed `SetLocationFolder`) at the agent — so a command-driven
    /// bind and a user's bind are one code path. That is what makes it a fixture
    /// **precondition** rather than a test-only backdoor that could pass while the
    /// real gesture is broken (the line linux and tui draw against
    /// `sync_inject_locations`, which touches only the render fixture and starts no
    /// engine).
    ///
    /// The macOS peer of linux's `sync_add_location` arm and tui's
    /// `automation.rs::"sync_add_location"`; consumed by `bound_location_media_app`
    /// and `test_sync_live_apply.py`. Needs a real agent behind it — an e2e launch
    /// only builds the spawner under `FAUNA_E2E_REAL_SYNC_AGENT` (the
    /// `real_sync_agent` marker), and without it the bind has nothing to reach,
    /// which is precisely the state the refusal below names.
    @MainActor
    private func handleSyncAddLocation(_ command: [String: Any]) {
        let path = command["path"] as? String ?? ""
        let folder = command["folder"] as? String ?? ""
        let folderId = command["folder_id"] as? String ?? ""
        guard !path.isEmpty, !folder.isEmpty, !folderId.isEmpty else {
            // Convention 11: honour it or fail LOUDLY on the app's own surface.
            // A silent drop here reads downstream as "the engine never started",
            // which is a product-bug diagnosis for a harness mistake.
            testAgentFailure("sync_add_location: needs `path`, `folder` and `folder_id`")
            return
        }
        guard !locationsModel.isInjected else {
            testAgentFailure(
                "sync_add_location: the folder model is INJECTED (sync_inject_locations ran), "
                + "so this bind would never reach an agent")
            return
        }
        locationsModel.add(path: path, folder: folder, folderId: folderId)
    }

    /// `sync_remove_location` — unbind live: the agent stops the set's engine and
    /// forgets the binding (the nest folder is untouched).
    ///
    /// Accepts linux's `folder` key **or** a native `path` — the same either-key
    /// tolerance tui and windows landed, so the shared action layer stays uniform.
    /// The macOS model removes by path, so both keys are resolved against the
    /// rendered rows: an unbound path or an unknown set is a LOUD refusal, never
    /// the model's own silent `removeAll`-matching-nothing no-op (convention 11 —
    /// an unbind that acks without unbinding reads downstream as "the engine
    /// never stopped").
    @MainActor
    private func handleSyncRemoveLocation(_ command: [String: Any]) {
        let requestedPath = command["path"] as? String
        let requestedSet = command["folder"] as? String
        let bound = locationsModel.mappings.first { row in
            if let requestedPath, row.path == requestedPath { return true }
            if let requestedSet, row.folder == requestedSet { return true }
            return false
        }
        guard let path = bound?.path else {
            testAgentFailure(
                "sync_remove_location: no bound folder for path=\(requestedPath ?? "nil") "
                + "folder=\(requestedSet ?? "nil"); bound: "
                + "\(locationsModel.mappings.map { "\($0.folder)@\($0.path)" })")
            return
        }
        guard !locationsModel.isInjected else {
            testAgentFailure(
                "sync_remove_location: the folder model is INJECTED (sync_inject_locations ran), "
                + "so this unbind would never reach an agent")
            return
        }
        locationsModel.remove(path: path)
    }

    /// `sync_inject_locations` — seed the device-local folder map in memory so the
    /// cross-app `test_sync_folders.py` drives the Synced Folders render /
    /// remove surface without the native folder picker (the macOS peer of
    /// Linux's `inject_locations_for_test` / Windows' `InMemorySyncPipeClient`).
    /// Payload: `{"locations": [{"path": str, "folder": str, "mode": str,
    /// "folder_id": str?}]}`; an entry with no `folder` (an unbound location) is
    /// skipped. A binding is keyed by its set's `FolderRef`, so an entry without
    /// `folder_id` gets a synthetic `local:` ref per distinct folder name — this
    /// render fixture reaches no agent and no nest, so the ref only has to be
    /// well-formed and consistent.
    @MainActor
    private func handleSyncInjectLocations(_ command: [String: Any]) {
        let locations = command["locations"] as? [[String: Any]] ?? []
        var syntheticRefs: [String: String] = [:]
        let mappings = locations.compactMap { entry -> LocationBinding? in
            guard let path = entry["path"] as? String,
                  let folder = entry["folder"] as? String else { return nil }
            let folderId = entry["folder_id"] as? String ?? {
                if let known = syntheticRefs[folder] { return known }
                let minted = "local:\(syntheticRefs.count + 1)"
                syntheticRefs[folder] = minted
                return minted
            }()
            return LocationBinding(path: path, folder: folder, folderId: folderId)
        }
        locationsModel.injectForTest(mappings)
    }

    #endif

    // MARK: - Multi-account switching (`long-term-store.md` § Multi-account evolution)

    /// Guards the switch. The row and the row's container can both deliver one tap, and a
    /// user can hit two rows in a row — either way a second teardown/rebuild starting while
    /// the first is mid-flight would race two launch machines over one store. First trigger
    /// wins; cleared on **every** exit path, including the early bails. (Linux carries the
    /// same guard for the same reason.)
    @MainActor private static var switchInFlight = false

    /// Activate another identity held on this install: **`set_active` → teardown → rebuild**
    /// (`long-term-store.md`:268, design Decision 1 — switch-first). A live in-session
    /// reconnect, never a relaunch.
    ///
    /// **The registry mutation goes first, before anything is torn down.** `setActive` throws
    /// on an actor that is unknown *or* whose secret does not resolve, so a bad target fails
    /// while the current session is still whole and running. Do the teardown first and that
    /// same failure would leave the user with no session and no way back to the switcher.
    ///
    /// **The rebuild is `runLaunch()` — not a second launch path.** It resolves the launch
    /// binding (`FaunaAccounts.resolveLaunchBinding`), whose primary branch resolves the
    /// now-active account from the registry, and
    /// `completeAuthenticatedLaunch` then builds the session from the session account's own
    /// material. So the new identity is picked up by exactly the code a cold launch runs.
    ///
    /// **`confirmed` is the Stage-2 re-auth bit** (`long-term-store.md` § Multi-account
    /// evolution): true iff `AccountSwitcherVM.requestSwitch`'s native re-auth prompt just
    /// succeeded — then the activation goes through `setActiveConfirmed`. Plain `setActive`
    /// refuses a flagged account (`ConfirmationRequired`), so a path that skipped the
    /// prompt fails loudly here and keeps the current session.
    /// Route one `fauna://` deep link (`FaunaDeepLink` — the FP context-action
    /// vocabulary). Share lands on Settings → Folders (the share affordance's
    /// home — `folder-share-button`); Version history stages the `(set, rel)`
    /// target for the Media explorer (`MediaDeepOpen`) and navigates there — the
    /// explorer opens the item's `media-item-detail` once its snapshot holds it.
    /// Pre-auth links are dropped: an FP domain only exists for a signed-in
    /// session, so a link arriving before auth is stale by construction.
    @MainActor
    private func handleDeepLink(_ url: URL) {
        // The same-device handoff's consent route (`ios.md` § App Entry →
        // *In-app routes*) takes the one shared FaunaKit door first; signed out,
        // the door holds it and `applyHeldRoute` applies it after sign-in.
        if ConsentHandoff.shared.receive(url, authenticated: appState.isOnboarded) {
            if appState.isOnboarded { showConnectedApps() }
            return
        }
        guard let link = FaunaDeepLink.parse(url), appState.isOnboarded else { return }
        switch link {
        case .folderShare:
            appState.selectedSidebar = .settings
            appState.selectedSettingsPage = .folders
        case .fileVersions(let set, let rel):
            MediaDeepOpen.shared.stage(set: set, rel: rel)
            appState.selectedSidebar = .media
        }
        appState.navGeneration += 1
    }

    /// Land on Settings → Connected apps for a consent route. The bumped
    /// `navGeneration` restarts the page's visit, which opens the staged request
    /// through the shared machine whether or not the page was already on screen.
    @MainActor
    private func showConnectedApps() {
        appState.selectedSidebar = .settings
        appState.selectedSettingsPage = .connectedApps
        appState.navGeneration += 1
    }

    /// Apply a consent route that arrived signed out, now the session is
    /// authenticated (`ConsentHandoff.takeHeld`).
    @MainActor
    private func applyHeldRoute() {
        if ConsentHandoff.shared.takeHeld() { showConnectedApps() }
    }

    /// Throws the registry's refusal (`FfiError.General` carrying the shared
    /// `switch_refused_copy` line) so the Account page can paint it on its
    /// `error-message`; nothing has been torn down when it does. Callers
    /// outside the switcher (`try?`) keep the log-only behaviour.
    @MainActor
    private func switchAccount(to actorId: String, confirmed: Bool) async throws {
        logMessage(level: .info, target: "fauna.accounts",
                   message: "[account-switch] requested → \(actorId) (confirmed: \(confirmed))")
        guard !Self.switchInFlight else { return }
        Self.switchInFlight = true

        do {
            if confirmed {
                try FaunaAccounts.registry().setActiveConfirmed(actorId: actorId)
            } else {
                try FaunaAccounts.registry().setActive(actorId: actorId)
            }
        } catch {
            // Nothing has been torn down — the live session is untouched,
            // its push row included.
            logMessage(level: .error, target: "fauna.accounts",
                       message: "[account-switch] setActive(\(actorId)) failed: \(error); keeping the current session")
            Self.switchInFlight = false
            throw error
        }

        // The leave-gesture push drop (`common.md` § Registration, ruled
        // 2026-08-30): the OUTGOING actor's row falls while its authority is
        // still in hand — `api` is still the outgoing session's until the
        // teardown below. AFTER the activation, never before, as web's
        // `performSwitch` does: a refused switch leaves the user where they
        // were, and that includes their notifications (`multiple-accounts`
        // outcome 11). Best-effort inside `dropActorRow()`; the incoming actor
        // re-arms at its own session start (`completeAuthenticatedLaunch` →
        // `PushManager.onSessionStart`), while the install's bit is set.
        await appState.pushManager?.dropActorRow()

        await tearDownSessionForSwitch()
        Task { @MainActor in
            // FP teardown must COMPLETE before the incoming session starts
            // (file-sync.md § Apple File Provider binding, multi-account: the
            // capability store is single-slot, so a stale async revoke landing
            // after the new account's provision would wipe the fresh slot; and
            // the outgoing account's domains must not keep serving mid-switch).
            // Removal preserves dirty data (`.preserveDirtyUserData`). E2e-gated
            // like every FP surface — domains + the shared Keychain are
            // machine-global (testing.md § conventions point 10).
            if !FaunaE2E.isActive {
                await FileProviderCoordinator.signOut()
            }
            runLaunch()
            Self.switchInFlight = false
        }
    }

    /// Drop everything the OUTGOING identity owns, keeping its credentials (this is a switch,
    /// not a sign-out — the account stays in the registry and can be switched back to).
    ///
    /// The checklist is `resetToFactory()`'s minus the erase: anything a session owns that
    /// would otherwise keep running, or keep showing the previous identity's data, has to go
    /// here — an account switch is the one flow where "stale state from the last session"
    /// means *another person's data on screen*.
    /// Stop the sync-agent provisioning loop and tear down the agent's persisted
    /// capability (`UnprovisionCapability`). Required on EVERY session-teardown
    /// path — switch, logout, factory-reset re-onboard, e2e reset — or the
    /// external agent keeps syncing under the old identity, app-dead, forever.
    /// Awaited, not fire-and-forget: the reply is the agent's receipt that its
    /// own mount of the account store is down (`sync-agent.md` § Control plane
    /// split), and every caller's erase unlinks that store next — a spawned
    /// un-provision is a race the erase can win. Bounded by the pipe client's
    /// request ceiling; an unreachable agent or a refusal still degrades open,
    /// same as before. Matches tui's and linux's `teardown()`.
    @MainActor
    private func unprovisionSyncAgent() async {
        // The event listener and health poll share the provisioner's teardown
        // paths but not its nil-guard — stop them even if the provisioner
        // never built.
        appState.syncEventListener?.stop()
        appState.syncEventListener = nil
        syncAgentHealthModel.stop()
        guard let provisioner = appState.syncAgentProvisioner else {
            // Now means exactly one thing: this session never provisioned an
            // agent (no spawner — the default e2e launch — or the build threw).
            // It used to ALSO be what a `@State` read through
            // `handleTestCommand`'s detached, init-time-captured self
            // (`MachineMethodResultBox`'s doc comment) saw while the LIVE
            // instance held a real provisioner, which is the ambiguity that let
            // the e2e reset tear nothing down in silence
            // — `appState` is a class, so both selves reach the same slot
            // and the two cases cannot be confused again. Logged at `.info`
            // (not a warning — it is the normal case) so a live check can tell
            // "nothing to unprovision" apart from "unprovisioned for real"
            // below, rather than inferring it from silence.
            logMessage(level: .info, target: "fauna.sync",
                       message: "[sync-agent] unprovision skipped — no provisioner on this self")
            return
        }
        appState.syncAgentProvisioner = nil
        logMessage(level: .info, target: "fauna.sync",
                   message: "[sync-agent] unprovisioning")
        await awaitSyncAgentUnprovision(provisioner)
    }

    /// Drop every piece of in-memory actor-scoped state this target owns. **The**
    /// canonical list — every teardown path calls it and hand-lists none of it.
    ///
    /// The shared half (the two cadences + `FeedVM`'s statics) lives in FaunaKit's
    /// `ActorScope.resetSharedState()`; the app-owned half (this app's
    /// `criticalAlertsHost`, `conversationsVM`, `screenTime`, `modelContainer`
    /// and `appState`'s four e2e-serialization caches) is
    /// `ActorScope.dropAppOwnedState(...)` — shared with iOS too since every
    /// value involved is identically typed. See `ActorScope` for why this is
    /// one explicit function rather than a registration registry, and
    /// `account-scoping.md` § The scoping taxonomy for the contract it serves.
    ///
    /// Deliberately does **not** include `client?.shutdown()`, `unprovisionSyncAgent()`,
    /// session-field nils or navigation resets: those are ordered differently per
    /// site (a bound secondary's logout returns early between them), and this drops
    /// *state*, matching linux's `actor_scope::reset_actor_scoped_state`.
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

    @MainActor
    private func tearDownSessionForSwitch() async {
        // Convention 14's teardown counter, bumped HERE rather than at the three
        // call sites (switch / sign-out / nest-identity-changed) so an arm added
        // later counts itself. This is the commit point: callers ahead can still
        // bail, everything below is synchronous — `SessionGeneration` documents
        // why the deferred FP sign-out + `runLaunch()` would be too late.
        SessionGeneration.recordTeardown()
        // The client owns the WS, both observer loops, the sync host and the upload driver —
        // none of which ARC can reclaim, so shutting it down is not optional.
        await client?.shutdown()
        await appState.liveClient?.shutdown()
        await unprovisionSyncAgent()

        // Every in-memory surface keyed to the outgoing identity, in one call.
        dropActorScopedState()

        launchMachineBox.machine = nil
        appState.launchGate = .ready
        // This is also macOS's guarantee for every page-owned view model (`@State` in a
        // view): flipping `isOnboarded` unmounts `MainWindowView` wholesale, so they all
        // die with it and none needs a per-view seam. Where the guarantee comes from,
        // and what iOS — whose shell is reused — owes instead, is stated once in
        // `ActorScope` § *State a view owns*.
        appState.isOnboarded = false

        // Land the incoming identity on its own default view, not wherever the previous
        // one was standing. Load-bearing, not cosmetic: the switcher lives INSIDE
        // Settings, and the desktop Settings shell *swaps out the sidebar* — so a switch
        // that left `selectedSidebar == .settings` would rebuild the session behind the
        // settings rail, where the gated `admin-tab` does not exist. The user would
        // switch to their admin identity and see no admin shell. (Linux gets this free:
        // its switch destroys every window, so the rebuilt one opens on the default
        // page.) `profileActorId` goes too — it names an actor the new identity may not
        // even be able to see.
        appState.selectedSidebar = .conversations
        appState.selectedSettingsPage = .status
        appState.profileActorId = nil

        appState.session.clearAuthenticatedOverride()
        appState.session.secretHex = nil
        appState.session.actorId = nil
        appState.session.nodeUrl = nil
        appState.session.deviceId = nil
        appState.session.handle = nil
        // Onboarding-only provisioning intents. An appended identity must not re-provision
        // the nest it is joining (linux skips this glue on append by never passing it on),
        // and a plain switch must not replay the previous identity's claim-time intents.
        appState.session.pendingFirstSetupMail = nil
        appState.session.pendingCaldavEnable = false
        appState.session.pendingCarddavEnable = false
        appState.session.pendingWebdavEnable = false
        appState.session.pendingTrustPromptGranted = false
        appState.session.pendingRecoveryKitHex = nil

        client = nil
        appState.liveClient = nil
        appDelegate.menuBarController.client = nil
    }

    /// "Add account" → onboarding in **append** mode. Same wizard; two differences, both
    /// from the linux reference: dismissing it merely closes the sheet (the live session
    /// keeps running — it must never quit the app), and its `LoggedIn` exit registers the
    /// new identity instead of booting a first one.
    @MainActor
    private func beginAddAccount() {
        onboardingVM.machine.reset()
        appState.isAddingAccount = true
    }

    /// Forwards to the shared `FaunaKit.completeAppendedAccount` — see there for
    /// the full rationale (the append-vs-registry ordering argument).
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
        // FP teardown first (same as logoutKeepData; e2e-gated, and "reset" is a
        // test-agent command, so in practice this fires only if a production
        // caller ever adopts the reset path).
        if !FaunaE2E.isActive {
            Task { await FileProviderCoordinator.signOut() }
        }
        // Sign-out-shaped stop, and the erase below is what makes it one. Until
        // 2026-09-21 this reset swept only `KeychainStore`'s own
        // `social.fauna.account` service, which on macOS — unlike iOS, where the
        // lent store carries everything — cannot reach the shared
        // `fauna-account-store` namespace Rust drives through its own native
        // Keychain arm (`installPlatformCredentialStore`'s doc: "harmless on
        // macOS … resolution never consults the lent store where a native arm
        // exists"). So the writer key and the reset actor's principal bundle
        // survived a factory reset, which § Cleanup contract governs exactly as
        // it governs "Sign Out" (`long-term-store.md` § Implementation status
        // today → *Ending a session is not the erase*; hole 3 is the shape).
        // `registry.clearAll()` below reaches that namespace through the
        // registry's erase-only auxiliary-store list, so the slot now DOES go —
        // and per `sync-agent-credentials.md` § Credential model → *The signed-out
        // reconcile*'s rule (the stop follows the erase, which is why iOS's twin
        // is already sign-out-shaped) this stop follows it. Same three calls, in
        // the same order and for the same reasons, as `StatusVM.signOut`: retire
        // the enrollment nest-side while the runtime still holds the writer key,
        // then hand over the conversations engine, because an OPEN store is an
        // unerasable store (`account-scoping.md` § Erasure follows scope).
        await liveFaunaClient?.api.stopAccountRuntimeForSignOut()
        await liveFaunaClient?.api.releaseAccountScopedStores()
        // Stop everything the session owns (observers, backup driver, MLS) and
        // unprovision the external agent — ARC-nulling `client` below reclaims
        // none of it (the same leak `tearDownSessionForSwitch` documents).
        // `shutdown()`'s own switch-shaped `stopAccountRuntime()` is a safe no-op
        // once the sign-out-shaped one above has already torn the runtime down.
        await liveFaunaClient?.shutdown()
        await unprovisionSyncAgent()
        // Both namespaces, and in THIS order — neither alone (`long-term-store.md`
        // § Cleanup contract). The per-actor sweep runs first because it enumerates
        // through the `fauna/index` the wholesale wipe destroys, and it is the only
        // half that crosses into the shared `fauna-account-store` namespace; the
        // wipe then takes EVERY key of this app's own service, not the three
        // identity ones. The app process is session-scoped and reused across the
        // tests in a module, so anything left here leaks into the next test — and a
        // surviving pending-factory-reset slot launches it straight onto a
        // pre-filled claim page for a nest that no longer exists. See `deleteAll()`.
        let keychain = KeychainStore()
        _ = FaunaAccounts.registry(keychain: keychain).clearAll()
        keychain.deleteAll()
        // Erasure follows SCOPE, not just the credential namespace
        // (`account-scoping.md` § Erasure follows scope): the credential wipe above
        // just took this machine's writer key, so an account store left on disk is
        // a store whose writer nobody holds — and it is the signed-out user's
        // readable data besides. linux's and tui's resets have always swept it.
        AccountStateDir.eraseAll()
        // Reset the wizard machine so the next test (which doesn't relaunch
        // the app) starts at identity_choice. `machine.reset()` clears the
        // in-memory state and fires an observer notification so SwiftUI
        // re-renders the active OnboardingContainerView with the new step.
        // Post the onboarding-persistence cleanup the wizard owns no durable
        // state, so deleting the long-term `KeychainStore` keys above is
        // enough.
        onboardingVM.machine.reset()
        // The launch surfaces are launch-scoped, not app-scoped: a test that left the app
        // on the blocking identity-changed / needs-update surface must not hand it to the
        // next one.
        appState.launchGate = .ready
        launchMachineBox.machine = nil

        appState.session.clearAuthenticatedOverride()
        appState.session.secretHex = nil
        appState.session.actorId = nil
        appState.session.nodeUrl = nil
        appState.session.deviceId = nil
        appState.session.handle = nil
        appState.isOnboarded = false
        self.client = nil
        appState.liveClient = nil
        // Not overwritten by ordinary settings nav (only the mail-lists Members
        // button sets it) — must be cleared explicitly or it leaks into the next
        // test's direct mail-list-members navigation.
        appState.selectedMailListId = nil
        appState.selectedMailListName = nil

        // Same canonical drop the production paths run — a reset is a teardown, and
        // the next test in the module does not relaunch the app, so anything left
        // here leaks into it. (This site used to reach the `_for_test` conversations
        // seam; `clear_for_test` is a thin alias for the production
        // `clear_for_identity_change` the canonical drop calls, so no wipe is lost.)
        dropActorScopedState()
        // The succession hand-off's ONE clear point, and deliberately not part of
        // the canonical drop above: those four fields exist to *survive* a switch
        // teardown (see `SuccessionHandoff`). A reset is the other case — it just
        // deleted every identity on the box, so there is no successor left to owe a
        // kit to, no predecessor row for that kit to seal, and nothing left for the
        // sweep to describe. Same clear point as tui's, and the same reason.
        SuccessionHandoff.clearOnFactoryReset()
        // The dial budget is per PROCESS and this reset does not relaunch it, so
        // one test's dial burst would otherwise spend the next test's
        // (`transport-connection.md` § The dial budget). Same point as windows'
        // reset arm: after the clients are torn down.
        dialBudgetClearForTest()
    }

    @MainActor
    private func logoutKeepData() async {
        // A logout is a teardown (`SessionGeneration`).
        SessionGeneration.recordTeardown()
        let keychain = KeychainStore()
        // Route the logout through the registry (`account-scoping.md`
        // § Concurrent instances, the delete corollary): `remove` deletes the
        // session account's per-actor slots + index row (the first remaining
        // account, if any, becomes active).
        //
        // This does not touch the account-store slot (the writer key): it
        // writes through the registry's own `fauna/…` namespace, never
        // `fauna-account-store/…`, and macOS's copy of that namespace lives in
        // a separate native Keychain service besides (`resetToFactory`'s
        // classification comment above). So both `client?.shutdown()` calls
        // below keep the switch-shaped stop, per `sync-agent-credentials.md`
        // § Credential model → *The signed-out reconcile*'s rule.
        let registry = FaunaAccounts.registry(keychain: keychain)
        if let actor = appState.boundActorId ?? registry.active() {
            do {
                try registry.remove(actorId: actor)
            } catch {
                logMessage(level: .error, target: "fauna.app",
                           message: "[logout] registry remove(\(actor)) failed: \(error)")
            }
        }

        // A bound secondary does not outlive its account — and must not tear
        // down the machine-singleton sync surfaces below (agent, File
        // Provider), which serve the STORE-ACTIVE account, not this
        // instance's (`account-scoping.md` § Concurrent instances: sync
        // surfaces stay active-account-only). Shut down this session's own
        // client and exit under the terminal contract; a bound instance
        // never falls into the wizard.
        if let bound = appState.boundActorId {
            await liveFaunaClient?.shutdown()
            refuseLaunch(
                actorId: bound,
                reason: "logged out — a bound instance does not outlive its account")
            return
        }

        // Stop everything the session owns and unprovision the external agent —
        // ARC-nulling `client` below reclaims none of it, and a logout that left
        // the agent provisioned would keep syncing the signed-out identity.
        await liveFaunaClient?.shutdown()
        await unprovisionSyncAgent()

        // This path `runLaunch()`s straight into the promoted account on a
        // multi-account install, so every surface below would otherwise be read by
        // the INCOMING account.
        dropActorScopedState()

        // Tear down the File Provider presence: remove every domain + revoke the
        // shared-Keychain capability so a still-running extension fails closed.
        // e2e-gated for the same machine-global-state reason as the reconcile
        // (this method is also the test agent's "logout" command).
        if !FaunaE2E.isActive {
            Task { await FileProviderCoordinator.signOut() }
        }

        appState.session.clearAuthenticatedOverride()
        appState.session.secretHex = nil
        appState.session.actorId = nil
        appState.session.nodeUrl = nil
        appState.session.deviceId = nil
        appState.session.handle = nil
        appState.isOnboarded = false
        self.client = nil
        appState.liveClient = nil

        // This process no longer serves the removed account — release its
        // single-instance lock. If the removal promoted another account
        // (multi-account install), boot into it: `remove` made it active, so
        // a logout lands on the next signed-in account exactly as the
        // switcher's remove does — not on a wizard that would mint a fresh
        // identity beside the remaining ones. With no account left, this is
        // the classic signed-out end state (wizard root, unchanged).
        appState.instanceLock = nil
        appState.instanceLockActorId = nil
        if registry.active() != nil {
            runLaunch()
        }
    }

    #endif

    /// Factory-reset re-onboard (admin-nest Danger Zone → `AdminNestView` →
    /// `appState.onFactoryReset`). After `fauna.admin.factory_reset` wipes the box
    /// and returns the post-reset `claimCode` (the human never sees it), drop the
    /// authed session but **keep local creds** — the box was wiped, not the
    /// client, so the SAME identity re-claims the fresh nest — and re-seed the
    /// wizard at the claim-code step with the code pre-filled, so the just-reset
    /// nest is immediately re-claimable. Mirrors linux
    /// `main.rs::register_factory_reset_handler`; the re-seed lives here (not the
    /// shared FaunaKit view) because it touches the App-owned `onboardingVM` +
    /// `isOnboarded`. Per `docs/goal/behavior/mail-bridge-lifecycle.md` § Factory
    /// reset + `architecture/nest/common.md` § Client-state recoverability.
    @MainActor
    private func factoryResetReonboard(claimCode: String) async {
        // Capture identity material BEFORE dropping the session (kept in the
        // credential store — only the in-memory session is torn down). The
        // session values were built from the session account's material at
        // launch (`completeAuthenticatedLaunch`), and this hook only fires
        // from an authenticated admin session, so they are the sole source
        // here — an active-account fallback would read the *active* account's
        // slots, wrong under a bound launch (`account-scoping.md`
        // § Concurrent instances → session identity).
        let secretHex = appState.session.secretHex
        let nestUrl = appState.session.nodeUrl ?? ""
        let handle = appState.session.handle ?? ""
        guard let secretHex, !nestUrl.isEmpty else {
            logMessage(level: .error, target: "fauna.app", message: "[FaunaMacApp] factoryResetReonboard: missing identity material; cannot re-onboard")
            return
        }

        // Drop the authed session, KEEPING local creds (secretKey / nodeUrl /
        // deviceId stay in the keychain — the identity re-claims the fresh nest).
        // Shutdown + unprovision first: the box was wiped, so the agent's
        // capability points at a nest that no longer knows it; the re-claim
        // launch provisions afresh.
        //
        // Counted here and not above the guard: a missing-material bail drops
        // nothing (`SessionGeneration` counts teardowns, not attempts).
        SessionGeneration.recordTeardown()
        await liveFaunaClient?.shutdown()
        await unprovisionSyncAgent()
        // The box was wiped, so every in-memory surface built against it is stale —
        // including the two cadences, which this path used to leave running. That
        // was a production defect, not merely untidy: `DnsAutoRenewCadence.start`
        // is latched (`guard loop == nil`), so the re-claim's own `start` silently
        // no-ops and the cadence stays bound to the PRE-RESET `APIClient` for the
        // rest of the process, issuing certs against a nest that no longer knows
        // this client. Same shape as linux's never-reset `AtomicBool` latch
        // (`account-scoping.md`, the `linux (in-memory)` row).
        dropActorScopedState()
        appState.session.clearAuthenticatedOverride()
        self.client = nil
        appState.liveClient = nil

        // Re-seed the wizard at claim-code with the returned code pre-filled
        // (`claim_code_prefill()` feeds the input; the human never types it).
        onboardingVM.machine.reset()
        onboardingVM.machine.seedIdentity(secret: secretHex)
        onboardingVM.machine.navigateToClaimCodeForKnownNestWithCode(
            nestUrl: nestUrl, handle: handle, code: claimCode)

        appState.isOnboarded = false
    }
}
