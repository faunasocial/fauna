import SwiftUI

/// One headline metric on the admin Dashboard (admin.md § 1 Dashboard).
///
/// The shared *definition* of a stat card — its label, formatted value, SF
/// Symbol, and accent color — so the macOS card grid (`AdminDashboardView` →
/// `LazyVGrid` of `StatCard`) and the iOS stat-row list (`AdminDashboardView` →
/// `List` of `statRow`) render the **same** metric set, in the same order, with
/// the same labels, formatting, and colors, from one place. Each platform keeps
/// its own layout *shell* (the grid vs. the list is a form-factor divergence —
/// a 4-col grid is wrong on a phone, plain rows are sparse on a wide Mac) but
/// consumes this single item list, so an "add / relabel / reorder / recolor a
/// stat" change lands once instead of in two near-identical files.
///
/// The per-card e2e ids (`admin-stat-card`, `admin-stat-card-value`,
/// `admin-stat-card-label`) are constant across every card, so they are applied
/// by the rendering shell, not carried per item.
public struct DashboardStatItem: Identifiable {
    /// Stable key for `ForEach` (also distinguishes the per-tier cards).
    public let id: String
    public let label: String
    public let value: String
    public let systemImage: String
    public let color: Color

    public init(id: String, label: String, value: String, systemImage: String, color: Color) {
        self.id = id
        self.label = label
        self.value = value
        self.systemImage = systemImage
        self.color = color
    }
}

/// The canonical headline-metric list for the admin Dashboard, in display order:
/// version, users, suspended, storage, connections, then one card per user
/// tier (`stats.usersByTier`). Shared by the macOS + iOS `AdminDashboardView`s
/// (admin.md § 1 Dashboard). The version card always renders (`"—"` when the
/// node-info version hasn't loaded), mirroring the prior macOS behavior.
public func dashboardStatItems(stats: FfiAdminStats, version: String?) -> [DashboardStatItem] {
    var items: [DashboardStatItem] = [
        DashboardStatItem(id: "version", label: L.admin.dashboard.version,
                          value: version ?? "—", systemImage: "number", color: .green),
        // `"Users"` is a literal in both prior views — there is no
        // `L.admin.dashboard.users`; kept verbatim to preserve behavior.
        DashboardStatItem(id: "users", label: "Users",
                          value: "\(stats.totalUsers)", systemImage: "person.3", color: .blue),
        DashboardStatItem(id: "suspended", label: L.admin.dashboard.suspended,
                          value: "\(stats.suspendedUsers)",
                          systemImage: "exclamationmark.triangle",
                          color: stats.suspendedUsers > 0 ? .orange : .green),
        DashboardStatItem(id: "storage", label: L.admin.dashboard.totalStorage,
                          value: ValueFormat.byteSize(UInt64(max(0, stats.totalStorageBytes))),
                          systemImage: "externaldrive", color: .purple),
        DashboardStatItem(id: "connections", label: L.admin.dashboard.connections,
                          value: "\(stats.wsConnections)", systemImage: "wifi", color: .green),
    ]
    for row in stats.usersByTier {
        items.append(DashboardStatItem(id: "tier-\(row.tier)", label: row.tier.capitalized,
                                       value: "\(row.count)", systemImage: "person", color: .secondary))
    }
    return items
}
