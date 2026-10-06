import SwiftUI
import FaunaKit

/// Thin macOS wrapper over the shared `FaunaKit.PostCardBody` — the two platform card bodies were
/// ~90% line-identical; this file now owns only what's genuinely
/// macOS-specific: the `MacAppState` environment read and the `onReply`
/// closure (its parent `MacFeedDetailView` presents `FeedReplyDialog`; iOS
/// presents it inline off the card instead — see `PostCardView`).
struct MacPostCardView: View {
    let post: PostSummary
    let vm: FeedVM
    let onReply: () -> Void
    let onNavigateToPersonalization: () -> Void

    @Environment(MacAppState.self) private var appState

    var body: some View {
        PostCardBody(
            post: post, vm: vm, appState: appState,
            verticalPadding: 4, cardSpacing: 6, bodyFont: .body, bodyLineLimit: nil,
            selfRegistersPostCardId: true,
            onReply: onReply,
            onNavigateToPersonalization: onNavigateToPersonalization
        )
    }
}
