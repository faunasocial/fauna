import SwiftUI

/// One row in the unified conversations list — shared by the macOS list pane
/// and the iOS list screen. Renders off a `ConversationRowModel` only — mixes
/// 1:1 chats, MLS groups, and email-shaped threads with no `rail` branch on
/// layout (the `protocol-icon` glyph is the only rail-derived bit, and it's
/// cosmetic). There is no Groups page: group threads live here with
/// `flavor: .mlsGroup`.
///
/// Child element IDs (ui.yaml `conversation-list-item` / `conversations`):
/// `dm-unread-indicator`, `protocol-icon` (indexed), `dm-subject` (the
/// subject/snippet line), `conversation-item-timestamp` (indexed, the last-activity
/// time; absent when the thread has none). The *caller* wraps this in the indexed
/// `conversation-item` element — a `NavigationLink` on iOS, a `List` row with a
/// `.tag` on macOS — so the "this is a tappable list item" boundary stays out
/// of the reusable row component.
public struct ConversationListRow: View {
    public let model: ConversationRowModel

    public init(model: ConversationRowModel) { self.model = model }

    private var displayLabel: String {
        if model.flavor == .mlsGroup, model.participantCount > 0 {
            return "\(model.label) (\(model.participantCount))"
        }
        return model.label
    }

    public var body: some View {
        HStack(alignment: .top, spacing: 10) {
            // Unread dot — present only while the row is unread; a read row has
            // NO dot, not a zero-size one (`dm-unread-indicator` is absent when
            // read on every app, and the automation registry counts a zero-size
            // slot as on screen). The fixed 7pt slot keeps the text column still
            // whichever the row is.
            Color.clear
                .frame(width: 7, height: 7)
                .overlay {
                    if model.unreadCount > 0 {
                        Circle()
                            .fill(Color.accentColor)
                            .accessibilityIdentifier(Ids.dmUnreadIndicator)
                            .automationValue(Ids.dmUnreadIndicator, text: { String(model.unreadCount) })
                    }
                }
                .padding(.top, 5)

            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(displayLabel)
                        .font(.body.weight(model.unreadCount > 0 ? .semibold : .regular))
                        .lineLimit(1)
                    Spacer(minLength: 4)
                    // Last-activity time — absent (not an empty label) when the
                    // thread has none (`conversation-item-timestamp` is absent at
                    // `last_activity_ms == 0`); the text is the shared
                    // `conversation_timestamp_display` bucket via `ValueFormat`.
                    if model.lastActivityMs > 0 {
                        automationText(
                            Ids.conversationItemTimestamp,
                            ValueFormat.conversationTimestamp(thenMs: model.lastActivityMs))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Text(ConversationsUI.glyphEmoji(model.glyph))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier(Ids.protocolIcon)
                        // Read-only glyph — expose the D5 concept emoji.
                        .automationValue(Ids.protocolIcon, text: { ConversationsUI.glyphEmoji(model.glyph) })
                }
                automationText(Ids.dmSubject, model.snippet)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
        }
        .padding(.vertical, 4)
        .contentShape(Rectangle())
    }
}
