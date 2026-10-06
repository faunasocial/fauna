import SwiftUI
import FaunaKit

/// The admin Dashboard page — headline nest metrics from `fauna.admin.stats`
/// (+ `fauna.nest.info` for the version), admin.md § 1 Dashboard. Session-authed
/// over the FFI-backed `AdminVM` (no admin token, no `/admin/api/*`). The user
/// list / invite minting / pending requests moved to the consolidated
/// `admin-users` hub (`AdminUsersHubView`); the legacy audit log was dropped
/// (not in the § 1 spec).
struct AdminDashboardView: View {
    @Bindable var vm: AdminVM
    var reloadToken: Int = 0

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                HStack {
                    automationText(Ids.adminDashboardHeading, L.admin.dashboard.nestDashboard)
                        .font(.title)
                    Spacer()
                    Button {
                        Task { await vm.loadDashboard() }
                    } label: {
                        Image(systemName: "arrow.clockwise")
                    }
                }

                if let stats = vm.stats {
                    statsSection(stats)
                } else if vm.isLoading {
                    ProgressView(L.admin.dashboard.loadingDashboard)
                        .frame(maxWidth: .infinity)
                }

                if let error = vm.errorMessage {
                    ErrorBanner(message: error)
                }
            }
            .padding(24)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task(id: reloadToken) { await vm.loadDashboardUntilReady() }
    }

    private func statsSection(_ stats: FfiAdminStats) -> some View {
        // Stat *definitions* are shared with iOS via `dashboardStatItems` so an
        // add/relabel/reorder/recolor lands once (admin.md § 1); macOS keeps its
        // own card-grid layout shell (a phone-wrong 4-col grid).
        LazyVGrid(columns: [
            GridItem(.flexible()),
            GridItem(.flexible()),
            GridItem(.flexible()),
            GridItem(.flexible()),
        ], spacing: 16) {
            ForEach(dashboardStatItems(stats: stats, version: vm.version)) { item in
                StatCard(item: item)
            }
        }
    }
}

private struct StatCard: View {
    let item: DashboardStatItem

    var body: some View {
        VStack(spacing: 8) {
            Image(systemName: item.systemImage)
                .font(.title2)
                .foregroundStyle(item.color)
            automationText(Ids.adminStatCardValue, item.value)
                .font(.title3.bold())
            automationText(Ids.adminStatCardLabel, item.label)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
        .padding(16)
        .background(.background.secondary)
        .clipShape(RoundedRectangle(cornerRadius: 12))
        // `.contain` so the child `admin-stat-card-value`/`-label` ids survive
        // instead of being overwritten by the card's own id — the e2e
        // `dashboard_card_value` counts `admin-stat-card-label`, which returned 0
        // (→ empty card values) without this. Mirrors `device-card` in
        // DevicesContent (memory: apple-section-accessibilityid-clobbers-children).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminStatCard)
        // Per-card read so the driver can count/address cards (indexed registry
        // entry per ForEach row); exposes the card's value, mirroring the e2e
        // `dashboard_card_value` read.
        .automationValue(Ids.adminStatCard, text: { item.value })
    }
}
