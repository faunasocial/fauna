import SwiftUI

/// Shared reply-compose surface (ui.yaml `feed-reply-dialog`) — macOS+iOS,
/// presented as a `.sheet(item:)` by the host view (`feed-reply-button` on
/// `PostCardView`/`MacPostCardView` list cards and `PostDetailView`/
/// `PostDetailSheet` detail views) when tapped. Composes via
/// `FeedManager::reply(post_id, body)` — never `fauna.posts.interact`, whose
/// native arm discards `body` entirely on a native post (feed.md §
/// Implementation status today). Mirrors `FeedComposeDialog`'s shared-target
/// shape; unlike quote, an empty reply is meaningless, so submit stays
/// disabled on blank text.
public struct FeedReplyDialog: View {
    let post: PostSummary
    let vm: FeedVM
    @Environment(\.dismiss) private var dismiss
    @State private var replyText = ""

    public init(post: PostSummary, vm: FeedVM) {
        self.post = post
        self.vm = vm
    }

    private var canSubmit: Bool { !replyText.trimmingCharacters(in: .whitespaces).isEmpty }

    private func submit() {
        Task {
            await vm.replyToPost(postId: post.postId, body: replyText)
            dismiss()
        }
    }

    public var body: some View {
        VStack(spacing: 16) {
            Text(L.feed.post.replyingToUser(user: shortId(hex: post.author)))
                .font(.headline)

            Text(post.body)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(3)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(8)
                .background(.background.secondary)
                .clipShape(RoundedRectangle(cornerRadius: 6))

            TextEditor(text: $replyText)
                .frame(height: 100)
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(.separator))
                .accessibilityIdentifier(Ids.feedReplyTextField)
                .automationField(Ids.feedReplyTextField, text: $replyText)

            HStack {
                Button(L.common.cancel) { dismiss() }
                Button(L.common.reply) { submit() }
                    .buttonStyle(.borderedProminent)
                    .disabled(!canSubmit)
                    .accessibilityIdentifier(Ids.feedReplySubmitButton)
                    .automationActivate(Ids.feedReplySubmitButton, isEnabled: { canSubmit }, perform: submit)
            }
        }
        .padding(24)
        #if os(macOS)
        .frame(width: 450, height: 350)
        #endif
        .accessibilityIdentifier(Ids.feedReplyDialog)
        .automationValue(Ids.feedReplyDialog, text: { "" })
    }
}
