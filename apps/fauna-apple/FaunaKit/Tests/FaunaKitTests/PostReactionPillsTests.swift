import Testing
import Foundation
@testable import FaunaKit

// The feed's reaction pills (`docs/goal/ui/feed.md` § Layout & flow → *Reaction
// posts count and paint*, ruling 4). Modelled on tui's
// `reaction_pills_paint_in_order_with_counts_and_the_viewers_own_highlighted`:
// each group paints "<emoji> <count>" in snapshot order, and only a group the
// viewer holds a live React post of (`viewerReactId != nil`) reads as their own.

@Test func reactionPillsPaintEmojiAndCountInOrder() {
    let groups = [
        PostReactionGroup(emoji: "🔥", count: 2, viewerReactId: nil),
        PostReactionGroup(emoji: "🎉", count: 1, viewerReactId: nil),
    ]
    #expect(groups.map(PostReactionPills.label) == ["🔥 2", "🎉 1"])
}

@Test func reactionPillHighlightsOnlyTheViewersOwnReaction() {
    let others = PostReactionGroup(emoji: "🔥", count: 1, viewerReactId: nil)
    let mine = PostReactionGroup(emoji: "🎉", count: 2, viewerReactId: "ab12")
    #expect(!PostReactionPills.isMine(others))
    #expect(PostReactionPills.isMine(mine))
}
