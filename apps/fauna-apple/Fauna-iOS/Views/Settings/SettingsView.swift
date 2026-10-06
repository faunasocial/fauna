import SwiftUI
import FaunaKit

struct SettingsView: View {
    @Environment(AppState.self) private var appState
    @Environment(FaunaClient.self) private var client: FaunaClient?
    @State private var statusVM = StatusVM()
    // The Status MLS leg's channel count is the conversations manager's own
    // (`secureChannelCount()`); iOS has no local sync agent, so its Sync leg
    // stays `nil` and the Sync section is never painted (`ui/status.md`).
    @Environment(ConversationsVM.self) private var conversationsVM
    @State private var showDeleteConfirm = false
    @State private var watchManager = IOSWatchConnectivityManager()

    /// The `NavigationStack` path derived from `selectedSettingsPage`: empty on
    /// the settings root (the page list), `[page]` on a sub-page. A computed
    /// binding (not `@State`) keeps `selectedSettingsPage` the single source of
    /// truth — a human tapping a `NavigationLink(value:)` appends to the path
    /// (→ `selectedSettingsPage` set); the system back pops it (→ `nil`); the e2e
    /// state protocol setting `selectedSettingsPage` makes the getter return
    /// `[page]` so the stack pushes it. Mirrors `AdminShellView.path`.
    private var path: Binding<[SettingsPage]> {
        Binding(
            get: { appState.selectedSettingsPage.map { [$0] } ?? [] },
            set: { appState.selectedSettingsPage = $0.last }
        )
    }

    var body: some View {
        NavigationStack(path: path) {
            // Eager `ScrollView { VStack }`, NOT `List` (`apple-e2e-
            // automation.md` rule 6: an iOS `List` realizes rows lazily, so a row
            // far enough down the page never `.onAppear`-registers. `admin-tab` sat
            // behind ~25 nav rows and could never be proven present OR absent — a
            // negative `is_absent(ADMIN_TAB)` pre-check stayed green under a
            // gate-bypass mutant that reddened the same check on macOS, because the
            // row simply never registered for anyone, gate correct or broken
            // (e2e-conventions.md convention 6's mutation-duty rider). Every other
            // `Admin*View` already uses this shape, including `AdminDashboardView`'s
            // own page-switcher `NavigationLink` list — the direct
            // precedent this mirrors. Rows lose `List`'s `.insetGrouped` inset/
            // divider/chevron chrome; `Divider()` between rows is the accepted
            // rule-6 tradeoff (same as `SnapshotListView`/`AdminDashboardView`).
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    if let handle = appState.session.handle {
                        Text("@\(handle)")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                            .accessibilityIdentifier(Ids.settingsHandleLabel)
                            // Read-only label; re-read the live handle so the
                            // in-process driver's /element/text reflects current
                            // session state (no a11y tree to walk).
                            .automationValue(Ids.settingsHandleLabel,
                                             text: { appState.session.handle.map { "@\($0)" } })
                    }

                    // Inbox-mode lives on the shared Privacy sub-page (the canonical
                    // `inbox-mode-<id>` radios in `PrivacySettingsView`, matching linux
                    // `settings/privacy.rs` + web), NOT a duplicate inline section on
                    // the Settings root — the old root section here drove a second copy
                    // of the same ids off `appState.inboxMode` directly, drift (#1/#4)
                    // removed 2026-06-14. The privacy sub-page now writes the picked
                    // mode back into `appState.inboxMode` via the `onInboxModeChanged`
                    // closure (below), so the e2e snapshot still round-trips.

                    // Settings sub-pages are pushed by value (SettingsPage) so the
                    // cross-app two-element nav `{"view":"settings"},{"view":"settings",
                    // "id":"<navId>"}` can drive them programmatically via the path
                    // binding — see `navigationDestination(for: SettingsPage.self)`.
                    // Bridges is NOT a settings sub-page (its own surface), so it stays
                    // a closure-based push.
                    VStack(alignment: .leading, spacing: 0) {
                        navRow {
                            NavigationLink(value: SettingsPage.account) {
                                Label(L.common.account, systemImage: "person.circle")
                            }
                            .accessibilityIdentifier(Ids.accountSettingsLink)
                            // Drives the same NavigationStack push the tap performs (.accessibilityIdentifier alone is invisible to the in-process
                            // AutomationRegistry, which registers presence off automation*).
                            .automationActivate(Ids.accountSettingsLink) { appState.selectedSettingsPage = .account }
                        }
                        // The permanent unattested-member review backlog (row
                        // 308) — reachable at all times, directly after Account
                        // per tui's ratified rail placement. No ui.yaml id: the
                        // page itself is reached by the e2e nav-id protocol, not
                        // a labeled row (mirrors the untagged Bridges row below).
                        navRow {
                            NavigationLink(value: SettingsPage.memberReview) {
                                Label(L.settings.memberReviewPage.title, systemImage: "person.crop.circle.badge.questionmark")
                            }
                        }
                        navRow {
                            NavigationLink {
                                BridgesView()
                            } label: {
                                Label(L.bridges.title, systemImage: "network")
                            }
                        }
                        // Nostr is its own page (like Mail), not a bridge under Bridges
                        // (nostr.md § Page structure / bridges.md § Scope, 2026-06-13).
                        navRow {
                            NavigationLink(value: SettingsPage.nostr) {
                                Label(L.nostr.title, systemImage: "antenna.radiowaves.left.and.right")
                            }
                            .accessibilityIdentifier(Ids.nostrSettingsLink)
                            .automationActivate(Ids.nostrSettingsLink) { appState.selectedSettingsPage = .nostr }
                        }
                        // Bluesky is likewise its own dedicated page (the integration-
                        // depth selector), not a bridge under Bridges (ui/atproto.md).
                        navRow {
                            NavigationLink(value: SettingsPage.atproto) {
                                Label(L.atprotoSettings.title, systemImage: "at")
                            }
                            .accessibilityIdentifier(Ids.atprotoSettingsLink)
                            .automationActivate(Ids.atprotoSettingsLink) { appState.selectedSettingsPage = .atproto }
                        }
                        navRow {
                            NavigationLink(value: SettingsPage.mail) {
                                Label(L.mailSettings.title, systemImage: "envelope")
                            }
                            .accessibilityIdentifier(Ids.mailSettingsLink)
                            .automationActivate(Ids.mailSettingsLink) { appState.selectedSettingsPage = .mail }
                        }
                        navRow {
                            NavigationLink(value: SettingsPage.mailAliases) {
                                Label(L.mailAliases.title, systemImage: "at")
                            }
                        }
                        navRow {
                            NavigationLink(value: SettingsPage.mailLists) {
                                Label(L.mailLists.title, systemImage: "list.bullet.rectangle")
                            }
                        }
                        navRow {
                            NavigationLink(value: SettingsPage.mailListMembers) {
                                Label(L.mailLists.membersTitle, systemImage: "person.2")
                            }
                        }
                        navRow {
                            NavigationLink(value: SettingsPage.mailExport) {
                                Label(L.mailExport.title, systemImage: "square.and.arrow.up")
                            }
                        }
                        navRow {
                            NavigationLink(value: SettingsPage.mailImport) {
                                Label(L.mailImport.title, systemImage: "square.and.arrow.down")
                            }
                        }
                        navRow {
                            NavigationLink(value: SettingsPage.mailSpam) {
                                Label(L.mailSpam.title, systemImage: "xmark.bin")
                            }
                        }
                        navRow {
                            NavigationLink(value: SettingsPage.web) {
                                Label(L.webSettings.title, systemImage: "globe")
                            }
                            .accessibilityIdentifier(Ids.webSettingsLink)
                            .automationActivate(Ids.webSettingsLink) { appState.selectedSettingsPage = .web }
                        }
                        // Consumer-side subscriptions ("my subscriptions across all
                        // creators"), a Settings sub-page sibling of mail/web
                        // (monetization.md § Pillar 1).
                        navRow {
                            NavigationLink(value: SettingsPage.subscriptions) {
                                Label(L.subscriptions.title, systemImage: "star")
                            }
                            .accessibilityIdentifier(Ids.subscriptionSettingsLink)
                            .automationActivate(Ids.subscriptionSettingsLink) { appState.selectedSettingsPage = .subscriptions }
                        }
                        navRow {
                            NavigationLink(value: SettingsPage.linkedNests) {
                                Label(L.linkedNests.title, systemImage: "link")
                            }
                            .accessibilityIdentifier(Ids.linkedNestsLink)
                            .automationActivate(Ids.linkedNestsLink) { appState.selectedSettingsPage = .linkedNests }
                        }
                        // The cross-participant runner + assignment surface
                        // (participants.md § Task delegation), rail-placed right
                        // after Nests per its ratified placement.
                        navRow {
                            NavigationLink(value: SettingsPage.taskDelegation) {
                                Label(L.taskDelegation.title, systemImage: "gearshape.2")
                            }
                            .accessibilityIdentifier(Ids.taskDelegationLink)
                            .automationActivate(Ids.taskDelegationLink) { appState.selectedSettingsPage = .taskDelegation }
                        }
                        // The one roster over every third-party principal
                        // (connected-apps.md), rail-placed right after Task
                        // delegation per settings.md § Navigation model. ui.yaml
                        // mints no link id for it (the e2e navigates by nav id).
                        navRow {
                            NavigationLink(value: SettingsPage.connectedApps) {
                                Label(L.connectedApps.title, systemImage: SettingsPage.connectedApps.systemImage)
                            }
                        }
                        navRow {
                            NavigationLink(value: SettingsPage.privacy) {
                                Label(L.settings.privacy, systemImage: "lock.shield")
                            }
                        }
                        // Per-keyword conversation-collapse filter (shared
                        // FaunaKit; content-moderation-and-ranking.md § Q3),
                        // sibling of Privacy per settings.md's ratified rail order.
                        navRow {
                            NavigationLink(value: SettingsPage.mutedWords) {
                                Label(L.mutedWords.title, systemImage: "speaker.slash")
                            }
                        }
                        // Feeds / Muted-words / Community-labelers hub (shared
                        // FaunaKit; content-moderation-and-ranking.md § Tier-3
                        // community models), right after Muted words per its
                        // ratified placement.
                        navRow {
                            NavigationLink(value: SettingsPage.personalization) {
                                Label(L.personalization.title, systemImage: "sparkles")
                            }
                        }
                        navRow {
                            NavigationLink(value: SettingsPage.labelerCatalog) {
                                Label(L.labelerCatalog.title, systemImage: "shield.checkerboard")
                            }
                        }
                        navRow {
                            NavigationLink(value: SettingsPage.encryption) {
                                Label(L.settings.encryptionPage.title, systemImage: "key")
                            }
                        }
                        // Devices (roster) + Folders (control plane) replaced the
                        // former single "Sync" sub-page (2026-06-28 unification). Reached
                        // inside Settings, not the top-level "More" menu (settings.md
                        // § Navigation model — Mobile).
                        navRow {
                            NavigationLink(value: SettingsPage.devices) {
                                Label(L.devices.title, systemImage: "desktopcomputer")
                            }
                            .accessibilityIdentifier(Ids.devicesSettingsLink)
                            .automationActivate(Ids.devicesSettingsLink) { appState.selectedSettingsPage = .devices }
                        }
                        navRow {
                            NavigationLink(value: SettingsPage.folders) {
                                Label(L.folders.title, systemImage: "folder")
                            }
                            .accessibilityIdentifier(Ids.foldersSettingsLink)
                            .automationActivate(Ids.foldersSettingsLink) { appState.selectedSettingsPage = .folders }
                        }
                        navRow {
                            NavigationLink(value: SettingsPage.general) {
                                // Canonical rail label is "General" (settings.md § Navigation
                                // model, ratified — "Status · Account · Privacy · General · …";
                                // = macOS SettingsPage.general.label + linux settings::GENERAL).
                                // iOS's old "Notifications" was the lone divergence (#4).
                                Label(L.settings.general, systemImage: "bell.badge")
                            }
                        }
                        // The client's durable log record — pushed by value (like the
                        // other sub-pages) so the cross-app {"view":"settings",
                        // "id":"logs"} nav routes it via settingsDestination(.logs).
                        navRow(last: true) {
                            NavigationLink(value: SettingsPage.logs) {
                                Label(L.logs.title, systemImage: "doc.text.magnifyingglass")
                            }
                        }
                    }

                    Divider()

                    VStack(alignment: .leading, spacing: 0) {
                        navRow {
                            NavigationLink {
                                SnapshotListView()
                            } label: {
                                Label(L.backups.title, systemImage: "clock.arrow.circlepath")
                            }
                        }
                        navRow(last: true) {
                            NavigationLink(value: SettingsPage.status) {
                                Label(L.common.status, systemImage: "info.circle")
                            }
                        }
                    }

                    // The gated admin entry (admin.md § Navigation model step 1):
                    // shown only when the shared `am-i-admin` gate passes
                    // (`appState.isAdmin`) so non-admins do not see it (admin.md:19),
                    // fail-closed and refreshed in `.task`/`.onReconnect` below.
                    // Mirrors web's `{#if userIsAdmin}` admin-tab + the macOS gated
                    // sidebar row. Opens the admin shell as a top-level peer, not a
                    // NavigationLink nested under Settings, so `admin-nav-back` exits
                    // to the primary view (Conversations), not back through Settings.
                    if appState.isAdmin {
                        Divider()
                        navRow(last: true) {
                            Button {
                                enterAdmin()
                            } label: {
                                Label(L.settings.nestAdmin, systemImage: "shield.lefthalf.filled")
                            }
                            .tint(.primary)
                            .accessibilityIdentifier(Ids.adminTab)
                            // Genuine tappable Button (sets `inAdmin`); fires the same
                            // `enterAdmin()` the Button action does, and the registration
                            // gives the in-process driver the presence it needs for
                            // `is_visible` (test_admin_tab_visible_for_admin — the gated
                            // entry only appears for an admin actor).
                            .automationActivate(Ids.adminTab) { enterAdmin() }
                        }
                    }

                    // The gated family entry (family-safety.md § App surface;
                    // ui.yaml navigation.gated_tabs — "mobile: in-Settings entry",
                    // the same placement `admin-tab` takes above). Shown only when
                    // `fauna.family.status` returned a relationship (guardian OR
                    // supervised); fail-closed, refreshed at the app root.
                    if appState.familyStatus.hasRelationship {
                        Divider()
                        navRow(last: true) {
                            Button {
                                enterFamily()
                            } label: {
                                Label(L.family.title, systemImage: "figure.2.and.child.holdinghands")
                            }
                            .tint(.primary)
                            .accessibilityIdentifier(Ids.familyTab)
                            .automationActivate(Ids.familyTab) { enterFamily() }
                        }
                    }
                }
                .padding()
            }
            .navigationDestination(for: SettingsPage.self) { page in
                settingsDestination(page)
            }
            .pageTitle(L.common.settings)
            // Watch connectivity is install-scoped, not account-scoped: it stays on
            // its own one-shot task so the session-keyed one below can re-fire freely.
            .task { watchManager.activate() }
            // Keyed on the session's client, not one-shot: this is the Settings
            // **root**, and the switch teardown clears `selectedSettingsPage` (popping
            // the value-based sub-pages) while `moreSelectedView` stays `"settings"` —
            // so the root itself is never unmounted and the outgoing account's quota
            // and feature limits would stay rendered under the incoming one.
            // `account-scoping.md` § The scoping taxonomy, the "reused shell" case.
            .task(id: SessionKey(client)) {
                if let client {
                    statusVM.configure(api: client.api)
                    if let actorId = appState.session.actorId {
                        // A failed fetch leaves `inboxMode` at whatever it
                        // already was (`nil` on a fresh mount) — never a
                        // guessed default (settings.md § Privacy sub-page
                        // item 6).
                        if let mode = try? await client.api.getInboxMode(actorId: actorId) {
                            appState.inboxMode = mode
                        }
                    }
                } else {
                    statusVM.reset()
                    return
                }
                await statusVM.refreshQuotaAndLimits()
                await loadStatusLegs()
            }
            // NOTE: the `am-i-admin` gate is NOT probed here. It lives at the app
            // root (`ContentView.refreshAdminStatus`), matching macOS — probing it
            // from inside Settings raced the Account page it mutates, because the
            // auto-default's registry write could land while that page was already
            // rendering the pre-write row. `appState.isAdmin` is therefore already
            // resolved by the time this view mounts.
            // Re-pull account quota + feature limits on WS-RPC reconnect (mirrors linux).
            .onReconnect {
                await statusVM.refreshQuotaAndLimits()
                await loadStatusLegs()
            }
        }
    }

    /// The Status snapshot's node and MLS legs — one nest read each per visit /
    /// reconnect (`ui/status.md` § State & data shape).
    private func loadStatusLegs() async {
        let manager = conversationsVM.manager
        await statusVM.fetchStatusLegs(
            actorId: appState.session.actorId,
            secureChannelCount: { manager.secureChannelCount() })
    }

    /// Uniform row chrome for the eager `ScrollView { VStack }` layout above (row
    /// 384) — a touch-target-sized vertical pad plus a trailing `Divider()`,
    /// replacing the row/divider chrome a `List` gave for free. `last: true` skips
    /// the divider for a group's final row (its own group already gets a trailing
    /// `Divider()`/gap from the caller).
    @ViewBuilder
    private func navRow<Content: View>(last: Bool = false, @ViewBuilder _ content: () -> Content) -> some View {
        content()
            .padding(.vertical, 10)
        if !last {
            Divider()
        }
    }

    /// Open the admin shell as a top-level peer (admin.md § Navigation model
    /// step 1). Shared by the `admin-tab` Button action and its `automationActivate`
    /// closure so the human-tap and the in-process driver exercise the same path.
    private func enterAdmin() {
        appState.selectedAdminPage = .dashboard
        appState.inAdmin = true
    }

    /// Open the shared Family page. Settings is itself a "More" sub-view, so
    /// retargeting `moreSelectedView` swaps the More stack's content to the
    /// Family page — the same destination `{"view":"family"}` and the global
    /// `supervised-indicator` reach. Shared by the `family-tab` Button action and
    /// its `automationActivate` closure so a human tap and the in-process driver
    /// exercise one path.
    private func enterFamily() {
        appState.selectedSettingsPage = nil
        appState.moreSelectedView = "family"
    }

    /// Render the iOS destination for a settings sub-page pushed by value. The
    /// page *set* is the shared cross-app `SettingsPage` contract; iOS renders
    /// the 17 sub-pages it has surfaces for (the shared FaunaKit `Mail*`/`Web`/
    /// `Subscriptions`/`LinkedNests`/`TaskDelegation`/`Logs` views + the iOS
    /// `*SettingsView` panes).
    /// `subscription-settings` renders the shared FaunaKit `SubscriptionSettingsView`
    /// (the consumer "my subscriptions" page — monetization.md § Pillar 1). `nests`
    /// renders the shared FaunaKit `LinkedNestsView` (same view macOS uses; only
    /// the nav idiom differs — a `NavigationLink` sub-page here, an inline rail
    /// pane on macOS), reached from the `linked-nests-link` settings row.
    /// `task-delegation` renders the shared FaunaKit `TaskDelegationView` (the
    /// cross-participant runner + assignment surface, participants.md § Task
    /// delegation), reload-on-every-visit via `appState.navGeneration` since the
    /// runner column is live advisory-lease state. `logs` renders the shared
    /// FaunaKit `LogsView` (observability.md § Surfaces).
    /// `nostr` renders the shared FaunaKit `NostrSettingsView` (its own dedicated
    /// page, not folded into Bridges — nostr.md § Page structure). `member-review`
    /// renders the shared FaunaKit `MemberReviewView` (the permanent
    /// unattested-member review backlog, reachable at all times). (There is no
    /// `p2p` case at all: neither Apple app has a p2p page surface per
    /// `p2p.md` § Implementation status + `ui.yaml`'s p2p page notes — the
    /// macOS-only `PeerContactsView` cluster was deleted 2026-07-13. `photo-backup`
    /// has no iOS *settings* surface either — iOS surfaces the shared
    /// `PhotoBackupControlsView` on the Media tab; the rail sub-page is macOS-only.)
    @ViewBuilder
    private func settingsDestination(_ page: SettingsPage) -> some View {
        switch page {
        case .status: StatusDetailView(vm: statusVM)
        case .memberReview:
            // The permanent unattested-member review backlog (shared
            // FaunaKit; row 308, succession-aftermath.md § Propagation
            // item (iv)) — reachable at all times, not just after a
            // succession.
            MemberReviewView()
        case .account:
            AccountSettingsView(
                session: appState.session,
                onAccountReset: {
                    appState.onSignOut?()
                    watchManager.sendSignOut()
                    appState.isOnboarding = true
                },
                onSwitchAccount: { try await appState.onSwitchAccount?($0, $1) },
                onAddAccount: { appState.onAddAccount?() }
            )
        case .privacy: PrivacySettingsView(onInboxModeChanged: { appState.inboxMode = $0 })
        case .mutedWords: MutedWordsView()
        case .personalization:
            PersonalizationView(
                onNavigateFeed: {
                    appState.selectedSettingsPage = nil
                    appState.selectedTab = "feed"
                },
                onNavigateMutedWords: { appState.selectedSettingsPage = .mutedWords },
                onNavigateCatalog: { appState.selectedSettingsPage = .labelerCatalog }
            )
        case .labelerCatalog: LabelerCatalogView()
        case .general: NotificationSettingsView()
        case .encryption: EncryptionSettingsView()
        case .devices: DevicesView(reloadToken: appState.navGeneration)  // roster (DevicesContent)
        case .folders: FoldersView(reloadToken: appState.navGeneration)  // control plane (FoldersContent)
        case .mail: MailSettingsView()
        case .mailAliases: MailAliasesView()
        case .mailSpam: MailSpamView()
        case .mailExport: MailExportView()
        case .mailImport:
            // The foreign-mailbox migration wizard (shared FaunaKit;
            // mailbox-migration.md). Its two Done deep-links go to Conversations,
            // where mail lives — the tui lead app's own resolution, since no
            // "view inbox" / "skip log" RPC exists.
            MailImportView(onNavigateToConversations: {
                appState.selectedSettingsPage = nil
                appState.selectedTab = "conversations"
            })
        case .mailLists:
            MailListsView(onNavigateToMembers: { listIdHex, friendlyName in
                appState.selectedMailListId = listIdHex
                appState.selectedMailListName = friendlyName
                appState.selectedSettingsPage = .mailListMembers
            })
        case .mailListMembers:
            if let listIdHex = appState.selectedMailListId, let name = appState.selectedMailListName {
                MailListMembersView(listIdHex: listIdHex, listName: name)
            } else {
                MailListMembersView()
            }
        case .web: WebSettingsView(webPublish: appState.webPublish, reloadToken: appState.navGeneration)
        case .subscriptions: SubscriptionSettingsView()  // consumer "my subscriptions" page (shared FaunaKit)
        case .linkedNests: LinkedNestsView(reloadToken: appState.navGeneration)
        case .taskDelegation: TaskDelegationView(reloadToken: appState.navGeneration)
        case .connectedApps: ConnectedAppsView(reloadToken: appState.navGeneration)  // shared FaunaKit roster (connected-apps.md)
        case .logs: LogsView(source: .clientRing, headingId: Ids.pageHeading)
        case .nostr: NostrSettingsView()  // shared FaunaKit standalone Nostr page
        case .atproto: AtprotoSettingsView()  // shared FaunaKit standalone AT Protocol page (ui/atproto.md)
        }
    }
}

/// Inline status view used within Settings (replaces the old tab)
struct StatusDetailView: View {
    let vm: StatusVM
    @Environment(AppState.self) private var appState
    @State private var watchManager = IOSWatchConnectivityManager()

    var body: some View {
        // Eager `ScrollView { VStack { GroupBox } }`, NOT `List`/`Form`
        // (`apps/apple-e2e-automation.md` rule 6 — an iOS `List` is lazy, so
        // an off-screen section/row never `.onAppear`-registers; mirrors
        // `MacStatusView` and every `Admin*View`). Converted 2026-08-25 (row
        // 122) when the feature-limits section, appended after Quota, pushed
        // p2p-share's row off-screen and the driver 404'd on it under a List.
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                GroupBox(L.common.identity) {
                    VStack(alignment: .leading, spacing: 8) {
                        if let actorId = appState.session.actorId {
                            HStack {
                                Text(L.common.actorId)
                                Spacer()
                                // Live-data placement: the Status sub-page carries
                                // `account-actor-id` (settings.md:20) so a plain Settings
                                // `navigate()` (lands here) keeps `test_actor_id_visible`
                                // green — mirrors macOS `MacStatusView`. Leaf id only;
                                // `automationValue` exposes the full actorId regardless
                                // of the shortened visual display.
                                Text(shortId(hex: actorId))
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                    .accessibilityIdentifier(Ids.accountActorId)
                                    .automationValue(Ids.accountActorId, text: { actorId })
                                CopyButton(Ids.statusActorIdCopyBtn, text: actorId)
                            }
                        }
                        if let nodeUrl = appState.session.nodeUrl {
                            HStack {
                                Text(L.status.node.title)
                                Spacer()
                                Text(nodeUrl)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                CopyButton(Ids.statusNodeUrlCopyBtn, text: nodeUrl)
                            }
                        }
                        if let deviceId = appState.session.deviceId {
                            HStack {
                                Text(L.common.deviceId)
                                Spacer()
                                Text(shortId(hex: deviceId))
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                    .padding(8)
                }

                if let quota = vm.quota {
                    GroupBox(L.common.storage) {
                        VStack(alignment: .leading, spacing: 8) {
                            QuotaBar(label: L.common.storage, used: quota.storage.usedBytes, max: quota.storage.maxBytes)
                                .accessibilityIdentifier(Ids.quotaStorage)
                                // Read-only quota rows; re-read live `quota.*` so the
                                // in-process driver's /element/text reflects current usage.
                                // Mirrors MacStatusView's per-row automationValue reads.
                                .automationValue(Ids.quotaStorage,
                                                 text: { "\(quota.storage.usedBytes) / \(quota.storage.maxBytes)" })
                            QuotaBar(label: L.common.inbox, used: quota.inbox.usedBytes, max: quota.inbox.maxBytes)
                                .accessibilityIdentifier(Ids.quotaInbox)
                                .automationValue(Ids.quotaInbox,
                                                 text: { "\(quota.inbox.usedBytes) / \(quota.inbox.maxBytes)" })
                            HStack {
                                Text(L.common.devices)
                                Spacer()
                                Text("\(quota.devices.used) / \(quota.devices.max)")
                                    .foregroundStyle(.secondary)
                            }
                            .accessibilityIdentifier(Ids.quotaDevices)
                            .automationValue(Ids.quotaDevices,
                                             text: { "\(quota.devices.used) / \(quota.devices.max)" })
                        }
                        .padding(8)
                    }
                    .accessibilityIdentifier(Ids.quotaSection)
                    // `.contain` keeps the per-row child ids queryable alongside the
                    // container id; the read exposes the tier (mirrors MacStatusView).
                    .accessibilityElement(children: .contain)
                    .automationValue(Ids.quotaSection, text: { quota.tier })
                }

                // Feature limits — placed directly after Quota as its sibling
                // "what bounds me" surface (settings.md § Layout & flow item
                // 2b, dynamic-features.md § Transparency & auditability).
                // `.contain` mirrors the quota-section container-clobber rule.
                if let featureRows = vm.featureRows {
                    GroupBox(L.features.sectionTitle) {
                        FeatureLimitsSection(rows: featureRows)
                            .padding(8)
                    }
                    .accessibilityIdentifier(Ids.featureLimitsSection)
                    .accessibilityElement(children: .contain)
                    .automationValue(Ids.featureLimitsSection, text: { L.features.sectionTitle })
                }

                // Region — the region content plane's transparency surface
                // (region-blocking.md § The blocked render and the transparency
                // surface), after feature limits as on macOS, linux and web.
                GroupBox(L.region.sectionTitle) {
                    RegionSettingsSection(region: appState.region)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(8)
                }
                .accessibilityIdentifier(Ids.settingsRegionSection)
                .accessibilityElement(children: .contain)
                .automationValue(Ids.settingsRegionSection, text: { L.region.sectionTitle })

                // Node, Encryption and Build — the shared snapshot's legs
                // (`ui/status.md` § Layout & flow sections 5–8), the same shared
                // view `MacStatusView` paints; iOS carries no Sync leg.
                StatusLegSections(text: vm.sectionText())

                // The "Backup status" entry retired with the B2 engine cutover. It was an
                // iOS-only, unspecced page (no ui.yaml IDs, no peer on any other app)
                // that listed SwiftData `SyncAnchor` rows — the per-set cursor the deleted
                // Swift engine wrote. iOS binds no folders, so post-cutover it would list
                // nothing at all. Per-file sync state now surfaces where it belongs: on the
                // Media page's `sync-state-badge` (file-sync.md § Per-file sync-status
                // display). Photo-backup configuration lives in Settings → Folders.

                GroupBox(L.common.actions) {
                    VStack(alignment: .leading, spacing: 8) {
                        Button(L.status.actions.clearCache) {
                            vm.clearCache()
                        }
                    }
                    .padding(8)
                }

                if watchManager.isPaired {
                    GroupBox("Apple Watch") {
                        VStack(alignment: .leading, spacing: 8) {
                            if watchManager.isWatchAppInstalled {
                                Button("Send Credentials to Watch") {
                                    if let secret = appState.session.secretHex,
                                       let nodeUrl = appState.session.nodeUrl {
                                        let handle = "" // TODO: get from session
                                        watchManager.sendCredentials(
                                            secret: secret,
                                            nodeUrl: nodeUrl,
                                            handle: handle
                                        )
                                    }
                                }
                            } else {
                                Text("Fauna Watch app not installed")
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .padding(8)
                    }
                }
            }
            .padding()
        }
        .navigationTitle(L.common.status)
        .task { watchManager.activate() }
    }
}
