import SwiftUI
import FaunaKit

struct MacFeedDetailView: View {
    let vm: FeedVM

    @Environment(MacAppState.self) private var appState
    @State private var replyingTo: PostSummary?
    @State private var selectedPost: PostSummary?
    @State private var showComposeDialog = false

    // Composer text/tags live in the manager (compose-error / compose-file-ready
    // render from the snapshot — feed.md § Architectural rules).
    private var composeText: Binding<String> {
        Binding(get: { vm.composeText }, set: { vm.setComposeText($0) })
    }
    private var composeTags: Binding<String> {
        Binding(get: { vm.composeTags }, set: { vm.setComposeTags($0) })
    }
    // Gate-to-tier: the select's value is the tier name, or "Public" when ungated
    // (`nil`).
    private var gateBinding: Binding<String> {
        Binding(get: { vm.composeGateSelection }, set: { vm.setComposeGateSelection($0) })
    }
    // The teaser is staged ALONE (`FeedVM.setComposeGatePreview` →
    // `update_compose_preview`) — routing it through whichever answer's own
    // setter (`setComposeGate`/`setComposeGateRoom`/`setComposeSell`) would
    // re-read that mode and could flip it (a staged room answer would be
    // dropped by `update_compose_gate`'s unconditional `gate_room = None`;
    // `ui/feed.md` § Encryption at rest → *The composer's fourth answer*).
    private var gatePreview: Binding<String> {
        Binding(get: { vm.composeGatePreview }, set: { vm.setComposeGatePreview($0) })
    }
    // Sell-this-post: the select's third answer (monetization.md § Per-post
    // pay-to-unlock). The price binding keeps the current toggle, and vice versa.
    private var sellPrice: Binding<String> {
        Binding(get: { vm.composeSellPrice },
                set: { vm.setComposeSell(price: $0, subscribersGetItFree: vm.composeSellSubscribersFree) })
    }
    // The machine-comparable threshold (monetization.md § The asking price) —
    // independent of sellPrice above, never inferred from it.
    private var sellAskingPrice: Binding<String> {
        Binding(get: { vm.composeSellAskingPrice },
                set: { vm.setComposeSell(price: vm.composeSellPrice,
                                          subscribersGetItFree: vm.composeSellSubscribersFree,
                                          askingPrice: $0) })
    }

    var body: some View {
        VStack(spacing: 0) {
            // Compose
            VStack(spacing: 6) {
                HStack {
                    TextField(L.feed.post.whatsOnYourMind, text: composeText)
                        .textFieldStyle(.roundedBorder)
                        .accessibilityIdentifier(Ids.composeTextField)
                        .automationField(Ids.composeTextField, text: composeText)
                    ComposeAttachButton(vm: vm)
                    Button(L.common.post) {
                        submitPost()
                    }
                    .disabled(
                        vm.composeText.trimmingCharacters(in: .whitespaces).isEmpty
                        || !vm.composeReady
                    )
                    .buttonStyle(.borderedProminent)
                    .controlSize(.small)
                    .accessibilityIdentifier(Ids.postSubmitButton)
                    .automationActivate(
                        Ids.postSubmitButton,
                        isEnabled: {
                            !vm.composeText.trimmingCharacters(in: .whitespaces).isEmpty
                            && vm.composeReady
                        }
                    ) { submitPost() }
                }
                TextField(L.feed.post.tagsPlaceholder, text: composeTags)
                    .textFieldStyle(.roundedBorder)
                    .font(.caption)
                    .accessibilityIdentifier(Ids.composeTagsField)
                    .automationField(Ids.composeTagsField, text: composeTags)
                MarkdownToolbar(text: composeText)

                // Gate-to-tier controls (`compose-gate-tier-select` /
                // `compose-gate-preview-field`; feed.md § Encryption at rest,
                // monetization.md § Pillars 2+3). The select offers "Public" (the
                // ungated default) + the author's own tiers from `snapshot.own_tiers`
                // + one option per room from `snapshot.own_rooms` ("Room: ‹label›")
                // + "Sell this post…" always last; picking any of them reveals the
                // public-teaser field. Mirrors linux `post_list.rs` (gate_select ←
                // own_tiers, own_rooms, Sell last) and web `+page.svelte`.
                Picker(L.feed.post.gateAudience, selection: gateBinding) {
                    Text(L.feed.post.gatePublic).tag(L.feed.post.gatePublic)
                    ForEach(vm.ownTiers, id: \.name) { tier in
                        Text(tier.name).tag(tier.name)
                    }
                    ForEach(vm.ownRooms, id: \.room) { room in
                        Text(L.feed.post.gateRoom(room: room.label)).tag(L.feed.post.gateRoom(room: room.label))
                    }
                    Text(L.feed.post.gateSell).tag(L.feed.post.gateSell)
                }
                .labelsHidden()
                .controlSize(.small)
                .accessibilityIdentifier(Ids.composeGateTierSelect)
                .automationSelect(
                    Ids.composeGateTierSelect,
                    value: { vm.composeGateSelection },
                    options: { vm.gateOptions }
                ) { newValue in
                    vm.setComposeGateSelection(newValue)
                }
                if vm.composeGateTier != nil || vm.composeGateRoom != nil || vm.composeSell != nil {
                    TextField(L.feed.post.gatePreviewPlaceholder, text: gatePreview)
                        .textFieldStyle(.roundedBorder)
                        .font(.caption)
                        .accessibilityIdentifier(Ids.composeGatePreviewField)
                        .automationField(Ids.composeGatePreviewField, text: gatePreview)
                }
                // "Sell this post…" controls (monetization.md § Per-post
                // pay-to-unlock; IDs user-approved 2026-07-29) — visible only
                // while Sell is the select's current answer.
                if vm.composeSell != nil {
                    TextField(L.feed.post.sellPricePlaceholder, text: sellPrice)
                        .textFieldStyle(.roundedBorder)
                        .font(.caption)
                        .accessibilityIdentifier(Ids.composeSellPrice)
                        .automationField(Ids.composeSellPrice, text: sellPrice)
                    #if !FAUNA_EXCISE_PAYMENTS
                    // The money plane's compose-side half, excised with the
                    // tier form's own asking-price input (ProfileView.swift).
                    TextField(L.feed.post.sellAskingPricePlaceholder, text: sellAskingPrice)
                        .textFieldStyle(.roundedBorder)
                        .font(.caption)
                        .accessibilityIdentifier(Ids.composeSellAskingPrice)
                        .automationField(Ids.composeSellAskingPrice, text: sellAskingPrice)
                    #endif
                    HStack {
                        Text(L.feed.post.sellSubscribersFree)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        // Defaults CHECKED (user-ratified 2026-07-29): an existing
                        // paying subscriber is not charged twice for a post their
                        // subscription would reasonably cover.
                        Toggle("", isOn: Binding(
                            get: { vm.composeSellSubscribersFree },
                            set: { vm.setComposeSell(price: vm.composeSellPrice, subscribersGetItFree: $0) }
                        ))
                        .labelsHidden()
                        .accessibilityIdentifier(Ids.composeSellSubscribersFree)
                        .automationActivate(Ids.composeSellSubscribersFree,
                                            value: { vm.composeSellSubscribersFree ? "on" : "off" }) {
                            vm.setComposeSell(price: vm.composeSellPrice,
                                               subscribersGetItFree: !vm.composeSellSubscribersFree)
                        }
                    }
                }

                if let composeError = vm.composeError {
                    Text(composeError)
                        .font(.caption)
                        .foregroundStyle(.red)
                        .accessibilityIdentifier(Ids.composeError)
                        // Optional-inside-`if let`: keep the literal id + re-read
                        // live (the error may toggle between body passes) — same
                        // reasoning as iOS's compose-error (FeedListView).
                        .automationValue(Ids.composeError, text: { vm.composeError })
                }

                if let attached = vm.composeAttachedFile {
                    HStack(spacing: 4) {
                        Image(systemName: "checkmark.circle.fill")
                            .foregroundStyle(.green)
                        automationText(Ids.composeFileReady,
                                       "\(attached.name)  \(ValueFormat.byteSize(attached.size))")
                            .foregroundStyle(.green)
                        Button {
                            vm.removeComposeAttachment()
                        } label: {
                            Image(systemName: "xmark.circle.fill")
                        }
                        .buttonStyle(.plain)
                        .help(L.common.remove)
                        .accessibilityIdentifier(Ids.composeFileRemove)
                        .automationActivate(Ids.composeFileRemove) { vm.removeComposeAttachment() }
                    }
                    .font(.caption)
                }
            }
            .padding()

            HStack {
                Spacer()
                Button {
                    showComposeDialog = true
                } label: {
                    Image(systemName: "doc.richtext")
                }
                .buttonStyle(.plain)
                .controlSize(.small)
                .accessibilityIdentifier(Ids.composeDialogButton)
                .automationActivate(Ids.composeDialogButton) {
                    showComposeDialog = true
                }
            }
            .padding(.horizontal)

            Divider()

            if vm.showsLoadingSpinner {
                ProgressView()
                    .frame(maxHeight: .infinity)
            } else if let emptyState = vm.emptyState {
                FeedEmptyStateView(state: emptyState)
                    .frame(maxHeight: .infinity)
            } else {
                // Eager `ScrollView{VStack}`, NOT `List` — a `List`'s NSTableView
                // row recycling can silently DECOUPLE the in-process driver's
                // registration order from the array/visual order once the row
                // set REORDERS in place (a trained-topic re-rank moves a post to
                // a new index without changing the total count or row heights),
                // so an indexed `feed-post-actions-button[i]` click can resolve
                // to the WRONG post after a re-rank. Same fix shape as
                // `MacCalendarListView`'s and `LinkedNestsView`'s prior List→
                // ScrollView rewrites for this exact bug class (confirmed via
                // an `.onAppear` id trace: `open_post_actions(index: 0)` after a
                // re-rank opened a DIFFERENT post than the one actually trained).
                ScrollView {
                    VStack(spacing: 0) {
                        // `enumerated` so each card can push `post-card[offset]` as the
                        // ancestor scope for its in-process-driver children (the
                        // `quoted-post` embed + `unverified-source-badge`), giving the
                        // scoped queries `post-card[i]/quoted-post` real subtree
                        // resolution. `offset` is the document-order row index the
                        // driver's `post-card[i]` addresses.
                        ForEach(Array(vm.posts.enumerated()), id: \.element.id) { offset, post in
                            MacPostCardView(
                                post: post, vm: vm,
                                onReply: { replyingTo = post },
                                onNavigateToPersonalization: {
                                    appState.selectedSidebar = .settings
                                    appState.selectedSettingsPage = .personalization
                                }
                            )
                            .padding(.horizontal)
                            .onTapGesture { openPostCard(post) }
                            // In-process automation actuation of the card (opens the
                            // detail sheet). `.onTapGesture` is a native gesture the
                            // registry can't invoke, so register the same action here —
                            // one Entry carrying both the activate AND the body read
                            // (`value:`), so `/element/click` and `/element/text` both
                            // resolve to this slot rather than a separate
                            // `.automationValue` entry splitting them (AutomationRegistry
                            // § automationActivate). This is why `MacPostCardView` no
                            // longer self-registers `post-card` via `.automationValue`.
                            .automationActivate(Ids.postCard, value: { post.body }) {
                                openPostCard(post)
                            }
                            .automationScope(Ids.postCard, index: offset)
                            // Engagement-cue capture (engagement-cues.md § Layer
                            // B) — publishes this card's frame so `cueCapture`
                            // on the ScrollView can measure its honest viewport
                            // dwell. Keyed on the bound post's own id, never
                            // `offset`: a re-rank reorders rows, and an
                            // index-derived key would credit one post's dwell to
                            // another.
                            .cueCard(postId: post.postId)
                            Divider()
                        }

                        if vm.hasMore {
                            Button(L.common.loadMore) {
                                Task { await vm.loadMore() }
                            }
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 8)
                        }
                    }
                }
                // Engagement-cue capture: reads this ScrollView's frame as the
                // viewport every `cueCard` row is measured against, ticks while
                // the feed is on screen, and drains + seals on disappear.
                .cueCapture(feedVM: vm) { generation, message in
                    vm.landClientErrorMessage(generation: generation, message: message)
                }
            }

            if let error = vm.errorMessage {
                ErrorBanner(message: error).padding()
            }
        }
        .pageTitle(L.feed.list.title)
        .sheet(item: $replyingTo) { post in
            FeedReplyDialog(post: post, vm: vm)
        }
        .sheet(item: $selectedPost) { post in
            PostDetailSheet(post: post, vm: vm)
        }
        .sheet(isPresented: $showComposeDialog) {
            FeedComposeDialog(vm: vm, onClose: { showComposeDialog = false })
        }
        // A re-login rebuilds the `FeedManager` (managerGeneration bumps): dismiss
        // any detail the previous actor left open, so its fire-once gated-unlock
        // `.task` can't fire against the new actor's feed (uniform with iOS
        // `FeedListView`; keyed on managerGeneration, not secretHex — priority #1).
        .onChange(of: vm.managerGeneration) { selectedPost = nil }
        // A cross-page deep link (`search-result-item` activation,
        // `ui/search.md` § Where logic lives → Result navigation): switch to
        // this page happens synchronously at the call site (AppState.selectedSidebar),
        // this task then resolves the target post — fast path if the timeline
        // already has it, else the one round trip `resolvePost` needs — and opens
        // it the same way an in-list card tap does.
        .task(id: vm.pendingPostOpen) {
            guard let postId = vm.pendingPostOpen else { return }
            vm.pendingPostOpen = nil
            if let post = vm.findPost(postId: postId) {
                selectedPost = post
                return
            }
            await vm.resolvePost(postId)
            // `.unavailable`/no snapshot hit degrades to a no-op — the same
            // posture every pre-existing stale-id open takes.
            if let post = vm.findPost(postId: postId) {
                selectedPost = post
            }
        }
    }

    /// The post-submit button's action. Compose state already lives in the manager
    /// (set via the field bindings); `submitPost` validates + builds + sends + clears.
    private func submitPost() {
        Task { await vm.submitPost() }
    }

    /// Shared logic — FaunaKit's `openPostCard(_:vm:selectedPost:)`.
    private func openPostCard(_ post: PostSummary) {
        FaunaKit.openPostCard(post, vm: vm, selectedPost: &selectedPost)
    }
}

private struct PostDetailSheet: View {
    let post: PostSummary
    let vm: FeedVM
    @Environment(\.dismiss) private var dismiss
    /// The owner of the private-overlay projection the author's name is read
    /// through (`contacts.md` § The private overlay).
    @Environment(ConversationsVM.self) private var conversationsVM
    @State private var replyingTo: PostSummary?

    private var livePost: PostSummary { vm.livePost(for: post) }

    var body: some View {
        Group {
            if let reference = livePost.legalTakedownRef {
                legalTakedownContent(reference)
            } else {
                // The region content plane's arm (region-blocking.md § Where it
                // composes) — shared with iOS `PostDetailView`.
                RegionGatedPostContent(post: livePost) { normalPostContent }
            }
        }
        .padding(24)
        .frame(width: 500, height: 400)
        .accessibilityIdentifier(Ids.feedPostDetailDialog)
        .automationValue(Ids.feedPostDetailDialog, text: { "" })
        // A scope container, so `scope="feed-post-detail-dialog"` reads THIS detail's
        // `post-image` rather than falling back to a flat index that can land on a
        // list card's still-registered image beneath the sheet. Outside the reply
        // `.sheet` below, whose dialog is not part of this detail.
        .automationScope(Ids.feedPostDetailDialog, index: 0)
        .sheet(item: $replyingTo) { post in
            FeedReplyDialog(post: post, vm: vm)
        }
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
        // Widened to `repostedPostId` too — mirrors `MacPostCardView`'s
        // identical widening (feed.md § Interaction bar → Repost); routing
        // (below) never opens a repost row's OWN detail, but this keeps the
        // fire-once resolve consistent with the list card's door.
        .task(id: (livePost.quotedPostId ?? livePost.repostedPostId) != nil && documentQuotedPost(livePost.document) == nil) {
            if let qid = livePost.quotedPostId ?? livePost.repostedPostId, documentQuotedPost(livePost.document) == nil {
                _ = await vm.resolveQuotedPost(qid)
            }
        }
        // Fire-once resolve, guarded on `tips == nil` — mirrors the list
        // card's identical `.task` in `MacPostCardView`.
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
        // teaser (the manager can't decrypt without a key). Sibling of the resolves above.
        .task(id: livePost.gatedTier != nil && !livePost.gatedUnlocked) {
            if livePost.gatedTier != nil && !livePost.gatedUnlocked {
                await vm.unlockGatedPost(post.postId)
            }
        }
    }

    /// A post taken down under a legal obligation (`moderation.md` § Categories &
    /// enforcement item 1): the nest withholds the sealed body under a legal
    /// obligation, so the detail collapses to the shared localized tombstone —
    /// no author, no badges, no tags, no image, no quoted-post embed, no
    /// actions. Mirrors `DmMessageBubble`'s identical branch and linux's
    /// `build_post_detail` (`views/feed/post_detail.rs`).
    private func legalTakedownContent(_ reference: String) -> some View {
        VStack(alignment: .leading, spacing: 16) {
            automationText(Ids.feedPostDetailBody, renderLocalizedText(legalTakedownTombstone(reference: reference)))
                .font(.body)
                .foregroundStyle(.secondary)
            Spacer()
            HStack {
                Spacer()
                Button(L.common.close) { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
        }
    }

    private var normalPostContent: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                // The shared resolver's name for the author — the viewer's
                // nickname, else the public name, else the canonical short id —
                // the same string the list card paints.
                automationText(Ids.feedPostDetailAuthor, conversationsVM.postAuthorLabel(post))
                    .font(.headline.monospaced())
                    .lineLimit(1)
                    .truncationMode(.middle)
                // Focal-post unverified-source badge — mirrors the list card
                // (`MacPostCardView`) so the post-detail pane shows the same 2a badge
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
            // dispatches to the manager, which re-emits with `revealed = true`; `livePost` re-reads
            // the fresh snapshot and the detail repaints (no client-side flag — render-model.md § D3).
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

            // Link previews — the document's folded `LinkPreview` blocks (render-model.md § D4). One
            // card per Resolved block; Resolving/Failed → no card. og:image reveal-gated off `livePost`
            // (the re-read live snapshot, as the reveal button + quoted badge above do).
            ForEach(Array(documentResolvedLinkPreviews(livePost.document).enumerated()), id: \.offset) { index, preview in
                LinkPreviewCard(
                    url: preview.url, title: preview.title, description: preview.description,
                    imageURL: (preview.revealed ? preview.imageHash : nil).flatMap { vm.blobURL($0) },
                    index: index)
            }

            if !post.tags.isEmpty {
                HStack(spacing: 4) {
                    ForEach(post.tags, id: \.self) { tag in
                        Text("#\(tag)")
                            .font(.caption2)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(.blue.opacity(0.1))
                            .clipShape(Capsule())
                    }
                }
            }

            // Real counts from the re-read live snapshot (icon + count, hidden at 0).
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

            // The tip surface (monetization.md § Tips) — mirrors the list
            // card (`MacPostCardView`), off the re-read live snapshot.
            #if !FAUNA_EXCISE_PAYMENTS
            TipSurface(tips: livePost.tips)
            #endif

            Spacer()

            HStack {
                Spacer()
                Button(L.common.close) { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
        }
    }
}
