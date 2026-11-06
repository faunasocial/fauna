// Excised from the Fauna Kids build with the feed plane it opens from
// (family-safety.md § The account age band, the kids-app bullet, item (4)).
#if !FAUNA_KIDS
import SwiftUI

/// `feed-post-view-thread-button` — the post detail's door to the Bluesky
/// thread page (`bridges.md` § Bluesky-native thread view), shown only for a
/// post the shared `post_offers_thread_view` says offers one (its source is
/// `bluesky`). One component for both Apple details, the way they share
/// `InteractionBar`.
public struct FeedViewThreadButton: View {
    public let post: PostSummary
    public let action: () -> Void

    public init(post: PostSummary, action: @escaping () -> Void) {
        self.post = post
        self.action = action
    }

    public var body: some View {
        if postOffersThreadView(post: post) {
            Button(action: action) {
                Label(L.feed.post.viewThread, systemImage: "bubble.left.and.bubble.right")
            }
            .buttonStyle(.borderless)
            .accessibilityIdentifier(Ids.feedPostViewThreadButton)
            .automationActivate(Ids.feedPostViewThreadButton, perform: action)
        }
    }
}

/// ui.yaml `feed.sub_pages.thread` — the Bluesky thread page, presented over
/// the post detail it was opened from (dismissing it returns there, not to the
/// list). All state is the shared `FeedSnapshot.thread` (`open_thread`): one
/// `feed-thread-post` per post in thread order (ancestors oldest-first, the
/// opened post, its direct replies), each reading `‹author› ‹post text›` with
/// the author from the shared `thread_post_author_label`, and a `selected`
/// attribute `"true"` on the focal post only — the nest names it, no app
/// guesses. A failed fetch paints no posts and its reason on the page's own
/// `error-message`. Closing the page closes the shared page state, so a slow
/// fetch never lands on a page the reader left. Named `FeedThreadView`, not
/// `ThreadPage*`, to stay clear of the UniFFI data types in the flat module.
public struct FeedThreadView: View {
    public let vm: FeedVM
    public let postId: String
    @Environment(\.dismiss) private var dismiss

    public init(vm: FeedVM, postId: String) {
        self.vm = vm
        self.postId = postId
    }

    /// The shared page, only while it is still open on THIS post.
    private var page: ThreadPage? {
        vm.snapshot?.thread.flatMap { $0.postId == postId ? $0 : nil }
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack {
                Text(L.feed.post.thread).font(.headline)
                Spacer()
                Button(L.common.close) { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            if let error = page?.error {
                ErrorBanner(message: renderLocalizedText(error))
            }
            if let page, page.status != .loading {
                ScrollView {
                    VStack(alignment: .leading, spacing: 12) {
                        ForEach(Array(page.posts.enumerated()), id: \.offset) { _, post in
                            FeedThreadPostRow(post: post)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            } else {
                ProgressView(L.common.loading)
                    .frame(maxWidth: .infinity)
                Spacer()
            }
        }
        .padding()
        #if os(macOS)
        .frame(width: 500, height: 420)
        #endif
        .accessibilityIdentifier(Ids.feedThreadPage)
        .automationValue(Ids.feedThreadPage, text: { L.feed.post.thread }, pressKey: { key in
            // ui.yaml gives the page no dismiss element; Escape (the sheet's
            // `.cancelAction`) is the way back, as on `post_detail`.
            guard key == "Escape" else { return "thread page takes only Escape, got \(key)" }
            dismiss()
            return nil
        })
        .automationScope(Ids.feedThreadPage, index: 0)
        .task(id: postId) { await vm.openThread(postId) }
        .onDisappear { vm.closeThread() }
    }
}

/// One `feed-thread-post`: its text the single `‹author› ‹post text›` string
/// every app's driver reads (the tui leg's shape), `selected` on the focal post.
private struct FeedThreadPostRow: View {
    let post: ThreadPostView

    var body: some View {
        let author = threadPostAuthorLabel(post: post)
        let selected = post.isFocal ? "true" : "false"
        (Text(author).fontWeight(.semibold).foregroundColor(.accentColor) + Text(" ") + Text(post.text))
            .font(.body)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(8)
            .background(post.isFocal ? Color.accentColor.opacity(0.12) : Color.clear)
            .clipShape(RoundedRectangle(cornerRadius: 6))
            .accessibilityIdentifier(Ids.feedThreadPost)
            .accessibilityAddTraits(post.isFocal ? .isSelected : [])
            .automationValue(Ids.feedThreadPost, text: { "\(author) \(post.text)" },
                             attributes: { ["selected": selected] })
    }
}
#endif // !FAUNA_KIDS
