import SwiftUI

/// The Users hub's STANDING pending-admin-actions section (`docs/goal/behavior/
/// admin.md` § Pending admin actions), shared by macOS + iOS — the co-admin
/// approve / veto surface over every account's still-`pending` delayed admin
/// action, mounted below the user list. Mirrors tui's `pending_section`
/// (`apps/fauna-tui/src/admin/users.rs`) / linux's `admin_pending_actions.rs` /
/// web's `admin/users` section / android's `PendingActionsSection`.
///
/// ALWAYS rendered: the container's text (`admin-users-pending-section`) is the
/// empty-state line or the counted title; one row per pending action with the
/// shared description (`FfiAdminPendingAction.description`, derived in Rust
/// beside the scheduling admin's short id), when it applies, its approvals
/// readout, and two one-click controls (no confirm).
public struct AdminPendingActionsSection: View {
    let pendingActions: [FfiAdminPendingAction]
    let onApprove: (Int64) -> Void
    let onCancel: (Int64) -> Void

    public init(
        pendingActions: [FfiAdminPendingAction],
        onApprove: @escaping (Int64) -> Void,
        onCancel: @escaping (Int64) -> Void
    ) {
        self.pendingActions = pendingActions
        self.onApprove = onApprove
        self.onCancel = onCancel
    }

    private var title: String {
        pendingActions.isEmpty
            ? L.admin.usersPage.pendingNone
            : L.admin.usersPage.pendingCount(count: "\(pendingActions.count)")
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            automationText(Ids.adminUsersPendingSection, title)
                .font(.headline)
            ForEach(Array(pendingActions.enumerated()), id: \.offset) { index, action in
                row(action, index: index)
            }
        }
    }

    @ViewBuilder
    private func row(_ action: FfiAdminPendingAction, index: Int) -> some View {
        let description = action.description + " "
            + L.admin.usersPage.pendingBy(who: action.actorShort)
        HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 4) {
                automationText(Ids.adminPendingActionDescription, description)
                automationText(
                    Ids.adminPendingActionExecuteAfter,
                    L.settings.pendingActions.applies(time: formatUnixLocal(secs: action.executeAfter))
                )
                .font(.caption)
                .foregroundStyle(.secondary)
                automationText(
                    Ids.adminPendingActionApprovals,
                    L.admin.usersPage.pendingApprovals(
                        given: "\(action.approvals.count)",
                        needed: "\(action.requiresQuorum)")
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Spacer()
            Button(L.admin.usersPage.pendingApprove) {
                onApprove(action.id)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.small)
            .accessibilityIdentifier(Ids.adminPendingActionApproveButton)
            .automationActivate(Ids.adminPendingActionApproveButton) {
                onApprove(action.id)
            }

            Button(L.settings.pendingActions.cancel) {
                onCancel(action.id)
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .accessibilityIdentifier(Ids.adminPendingActionCancelButton)
            .automationActivate(Ids.adminPendingActionCancelButton) {
                onCancel(action.id)
            }
        }
        .padding(.vertical, 4)
        // Container id + `.contain` so the row's children stay queryable
        // alongside the row's own id (apple container-a11y rule).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.adminPendingActionItem)
        .automationValue(Ids.adminPendingActionItem, text: { description })
        .automationScope(Ids.adminPendingActionItem, index: index)
    }
}
