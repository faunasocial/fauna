import SwiftUI

/// Post interaction buttons: reply, repost, quote, like.
///
/// Each button shows a recognizable icon plus its interaction count — **no word
/// label** — and the count is **hidden when 0** (a clean icon-only button until
/// the post has activity), uniform across all seven apps (feed.md § Interaction
/// bar, ratified 2026-06-27). The counts are the shared
/// `PostSummary.{like,reply,repost,quote}_count` read from the FeedManager
/// snapshot — never computed per client. Each button is a thin
/// `APIClient.interactWithPost` glue call (the same per-app interact pattern
/// Linux/Android/web use), NOT a `FeedManager` action.
public struct InteractionBar: View {
    public var replyCount: Int
    public var repostCount: Int
    public var quoteCount: Int
    public var likeCount: Int
    public var isLiked: Bool
    public var isReposted: Bool
    public var onReply: () -> Void
    public var onRepost: () -> Void
    public var onQuote: () -> Void
    public var onLike: () -> Void

    public init(
        replyCount: Int = 0,
        repostCount: Int = 0,
        quoteCount: Int = 0,
        likeCount: Int = 0,
        isLiked: Bool = false,
        isReposted: Bool = false,
        onReply: @escaping () -> Void = {},
        onRepost: @escaping () -> Void = {},
        onQuote: @escaping () -> Void = {},
        onLike: @escaping () -> Void = {}
    ) {
        self.replyCount = replyCount
        self.repostCount = repostCount
        self.quoteCount = quoteCount
        self.likeCount = likeCount
        self.isLiked = isLiked
        self.isReposted = isReposted
        self.onReply = onReply
        self.onRepost = onRepost
        self.onQuote = onQuote
        self.onLike = onLike
    }

    public var body: some View {
        // Order matches the goal doc + ui.yaml (feed.md § Interaction bar,
        // ratified 2026-06-27): like, reply, repost, quote (priorities #1/#3 —
        // same layout on every app).
        HStack(spacing: 0) {
            Button(action: onLike) {
                interactionLabel(count: likeCount, systemImage: isLiked ? "heart.fill" : "heart")
                    .foregroundStyle(isLiked ? .red : .secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityIdentifier(Ids.feedLikeButton)
            // Toggle-shaped (isLiked), so activate+read share one Entry
            // (AutomationRegistry.swift cycle/toggle-button pattern).
            .automationActivate(Ids.feedLikeButton, text: { Self.painted("like", likeCount) },
                                value: { isLiked ? "on" : "off" }, perform: onLike)

            Button(action: onReply) {
                interactionLabel(count: replyCount, systemImage: "bubble.right")
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityIdentifier(Ids.feedReplyButton)
            .automationActivate(Ids.feedReplyButton, text: { Self.painted("reply", replyCount) }, perform: onReply)

            Button(action: onRepost) {
                interactionLabel(count: repostCount, systemImage: "arrow.2.squarepath")
                    .foregroundStyle(isReposted ? .green : .secondary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityIdentifier(Ids.feedRepostButton)
            .automationActivate(Ids.feedRepostButton, text: { Self.painted("repost", repostCount) },
                                value: { isReposted ? "on" : "off" }, perform: onRepost)

            Button(action: onQuote) {
                interactionLabel(count: quoteCount, systemImage: "quote.bubble")
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityIdentifier(Ids.feedQuoteButton)
            .automationActivate(Ids.feedQuoteButton, text: { Self.painted("quote", quoteCount) }, perform: onQuote)
        }
        .font(.caption)
        .foregroundStyle(.secondary)
    }

    /// What one button paints, as the in-process driver reads it back (the
    /// button's `/element/text`): its glyph, named by the verb it stands for —
    /// the SF Symbol carries no text, and its own name can hold a numeral
    /// (`arrow.2.squarepath`) — plus the count only while it is shown, so a read
    /// of a post with no activity finds an icon and no number (`feed.md` §
    /// Interaction bar). Kept beside `interactionLabel` so the two cannot
    /// disagree on when the count shows. linux declares the same text
    /// (`post_list.rs::interaction_button`). The toggles' `value` stays their
    /// "on"/"off" state.
    static func painted(_ glyph: String, _ count: Int) -> String {
        count > 0 ? "\(glyph) \(count)" : glyph
    }

    /// One interaction button's label: the icon, plus the count **only when > 0**
    /// (count hidden at 0 — feed.md § Interaction bar). The icon-only `Image` at 0
    /// keeps the bar a clean row of glyphs until the post has activity.
    @ViewBuilder
    private func interactionLabel(count: Int, systemImage: String) -> some View {
        if count > 0 {
            Label("\(count)", systemImage: systemImage)
        } else {
            Image(systemName: systemImage)
        }
    }
}
