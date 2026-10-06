import SwiftUI

/// Renders the `repost-attribution` marker beside the author on a REPOST ROW
/// (`feed.md` § Interaction bar → Repost, ratified 2026-08-10; id
/// user-approved 2026-08-11) — what lets an e2e tell a repost card from an
/// empty-commentary quote card BY ELEMENT. Mirrors tui/linux's identical
/// "⇄ {reposted}" text.
///
/// Used from the shared FaunaKit `PostCardBody`, which unified macOS `MacPostCardView`/iOS
/// `PostCardView` — kept as its own leaf component rather than inlined,
/// same convention as `UnverifiedSourceBadge`/`GatedPostBadge`: the
/// `isRepost` predicate lives inside the badge, so a call site just passes
/// its own `isRepostRow`.
public struct RepostAttributionBadge: View {
    public let isRepost: Bool

    public init(isRepost: Bool) {
        self.isRepost = isRepost
    }

    public var body: some View {
        if isRepost {
            // `automationText` renders the label AND registers its read for
            // the in-process e2e driver — the canonical presence-readable-
            // badge pattern (`GatedPostBadge`/`UnverifiedSourceBadge`).
            automationText(Ids.repostAttribution, "⇄ \(L.feed.post.repostedMarker)")
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
    }
}
