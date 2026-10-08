import SwiftUI
import FaunaKit

struct ContentView: View {
    @Environment(MacAppState.self) private var appState
    let onboardingVM: OnboardingVM
    let onRetryLaunch: (() -> Void)?
    /// Surviving-device recovery entry on the retry surface: the launch-time box
    /// read (`RecoverableBoxes.load`, which falls back to the device's own store
    /// when the saved nest is dead) and the click that drops into `nest_recovery`.
    let loadLaunchRecoverableBoxes: (() async -> [String])?
    let onRecoverFromLaunch: (([String]) -> Void)?
    let onUseDifferentNest: (() -> Void)?
    let onRetrySignIn: (() -> Void)?
    let onTrustNestIdentity: (() -> Void)?
    let onAccountIndexStartOver: (() -> Void)?
    let onAccountIndexConfirmStartOver: (() -> Void)?

    init(
        onboardingVM: OnboardingVM,
        onRetryLaunch: (() -> Void)? = nil,
        loadLaunchRecoverableBoxes: (() async -> [String])? = nil,
        onRecoverFromLaunch: (([String]) -> Void)? = nil,
        onUseDifferentNest: (() -> Void)? = nil,
        onRetrySignIn: (() -> Void)? = nil,
        onTrustNestIdentity: (() -> Void)? = nil,
        onAccountIndexStartOver: (() -> Void)? = nil,
        onAccountIndexConfirmStartOver: (() -> Void)? = nil
    ) {
        self.onboardingVM = onboardingVM
        self.onRetryLaunch = onRetryLaunch
        self.loadLaunchRecoverableBoxes = loadLaunchRecoverableBoxes
        self.onRecoverFromLaunch = onRecoverFromLaunch
        self.onUseDifferentNest = onUseDifferentNest
        self.onRetrySignIn = onRetrySignIn
        self.onTrustNestIdentity = onTrustNestIdentity
        self.onAccountIndexStartOver = onAccountIndexStartOver
        self.onAccountIndexConfirmStartOver = onAccountIndexConfirmStartOver
    }

    var body: some View {
        switch appState.launchGate {
        case .launching:
            LaunchProgressView()
        case .retrying(let error):
            LaunchRetryView(
                error: error,
                onRetry: { onRetryLaunch?() },
                loadRecoverableBoxes: loadLaunchRecoverableBoxes,
                onRecover: { onRecoverFromLaunch?($0) })
        case .needsUpdate(let message):
            // Non-retry "update required" surface — shared FaunaKit view, also
            // used by iOS (priority #2). version-compatibility.md Dim 4.
            LaunchNeedsUpdateView(message: message, onUseDifferentNest: { onUseDifferentNest?() })
        case .signInRefused(let message):
            // `launch_sign_in_refused` — the one terminal surface WITH Retry
            // (`onboarding.md` § App-launch routing, the previously-signed-in
            // row). Shared FaunaKit view, also used by iOS (priority #2).
            LaunchSignInRefusedView(
                message: message,
                onRetry: { onRetrySignIn?() },
                onUseDifferentNest: { onUseDifferentNest?() }
            )
        case .identityChanged(let pinnedHex, let seenHex):
            // BLOCKING nest-identity-changed surface — shared FaunaKit view, also used
            // by iOS (priority #2). security.md § Transport trust. Note it
            // takes NO retry callback: a retry cannot change the verdict.
            LaunchIdentityChangedView(
                pinnedHex: pinnedHex,
                seenHex: seenHex,
                onTrust: { onTrustNestIdentity?() },
                onUseDifferentNest: { onUseDifferentNest?() }
            )
        case .accountIndexUnreadable(let refusal, let confirming, let error):
            // The saved account index is present and unreadable — shared
            // FaunaKit view, also used by iOS (priority #2). NO retry, NO
            // fallthrough: version-compatibility.md § 5 item 9.
            LaunchAccountIndexUnreadableView(
                refusal: refusal,
                confirming: confirming,
                error: error,
                onStartOver: { onAccountIndexStartOver?() },
                onConfirmStartOver: { onAccountIndexConfirmStartOver?() }
            )
        case .ready:
            if appState.isOnboarded {
                MainWindowView()
            } else {
                OnboardingContainerView(vm: onboardingVM)
            }
        }
    }
}

private struct LaunchProgressView: View {
    var body: some View {
        VStack(spacing: 16) {
            ProgressView()
            Text(L.launch.signingIn)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier(Ids.launchStatus)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

private struct LaunchRetryView: View {
    let error: String
    let onRetry: () -> Void
    let loadRecoverableBoxes: (() async -> [String])?
    let onRecover: ([String]) -> Void

    /// The custodied boxes the launch-time read found — `launch-recover-button`
    /// is revealed only once ≥1 resolves (`ui.yaml`), after the retry surface has
    /// already painted, so a slow or dead saved nest never delays it.
    @State private var recoverableBoxes: [String] = []

    var body: some View {
        VStack(spacing: 16) {
            Text(L.launch.retryTitle)
                .font(.title3)
                .accessibilityIdentifier(Ids.launchErrorTitle)
            // `ErrorBanner`, not a raw `Text`, so the launch failure also
            // publishes into `AppMessages.error` — `serializeState()` builds
            // `state["messages"]` unconditionally (not gated on `isOnboarded`),
            // so this pre-authenticated surface is reachable through
            // `error_text()` exactly like any authenticated page.
            ErrorBanner(message: error)
            Button(L.launch.retryButton) { onRetry() }
                .keyboardShortcut(.defaultAction)
                .accessibilityIdentifier(Ids.launchRetryButton)
                .automationActivate(Ids.launchRetryButton) { onRetry() }
            if !recoverableBoxes.isEmpty {
                Button(L.onboarding.launch.recoverLostBox) { onRecover(recoverableBoxes) }
                    .accessibilityIdentifier(Ids.launchRecoverButton)
                    .automationActivate(Ids.launchRecoverButton) { onRecover(recoverableBoxes) }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding()
        .task { recoverableBoxes = await loadRecoverableBoxes?() ?? [] }
    }
}

struct MainWindowView: View {
    @Environment(MacAppState.self) private var appState
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @Environment(ConversationsVM.self) private var conversationsVM
    @Environment(SyncAgentHealthModel.self) private var syncAgentHealth: SyncAgentHealthModel?
    @Environment(CriticalAlertsHost.self) private var criticalAlertsHost: CriticalAlertsHost?
    @Environment(\.modelContext) private var modelContext
    @State private var searchVM = SearchVM()
    // Pin the sidebar column visible **only under the in-process e2e driver** so
    // its `*-tab` rows lay out + `.onAppear` (→ register in the AutomationRegistry).
    // Headless, SwiftUI's adaptive `.balanced` split collapses the sidebar column
    // and its `mainList` rows never appear, so the whole sidebar nav reads as
    // absent in-process (even `admin-tab`, which already carries its automation
    // modifier — the detail column still registers, which is why only sidebar ids
    // were missing). Production keeps `.automatic` (unchanged collapse behavior) —
    // apple-e2e-automation.md § sidebar registration.
    @State private var columnVisibility: NavigationSplitViewVisibility =
        FaunaE2E.isActive ? .all : .automatic

    var body: some View {
        @Bindable var state = appState
        @Bindable var searchVM = searchVM

        NavigationSplitView(columnVisibility: $columnVisibility) {
            // `.focusRegionAnchor("sidebar")` sits on this OUTER ZStack, not
            // directly on `SidebarView`: `SidebarView.body` itself swaps its
            // entire content (`mainList`/`SettingsNavRail`/`AdminNavRail`,
            // `SidebarView.swift`'s own "sidebar-swap" comment) on every
            // selection change, and — measured (2026-08-17, same
            // `test_ui_walk_sweep.py` run that found the `page` race above) —
            // that internal swap raced the walk into Settings the identical
            // way: "no `sidebar` region is mounted" on the FIRST transition
            // that reached it. Same fix, same reason: a ZStack wrapper has no
            // identity of its own tied to `selection`, so the marker riding
            // its `.background()` survives the inner swap untouched.
            ZStack {
                SidebarView(selection: $state.selectedSidebar)
            }
            .focusRegionAnchor("sidebar")
        } detail: {
            // `.focusRegionAnchor("page")` sits on this OUTER ZStack, not on the
            // `.id()`-reset Group below: `switch_pane`'s marker must survive a
            // sidebar-selection change, but the Group's `.id()` deliberately
            // tears the whole subtree down and rebuilds it on every switch (see
            // its own comment) — measured (2026-08-17, first real
            // `test_ui_walk_sweep.py --walk-sweep --app macos` run) to race the
            // walk: the first post-login navigation hit `switch_pane` before
            // the freshly-recreated marker had re-registered, "no `page` region
            // is mounted". A ZStack wrapper has no `.id()` of its own, so its
            // identity — and the marker riding its `.background()` — survives
            // the inner reset untouched, while the reconciliation crash fix
            // below is unaffected (still applies to the same Group).
            ZStack {
            // Force SwiftUI to recreate the detail view (and its toolbar items)
            // on every sidebar selection change. Without .id(), AppKit tries to
            // reconcile toolbar items across different detail views during the
            // transition, which crashes in -[NSToolbar _insertNewItemWithItemIdentifier:].
            Group {
            if searchVM.hasSearched {
                SearchResultsView(vm: searchVM)
            } else {
                switch appState.selectedSidebar {
                case .conversations:
                    MacConversationsView()
                case .contacts:
                    ContactSplitView()
                case .events:
                    EventSplitView()
                case .feed:
                    FeedSplitView()
                case .profile:
                    ProfileView(
                        session: appState.session,
                        actorId: appState.profileActorId,
                        onStartDm: { actorIdHex in
                            // Seed the new-thread composer (shared FaunaKit) then
                            // switch to Conversations (mirrors linux start_dm +
                            // on_open_conversations).
                            conversationsVM.startDirectMessage(actorIdHex: actorIdHex)
                            appState.selectedSidebar = .conversations
                        }
                    )
                case .media:
                    MediaSplitView()
                case .backups:
                    BackupSplitView()
                case .bridges:
                    BridgesSettingsView()
                case .notifications:
                    MacNotificationsView()
                case .moderation:
                    // The standalone Moderation queue (shared FaunaKit) over
                    // `fauna.moderation.actions` — the user's flagged/actioned
                    // content + per-row training corrections (moderation.md
                    // § Goal). Spam *preferences* stay on Settings → Privacy
                    // (`PrivacySettingsView`); the queue consumes them, not hosts.
                    ModerationQueueView()
                        .pageTitle(L.settings.moderationPage.title)
                case .settings:
                    SettingsShellView()
                case .admin:
                    AdminShellView()
                case .family:
                    // The shared FaunaKit Family surface (iOS reuses the same
                    // view) — family-safety.md § App surface. `navGeneration`
                    // makes a RE-navigation to the same page refetch, which the
                    // e2e's `reload()`-then-poll loop depends on.
                    FamilyView(reloadToken: appState.navGeneration)
                        .pageTitle(L.family.title)
                }
            }
            }
            // Recreate the detail view on sidebar change AND on profile-actor
            // change (profile→another's-profile keeps selectedSidebar == .profile,
            // so include the actor id to force a fresh ProfileView + re-fetch).
            .id("\(appState.selectedSidebar.rawValue)|\(appState.profileActorId ?? "")")
            .accessibilityElement(children: .contain)
            }
            .focusRegionAnchor("page")
        }
        .navigationSplitViewStyle(.balanced)
        // The ward's full-screen screen-time lock (family-safety.md § Screen
        // time) — covers the content only, applied BEFORE the safeAreaInset
        // top chrome below so the supervised-indicator there stays clickable.
        // Never mounted on the Family page itself: a locked ward must always
        // be able to read who supervises them and what the policy is.
        .overlay {
            if let message = appState.screenTime.lockMessage, appState.selectedSidebar != .family {
                ScreenTimeLockOverlay(message: message)
            }
        }
        // Global connection-status indicator pinned to the top of the shell, above
        // the sidebar/detail split. Shared FaunaKit view, fed the live
        // `FaunaClient.connectionState` (transport.md § Connection-status indicator).
        // The off-box-recovery custody warning rides just above it — shown only when the launch
        // glue couldn't confirm custody; tap-to-dismiss.
        .safeAreaInset(edge: .top) {
            VStack(spacing: 0) {
                // Highest-severity global chrome — deliberately above every
                // other banner (critical-alerts.md § Severity bar).
                CriticalAlertsBanner(host: criticalAlertsHost)
                RecoveryCustodyBanner(session: appState.session)
                ConnectionStatusBar(client: client)
                // Global local sync-agent process-health indicator, sibling to
                // `connection-status` (`sync-agent.md` § Local agent health) —
                // distinct from the WS-RPC link above it.
                SyncAgentStatusBar(model: syncAgentHealth)
                // The global `supervised-indicator` (family-safety.md § Client
                // surface — permanent, non-dismissable chrome on EVERY page for a
                // supervised account). It lives here, above the split, and NOT in
                // the sidebar: the Settings and Admin shells swap the sidebar out
                // in place, so a sidebar-hosted indicator would vanish on exactly
                // the pages a supervised user is most likely to visit. (Linux uses
                // its header bar for the same reason.)
                SupervisedIndicatorBar(store: appState.familyStatus) {
                    appState.selectedSidebar = .family
                }
                // The shared report sheet + its acknowledgement
                // (moderation.md § User-initiated reporting): ONE host over
                // every page, because the three verbs (feed ⋯, message ⋯, an
                // OTHER profile) live on different pages and a report filed from
                // a card the reporter-side hide then replaces must still
                // paint its `report-status` somewhere that survives it.
                ReportHost()
            }
        }
        // .searchable(placement: .toolbar) crashes on macOS 26 during page
        // transitions — AppKit's NSToolbar throws EXC_BREAKPOINT in
        // _insertNewItemWithItemIdentifier when the detail view changes.
        // Use placement: .sidebar instead, which embeds the field in the
        // sidebar list rather than the NSToolbar.
        .searchable(text: $searchVM.query, placement: .sidebar,
                     prompt: L.searchPage.searchMessages)
        .accessibilityIdentifier(Ids.searchQueryField)
        .automationField(Ids.searchQueryField, text: $searchVM.query)
        .onSubmit(of: .search) {
            Task { await searchVM.search() }
        }
        // Choosing a page leaves Search. The detail pane shows the search
        // results over EVERY page while `hasSearched` (above), so without this a
        // sidebar pick, or a nav to the page already under the overlay, changed
        // nothing on screen. It is the dismissal `SearchResultsView.openResult`
        // already does before it switches pages.
        .onChange(of: appState.selectedSidebar) { dismissSearchForPageChange() }
        .onChange(of: appState.pageNavRequest) { dismissSearchForPageChange() }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    searchVM.isSearchVisible.toggle()
                } label: {
                    Image(systemName: searchVM.isSearchVisible ? "magnifyingglass.circle.fill" : "magnifyingglass")
                }
                .accessibilityIdentifier(Ids.searchToggleButton)
                .automationActivate(Ids.searchToggleButton) {
                    searchVM.isSearchVisible.toggle()
                }
            }
            ToolbarItem(placement: .primaryAction) {
                Button {
                    Task { await searchVM.search() }
                } label: {
                    Image(systemName: "magnifyingglass")
                }
                .accessibilityIdentifier(Ids.searchSubmitButton)
                .disabled(searchVM.query.trimmingCharacters(in: .whitespaces).isEmpty)
                .automationActivate(
                    Ids.searchSubmitButton,
                    isEnabled: { !searchVM.query.trimmingCharacters(in: .whitespaces).isEmpty }
                ) {
                    Task { await searchVM.search() }
                }
            }
            // `search-type-filter` — one shared mapping applies the token to
            // BOTH search backends (search.md § State & data shape); re-fires
            // the LIVE buffer under the new token (a no-op while the buffer is
            // empty, mirroring tui/linux/web — the picker then redraws back to
            // the still-committed value on the next snapshot read).
            ToolbarItem(placement: .primaryAction) {
                Picker("", selection: Binding(
                    get: { searchVM.typeFilter },
                    set: { token in Task { await searchVM.search(newTypeFilter: token) } }
                )) {
                    ForEach(FaunaFFISwift.searchTypeFilterOptions(), id: \.self) { token in
                        Text(renderLocalizedText(FaunaFFISwift.searchTypeFilterLabel(token: token))).tag(token)
                    }
                }
                .pickerStyle(.menu)
                .accessibilityIdentifier(Ids.searchTypeFilter)
                .automationSelect(
                    Ids.searchTypeFilter,
                    value: { searchVM.typeFilter }
                ) { token in Task { await searchVM.search(newTypeFilter: token) } }
            }
            // Always include these toolbar items to avoid NSToolbar crash
            // (conditional ToolbarItem count changes during layout cause
            // EXC_BREAKPOINT in -[NSToolbar _insertNewItemWithItemIdentifier:]).
            // Hide them with .opacity + .disabled instead.
            ToolbarItem(placement: .primaryAction) {
                Button {
                    searchVM.clear()
                } label: {
                    Image(systemName: "xmark.circle.fill")
                }
                .accessibilityIdentifier(Ids.searchClearButton)
                .opacity(searchVM.query.isEmpty ? 0 : 1)
                .disabled(searchVM.query.isEmpty)
                .automationActivate(
                    Ids.searchClearButton,
                    isEnabled: { !searchVM.query.isEmpty }
                ) {
                    searchVM.clear()
                }
            }
            ToolbarItem(placement: .primaryAction) {
                Button {
                    searchVM.cancel()
                } label: {
                    Text(L.common.cancel)
                }
                .accessibilityIdentifier(Ids.searchCancelButton)
                .opacity(searchVM.hasSearched ? 1 : 0)
                .disabled(!searchVM.hasSearched)
                .automationActivate(
                    Ids.searchCancelButton,
                    isEnabled: { searchVM.hasSearched }
                ) {
                    searchVM.cancel()
                }
            }
        }
        // Keyed on the client INSTANCE (`SessionKey`), not `client != nil` (row
        // 384 — uniformity with the iOS fix for the identical race, priority #1).
        // A bare boolean is still right for "the authenticated client can be nil
        // at first appear" (the original reason for this key — a fresh
        // in-process login wires it afterwards, so a bare one-shot `.task` would
        // probe `am-i-admin` against a nil client, fail-closed, and never retry)
        // but it ALSO only flips across a switch's momentary nil phase — a switch
        // whose incoming client lands in the same view-update pass as the
        // outgoing one's teardown never re-fires this task, since `true == true`
        // either side of it. Today macOS is *not observed* hitting this: per the
        // `searchVM.reset()` comment below, `tearDownSessionForSwitch` flips
        // `isOnboarded = false` synchronously with `client = nil`, which unmounts
        // this whole `MainWindowView` and rebuilds it fresh — a remount always
        // re-runs `.task(id:)` regardless of key semantics, incidentally masking
        // the race. That masking is a property of the CURRENT teardown shape, not
        // a guarantee this key relies on, and iOS's identical `client != nil` key
        // was measured losing the race live (its shell stays mounted across a
        // switch) — see `Fauna-iOS/App/ContentView.swift`. `SessionKey` (already
        // the fix for the same bug class on `ConversationsListView`/`ProfileView`)
        // re-fires on ANY client-instance change, nil phase included, so it closes
        // the race without depending on remount-by-accident.
        .task(id: SessionKey(client)) {
            // Restore the persisted last-known supervision snapshot AHEAD of
            // the three live refreshes below (family-safety.md § Content
            // policy, clause 2). Firing at every
            // `client` transition (including the nil teardown phase, e.g. an
            // account switch) is what keeps a departing account's floor from
            // bleeding into the next one's first paint.
            seedSupervisionSnapshot(
                api: client?.api, familyStatus: appState.familyStatus,
                contentPolicy: appState.contentPolicy, screenTime: appState.screenTime)
            if let client {
                await searchVM.configure(api: client.api)
            } else {
                // The nil-client phase of a switch / sign-out: drop the departing
                // account's search (manager, results, typed query) beside its
                // supervision snapshot above. On macOS this is belt-and-braces —
                // `tearDownSessionForSwitch` flips `isOnboarded = false` in the same
                // synchronous run as `client = nil`, which unmounts this whole
                // `MainWindowView` (`.ready && isOnboarded`, above) and the `@State`
                // VM with it — but an in-memory drop that rides a shell teardown is
                // correct only while that teardown is guaranteed
                // (`account-scoping.md` § The scoping taxonomy, the in-memory
                // corollary), and iOS keeps its shell mounted, so both targets carry
                // the same identity-keyed reset.
                searchVM.reset()
            }
            await refreshAdminStatus()
            await appState.familyStatus.refresh(api: client?.api)
            await appState.contentPolicy.refresh(api: client?.api)
            await appState.region.refresh(api: client?.api)
            await appState.screenTime.refresh(api: client?.api)
            await refreshUnreadNotificationCount()
        }
        // Re-pull the admin gate on WS-RPC reconnect so the `admin-tab` row
        // tracks `am-i-admin` after a session re-establishes (mirrors the
        // snapshot re-hydrate other macOS surfaces do). The family gate rides the
        // same trigger, so `family-tab` + `supervised-indicator` also re-resolve
        // after a reconnect — and after a graduation drops the relationship (web
        // re-reads on every connection-state change for exactly this). Also
        // retries the search local-index attach — the first attempt (inside
        // `configure`) can race the login-time `conversationsSession` build, so
        // a reconnect is the natural, already-wired second chance.
        .onReconnect {
            await refreshAdminStatus()
            await appState.familyStatus.refresh(api: client?.api)
            await appState.contentPolicy.refresh(api: client?.api)
            await appState.region.refresh(api: client?.api)
            await appState.screenTime.refresh(api: client?.api)
            await searchVM.attachLocalIndexIfNeeded()
            await refreshUnreadNotificationCount()
        }
        // The app-global unread count follows every `fauna.notification` push
        // wherever the user is, not only while Notifications is mounted
        // (`notifications.md` § Architectural rules, rule 4).
        .onPushNotification { await refreshUnreadNotificationCount() }
    }

    private func refreshAdminStatus() async {
        appState.isAdmin = await FaunaClient.refreshAdminStatus(client: client)
    }

    /// `search-cancel-button`'s own act, taken when the user chooses a page.
    private func dismissSearchForPageChange() {
        if searchVM.hasSearched { searchVM.cancel() }
    }

    /// The in-flight clause: a read still suspended when the session changes
    /// returns for the OUTGOING client and must not land on the incoming one.
    private func refreshUnreadNotificationCount() async {
        let asked = client
        guard let count = await FaunaClient.fetchUnreadNotificationCount(client: asked),
              !Task.isCancelled, asked === client else { return }
        appState.notificationsUnreadCount = count
    }
}
