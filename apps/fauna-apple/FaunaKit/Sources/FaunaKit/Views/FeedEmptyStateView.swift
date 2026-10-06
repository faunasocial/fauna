import SwiftUI

/// The Feed page's empty state — `feed-empty-state` (no posts, no search) or
/// `feed-no-results` (a search matched nothing), at most one present
/// (`docs/goal/ui/feed.md` § Errors & edge cases). Paints whatever
/// ``FeedVM/emptyState`` answers — the shared `FeedSnapshot::empty_state`
/// decision — so neither app re-derives the state or picks the copy by its
/// own reading of the search term. Shared by the macOS and iOS feed views.
public struct FeedEmptyStateView: View {
    let state: FeedEmptyState

    public init(state: FeedEmptyState) {
        self.state = state
    }

    public var body: some View {
        Group {
            switch state {
            case .noPosts:
                automationText(Ids.feedEmptyState, L.feed.list.noPosts)
            case .noMatches:
                automationText(Ids.feedNoResults, L.feed.list.noMatchingPosts)
            }
        }
        .foregroundStyle(.secondary)
    }
}
