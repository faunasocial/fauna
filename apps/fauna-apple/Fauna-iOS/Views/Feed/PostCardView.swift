import SwiftUI
import FaunaKit

/// Thin iOS wrapper over the shared `FaunaKit.PostCardBody` — the two platform card bodies were ~90%
/// line-identical; this file now owns only what's genuinely iOS-specific:
/// the `AppState` environment read and presenting `FeedReplyDialog` inline
/// off this card. Mirrors `MacPostCardView`, whose parent
/// `MacFeedDetailView` presents the same dialog itself instead.
struct PostCardView: View {
    let post: PostSummary
    let vm: FeedVM
    let onNavigateToPersonalization: () -> Void

    @Environment(AppState.self) private var appState
    /// The reply-compose target — mirrors `MacFeedDetailView.replyingTo`
    /// (macOS presents the same `FeedReplyDialog` from its list-detail
    /// parent; iOS presents it inline off this card).
    @State private var replyingTo: PostSummary?

    var body: some View {
        PostCardBody(
            post: post, vm: vm, appState: appState,
            verticalPadding: 2, cardSpacing: 4, bodyFont: nil, bodyLineLimit: 3,
            selfRegistersPostCardId: false,
            onReply: { replyingTo = post },
            onNavigateToPersonalization: onNavigateToPersonalization
        )
        .sheet(item: $replyingTo) { post in
            FeedReplyDialog(post: post, vm: vm)
        }
    }
}
