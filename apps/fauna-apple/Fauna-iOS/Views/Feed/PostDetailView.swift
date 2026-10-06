import SwiftUI
import FaunaKit

struct PostDetailView: View {
    let vm: FeedVM
    let post: PostSummary

    /// The owner of the private-overlay projection the author's name is read
    /// through (`contacts.md` § The private overlay).
    @Environment(ConversationsVM.self) private var conversationsVM

    /// The reply-compose target — mirrors `PostCardView.replyingTo` /
    /// macOS's `PostDetailSheet.replyingTo`.
    @State private var replyingTo: PostSummary?

    private var livePost: PostSummary { vm.livePost(for: post) }

    var body: some View {
        Group {
            if let reference = livePost.legalTakedownRef {
                legalTakedownContent(reference)
            } else {
                // The region content plane's arm (region-blocking.md § Where it
                // composes) — shared with macOS `PostDetailSheet`.
                RegionGatedPostContent(post: livePost) { normalPostContent }
            }
        }
        .navigationTitle("Post")
        .navigationBarTitleDisplayMode(.inline)
        .accessibilityIdentifier(Ids.feedPostDetailDialog)
        .automationValue(Ids.feedPostDetailDialog, text: { "" })
        // A scope container, so `scope="feed-post-detail-dialog"` reads THIS detail's
        // `post-image` rather than falling back to a flat index that can land on the
        // pushed-over list card's image. Mirrors macOS `PostDetailSheet`.
        .automationScope(Ids.feedPostDetailDialog, index: 0)
        // Fire-once resolution → the manager folds the block + re-emits → `livePost` reads it.
        // Each `id:` condition naturally evaluates false for a legal-takedown post (its
        // `document` is withheld/empty), so these stay safely attached unconditionally.
        // `mediaHash`, never the document's blob image: a resolve leaves it `Some("")` for an
        // all-remote bridged post, so the guard stays fire-once (render-model.md § D6c).
        .task(id: livePost.hasMedia && livePost.mediaHash == nil) {
            if livePost.hasMedia && livePost.mediaHash == nil {
                await vm.resolveMedia(post.postId)
            }
        }
        // Widened to `repostedPostId` too — mirrors `PostCardView`'s
        // identical widening (feed.md § Interaction bar → Repost); routing
        // never opens a repost row's OWN detail, but this keeps the
        // fire-once resolve consistent with the list card's door.
        .task(id: (livePost.quotedPostId ?? livePost.repostedPostId) != nil && documentQuotedPost(livePost.document) == nil) {
            if let qid = livePost.quotedPostId ?? livePost.repostedPostId, documentQuotedPost(livePost.document) == nil {
                _ = await vm.resolveQuotedPost(qid)
            }
        }
        // Fire-once resolve, guarded on `tips == nil` — mirrors the list
        // card's identical `.task` in `PostCardView`.
        #if !FAUNA_EXCISE_PAYMENTS
        .task(id: livePost.tips == nil) {
            if livePost.tips == nil {
                await vm.resolvePostTips(post.postId)
            }
        }
        #endif
        // Fire-once resolve for each still-`Resolving` link preview (render-model.md § D4) → the
        // manager folds `Resolved`/`Failed` onto the block + re-emits → `livePost` reads it.
        .task(id: resolvingLinkPreviewUrls(livePost.document)) {
            for url in resolvingLinkPreviewUrls(livePost.document) {
                await vm.resolveLinkPreview(url)
            }
        }
        // Fire-once gated unlock (feed.md § Encryption at rest): resolve the sealed
        // blob, fetch + decrypt, swap the full body into the snapshot → `livePost`
        // re-reads it and the body repaints. A non-entitled reader stays on the
        // teaser (the manager can't decrypt without a key). Sibling of the resolves
        // above; mirrors macOS `PostDetailSheet`.
        .task(id: livePost.gatedTier != nil && !livePost.gatedUnlocked) {
            if livePost.gatedTier != nil && !livePost.gatedUnlocked {
                await vm.unlockGatedPost(post.postId)
            }
        }
        .sheet(item: $replyingTo) { post in
            FeedReplyDialog(post: post, vm: vm)
        }
    }

    /// A post taken down under a legal obligation (`moderation.md` § Categories &
    /// enforcement item 1): the nest withholds the sealed body under a legal
    /// obligation, so the detail collapses to the shared localized tombstone —
    /// no author, no badges, no tags, no image, no quoted-post embed, no
    /// actions. Mirrors macOS `PostDetailSheet`'s identical branch,
    /// `DmMessageBubble`, and linux's `build_post_detail`.
    private func legalTakedownContent(_ reference: String) -> some View {
        ScrollView {
            automationText(Ids.feedPostDetailBody, renderLocalizedText(legalTakedownTombstone(reference: reference)))
                .font(.body)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding()
        }
    }

    private var normalPostContent: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                // Author
                HStack {
                    // The shared resolver's name for the author — the viewer's
                    // nickname, else the public name, else the canonical short id —
                    // the same string the list card paints (mirrors macOS
                    // `MacFeedDetailView`).
                    automationText(Ids.feedPostDetailAuthor, conversationsVM.postAuthorLabel(post))
                        .font(.subheadline.monospaced())
                        .lineLimit(1)
                        .truncationMode(.middle)
                    // Focal-post unverified-source badge — mirrors the list card
                    // (`PostCardView`) so the post-detail pane shows the same 2a badge
                    // every other app paints (security.md § Client display of unverified
                    // content). Reads `livePost.verification` (the re-read live snapshot, as
                    // the body/quoted badge below do); renders iff `.failed`.
                    UnverifiedSourceBadge(verification: livePost.verification)
                    // Focal-post delegated-origin badge (D10 § Audit) — same mirroring
                    // rationale as the badge above: the detail pane paints what the list
                    // card paints, off the same live snapshot.
                    DelegatedOriginBadge(authoringOrigin: livePost.authoringOrigin)
                    Spacer()
                    Text(ValueFormat.relativeTime(thenMs: post.timestamp))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                // Body — the shared document walk (render-model.md § D6); replaces flat `post.body`.
                DocumentBodyView(document: livePost.document)
                    .font(.body)
                    .accessibilityIdentifier(Ids.feedPostDetailBody)
                    .automationValue(Ids.feedPostDetailBody, text: { renderDocumentToPlaintext(document: livePost.document) })

                // Per-card reveal when the body carries ≥1 still-blocked remote image (D3). Tapping
                // dispatches to the manager, which re-emits with `revealed = true`; `livePost`
                // re-reads the fresh snapshot and the detail repaints (no flag — render-model.md § D3).
                if hasBlockedRemoteImages(livePost.document) {
                    Button { vm.revealRemoteImages(post.postId) } label: {
                        Label(L.conversations.detail.loadRemoteContent, systemImage: "photo")
                            .font(.caption2)
                    }
                    .buttonStyle(.borderless)
                    .accessibilityIdentifier(Ids.loadRemoteContentButton)
                    .automationActivate(Ids.loadRemoteContentButton) { vm.revealRemoteImages(post.postId) }
                }

                // Media — the document's folded `Image` block (render-model.md § D6), via `post-image`.
                // The detail is where a gated post is unlocked, so it is the first place a
                // sealed item becomes openable; `vm.documentPostImage` picks the source
                // (`ui/media.md` § Encryption at rest), or a bridged post's `ProxiedImage`
                // (render-model.md § D6c) when it has no blob image.
                let mediaSource = vm.documentPostImage(livePost.document)
                if let mediaSource, !mediaSource.isUnavailable {
                    PostImage(source: mediaSource)
                        .clipShape(RoundedRectangle(cornerRadius: 8))
                }
                // `video-thumbnail` — the D6b `Video` sibling of `Image` (render-model.md § D6b);
                // mutually exclusive with the image branch above, same fold. A tap plays it in
                // place off the shared `playback_source` (§ D6c → Inline playback).
                if let hash = documentMediaVideoHash(livePost.document) {
                    VideoThumbnailView(hash: hash, resolve: { await vm.videoPlaybackURL(hash) })
                }

                // Quoted post — the document's folded `QuotedPost` block (render-model.md § D6).
                if let quote = documentQuotedPost(livePost.document) {
                    QuotedPostCard(author: shortId(hex: quote.author), postBody: quote.body, verification: quote.verification,
                               authoringOrigin: quote.authoringOrigin,
                               legalTakedownRef: quote.legalTakedownRef, notFound: quote.notFound)
                }

                // Link previews — the document's folded `LinkPreview` blocks (render-model.md § D4).
                // One card per Resolved block; Resolving/Failed → no card. og:image reveal-gated off
                // `livePost` (the re-read live snapshot, as the reveal button + quoted badge above do).
                ForEach(Array(documentResolvedLinkPreviews(livePost.document).enumerated()), id: \.offset) { index, preview in
                    LinkPreviewCard(
                        url: preview.url, title: preview.title, description: preview.description,
                        imageURL: (preview.revealed ? preview.imageHash : nil).flatMap { vm.blobURL($0) },
                        index: index)
                }

                // Tags
                if !post.tags.isEmpty {
                    Text(post.tags.map { "#\($0)" }.joined(separator: " "))
                        .font(.caption)
                        .foregroundStyle(.blue)
                }

                // Interaction bar (full width in detail) — real counts from the
                // re-read live snapshot (icon + count, hidden at 0).
                InteractionBar(
                    replyCount: Int(livePost.replyCount),
                    repostCount: Int(livePost.repostCount),
                    quoteCount: Int(livePost.quoteCount),
                    likeCount: Int(livePost.likeCount),
                    isLiked: livePost.viewerLiked,
                    isReposted: livePost.viewerRepostId != nil,
                    onReply: { replyingTo = livePost },
                    onRepost: { Task { await vm.repostPost(postId: post.postId) } },
                    onQuote: { Task { await vm.quotePost(postId: post.postId) } },
                    onLike: { Task { await vm.likePost(postId: post.postId) } }
                )
                .padding(.top, 8)

                // The tip surface (monetization.md § Tips) — mirrors the list
                // card (`PostCardView`), off the re-read live snapshot.
                #if !FAUNA_EXCISE_PAYMENTS
                TipSurface(tips: livePost.tips)
                #endif
            }
            .padding()
        }
    }
}
