import SwiftUI

/// A notification row's `notification-dismiss` — ONE leaf for both apple
/// Notifications pages (`docs/goal/behavior/notifications.md` § Retention
/// rule 2). Every loaded row gets one, a muted-collapsed row included
/// (§ Muted words rule 4). Greyed while the shared `notification_retained_for`
/// decision holds (``NotificationsVM/isRetained(_:)``); the page paints it
/// after the row's `notification-item`, so ids index per row.
///
/// A disabled activation is refused by the automation server (HTTP 409), which
/// is the honest answer for a greyed control.
public struct NotificationDismissButton: View {
    private let isRetained: Bool
    private let onDismiss: () -> Void

    public init(isRetained: Bool, onDismiss: @escaping () -> Void) {
        self.isRetained = isRetained
        self.onDismiss = onDismiss
    }

    public var body: some View {
        Button(L.notifications.dismiss) { onDismiss() }
            .buttonStyle(.borderless)
            .font(.caption)
            .disabled(isRetained)
            .accessibilityIdentifier(Ids.notificationDismiss)
            .automationActivate(Ids.notificationDismiss, isEnabled: { !isRetained }) { onDismiss() }
    }
}
