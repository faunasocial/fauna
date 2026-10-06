import SwiftUI
import FaunaKit

/// The iOS admin shell — the mobile peer of the macOS `AdminShellView` +
/// `AdminNavRail` (admin.md § Navigation model). Admin is a **top-level nav
/// peer**: `AppState.inAdmin` shows this in place of the main `TabView`, and
/// `admin-nav-back` exits to the primary view (Conversations) — not the Settings
/// shell (admin.md:30, corrected 2026-06-07).
///
/// Mobile keeps idiomatic nav (admin.md:29 — "there is no persistent sidebar to
/// swap"): a `NavigationStack` whose path is bound to `appState.selectedAdminPage`
/// so the dashboard's page list pushes a page (human), and the e2e state protocol
/// (`nav.stack[1].id` → `selectedAdminPage`) pushes it programmatically. The page
/// *content* is the shared FaunaKit `Admin*View` set (priority #2 — the same
/// views macOS renders in its rail); only this switcher widget differs. The
/// system back returns to the dashboard switcher (idiomatic inter-page return);
/// `admin-nav-back` (present on every page) leaves the shell entirely.
///
/// Owns one shared `AdminVM` (FFI-backed, `fauna.admin.*`) passed to the
/// dashboard + users hub, configured once the `FaunaClient` env is available
/// (keyed on its presence so it re-runs if the client arrives after first
/// appear — a one-shot `.task` could fire before the client is wired and never
/// retry, leaving every admin load failing "AdminVM not configured").
struct AdminShellView: View {
    @State private var vm = AdminVM()
    @Environment(AppState.self) private var appState
    @Environment(FaunaClient.self) private var client: FaunaClient?

    /// The `NavigationStack` path derived from `selectedAdminPage`: empty on the
    /// dashboard (the root), `[page]` on any sub-page. A computed binding (not
    /// `@State`) keeps it the single source of truth — no two-way `onChange`
    /// feedback loop. A human tapping a `NavigationLink(value:)` appends to the
    /// path (→ `selectedAdminPage` set); the system back pops it (→ `.dashboard`);
    /// the e2e setting `selectedAdminPage` makes the getter return `[page]` so the
    /// stack pushes it.
    private var path: Binding<[AdminPage]> {
        Binding(
            get: { appState.selectedAdminPage == .dashboard ? [] : [appState.selectedAdminPage] },
            set: { appState.selectedAdminPage = $0.last ?? .dashboard }
        )
    }

    var body: some View {
        NavigationStack(path: path) {
            AdminDashboardView(vm: vm, reloadToken: appState.navGeneration)
                .toolbar { adminNavBackItem(placement: .navigationBarLeading) }
                .navigationDestination(for: AdminPage.self) { page in
                    pageView(page)
                        // System back (leading) returns to the dashboard switcher;
                        // admin-nav-back (trailing) leaves the shell — both present
                        // on every sub-page (admin.md:30).
                        .toolbar { adminNavBackItem(placement: .navigationBarTrailing) }
                }
        }
        .task(id: client != nil) {
            if let api = client?.api { vm.configure(api: api) }
        }
    }

    @ToolbarContentBuilder
    private func adminNavBackItem(placement: ToolbarItemPlacement) -> some ToolbarContent {
        ToolbarItem(placement: placement) {
            Button {
                appState.leaveAdmin()
            } label: {
                Label(L.admin.exit, systemImage: "chevron.left")
            }
            .accessibilityIdentifier(Ids.adminNavBack)
            // Tappable Button (the driver clicks admin-nav-back to leave the
            // shell); fire the same appState.leaveAdmin() the action does
            // (mirrors macOS SidebarView's admin-nav-back wiring).
            .automationActivate(Ids.adminNavBack) { appState.leaveAdmin() }
        }
    }

    /// Render the shared FaunaKit page for `page`. The dashboard is the stack root
    /// (never a destination — the path binding maps `.dashboard` to an empty
    /// path), so it is omitted here. The Nest page's Factory Reset re-onboard
    /// touches App-owned onboarding state, so the shared view delegates it via the
    /// `onFactoryReset` closure wired by `FaunaApp` through `appState.onFactoryReset`.
    @ViewBuilder
    private func pageView(_ page: AdminPage) -> some View {
        switch page {
        case .dashboard:
            AdminDashboardView(vm: vm, reloadToken: appState.navGeneration)
        case .users:
            AdminUsersHubView(vm: vm, reloadToken: appState.navGeneration)
                .navigationTitle(page.label)
                .navigationBarTitleDisplayMode(.inline)
        case .tiers:
            AdminTiersView(reloadToken: appState.navGeneration)
                .navigationTitle(page.label)
                .navigationBarTitleDisplayMode(.inline)
        case .nest:
            AdminNestView(reloadToken: appState.navGeneration) { claimCode in
                appState.onFactoryReset?(claimCode)
            }
            .navigationTitle(page.label)
            .navigationBarTitleDisplayMode(.inline)
        case .mail:
            AdminMailView(reloadToken: appState.navGeneration)
                .navigationTitle(page.label)
                .navigationBarTitleDisplayMode(.inline)
        case .calendar:
            AdminCalendarView(reloadToken: appState.navGeneration)
                .navigationTitle(page.label)
                .navigationBarTitleDisplayMode(.inline)
        case .contacts:
            AdminContactsView(reloadToken: appState.navGeneration)
                .navigationTitle(page.label)
                .navigationBarTitleDisplayMode(.inline)
        case .files:
            AdminFilesView(reloadToken: appState.navGeneration)
                .navigationTitle(page.label)
                .navigationBarTitleDisplayMode(.inline)
        case .aliases:
            AdminAliasesView(reloadToken: appState.navGeneration)
                .navigationTitle(page.label)
                .navigationBarTitleDisplayMode(.inline)
        case .dns:
            AdminDnsView(reloadToken: appState.navGeneration)
                .navigationTitle(page.label)
                .navigationBarTitleDisplayMode(.inline)
        case .web:
            AdminWebView(reloadToken: appState.navGeneration)
                .navigationTitle(page.label)
                .navigationBarTitleDisplayMode(.inline)
        case .bridgesPending:
            AdminBridgesPendingView(reloadToken: appState.navGeneration)
                .navigationTitle(page.label)
                .navigationBarTitleDisplayMode(.inline)
        case .custodyHosting:
            AdminCustodyHostingView(reloadToken: appState.navGeneration)
                .navigationTitle(page.label)
                .navigationBarTitleDisplayMode(.inline)
        case .adminLogs:
            // The nest's `fauna_log` ring over `fauna.admin.logs` — same shared
            // `LogsView` as the client Logs page (observability.md § Surfaces).
            LogsView(
                source: .admin(load: { try await client?.api.adminClient().logs() ?? [] }),
                headingId: Ids.adminLogsHeading
            )
            .navigationTitle(page.label)
            .navigationBarTitleDisplayMode(.inline)
        }
    }
}
