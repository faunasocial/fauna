import SwiftUI

/// The STANDING pending-actions section (`docs/goal/ui/settings.md` §
/// Pending actions), shared by macOS + iOS — mounted at the bottom of the
/// Account settings page, below both delayed verbs this page hosts (the
/// third, snapshot delete, schedules from the Backups page and appears here
/// on the next Account visit's hydrate). ALWAYS rendered — a conditional
/// render would hide the affordance exactly when a mis-clicker goes looking
/// for it. Mirrors tui's `pending_actions_elements` / linux's
/// `build_pending_actions_group` / web's `+page.svelte` section / android's
/// `PendingActionsCard` exactly.
///
/// **`GroupBox` with no static label** (mirrors `SignOutSection`): the title
/// itself is the dynamic three-state text below, not a separate heading, so
/// `pending-actions-section`'s id carries whichever of the three readings is
/// current — the e2e action layer reads it directly (`d.get_text(
/// "pending-actions-section")`).
public struct PendingActionsSection: View {
    let pendingActions: [FfiPendingActionSummary]?
    let onCancel: (Int64) -> Void

    public init(pendingActions: [FfiPendingActionSummary]?, onCancel: @escaping (Int64) -> Void) {
        self.pendingActions = pendingActions
        self.onCancel = onCancel
    }

    /// Three-state honest title — bare (`nil`, not yet hydrated) / empty-state
    /// line (`[]`, hydrated and empty) / counted title (non-empty) — never a
    /// settled "nothing scheduled" claim before the first list read lands.
    private var title: String {
        guard let pendingActions else { return L.settings.pendingActions.title }
        if pendingActions.isEmpty { return L.settings.pendingActions.noneScheduled }
        return L.settings.pendingActions.titleCount(count: "\(pendingActions.count)")
    }

    public var body: some View {
        GroupBox {
            VStack(alignment: .leading, spacing: 8) {
                automationText(Ids.pendingActionsSection, title)
                    .font(.headline)
                if let pendingActions {
                    ForEach(Array(pendingActions.enumerated()), id: \.offset) { index, action in
                        row(action, index: index)
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    @ViewBuilder
    private func row(_ action: FfiPendingActionSummary, index: Int) -> some View {
        HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 4) {
                // `describePendingAction` is the shared verb+target sentence
                // renderer (`fauna_protocol::pending_actions`) — the target is
                // only ever DESCRIBED here, never cached (the section's own
                // iron-clad: the change has not applied yet).
                automationText(
                    Ids.pendingActionDescription,
                    describePendingAction(actionType: action.actionType, target: action.target)
                )
                automationText(
                    Ids.pendingActionExecuteAfter,
                    L.settings.pendingActions.applies(time: formatUnixLocal(secs: action.executeAfter))
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Spacer()
            Button(L.settings.pendingActions.cancel) {
                onCancel(action.id)
            }
            .accessibilityIdentifier(Ids.pendingActionCancelButton)
            .automationActivate(Ids.pendingActionCancelButton) {
                onCancel(action.id)
            }
        }
        .padding(.vertical, 4)
        // Container id + `.contain` so the row's children stay queryable
        // alongside the row's own id (apple container-a11y rule).
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier(Ids.pendingActionItem)
        .automationValue(Ids.pendingActionItem, text: {
            describePendingAction(actionType: action.actionType, target: action.target)
        })
        .automationScope(Ids.pendingActionItem, index: index)
    }
}
