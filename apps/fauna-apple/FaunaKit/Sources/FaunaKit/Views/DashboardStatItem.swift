import SwiftUI

/// One `admin-stat-card` on the admin Dashboard (admin.md § 1. Dashboard →
/// *The card set*), its label and value already resolved for display.
///
/// The card set itself — which cards, in what order, with what labels and
/// values — is the shared-Rust fold `admin_dashboard_snapshot()` (the five
/// reads, once, for all 7 apps); Apple chooses none of its own. The macOS card
/// grid and the iOS stat-row list are each platform's layout *shell* over this
/// one list.
///
/// The per-card e2e ids (`admin-stat-card`, `admin-stat-card-value`,
/// `admin-stat-card-label`) are constant across every card, so they are applied
/// by the rendering shell, not carried per item.
public struct DashboardStatItem: Identifiable {
    /// The card's position in the snapshot — stable for `ForEach` (two tiers
    /// could share a label, so the label is not a key).
    public let id: Int
    public let label: String
    public let value: String

    public init(id: Int, label: String, value: String) {
        self.id = id
        self.label = label
        self.value = value
    }
}

/// The snapshot's cards, in the fold's order, each `LocalizedText` resolved
/// against the app's string table. Shared by the macOS + iOS
/// `AdminDashboardView`s.
public func dashboardStatItems(_ snapshot: FfiAdminDashboardSnapshot) -> [DashboardStatItem] {
    snapshot.stats.enumerated().map { index, card in
        DashboardStatItem(id: index,
                          label: renderLocalizedText(card.label),
                          value: renderLocalizedText(card.value))
    }
}
