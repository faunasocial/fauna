import SwiftUI
import FaunaKit

/// The admin Dashboard page — the shared card set (`admin_dashboard_snapshot()`,
/// admin.md § 1. Dashboard → *The card set*), rendered as a card grid. Session-authed
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

                if let snapshot = vm.dashboard {
                    statsSection(snapshot)
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

    private func statsSection(_ snapshot: FfiAdminDashboardSnapshot) -> some View {
        // The cards are the shared fold's (admin.md § 1. Dashboard → *The card
        // set*); macOS keeps only its own card-grid layout shell (a phone-wrong
        // 4-col grid).
        //
        // Every card in a row is top-aligned and reserves the same lines for its
        // value and label, so a long value (the Mail card's "Mail: mail service
        // not connected") wraps inside its reserved lines instead of making its
        // card taller. A taller card re-centred its row-mates, so the values'
        // and the labels' on-screen order (what the indexed
        // `admin-stat-card-value[i]`/`-label[i]` reads follow) stopped agreeing.
        LazyVGrid(columns: Array(
            repeating: GridItem(.flexible(), alignment: .top), count: 4
        ), spacing: 16) {
            ForEach(dashboardStatItems(snapshot)) { item in
                StatCard(item: item)
            }
        }
    }
}

private struct StatCard: View {
    let item: DashboardStatItem

    var body: some View {
        VStack(spacing: 8) {
            automationText(Ids.adminStatCardValue, item.value)
                .font(.title3.bold())
                .multilineTextAlignment(.center)
                .lineLimit(3, reservesSpace: true)
            automationText(Ids.adminStatCardLabel, item.label)
                .font(.caption)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .lineLimit(2, reservesSpace: true)
        }
        .frame(maxWidth: .infinity, alignment: .top)
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
