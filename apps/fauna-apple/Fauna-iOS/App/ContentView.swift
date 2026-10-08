import SwiftUI
import FaunaKit

struct ContentView: View {
    @Environment(AppState.self) private var appState
    let onboardingVM: OnboardingVM

    var body: some View {
        if let changed = appState.identityChanged {
            // BLOCKING: the nest's pinned deployment identity changed, or a pinned nest
            // can no longer prove any identity (the shared machine's
            // `LaunchPhase::IdentityChanged`; security.md § Transport trust —
            // the SSH `known_hosts` model). Checked FIRST, ahead of every other surface:
            // this is a possible-MITM signal, so nothing else may render over it. No retry
            // CTA — a retry cannot change the verdict and must never silently re-pin. The
            // session was already torn down (`leaveAuthenticatedSession`), so there is no
            // authenticated UI behind this to leak through.
            //
            // Shared FaunaKit view, also used by macOS (priority #2).
            LaunchIdentityChangedView(
                pinnedHex: changed.pinnedHex,
                seenHex: changed.seenHex,
                onTrust: { appState.onTrustNestIdentity?() },
                onUseDifferentNest: { appState.onUseDifferentNest?() }
            )
        } else if let refusal = appState.accountIndexRefusal {
            // The saved account index is present and unreadable (the machine's
            // terminal `Offline{transient:false}` carrying
            // `account_index_refusal`). Checked at the SAME tier as
            // `identityChanged` above — both are terminal, blocking,
            // retry-less verdicts with no cached-session fallback (the
            // optimistic entry, if any, was already torn down by
            // `leaveAuthenticatedSession()`). NO retry, NO fallthrough:
            // version-compatibility.md § 5 item 9. Shared FaunaKit view, also
            // used by macOS (priority #2).
            LaunchAccountIndexUnreadableView(
                refusal: refusal,
                confirming: appState.accountIndexConfirming,
                onStartOver: { appState.onAccountIndexStartOver?() },
                onConfirmStartOver: { appState.onAccountIndexConfirmStartOver?() }
            )
        } else if let message = appState.signInRefusedMessage {
            // The saved nest no longer signs this identity in (the machine's
            // terminal `Offline{transient:false}` carrying `sign_in_refused`):
            // `launch_sign_in_refused`, the one terminal surface WITH Retry.
            // Checked ahead of the generic needs-update surface, whose "update
            // your nest" framing misstates the problem. Shared FaunaKit view,
            // also used by macOS (priority #2).
            LaunchSignInRefusedView(
                message: message,
                onRetry: { appState.onRetrySignIn?() },
                onUseDifferentNest: { appState.onUseDifferentNest?() }
            )
        } else if let message = appState.needsUpdateMessage {
            // The nest authoritatively reported it is outdated, the secret is invalid, or
            // the account is locked (the machine's terminal `Offline{transient:false}`).
            // Non-retry "update required" surface — version-compatibility.md Dim 4 /
            // onboarding.md § App-launch routing. iOS enters optimistically, so this
            // overlays the main UI rather than gating before it (no launch gate).
            LaunchNeedsUpdateView(
                message: message,
                onUseDifferentNest: { appState.onUseDifferentNest?() }
            )
        } else if appState.isOnboarding {
            WelcomeView(vm: onboardingVM)
        } else if appState.inAdmin {
            // Admin is a top-level nav peer, not nested under Settings: the shell
            // shows in place of the main TabView and `admin-nav-back` exits to the
            // primary view (admin.md § Navigation model, the 2026-06-07 correction).
            // ⚠ Because it REPLACES the tab shell, the global chrome has to be
            // applied here too — see `GlobalChrome`.
            AdminShellView().modifier(GlobalChrome())
        } else {
            MainTabView().modifier(GlobalChrome())
        }
    }
}

/// The global chrome `ui.yaml`'s `global` section mandates on **every
/// authenticated page** — `critical-alerts`, `connection-status` and
/// `supervised-indicator` (ui.yaml § global: *"Every element listed here MUST be
/// present on every authenticated page in all 7 apps"*; `transport.md`
/// § Connection-status indicator; `family-safety.md` § App surface).
///
/// **Why this is a modifier rather than an inset inside `MainTabView`.** It used
/// to hang off `MainTabView`'s `TabView` — its comment even claimed it was
/// "pinned to the app root", which it was not. iOS renders `AdminShellView()`
/// *in place of* the tab shell (unlike macOS, where admin is a sidebar
/// selection inside the same window shell and the chrome survives by
/// construction), so entering admin silently dropped all of it: no connection
/// indicator, no critical-alert banner, and no supervised indicator for a
/// supervised account. `test_offline_gate.py::test_the_admin_plane_desensitizes_with_no_nest`
/// skipped on iOS for exactly this reason while passing on macOS — the skip was
/// the only symptom, which is why this survived so long.
///
/// Applied to every authenticated branch, so a future shell that also replaces
/// the tab view inherits the chrome by taking this modifier.
private struct GlobalChrome: ViewModifier {
    @Environment(AppState.self) private var appState
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @Environment(CriticalAlertsHost.self) private var criticalAlertsHost: CriticalAlertsHost?

    func body(content: Content) -> some View {
        content
            // The ward's full-screen screen-time lock (family-safety.md §
            // Screen time) — covers the content only, applied BEFORE the
            // safeAreaInset top chrome below so the supervised-indicator
            // there stays tappable. Never mounted on the Family page itself
            // (the only route apple's More-stack Family page has): a locked
            // ward must always be able to read who supervises them and what
            // the policy is. Lives at this GlobalChrome level (not
            // `MainTabView` alone) for the same reason the banners above do —
            // it must survive the admin shell replacing the tab view too.
            .overlay {
                if let message = appState.screenTime.lockMessage,
                    appState.moreSelectedView != "family"
                {
                    ScreenTimeLockOverlay(message: message)
                }
            }
            .safeAreaInset(edge: .top) {
            VStack(spacing: 0) {
                // Highest-severity global chrome — deliberately above every
                // other banner (critical-alerts.md § Severity bar).
                CriticalAlertsBanner(host: criticalAlertsHost)
                // The off-box-recovery custody warning rides just above the connection bar —
                // shown only when the launch glue couldn't confirm custody;
                // tap-to-dismiss.
                RecoveryCustodyBanner(session: appState.session)
                ConnectionStatusBar(client: client)
                // The global `supervised-indicator` (family-safety.md § App
                // surface — permanent, non-dismissable chrome on every page for
                // a supervised account). Tapping it opens the Family page in the
                // More stack, the same destination the gated in-Settings
                // `family-tab` entry reaches. Mirrors the macOS placement.
                SupervisedIndicatorBar(store: appState.familyStatus) {
                    // Leaving admin is part of the navigation: the Family page
                    // lives in the More stack, which the admin shell replaces,
                    // so without this the tap would appear to do nothing.
                    // A no-op when the tab shell is already showing.
                    appState.inAdmin = false
                    appState.selectedTab = "more"
                    appState.moreSelectedView = "family"
                }
                // The shared report sheet + its acknowledgement
                // (moderation.md § User-initiated reporting): ONE host over
                // every page — and over the admin shell too, since this chrome
                // survives it — so a report filed from a card the
                // reporter-side hide then replaces still paints `report-status`.
                ReportHost()
            }
        }
    }
}

struct MainTabView: View {
    @Environment(AppState.self) private var appState
    @Environment(FaunaClient.self) private var client: FaunaClient?
    // No `CriticalAlertsHost` here — the critical-alerts banner moved out with
    // the rest of the global chrome (see `GlobalChrome`).
    @State private var searchVM = SearchVM()
    @FocusState private var searchFieldFocused: Bool

    var body: some View {
        @Bindable var appState = appState
        TabView(selection: $appState.selectedTab) {
            // Unified conversations tab — 1:1 chats, MLS group threads, and
            // email-shaped threads all live here (no separate Groups tab).
            ConversationsListView(reloadToken: appState.navGeneration)
                .tabItem { Label(L.conversations.list.title, systemImage: "bubble.left.and.bubble.right") }
                .tag("conversations")
            FeedListView()
                .tabItem { Label(L.common.feed, systemImage: "antenna.radiowaves.left.and.right") }
                .tag("feed")
            ContactsView()
                .tabItem { Label(L.common.contacts, systemImage: "person.2") }
                .tag("contacts")
            MoreView()
                .tabItem { Label(L.common.more, systemImage: "ellipsis") }
                .tag("more")
        }
        .accessibilityIdentifier(Ids.mainTabView)
        // Presence marker: the authenticated-root container many tests assert via
        // is_visible("main-tab-view") to confirm "logged in / on the main UI"
        // (iOS-only; desktop checks feed visibility instead).
        .automationValue(Ids.mainTabView, text: { "" })
        // The tab-bar items, `{page}-tab` as on every other app's page nav (the
        // macOS sidebar rows included): registered here on the tab view, which is
        // on screen whenever the bar is, so a registry walk sees all four whichever
        // tab is showing. Each performs the selection a tap on the bar performs.
        // Tests may still jump straight to a page via the state protocol
        // (applyNavPatch); these serve a journey that walks the page nav.
        .automationActivate(Ids.conversationsTab) { appState.selectedTab = "conversations" }
        .automationActivate(Ids.feedTab) { appState.selectedTab = "feed" }
        .automationActivate(Ids.contactsTab) { appState.selectedTab = "contacts" }
        .automationActivate(Ids.moreTab) { appState.selectedTab = "more" }
        // Each tab's NavigationStack lays out its large title against the safe
        // area present when it FIRST appears; MainTabView appears before the
        // authed-launch task can set `recoveryCustodyWarning`, so the banner
        // below grows the top safeAreaInset asynchronously and the already-laid-
        // out large titles never re-measure against it — the title renders
        // overlapped by the banner instead of below it (found + screenshot-
        // verified 2026-07-20). Re-keying on the banner's presence forces every
        // tab's NavigationStack to redo that initial layout pass on the (rare,
        // typically once-per-session) transition.
        .id(appState.session.recoveryCustodyWarning != nil)
        .safeAreaInset(edge: .top) {
            if appState.showSearchBar {
                HStack(spacing: 8) {
                    HStack(spacing: 6) {
                        Image(systemName: "magnifyingglass")
                            .foregroundStyle(.secondary)
                        TextField(L.searchPage.searchMessages, text: $searchVM.query)
                            .textFieldStyle(.plain)
                            .autocorrectionDisabled()
                            .textInputAutocapitalization(.never)
                            .focused($searchFieldFocused)
                            .accessibilityIdentifier(Ids.searchQueryField)
                            .automationField(Ids.searchQueryField, text: $searchVM.query)
                            .onSubmit {
                                Task { await searchVM.search() }
                            }
                        if !searchVM.query.isEmpty {
                            Button {
                                searchVM.clear()
                            } label: {
                                Image(systemName: "xmark.circle.fill")
                                    .foregroundStyle(.secondary)
                            }
                            .buttonStyle(.plain)
                            .accessibilityIdentifier(Ids.searchClearButton)
                            .automationActivate(Ids.searchClearButton) { searchVM.clear() }
                        }
                    }
                    .padding(8)
                    .background(.bar)
                    .clipShape(RoundedRectangle(cornerRadius: 10))

                    Button(L.common.search) {
                        Task { await searchVM.search() }
                    }
                    .accessibilityIdentifier(Ids.searchSubmitButton)
                    .automationActivate(Ids.searchSubmitButton) { Task { await searchVM.search() } }

                    Button(L.common.cancel) { cancelSearch() }
                    .accessibilityIdentifier(Ids.searchCancelButton)
                    .automationActivate(Ids.searchCancelButton) { cancelSearch() }
                }
                .padding(.horizontal)
                .padding(.vertical, 8)
                .background(.bar)
                .transition(.move(edge: .top).combined(with: .opacity))
            }
        }
        // The global chrome (connection-status, critical-alerts,
        // supervised-indicator) is NOT applied here: it hangs off every
        // authenticated shell branch via `GlobalChrome`, so the admin shell —
        // which replaces this tab view outright — keeps it too.
        .toolbar {
            ToolbarItem(placement: .navigationBarTrailing) {
                Button {
                    toggleSearchBar()
                } label: {
                    Image(systemName: "magnifyingglass")
                }
                .accessibilityIdentifier(Ids.searchToggleButton)
                .automationActivate(Ids.searchToggleButton) { toggleSearchBar() }
            }
        }
        .overlay {
            if searchVM.hasSearched {
                SearchResultsView(vm: searchVM)
                    .background(.background)
            }
        }
        // Keyed on the client INSTANCE (`SessionKey`), not `client != nil`: a bare
        // boolean is still right for "the authenticated client can be nil at first
        // appear" (the original reason for this key — a fresh in-process login
        // wires it afterwards, so a bare one-shot `.task` would probe
        // `fauna.family.status`/`am-i-admin` against a nil client, fail closed, and
        // never retry) but it ALSO only flips across a switch's momentary nil
        // phase — a switch whose incoming client lands in the same view-update
        // pass as the outgoing one's teardown never re-fires this task, since
        // `true == true` either side of it. Measured live: the
        // [admin-gate] log showed the SECOND `FaunaClient` build right after an
        // in-session switch with no matching `am_i_admin=` line ever following
        // it, so `appState.isAdmin` (and the family/content-policy/screen-time
        // refreshes below) silently kept serving the OUTGOING account's verdict.
        // iOS is exposed to this because `tearDownSessionForSwitch` leaves
        // `MainTabView` mounted across a switch — no remount to force a fresh
        // task — see the `searchVM.reset()` comment below. `SessionKey` (already
        // the fix for the identical class of bug on `ConversationsListView`/
        // `ProfileView`) re-fires on ANY client-instance change, nil phase
        // included, so it closes the race without relying on view-identity churn.
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
                // supervision snapshot above. This is the drop that actually runs on
                // iOS: `tearDownSessionForSwitch` leaves `MainTabView` — and this
                // `@State` VM — mounted (no launch gate; `isOnboarding = false` stays
                // on the same root branch), so without it the previous account's
                // results stay on screen through the gap and, if the next account's
                // first manager build throws, permanently (`account-scoping.md`
                // § The scoping taxonomy, the "reused shell" case). macOS carries the
                // same reset for uniformity.
                searchVM.reset()
            }
            await refreshAdminStatus()
            await appState.familyStatus.refresh(api: client?.api)
            await appState.contentPolicy.refresh(api: client?.api)
            await appState.region.refresh(api: client?.api)
            await appState.screenTime.refresh(api: client?.api)
            await refreshUnreadNotificationCount()
        }
        // Re-resolve the family gates on WS-RPC reconnect — and so the surfaces
        // drop after a graduation ends the relationship. Also retries the search
        // local-index attach (mirrors macOS — the first attempt inside `configure`
        // can race the login-time `conversationsSession` build).
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
        // (`notifications.md` § Architectural rules, rule 4) — the macOS shape.
        .onPushNotification { await refreshUnreadNotificationCount() }
    }

    /// The in-flight clause: a read still suspended when the session changes
    /// returns for the OUTGOING client and must not land on the incoming one.
    private func refreshUnreadNotificationCount() async {
        let asked = client
        guard let count = await FaunaClient.fetchUnreadNotificationCount(client: asked),
              !Task.isCancelled, asked === client else { return }
        appState.notificationsUnreadCount = count
    }

    /// The `am-i-admin` nav gate, resolved at the APP ROOT — the macOS
    /// `ContentView.refreshAdminStatus` shape (priorities #1/#3/#4). It used to live
    /// in `SettingsView`, the one placement that races the surface it mutates: the
    /// probe's auto-default writes `require_confirm_to_activate` into the account
    /// registry, and firing it from inside Settings means that write can land while
    /// the Account page is already rendering the pre-write row. Rooting it also
    /// makes `appState.isAdmin` resolve on every launch instead of only for a user
    /// who happens to open Settings.
    private func refreshAdminStatus() async {
        appState.isAdmin = await FaunaClient.refreshAdminStatus(client: client)
    }

    /// Real search-bar toggle, referenced by both the toolbar `Button` and its
    /// `automationActivate` so the two never diverge (apple-e2e-automation.md
    /// § Resolved design point). Pure view affordance — no search semantics
    /// (`search.md` § User actions: "Client glue, no search semantics"),
    /// mirroring macOS's toggle, which never touches the manager either.
    private func toggleSearchBar() {
        withAnimation {
            appState.showSearchBar.toggle()
            if appState.showSearchBar {
                searchFieldFocused = true
            }
        }
    }

    /// Real cancel action (full reset + hide), shared by the cancel `Button`
    /// and its `automationActivate`.
    private func cancelSearch() {
        searchVM.cancel()
        appState.showSearchBar = false
    }
}

/// Hub for features beyond the main 4 tabs.
/// Shows the selected sub-view directly based on appState.moreSelectedView.
/// Falls back to a menu list when no sub-view is selected.
struct MoreView: View {
    @Environment(AppState.self) private var appState
    @Environment(ConversationsVM.self) private var conversationsVM

    private struct MoreItem: Identifiable {
        let id: String       // canonical view name (matches state protocol)
        let label: String
        let icon: String
    }

    // `devices` (roster) + `files` (folder list) are no longer "More" entries —
    // they moved into the Settings shell (Settings → Devices + Settings → Folders)
    // at the 2026-06-28 sync/folder UI unification (settings.md § Navigation model
    // — Mobile). `media` is the content plane (the file browser); photo-library
    // backup config moved to Settings → Folders (media.md § Apple photo-backup
    // reframe).
    // Labels reuse the same canonical keys the macOS sidebar renders for these
    // nav concepts (`Fauna-macOS/App/AppState.swift` `SidebarItem.label`) —
    // one key per nav concept across all seven apps (priorities #1/#3).
    private static let items: [MoreItem] = [
        .init(id: "profile", label: L.profile.title, icon: "person.text.rectangle"),
        .init(id: "events", label: L.events.title, icon: "calendar"),
        .init(id: "media", label: L.media.title, icon: "photo.on.rectangle"),
        .init(id: "backups", label: L.backups.title, icon: "externaldrive"),
        .init(id: "bridges", label: L.common.bridges, icon: "network"),
        .init(id: "notifications", label: L.common.notifications, icon: "bell"),
        // Standalone Moderation queue — the macOS-sidebar `moderation` peer (same
        // `shield.checkered` glyph + `moderation-tab` id), here as a More entry
        // exactly as bridges/notifications map macOS-sidebar → iOS-More
        // (moderation.md § Architectural rules 1; priorities #1/#3).
        .init(id: "moderation", label: L.common.moderation, icon: "shield.checkered"),
        .init(id: "settings", label: L.common.settings, icon: "gearshape"),
    ]

    var body: some View {
        NavigationStack {
            if let selected = appState.moreSelectedView {
                moreDestination(selected)
                    .toolbar {
                        ToolbarItem(placement: .navigationBarLeading) {
                            Button { exitMoreSubview() } label: {
                                Label(L.common.back, systemImage: "chevron.left")
                            }
                            .accessibilityIdentifier(Ids.moreBackButton)
                            .automationActivate(Ids.moreBackButton) { exitMoreSubview() }
                        }
                    }
            } else {
                List(Self.items) { item in
                    Button {
                        // Entering Settings starts at the root list, not a stale
                        // sub-page from a previous visit.
                        if item.id == "settings" { appState.selectedSettingsPage = nil }
                        // The More-list profile row opens the viewer's OWN profile
                        // (a contact-row tap sets profileActorId for another's).
                        if item.id == "profile" { appState.profileActorId = nil }
                        appState.moreSelectedView = item.id
                    } label: {
                        Label(item.label, systemImage: item.icon)
                    }
                    .accessibilityIdentifier("\(item.id)-tab")
                }
                .navigationTitle(L.common.more)
                .accessibilityIdentifier(Ids.moreView)
            }
        }
    }

    /// Real "leave the More sub-view" action (drop the sub-view and any stale
    /// Settings sub-page), shared by the back `Button` and its `automationActivate`.
    private func exitMoreSubview() {
        appState.moreSelectedView = nil
        // Leaving the sub-view drops any Settings sub-page so re-entering Settings
        // starts at the root list.
        appState.selectedSettingsPage = nil
    }

    @ViewBuilder
    private func moreDestination(_ viewId: String) -> some View {
        switch viewId {
        case "profile": ProfileView(
                session: appState.session,
                actorId: appState.profileActorId,
                onStartDm: { actorIdHex in
                    // Seed the new-thread composer (shared FaunaKit) then switch to
                    // the Conversations tab (mirrors linux start_dm).
                    conversationsVM.startDirectMessage(actorIdHex: actorIdHex)
                    appState.selectedTab = "conversations"
                }
            )
        // `reloadToken` (the Media/Devices idiom): a nav patch targeting events
        // must show the events PAGE — the token bump pops any pushed event
        // detail, which no other signal reaches (the page's `NavigationStack`
        // pops off it; a same-value `moreSelectedView` re-set changes nothing).
        case "events": CalendarListView(reloadToken: appState.navGeneration)
        // Media content plane — the unified cross-set, Windows-Explorer-style
        // browser (media-view-toggle / -sort-select / -folder-filter / media-item)
        // off the shared `MediaMachine`, replacing the interim per-set file browser
        // (2026-06-28 sync/folder UI unification § Apple photo-backup reframe). The
        // photo-backup controls moved to Settings → Folders (media.md).
        case "media": MediaView(reloadToken: appState.navGeneration)
        case "backups": SnapshotListView()
        case "bridges": BridgesView()
        case "notifications": NotificationsView()
        // Shared FaunaKit Moderation queue — same view, same IDs as macOS.
        case "moderation": ModerationQueueView().pageTitle(L.settings.moderationPage.title)
        // Shared FaunaKit Family surface — same view, same IDs as macOS
        // (family-safety.md § App surface). Reached from the gated in-Settings
        // `family-tab` entry and from the global `supervised-indicator`; it is not
        // a More-list row (ui.yaml gated_tabs places it in Settings on mobile),
        // but it lives in the More stack so the cross-app `{"view":"family"}`
        // nav lands it. `navGeneration` makes a re-navigation refetch.
        case "family": FamilyView(reloadToken: appState.navGeneration).pageTitle(L.family.title)
        case "settings": SettingsView()
        default: Text("Unknown: \(viewId)")
        }
    }
}
