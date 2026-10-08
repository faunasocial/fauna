import SwiftUI

/// The subset of `AppState`/`MacAppState` a feed post-card needs to render:
/// content-policy verdicts, the web-publish store, and the viewer's own
/// session (actor id, handle, node url) — all three are already the exact
/// same FaunaKit types on both platform state classes (`SessionState`,
/// `ContentPolicyStore`, `WebPublishStore`), just reached through two
/// differently-shaped `@Observable` classes. `AppState` conforms below;
/// `MacAppState` conforms in `Fauna-macOS/App/AppState.swift` (it lives in
/// the app target, not FaunaKit, so its conformance can't live here).
public protocol PostCardAppStateProviding: AnyObject {
    var contentPolicy: ContentPolicyStore { get }
    /// The device's region content plane (`region-blocking.md`), composed into
    /// the same verdict as `contentPolicy`.
    var region: RegionStore { get }
    var webPublish: WebPublishStore { get }
    var session: SessionState { get }
}

extension AppState: PostCardAppStateProviding {}

/// The `PostCardView` (iOS) / `MacPostCardView` (macOS) shared body
/// (`feed.md` § Interaction bar) —
/// closes the "no shared FaunaKit post-card view to hang this off" gap
/// pass #91 named: the two platform bodies were ~90% line-identical (every
/// badge, the document/media/quote/link-preview/tag rendering, all four
/// fire-once `.task(id:)` resolvers, the `InteractionBar` wiring). The
/// genuine platform differences are threaded as parameters rather than
/// forced identical:
///
/// - `@Environment(AppState.self)` vs `@Environment(MacAppState.self)` — two
///   distinct `@Observable` classes, not one shared type with a platform
///   typealias. `appState` is read through `PostCardAppStateProviding`
///   instead (both classes already expose the exact same `SessionState`/
///   `ContentPolicyStore`/`WebPublishStore` FaunaKit types, so the protocol
///   adds no new surface, only a common name for what was already shared —
///   the same shape `FeedPostActionsButton`'s own doc comment already
///   threads individually as `webPublish`/`handle`).
/// - The reply mechanism genuinely differs by design: iOS presents
///   `FeedReplyDialog` inline off this card (its own `@State private var
///   replyingTo` + `.sheet(item:)`); macOS's parent `MacFeedDetailView`
///   presents the same dialog itself. `onReply` is a plain closure either
///   way — the platform wrapper decides what it does with it.
///
/// Everything else that differed (padding/spacing values, the post body's
/// `lineLimit` vs an explicit `.font(.body)`, whether `Ids.postCard` is
/// registered here or on the wrapping list row) was cosmetic, not a design
/// difference — threaded as parameters so this pass changes zero rendered
/// pixels on either platform, not collapsed to one shared value.
public struct PostCardBody: View {
    let post: PostSummary
    let vm: FeedVM
    let appState: any PostCardAppStateProviding
    let verticalPadding: CGFloat
    let cardSpacing: CGFloat
    /// `nil` on iOS (no override — inherits the ambient font); `.body` on
    /// macOS. See `documentBody` below for why this stays a conditional
    /// builder rather than an unconditional `.font(bodyFont)`.
    let bodyFont: Font?
    /// `3` on iOS; `nil` (unlimited) on macOS.
    let bodyLineLimit: Int?
    /// macOS registers `post-card` on this view's own outer modifier chain;
    /// iOS registers it once on the wrapping `ForEach` row in `FeedListView`
    /// instead (both register it exactly once — apple-e2e-automation.md rule
    /// 1's "no duplicate IDs" — just via different call chains; confirmed by
    /// pass #91).
    let selfRegistersPostCardId: Bool
    let onReply: () -> Void
    let onNavigateToPersonalization: () -> Void

    public init(
        post: PostSummary, vm: FeedVM, appState: any PostCardAppStateProviding,
        verticalPadding: CGFloat, cardSpacing: CGFloat, bodyFont: Font?, bodyLineLimit: Int?,
        selfRegistersPostCardId: Bool,
        onReply: @escaping () -> Void, onNavigateToPersonalization: @escaping () -> Void
    ) {
        self.post = post
        self.vm = vm
        self.appState = appState
        self.verticalPadding = verticalPadding
        self.cardSpacing = cardSpacing
        self.bodyFont = bodyFont
        self.bodyLineLimit = bodyLineLimit
        self.selfRegistersPostCardId = selfRegistersPostCardId
        self.onReply = onReply
        self.onNavigateToPersonalization = onNavigateToPersonalization
    }

    /// Own-post delete gate (`FeedPostActionsButton.isOwn`) — apple has no
    /// precomputed `PostSummary.isOwn` field (contrast conversations'
    /// `message.isOwn`), so this compares `post.author` against the caller's
    /// own actor id, mirroring linux's `client.actor_id() == post.author`.
    private var isOwn: Bool { post.author == appState.session.actorId }

    /// A REPOST ROW (`feed.md` § Interaction bar → Repost, ratified
    /// 2026-08-10): attribution + the folded original, no own interaction
    /// bar — the repost post is empty by construction, so its own bar
    /// would be all zeros. Mirrors tui/linux's `is_repost_row`.
    private var isRepostRow: Bool { post.repostedPostId != nil }

    /// The owner of the private-overlay projection the author's name is read
    /// through (`contacts.md` § The private overlay → *Where the nickname
    /// paints*).
    @Environment(ConversationsVM.self) private var conversationsVM

    /// Session-local reveal for THIS card instance only — mirrors
    /// `DmMessageBubble.mutedRevealed`.
    @State private var mutedRevealed = false
    /// The same, for a content-policy `collapse` (family-safety.md § Content
    /// policy). A `block` has no reveal, so this can only un-collapse.
    @State private var contentRevealed = false
    /// The tapped `post-image`'s lightbox item.
    @State private var lightboxItem: LightboxItem?

    /// This post's render decision — the strictest-wins compose of the region
    /// content policy, the guardian floor and the viewer's own thresholds,
    /// resolved entirely in shared Rust off the app-scoped caches the
    /// conversation bubble also reads. Also records this post's Guardian Notify
    /// enforcement — see `ContentPolicyInputs.recordedRender`'s doc.
    private var decision: RegionRenderDecision {
        appState.contentPolicy.inputs.recordedRender(
            itemId: post.postId, labels: post.labels, region: appState.region, subject: .post(post),
            reportKey: post.postId, reportAuthor: post.author)
    }

    private var witnessKey: String { "feed:\(post.postId)" }

    public var body: some View {
        let decision = decision
        arms(decision)
            .regionBlockWitness(witnessKey, blocked: decision.isRegionBlocked)
    }

    @ViewBuilder
    private func arms(_ decision: RegionRenderDecision) -> some View {
        let contentVerdict = decision.verdict
        // The region arm first, AHEAD of the family arm (same verb, better
        // attributed — `region-blocking.md` § The blocked render); its collapse
        // reveal shares the family reveal state. The content-policy verdict is
        // checked AHEAD of the mute arm (the linux/web/android ordering): a
        // guardian `block` must never be reachable through the muted-keyword
        // reveal.
        if let withheld = decision.withheld(revealed: contentRevealed) {
            postCardId(RegionPlaceholderView(placeholder: withheld, witnessKey: witnessKey) {
                contentRevealed = true
            }.padding(.vertical, verticalPadding))
        } else if contentVerdict == "block" {
            postCardId(ContentPolicyBlockedNotice(reported: decision.reported).padding(.vertical, verticalPadding))
        } else if contentVerdict == "collapse" && !contentRevealed {
            postCardId(ContentPolicyCollapsedPlaceholder { contentRevealed = true }.padding(.vertical, verticalPadding))
        } else if (vm.manager?.isMuted(postId: post.postId) ?? false) && !mutedRevealed {
            postCardId(FeedPostMutedPlaceholder(onReveal: {
                mutedRevealed = true
                FeedVM.revealedMutedPostIds.insert(post.postId)
            }).padding(.vertical, verticalPadding))
        } else {
            cardContent
        }
    }

    @ViewBuilder
    private func postCardId<V: View>(_ view: V) -> some View {
        if selfRegistersPostCardId {
            view.accessibilityIdentifier(Ids.postCard)
        } else {
            view
        }
    }

    /// Applies `bodyFont` only when non-nil, rather than an unconditional
    /// `.font(bodyFont)` — `.font(nil)` writes an explicit "no font" into the
    /// environment, which is NOT provably identical to never calling
    /// `.font()` at all if some ancestor ever sets one; this keeps iOS's
    /// original "no `.font()` call, `.lineLimit(3)` only" shape byte-exact.
    @ViewBuilder
    private var documentBody: some View {
        if let bodyFont {
            DocumentBodyView(document: post.document).font(bodyFont)
        } else {
            DocumentBodyView(document: post.document)
        }
    }

    private var cardContent: some View {
        postCardId(
            VStack(alignment: .leading, spacing: cardSpacing) {
                // Author row with source badge
                HStack {
                    automationText(Ids.postAuthor, conversationsVM.postAuthorLabel(post))
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)

                    RepostAttributionBadge(isRepost: isRepostRow)

                    ProtocolBadge(source: post.source)

                    UnverifiedSourceBadge(verification: post.verification)

                    // The D10 audit surface (`atproto-pds-full.md` § Problem 1 → D10 →
                    // *Audit*): iff an EXTERNAL app wrote this post as the account. Self-gates
                    // on `.delegated` alone — a `Failed` post reports `.unknown` here and shows
                    // the sibling badge above instead, never this one.
                    DelegatedOriginBadge(authoringOrigin: post.authoringOrigin)

                    GatedPostBadge(gatedTier: post.gatedTier, roomLabel: post.roomLabel)
                    // Sold-post buyer teaser (gap (2c), monetization.md § Per-post
                    // pay-to-unlock) — same row as the badge, mirroring linux/windows.
                    PostUnlockOfferTeaser(postId: post.postId, offer: post.unlockOffer, vm: vm)

                    // Per-row content-label verdict (moderation.md § Per-row badge data
                    // path) — the feed twin of DmMessageBubble's identical
                    // primaryContentLabel(labels:) call; PostSummary.labels is already
                    // FFI-carried (fauna-feed snapshot), this was the one missing render.
                    if let label = primaryContentLabel(labels: post.labels) {
                        ContentLabelBadge(category: label.category)
                    }

                    Spacer()

                    Text(ValueFormat.relativeTime(thenMs: post.timestamp))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }

                // Post body — the shared document walk (render-model.md § D6), the same model the
                // conversations bubble paints; replaces the flat `post.body` text render.
                documentBody
                    .lineLimit(bodyLineLimit)
                    .accessibilityIdentifier(Ids.feedPostText)
                    .automationValue(Ids.feedPostText, text: { renderDocumentToPlaintext(document: post.document) })

                // Per-card reveal when the body carries ≥1 still-blocked remote image (D3). Tapping
                // dispatches to the manager, which re-emits with `revealed = true`; the ForEach rebuilds
                // this card off the fresh snapshot post (no client-side flag — render-model.md § D3).
                if hasBlockedRemoteImages(post.document) {
                    Button { vm.revealRemoteImages(post.postId) } label: {
                        Label(L.conversations.detail.loadRemoteContent, systemImage: "photo")
                            .font(.caption2)
                    }
                    .buttonStyle(.borderless)
                    .accessibilityIdentifier(Ids.loadRemoteContentButton)
                    .automationActivate(Ids.loadRemoteContentButton) { vm.revealRemoteImages(post.postId) }
                }

                // Media — the document's folded `Image` block (render-model.md § D6), painted via
                // `post-image`; replaces the sibling `post.mediaHash` read. Tapping opens the
                // `image-lightbox` (ui.yaml `image-lightbox`; mirrors web's `C2paImage` onclick
                // wiring on the same list-card surface). The image's own `.onTapGesture` takes
                // priority over the card's outer `.onTapGesture` (post-detail push); the same
                // action handed to `PostImage` is what the in-process e2e driver actually
                // invokes (apple-e2e-automation.md rule 1 — the driver performs no real gestures),
                // registered by the leaf together with its paint read.
                //
                // Which source this is, is not a judgment the card makes: `vm.documentPostImage`
                // takes the blob image first — asking the shared manager, which holds the keys,
                // whether it is sealed (`ui/media.md` § Encryption at rest) — else a bridged
                // post's `ProxiedImage` (render-model.md § D6c), never an absolute URL.
                if let source = vm.documentPostImage(post.document), !source.isUnavailable {
                    let openLightbox = { lightboxItem = LightboxItem(source: source) }
                    PostImage(source: source, activate: openLightbox)
                        .frame(maxHeight: 200)
                        .clipShape(RoundedRectangle(cornerRadius: 8))
                        .contentShape(Rectangle())
                        .onTapGesture(perform: openLightbox)
                    // Provenance is a blob's (`ui/media.md` § C2PA provenance); a proxied
                    // picture carries no badge, as on tui.
                    if let hash = documentMediaImageHash(post.document) {
                        PostImageC2paBadge(hash: hash, check: { await vm.hasC2pa($0) })
                    }
                }
                // `video-thumbnail` — the D6b `Video` sibling of `Image` (render-model.md § D6b);
                // mutually exclusive with the image branch above, same fold. A tap plays it in
                // place off the shared `playback_source` (§ D6c → Inline playback).
                if let hash = documentMediaVideoHash(post.document) {
                    VideoThumbnailView(hash: hash, resolve: { await vm.videoPlaybackURL(hash) })
                } else if let path = documentMediaProxiedVideoPath(post.document) {
                    // A bridged post's `ProxiedVideo` (§ D6c → Proxied video): glyph + path, inert.
                    VideoThumbnailView(proxiedPath: path)
                }

                // Quoted post — the document's folded `QuotedPost` block (render-model.md § D6);
                // replaces the sibling `post.quotedPostId` + client-side resolve decode.
                if let quote = documentQuotedPost(post.document) {
                    QuotedPostCard(author: shortId(hex: quote.author), postBody: quote.body, verification: quote.verification,
                                   authoringOrigin: quote.authoringOrigin,
                                   legalTakedownRef: quote.legalTakedownRef, notFound: quote.notFound)
                }

                // Link previews — the document's folded `LinkPreview` blocks (render-model.md § D4). One
                // card per Resolved block; Resolving/Failed → no card (the inline body link already
                // shows). The og:image is reveal-gated — `imageURL` is wired only when `revealed`, so the
                // post's existing `load-remote-content-button` reveals it (it counts the og:image now).
                ForEach(Array(documentResolvedLinkPreviews(post.document).enumerated()), id: \.offset) { index, preview in
                    LinkPreviewCard(
                        url: preview.url, title: preview.title, description: preview.description,
                        imageURL: (preview.revealed ? preview.imageHash : nil).flatMap { vm.blobURL($0) },
                        index: index)
                }

                // Tags
                if !post.tags.isEmpty {
                    HStack(spacing: 4) {
                        ForEach(post.tags, id: \.self) { tag in
                            automationText(Ids.tagChip, "#\(tag)")
                                .font(.caption2)
                                .padding(.horizontal, 6)
                                .padding(.vertical, 2)
                                .background(.blue.opacity(0.1))
                                .clipShape(Capsule())
                        }
                    }
                }

                // Interaction bar — real counts from the shared snapshot (icon + count,
                // hidden at 0; feed.md § Interaction bar). Each button is interact glue.
                // A REPOST ROW renders no bar at all (`isRepostRow`): its own counters
                // are structurally dark (empty by construction), and the original's
                // live bar — with the toggle's `on` state — is one activation away.
                // Mirrors tui/linux's `if !is_repost_row`.
                if !isRepostRow {
                    InteractionBar(
                        replyCount: Int(post.replyCount),
                        repostCount: Int(post.repostCount),
                        quoteCount: Int(post.quoteCount),
                        likeCount: Int(post.likeCount),
                        isLiked: post.viewerLiked,
                        isReposted: post.viewerRepostId != nil,
                        onReply: onReply,
                        onRepost: { Task { await vm.repostPost(postId: post.postId) } },
                        onQuote: { Task { await vm.quotePost(postId: post.postId) } },
                        onLike: { Task { await vm.likePost(postId: post.postId) } }
                    )
                }

                // The tip surface (monetization.md § Tips) — nil until
                // `resolvePostTips` resolves, and permanently nil in a
                // payments-excised build.
                #if !FAUNA_EXCISE_PAYMENTS
                TipSurface(tips: post.tips)
                #endif

                FeedPostActionsButton(
                    postId: post.postId, vm: vm, isOwn: isOwn,
                    webSlug: post.webSlug, gatedTier: post.gatedTier,
                    webPublish: appState.webPublish,
                    handle: appState.session.handle ?? "",
                    reportTarget: isOwn ? nil : reportPostTarget(
                        cid: post.postId, author: post.author, plaintext: post.body,
                        gated: post.gatedTier != nil || post.gatedRoom != nil),
                    onNavigateToPersonalization: onNavigateToPersonalization)
            }
            .padding(.vertical, verticalPadding)
        )
        // Fire-once resolution: the manager folds the `Image`/`ProxiedImage` block into `document`
        // and re-emits, so the render reads it from the document above. Guarded on `mediaHash`,
        // which a resolve never leaves `nil` (`Some("")` for an all-remote bridged post) — the
        // document's blob image would stay absent for such a post and re-fire the resolve on every
        // appearance (render-model.md § D6c; tui keys on the same field).
        .task(id: post.hasMedia && post.mediaHash == nil) {
            if post.hasMedia && post.mediaHash == nil {
                await vm.resolveMedia(post.postId)
            }
        }
        // Widened to `repostedPostId` too (feed.md § Interaction bar →
        // Repost): a REPOST ROW folds the same original-post embed through
        // this identical door — `FeedManager::resolve_quoted_post` already
        // matches either id server-side, so a repost row needs no second
        // resolve path.
        .task(id: (post.quotedPostId ?? post.repostedPostId) != nil && documentQuotedPost(post.document) == nil) {
            if let qid = post.quotedPostId ?? post.repostedPostId, documentQuotedPost(post.document) == nil {
                _ = await vm.resolveQuotedPost(qid)
            }
        }
        // Sold-post buyer teaser fire-once resolve (gap (2c), monetization.md §
        // Per-post pay-to-unlock) → the manager folds it into `PostSummary.unlockOffer`
        // + re-emits → the card paints gated-post-price/-payment-link/-buy-button.
        // Guarded on `unlockOffer == nil` alone, same shape as resolveMedia/
        // resolveQuotedPost above — the re-offer-after-buy guard now lives
        // shared-Rust side (`FeedManager::unlock_purchase_requested`), so this
        // fire-once resolve needs no per-app twin.
        .task(id: post.gatedTier != nil && post.unlockOffer == nil) {
            if post.gatedTier != nil && post.unlockOffer == nil {
                await vm.resolvePostUnlockOffer(post.postId)
            }
        }
        // Fire-once resolve, guarded on `tips == nil` (no "has tips" signal
        // exists in the feed projection — the resolver writes a view on every
        // outcome) → the manager folds it into `PostSummary.tips` + re-emits →
        // the card paints. Mirrors resolveMedia/resolveQuotedPost above.
        #if !FAUNA_EXCISE_PAYMENTS
        .task(id: post.tips == nil) {
            if post.tips == nil {
                await vm.resolvePostTips(post.postId)
            }
        }
        #endif
        // Fire-once resolve for each still-`Resolving` link preview (render-model.md § D4) → the
        // manager folds `Resolved`/`Failed` onto the block + re-emits → the card paints. Keyed on the
        // resolving-url set so it fires once and settles (the media/quoted fire-once pattern).
        .task(id: resolvingLinkPreviewUrls(post.document)) {
            for url in resolvingLinkPreviewUrls(post.document) {
                await vm.resolveLinkPreview(url)
            }
        }
        .fullScreenCover(item: $lightboxItem) { item in
            ImageLightbox(source: item.source, onDismiss: { lightboxItem = nil })
        }
    }
}
