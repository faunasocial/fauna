// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// The per-emoji reaction pills under a post's interaction bar (feed.md § Layout
/// & flow → *Reaction posts count and paint*, rulings 2 and 4; `post-reaction-pill`,
/// user-approved 2026-11-01). One pill per ``PostReactionGroup`` in the snapshot's
/// order, painted `"<emoji> <count>"`; the viewer's own reaction
/// (`viewerReactId != nil`) is highlighted with `DmMessageBubble`'s
/// `dm-reaction-pill` accent and reads `on` (else `off`). A tap is the shared
/// `FeedManager::react` toggle (``FeedVM/reactPost(postId:emoji:)``). The
/// shared snapshot carries no group on a repost row's own bar (none is painted
/// there) nor on a `bluesky` row, so an empty list paints nothing. Mirrors tui's
/// `reaction_pills`.
public struct PostReactionPills: View {
    public let reactions: [PostReactionGroup]
    public let onToggle: (String) -> Void

    public init(reactions: [PostReactionGroup], onToggle: @escaping (String) -> Void) {
        self.reactions = reactions
        self.onToggle = onToggle
    }

    /// What one pill paints and the in-process driver reads back as its text.
    static func label(_ group: PostReactionGroup) -> String {
        "\(group.emoji) \(group.count)"
    }

    /// Whether the viewer's own React post of this emoji is live.
    static func isMine(_ group: PostReactionGroup) -> Bool {
        group.viewerReactId != nil
    }

    public var body: some View {
        if !reactions.isEmpty {
            HStack(spacing: 4) {
                ForEach(Array(reactions.enumerated()), id: \.offset) { _, group in
                    pill(group)
                }
                Spacer(minLength: 0)
            }
        }
    }

    private func pill(_ group: PostReactionGroup) -> some View {
        let label = Self.label(group)
        let mine = Self.isMine(group)
        return Button {
            onToggle(group.emoji)
        } label: {
            Text(label).font(.caption2)
        }
        .buttonStyle(.borderless)
        .padding(.horizontal, 6)
        .padding(.vertical, 2)
        .background(
            mine ? Color.accentColor.opacity(0.25) : Color.secondary.opacity(0.12),
            in: Capsule()
        )
        .accessibilityIdentifier(Ids.postReactionPill)
        // Toggle-shaped: activate + read share one Entry (the like button's
        // pattern) — text is "emoji count", value the own-reaction state.
        .automationActivate(
            Ids.postReactionPill, text: { label }, value: { mine ? "on" : "off" }
        ) { onToggle(group.emoji) }
    }
}
#endif // !FAUNA_KIDS
