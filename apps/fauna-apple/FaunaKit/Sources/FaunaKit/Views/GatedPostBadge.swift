import SwiftUI

/// Renders the `gated-post-badge` on a post-card **iff** the post is gated to a
/// subscriber tier OR room (`PostSummary.gatedTier`/`roomLabel != nil`) — the
/// visible text is the tier name, matching linux `post_list.rs`
/// (`gtk::Label::new(tier)`) and web `PostCard.svelte` (`>{post.gated_tier}`),
/// priorities #1/#3. The list card body stays the plaintext teaser; the detail
/// unseals the full body for an entitled reader (`docs/goal/ui/feed.md` §
/// Encryption at rest). The full "Subscribers only" / "Room members only"
/// caveat rides the tooltip / accessibility label (`feed.post.gated_badge_tooltip`
/// / `feed.post.gated_badge_room_tooltip`), the same split web (`title`) and
/// linux (`set_tooltip_text`) use.
///
/// **Room-restricted (`ui/feed.md` § Encryption at rest → *Room-restricted —
/// the app half* → *The card*).** `roomLabel` is `Some` exactly when the post's
/// room (`PostSummary.gatedRoom`) is one this device still holds a seat on —
/// derived fresh on every snapshot read from `own_rooms`, never taken from
/// anything the author sent. A member's card names the room (the composer's
/// own "Room: ‹label›" string); a reader off the floor — or a lost-seat
/// re-read — falls back to `gatedTier`, which the nest projects as the
/// reserved constant `room` for a room post: the honest degrade, not a
/// second code path. "Open" is unchanged (the card's own detail-open) — only
/// the badge text/tooltip differ, so `roomLabel` wins over `gatedTier` here
/// and nowhere else.
///
/// Mirrors the sibling `UnverifiedSourceBadge` post-card chrome. Shared between
/// both apple apps (macOS `MacPostCardView`, iOS `PostCardView`).
public struct GatedPostBadge: View {
    public let gatedTier: String?
    public let roomLabel: String?

    public init(gatedTier: String?, roomLabel: String? = nil) {
        self.gatedTier = gatedTier
        self.roomLabel = roomLabel
    }

    public var body: some View {
        if let room = roomLabel {
            automationText(Ids.gatedPostBadge, L.feed.post.gateRoom(room: room))
                .secondaryCaveatBadge()
                .help(L.feed.post.gatedBadgeRoomTooltip(room: room))
                .accessibilityLabel(L.feed.post.gatedBadgeRoomTooltip(room: room))
        } else if let tier = gatedTier {
            // `automationText` renders the label AND registers its read for the
            // in-process e2e driver (a bare `.accessibilityIdentifier` is invisible
            // to it) — the canonical presence-readable-badge pattern, same as
            // `UnverifiedSourceBadge`. The visible text is the tier name (matching
            // linux/web); the e2e's `gated_badge_text(0)` reads it.
            automationText(Ids.gatedPostBadge, tier)
                .secondaryCaveatBadge()
                .help(L.feed.post.gatedBadgeTooltip(tier: tier))
                .accessibilityLabel(L.feed.post.gatedBadgeTooltip(tier: tier))
        }
    }
}
