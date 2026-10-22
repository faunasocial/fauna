import SwiftUI

/// A notification row a muted word collapses (`docs/goal/behavior/notifications.md`
/// § Muted words, rule 3) — ONE leaf for both apple Notifications pages, the
/// twin of `FeedPostMutedPlaceholder` and `DmMessageBubble`'s muted placeholder.
///
/// It paints the type icon, `notification-muted` in place of the sentence, the
/// timestamp, and `notification-muted-reveal-button` — and nothing else: it is
/// no `notification-item` and no tap target, so a muted sender's name is never
/// painted as a control (linux's `build_muted_row`, tui's collapsed paint). The
/// reveal is the caller's (``NotificationsVM/revealMuted(_:)``), after which the
/// page paints the full row in this one's place.
public struct NotificationMutedRow: View {
    private let item: NotificationItem
    private let onReveal: () -> Void

    public init(item: NotificationItem, onReveal: @escaping () -> Void) {
        self.item = item
        self.onReveal = onReveal
    }

    public var body: some View {
        HStack(alignment: .top, spacing: 8) {
            NotificationTypeIcon(notifType: item.notifType)
            VStack(alignment: .leading, spacing: 4) {
                automationText(Ids.notificationMuted, L.mutedWords.collapsed)
                    .font(.subheadline)
                    .italic()
                    .foregroundStyle(.secondary)
                Text(ValueFormat.relativeTime(thenMs: Int64(item.createdAt) / 1000))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer(minLength: 4)
            Button(L.mutedWords.reveal) { onReveal() }
                .buttonStyle(.borderless)
                .font(.caption)
                .accessibilityIdentifier(Ids.notificationMutedRevealButton)
                .automationActivate(Ids.notificationMutedRevealButton) { onReveal() }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
