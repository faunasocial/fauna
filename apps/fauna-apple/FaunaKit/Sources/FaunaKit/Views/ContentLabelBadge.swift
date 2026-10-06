import SwiftUI

/// Shared content-moderation **category badge** — the canonical 5-category
/// vocabulary (`spam` / `trusted` / `nsfw` / `phishing` / `commercial`, else an
/// off-list label rendered grey-verbatim) rendered identically on macOS + iOS
/// (priority #2). Label / icon / colour come entirely from the shared
/// `fauna_core::content_category::content_label_style` map via the UniFFI
/// `contentLabelStyle` face — **no** client hard-codes the category strings or
/// their styling (moderation.md § Where logic lives, the drift #157 lift; the
/// apple analogue of linux painting the same hex and web's `ContentLabelBadge`).
///
/// The `content-label-badge` test id rides on the label `Text` (not the
/// container) so e2e reads the clean, resolved category — mirroring linux's
/// `views/moderation.rs`. Consumed by the moderation-queue rows, `DmMessageBubble`
/// (off `MessageSnapshot.labels`), and both feed post-cards (off
/// `PostSummary.labels`) — each site picks the entry via the shared
/// `primaryContentLabel(labels:)` free fn, never a client-side reduce.
public struct ContentLabelBadge: View {
    private let category: String

    public init(category: String) {
        self.category = category
    }

    public var body: some View {
        let style = contentLabelStyle(category: category)
        HStack(spacing: 3) {
            Text(style.icon)
                .font(.caption2)
            automationText(Ids.contentLabelBadge, renderLocalizedText(style.label))
                .font(.caption.weight(.semibold))
                .foregroundStyle(Color(hex: style.accent))
        }
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .background(Color(hex: style.tint).opacity(0.15))
        .clipShape(Capsule())
    }
}
