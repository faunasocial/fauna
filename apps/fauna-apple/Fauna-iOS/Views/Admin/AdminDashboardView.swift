import SwiftUI
import FaunaKit

/// iOS admin Dashboard page — the root of the `AdminShellView` `NavigationStack`.
/// It is both the headline-metrics page (`fauna.admin.stats` + `fauna.nest.info`,
/// admin.md § 1 Dashboard) and the mobile **page switcher**: the page list pushes
/// each shared FaunaKit admin page via `NavigationLink(value: AdminPage)`, which
/// the shell's `navigationDestination(for:)` renders (admin.md:29 — mobile keeps
/// idiomatic nav; the page set + IDs are uniform, only the switcher widget
/// differs from the desktop rail). Session-authed over the FFI-backed `AdminVM`
/// (no admin token, no `/admin/api/*`); the VM is owned + configured by the shell.
///
/// **`ScrollView { VStack }`, not `List`** (moved 2026-09-11, row 313):
/// `apple-e2e-automation.md` rule 6 — an iOS `List`/`Form` realizes rows lazily,
/// so a row far enough down the page never `.onAppear`-registers, and the driver
/// can't tell "not built" from "not landed". Adding the chunk-reseal stat pair
/// pushed `connections` and the per-tier rows past whatever the simulator
/// realized and surfaced exactly that: the new cards' labels never registered.
/// Every other `Admin*View` (shared FaunaKit) already uses this eager shape.
struct AdminDashboardView: View {
    @Bindable var vm: AdminVM
    var reloadToken: Int = 0

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                Text(L.admin.dashboard.nestDashboard)
                    .font(.title2.bold())
                    .accessibilityIdentifier(Ids.adminDashboardHeading)
                    // Read-only heading (static i18n constant); mirrors macOS's
                    // automationText("admin-dashboard-heading", …) read.
                    .automationValue(Ids.adminDashboardHeading,
                                     text: { L.admin.dashboard.nestDashboard })

                // Page switcher — pushes the shared FaunaKit page (admin.md § Navigation
                // model step 2; the mobile peer of the macOS `AdminNavRail`). `.dashboard`
                // is the root, so it is excluded. IDs match the macOS rail rows + linux
                // Stack child names (`admin-nav-<navId>`).
                VStack(alignment: .leading, spacing: 4) {
                    ForEach(AdminPage.built.filter { $0 != .dashboard }) { page in
                        NavigationLink(value: page) {
                            Label(page.label, systemImage: page.systemImage)
                        }
                        .accessibilityIdentifier("admin-nav-\(page.navId)")
                    }
                }

                if let stats = vm.stats {
                    // Stat *definitions* (set, order, labels, formatting, colors)
                    // are shared with macOS via `dashboardStatItems` so a change
                    // lands once (admin.md § 1); iOS keeps its own stat-row list
                    // layout shell. The per-tier breakdown is folded into the same
                    // list (each tier is a stat card, matching the macOS grid)
                    // instead of a separate "Users by Tier" section.
                    VStack(alignment: .leading, spacing: 0) {
                        Text(L.admin.dashboard.title)
                            .font(.headline)
                        ForEach(dashboardStatItems(stats: stats, version: vm.version)) { item in
                            statRow(item)
                            Divider()
                        }
                    }
                } else if vm.isLoading {
                    ProgressView(L.admin.dashboard.loadingDashboard)
                        .frame(maxWidth: .infinity)
                }

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
            .padding()
        }
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button {
                    Task { await vm.loadDashboard() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
            }
        }
        .task(id: reloadToken) { await vm.loadDashboardUntilReady() }
    }

    private func statRow(_ item: DashboardStatItem) -> some View {
        HStack {
            Label(item.label, systemImage: item.systemImage)
                .foregroundStyle(item.color)
                .accessibilityIdentifier(Ids.adminStatCardLabel)
                // Per-row read of the stat label (the e2e dashboard_card_value
                // counts admin-stat-card-label); mirrors macOS's automationText.
                .automationValue(Ids.adminStatCardLabel, text: { item.label })
            Spacer()
            automationText(Ids.adminStatCardValue, item.value)
                .font(.subheadline.weight(.medium))
        }
        .padding(.vertical, 8)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminStatCard)
        // Per-card read so the driver can count/address cards (indexed registry
        // entry per ForEach row); exposes the card's value (mirrors macOS StatCard).
        .automationValue(Ids.adminStatCard, text: { item.value })
    }
}
