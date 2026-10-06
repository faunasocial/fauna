import SwiftUI
import FaunaKit

/// The macOS admin shell content pane. Owns one shared `AdminVM` (FFI-backed,
/// `fauna.admin.*`) and renders the page selected in the admin nav rail
/// (`AdminNavRail`, the sidebar-swap). The page set + nav are admin.md
/// § Navigation model; the consolidated `admin-users` hub is the shared FaunaKit
/// `AdminUsersHubView` (priority #1/#2 — iOS reuses it).
///
/// All pages are built: `tiers` (`AdminTiersView` — tier definitions + in-place
/// cap editing) and `nest` (`AdminNestView` — storage-mode indicator + operator
/// pairing toggle + Factory Reset) landed 2026-06-13 (Track 2); `mail` (the flat
/// admin-mail policy form) built Bundle B (2026-06-12). The shared FaunaKit page
/// views are reused by iOS (priority #2). The Nest page's Factory Reset re-onboard
/// is delegated to the App via `appState.onFactoryReset` (the re-seed touches
/// App-owned onboarding state).
struct AdminShellView: View {
    @State private var vm = AdminVM()
    @Environment(MacAppState.self) private var appState
    @Environment(FaunaClient.self) private var client: FaunaClient?

    var body: some View {
        Group {
            switch appState.selectedAdminPage {
            case .dashboard:
                AdminDashboardView(vm: vm, reloadToken: appState.navGeneration)
            case .users:
                AdminUsersHubView(vm: vm, reloadToken: appState.navGeneration)
            case .bridgesPending:
                AdminBridgesPendingView(reloadToken: appState.navGeneration)
            case .custodyHosting:
                AdminCustodyHostingView(reloadToken: appState.navGeneration)
            case .aliases:
                AdminAliasesView(reloadToken: appState.navGeneration)
            case .dns:
                AdminDnsView(reloadToken: appState.navGeneration)
            case .web:
                AdminWebView(reloadToken: appState.navGeneration)
            case .mail:
                AdminMailView(reloadToken: appState.navGeneration)
            case .calendar:
                AdminCalendarView(reloadToken: appState.navGeneration)
            case .contacts:
                AdminContactsView(reloadToken: appState.navGeneration)
            case .files:
                AdminFilesView(reloadToken: appState.navGeneration)
            case .tiers:
                AdminTiersView(reloadToken: appState.navGeneration)
            case .nest:
                // The Factory Reset re-onboard (drop session keeping creds +
                // re-seed onboarding at claim-code with the returned code
                // pre-filled) touches the App-owned onboarding state, so the
                // shared FaunaKit view delegates it via this callback, wired by
                // FaunaMacApp through `appState.onFactoryReset`.
                AdminNestView(reloadToken: appState.navGeneration) { claimCode in
                    appState.onFactoryReset?(claimCode)
                }
            case .adminLogs:
                // The nest's `fauna_log` ring over `fauna.admin.logs` — same
                // shared `LogsView` as the client Logs page, no Clear
                // (observability.md § Surfaces).
                LogsView(
                    source: .admin(load: { try await client?.api.adminClient().logs() ?? [] }),
                    headingId: Ids.adminLogsHeading
                )
            }
        }
        // Configure the VM once the client env is available, keyed on its
        // presence so it re-runs if the client arrives after first appear (the
        // shell is recreated by ContentView's `.id(selectedSidebar)`); a one-shot
        // task could fire before the client is wired and never retry, leaving
        // every admin load failing "AdminVM not configured".
        .task(id: client != nil) {
            if let api = client?.api { vm.configure(api: api) }
        }
    }
}
