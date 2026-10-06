import Foundation

/// A `post-card` tap or activation opens detail — for a REPOST ROW
/// (`post.repostedPostId != nil`) that means the ORIGINAL, never the empty
/// repost carrier itself (`feed.md` § Interaction bar → Repost: "activation
/// opens the ORIGINAL's detail"). An ordinary post opens directly with no
/// round trip; a repost row routes through the same id-keyed
/// `pendingPostOpen` resolution the cross-page deep link already uses
/// (`findPost` fast path, else `resolvePost`) — reused rather than a second
/// fetch path. Mirrors tui's `detail_target`.
///
/// `selectedPost` is `inout` because it is each caller's own View-local
/// `@State` (macOS's `MacFeedDetailView`, iOS's `FeedListView`) — SwiftUI
/// state can't be shared across two independent View structs, the same
/// reason `toggleMailboxPending`/`isMailboxSelected` (`MailboxToggleSelection.swift`)
/// take their local echo `inout` too. Was a byte-identical per-target twin
/// (both callers' own doc comments already cross-referenced each other) until
/// this harvest pass found it .
@MainActor
public func openPostCard(_ post: PostSummary, vm: FeedVM, selectedPost: inout PostSummary?) {
    if let repostedId = post.repostedPostId {
        vm.pendingPostOpen = repostedId
    } else {
        selectedPost = post
    }
}
