// Excised from the Fauna Kids build — a whole plane the kids flavor compiles
// out (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
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
    /// Born unchecked at every open and never remembered (`@State` dies with the sheet).
    @State private var publicConfirmed = false

    public init(post: PostSummary, vm: FeedVM) {
        self.post = post
        self.vm = vm
    }

    private var canSubmit: Bool { !replyText.trimmingCharacters(in: .whitespaces).isEmpty }

    private func submit() {
        Task {
            // The box exists (hence can be ticked) only under *public by
            // confirmation*; the send path lives in `FeedVM.replyToPost`.
            await vm.replyToPost(postId: post.postId, body: replyText,
                                 publicConfirmed: needsPublicConfirm && publicConfirmed)
            dismiss()
        }
    }

    /// `feed-reply-audience` — the manager's own per-post answer
    /// (`PostSummary.replyAudience`), stated and never decided here. A public
    /// target has no answer and paints neither this line nor the checkbox
    /// (restricted-posts.md § Encryption at rest → *Ruling 5's build*, (d) + (e)).
    private var audienceText: String? {
        switch post.replyAudience {
        case .sealedToRoom: return L.feed.post.replyAudienceRoom(room: post.roomLabel ?? "")
        case .sealedToTier: return L.feed.post.replyAudienceTier(tier: post.gatedTier ?? "")
        case .publicByConfirmation: return L.feed.post.replyAudiencePublic
        case nil: return nil
        }
    }

    private var needsPublicConfirm: Bool { post.replyAudience == .publicByConfirmation }

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

            if let audienceText {
                Text(audienceText)
                    .font(.caption)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .accessibilityIdentifier(Ids.feedReplyAudience)
                    .automationValue(Ids.feedReplyAudience, text: { audienceText })
            }

            TextEditor(text: $replyText)
                .frame(height: 100)
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(.separator))
                .accessibilityIdentifier(Ids.feedReplyTextField)
                .automationField(Ids.feedReplyTextField, text: $replyText)

            if needsPublicConfirm {
                Toggle(L.feed.post.replyPublicConfirm, isOn: $publicConfirmed)
                    .accessibilityIdentifier(Ids.feedReplyPublicConfirm)
                    .automationActivate(
                        Ids.feedReplyPublicConfirm,
                        value: { publicConfirmed ? "on" : "off" }
                    ) { publicConfirmed.toggle() }
            }

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
        .frame(width: 450, height: 430)
        #endif
        .accessibilityIdentifier(Ids.feedReplyDialog)
        .automationValue(Ids.feedReplyDialog, text: { "" })
    }
}
#endif // !FAUNA_KIDS
