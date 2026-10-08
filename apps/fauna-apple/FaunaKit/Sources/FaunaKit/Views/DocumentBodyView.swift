import SwiftUI

/// Per-block SwiftUI renderer for a shared [`RenderDocument`] (render-model.md § D1/D6) — the
/// thin Apple *render* layer over the one structured body model every app paints.
///
/// **The body is produced once, in shared Rust** (`fauna_core::render`): the conversations
/// manager builds `MessageSnapshot.document` from `(body, body_format)` via `document_for_body`,
/// and the feed manager builds `PostSummary.document` via `markdown_to_document` (D6). This view
/// walks the typed `RenderBlock`/`Inline` tree into a body — a vertical stack of per-block `Text`
/// runs (inline bold/italic/code/link via `AttributedString`; block styling via fonts/indents) —
/// with remote `![alt](url)` images rendered **blocked by default**, revealed per-message on an
/// explicit opt-in (html-mail Slice 3 — `docs/goal/behavior/html-mail.md` § Rendering). No client
/// re-parses or re-formats the body — it only paints the document the manager already built
/// (render-model.md § The boundary). It replaces `MarkdownBodyView`, which re-parsed `msg.body`
/// at render time via the flat `FfiMdBlock` token model.
///
/// **Shared by the conversations bubble (`DmMessageBubble`) and the feed post cards** (priority
/// #1/#4 — one walker for both pages, mirroring linux `views/document.rs` and windows'
/// `DocumentPainter`).
///
/// **Embeds are extracted, not walked here.** The producers fold attachments / quoted posts /
/// trusted media / in-bubble reply-quotes into the document as first-class blocks (D2/D6), but those
/// render as their own elements (`dm-attachment-image`, `quoted-post`, `post-image`,
/// `dm-message-quote`, `link-preview-card`) **off** the text flow — so this walker SKIPS
/// `.attachment` / `.quotedPost` / `.image` / `.quotedMessage` / `.linkPreview` (all no-op); the
/// caller pulls them via the `documentAttachments` extractor (D2a), the bubble's
/// `documentQuotedMessage` reply-quote extractor (D2b), the feed `documentQuotedPost`/
/// `documentMediaImageHash` extractors (D6), and the `documentResolvedLinkPreviews` extractor (D4). A
/// `.remoteImage` (a markdown inline
/// image) *stays* in the body flow, rendered
/// blocked. Because the producer promotes every remote image out of its paragraph into a sibling
/// `RemoteImage` block, a `Paragraph`/`Heading` here carries text inlines only — image handling is
/// one flat block arm, no per-line segmentation.
public struct DocumentBodyView: View {
    let document: RenderDocument

    public init(document: RenderDocument) {
        self.document = document
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            ForEach(Array(document.blocks.enumerated()), id: \.offset) { _, block in
                RenderBlockView(block: block, marker: "")
            }
        }
    }
}

/// One [`RenderBlock`] → SwiftUI. A **struct** (not a recursive `@ViewBuilder` method): the
/// block-quote / list arms recurse by instantiating `RenderBlockView` for child blocks, and a
/// nominal type breaks the "opaque return type defined in terms of itself" inference a recursive
/// `-> some View` method hits. Covers every block kind the producers emit; `.attachment` /
/// `.quotedPost` are embeds the caller renders off the text flow (extractors below), so they no-op.
private struct RenderBlockView: View {
    let block: RenderBlock
    /// A list bullet/number prepended to a paragraph/heading's first inline run (only set by the
    /// list arm for an item's first block); "" otherwise.
    let marker: String

    var body: some View {
        switch block {
        case let .paragraph(inlines):
            let runs = renderInlineLineRuns(inlines: inlines)
            LineRunsView(count: runs.count) { i in
                Text(attributedInlines(runs[i], prefix: i == 0 ? marker : ""))
                    .textSelection(.enabled)
            }
        // A heading is one line by construction (ATX), so it never carries the hard
        // breaks the line-run split exists for.
        case let .heading(level, inlines):
            Text(attributedInlines(inlines, prefix: marker))
                .font(headingFont(level))
                .textSelection(.enabled)
        case let .blockQuote(blocks):
            HStack(spacing: 6) {
                RoundedRectangle(cornerRadius: 1).fill(Color.secondary).frame(width: 3)
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(Array(blocks.enumerated()), id: \.offset) { _, b in
                        RenderBlockView(block: b, marker: "")
                    }
                }
                .foregroundStyle(.secondary)
                Spacer(minLength: 0)
            }
            .fixedSize(horizontal: false, vertical: true)
        case let .listBlock(ordered, items):
            // The shared model carries no item numbers — an ordered list renumbers from 1, like
            // `<ol>`. Each item is a sub-document; the bullet/number prefixes its first block.
            VStack(alignment: .leading, spacing: 2) {
                ForEach(Array(items.enumerated()), id: \.offset) { i, item in
                    let m = ordered ? "\(i + 1).\u{00a0}" : "\u{2022}\u{00a0}"
                    ForEach(Array(item.blocks.enumerated()), id: \.offset) { j, b in
                        RenderBlockView(block: b, marker: j == 0 ? m : "")
                    }
                }
            }
        case let .taskList(items):
            // A GFM task list (render-model.md § D7a): a sibling of `.listBlock` whose items carry
            // a checked state. A static checked/unchecked box glyph prefixes each item's first
            // block (read-side render — the editable checkbox is the Notes editor's job).
            VStack(alignment: .leading, spacing: 2) {
                ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                    let m = item.checked ? "\u{2611}\u{00a0}" : "\u{2610}\u{00a0}"
                    ForEach(Array(item.blocks.enumerated()), id: \.offset) { j, b in
                        RenderBlockView(block: b, marker: j == 0 ? m : "")
                    }
                }
            }
        case let .codeBlock(_, text):
            // Code-block text is literal — no inline markup; monospace + a dim background.
            let runs = renderTextLineRuns(text: text)
            LineRunsView(count: runs.count) { i in
                Text(runs[i])
                    .font(.body.monospaced())
                    .textSelection(.enabled)
            }
            .padding(6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Color.secondary.opacity(0.12), in: RoundedRectangle(cornerRadius: 4))
        case let .remoteImage(url, alt, revealed):
            remoteImageView(url: url, alt: alt, revealed: revealed)
        case .image, .video, .proxiedImage, .proxiedVideo, .attachment, .quotedPost, .quotedMessage,
             .linkPreview:
            // `.proxiedImage` (a bridged post's nest-served picture, render-model.md § D6c) is the
            // feed page's `post-image` slot too, extracted by `documentMediaProxiedImagePath`.
            // `.proxiedVideo` (its video twin, § D6c → Proxied video) belongs to the
            // `video-thumbnail` slot, extracted by `documentMediaProxiedVideoPath`.
            //
            // Embeds (trusted media / attachment / feed quoted-post / in-bubble reply-quote) —
            // first-class blocks the manager folds in, but rendered as their own elements
            // (`post-image` / `dm-attachment-image` / `quoted-post` / `dm-message-quote`) **off**
            // the text flow. The caller extracts them via `documentMediaImageHash` /
            // `documentAttachments` / `documentQuotedPost` / `documentQuotedMessage`; here they
            // no-op so the body walk carries text + remote markdown images only (matches web +
            // android, which also skip `.quotedMessage` in the walk and paint the reply-quote card
            // above the body). The `dm-message-quote` card itself is the conversations bubble's job.
            //
            // `.linkPreview` (render-model.md § D4) is likewise extracted, not walked: the producer
            // emits it for a standalone bare-url paragraph (leaving the inline link in the paragraph
            // above, so the link always shows), and the caller pulls the blocks via
            // `documentResolvedLinkPreviews` and paints a `LinkPreviewCard` per Resolved block **off** the
            // text flow — the same extract-then-paint shape as `.quotedPost`. A `Resolving`/`Failed`
            // block paints no card (the inline link already shows — no skeleton). The arm stays a
            // no-op so the body walk carries text + remote markdown images only and the switch stays
            // exhaustive on macOS.
            EmptyView()
        }
    }

    /// A remote `![alt](url)` image. Blocked by default (placeholder); fetched only once the
    /// **manager** marks this image revealed (D3 — `revealed` is projected onto the block by
    /// `ConversationsManager`/`FeedManager` at the read boundary; the client holds no reveal state
    /// of its own). The `AsyncImage` here is the sole inbound-image request.
    ///
    /// Registers `doc-remote-image` (indexed; ID user-approved 2026-07-31; render-model.md § D3,
    /// `tui.md` § Rendering) in **every** state — blocked, revealed-but-loading, painted — under
    /// one `Group` so the id survives the state transition instead of disappearing and
    /// re-registering (which would defeat "the element registers in every state" — an absence
    /// assertion against an element that never registered would pass for the wrong reason). tui
    /// paints the picture as text and so can assert the paint itself; apple's `AsyncImage` has no
    /// headlessly-observable paint, so this only needs to prove presence + the reveal-gate
    /// transition (`test_feed_remote_image.py`) — never weaken that by trying to fake a paint
    /// signal here.
    @ViewBuilder private func remoteImageView(url: String, alt: String, revealed: Bool)
        -> some View
    {
        Group {
            if revealed, let parsed = URL(string: url) {
                AsyncImage(url: parsed) { phase in
                    switch phase {
                    case let .success(image):
                        image.resizable().scaledToFit()
                    case .failure:
                        blockedRemoteImage(alt: alt)
                    default:
                        ProgressView()
                    }
                }
                .frame(maxHeight: 240, alignment: .leading)
            } else {
                blockedRemoteImage(alt: alt)
            }
        }
        .accessibilityIdentifier(Ids.docRemoteImage)
        .automationValue(Ids.docRemoteImage, text: { alt })
    }

    private func headingFont(_ level: UInt8) -> Font {
        switch level {
        case 1: return .title2.bold()
        case 2: return .title3.bold()
        default: return .headline
        }
    }
}

/// A text block painted as its shared line runs (`renderInlineLineRuns` /
/// `renderTextLineRuns` — `fauna_core::render::inline_line_runs`, render-model.md § Where
/// logic lives). One run — a block of `MAX_LINES_PER_TEXT_RUN` lines or fewer, i.e. nearly
/// all prose — is its single `Text`, exactly as before. More than one stacks a `Text` per
/// run in a `LazyVStack`, so only the runs on screen are ever laid out.
///
/// Why: SwiftUI sizes a `Text` through `NSStringDrawing`, whose CoreText typesetter
/// re-shapes from every hard line break onward — quadratic in the line count. One `Text`
/// holding a ~3 MiB, ~40 000-line plain-text mail spun the main thread for tens of
/// minutes, freezing the whole app (`mail-message-size.md` § Implementation status today);
/// as lazy 16-line runs it opens in well under a second. A drag-selection spans one run.
private struct LineRunsView<Run: View>: View {
    let count: Int
    @ViewBuilder let run: (Int) -> Run

    var body: some View {
        if count == 1 {
            run(0)
        } else {
            LazyVStack(alignment: .leading, spacing: 0) {
                ForEach(0..<count, id: \.self) { i in
                    run(i)
                }
            }
        }
    }
}

/// A blocked-remote-image placeholder: a picture glyph + the alt text + a dim "Remote image
/// blocked" caption (shared i18n). Holds the alt only — never the bytes, never fetches.
private func blockedRemoteImage(alt: String) -> some View {
    HStack(spacing: 6) {
        Image(systemName: "photo")
            .foregroundStyle(.secondary)
        if !alt.isEmpty {
            Text(alt)
                .font(.caption)
                .foregroundStyle(.secondary)
                .lineLimit(1)
        }
        Text(L.conversations.detail.remoteImageBlocked)
            .font(.caption)
            .foregroundStyle(.secondary)
    }
    .padding(4)
    .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 4))
}

// MARK: - Inline rendering

/// Build one `AttributedString` from an inline run, prepending `prefix` (a list marker).
private func attributedInlines(_ inlines: [Inline], prefix: String) -> AttributedString {
    var attr = AttributedString(prefix)
    appendInlines(inlines, to: &attr, bold: false, italic: false)
    return attr
}

/// Append the typed [`Inline`] tree to `attr`, carrying bold/italic context. Plain text in no
/// emphasis carries **no** font attribute so it inherits the block font (a heading keeps its
/// size); emphasis/code set their own. Style precedence mirrors the shared HTML/GTK renderers
/// (`fauna_core::markdown`, linux `inlines_to_markup`): code, link, then bold/italic.
private func appendInlines(
    _ inlines: [Inline], to attr: inout AttributedString, bold: Bool, italic: Bool
) {
    for inline in inlines {
        switch inline {
        case let .text(text):
            var piece = AttributedString(text)
            if bold || italic { piece.font = bodyFont(bold: bold, italic: italic) }
            attr.append(piece)
        case let .code(text):
            var piece = AttributedString(text)
            piece.font = .body.monospaced()
            piece.backgroundColor = Color.secondary.opacity(0.18)
            attr.append(piece)
        case let .link(href, inner):
            // The link label (recursing for nested emphasis) as one accent-coloured, linked run.
            // A malformed href that `URL(string:)` rejects renders as plain text.
            var label = AttributedString("")
            appendInlines(inner, to: &label, bold: bold, italic: italic)
            if let url = URL(string: href) {
                label.link = url
                label.foregroundColor = .accentColor
            }
            attr.append(label)
        case let .bold(inner):
            appendInlines(inner, to: &attr, bold: true, italic: italic)
        case let .italic(inner):
            appendInlines(inner, to: &attr, bold: bold, italic: true)
        }
    }
}

private func bodyFont(bold: Bool, italic: Bool) -> Font {
    switch (bold, italic) {
    case (true, true): return .body.bold().italic()
    case (true, false): return .body.bold()
    case (false, true): return .body.italic()
    case (false, false): return .body
    }
}

// MARK: - Embed extractors (the document's embed blocks, pulled out of the text flow)

/// The `.attachment` embed blocks of a document, in body order — the D2a projection the
/// conversations manager folds in after the text (render-model.md § D2). The bubble renders each
/// as its own `dm-attachment-image`/`dm-attachment-file` element, replacing the former sibling
/// `message.attachments` loop. Mirrors windows `DocumentRenderer.Attachments` / linux's
/// `Attachment` arm.
public func documentAttachments(_ document: RenderDocument) -> [RenderBlock] {
    document.blocks.filter { if case .attachment = $0 { return true } else { return false } }
}

/// The content hash of the document's folded trusted-media `Image` block (the feed's resolved
/// media), or `nil` — the D6 projection the feed manager folds in **after** the body once
/// `resolve_media` resolves the blob hash (render-model.md § D6). The feed card paints it through
/// the client blob loader as `post-image`, replacing the sibling `post.mediaHash` read.
///
/// Wraps the shared `renderDocumentFirstImageHash` UniFFI face
/// (`fauna_core::render::RenderDocument::first_image_hash`, render.rs) — like its three siblings
/// below, and like `hasBlockedRemoteImages` before them, so the walk lives in one place. The shared
/// face **recurses** into block quotes / list items / task items, where this twin scanned top level
/// only; behaviour is identical on today's folds (the manager pushes the embed at top level) and the
/// recursion is what stops a future nested fold from being silently missed here alone (priority
/// #2/#4 — render-model.md § Implementation status, the four-twins-four-chances-to-miss-an-arm
/// rationale). Named `…Hash` to match web's `mediaImageHash` / android's `documentMediaImageHash`
/// (priority #3 — one concept, one name on all 7 apps).
public func documentMediaImageHash(_ document: RenderDocument) -> String? {
    renderDocumentFirstImageHash(document: document)
}

/// The nest-relative path of the document's first folded `ProxiedImage` block — a bridged
/// post's picture (render-model.md § D6c) — or `nil` when the post has a blob image, which
/// takes the one `post-image` slot first. The exact twin of tui's `proxied_post_image`, over
/// the shared `renderDocumentProxiedImages` face (which recurses, like its siblings).
public func documentMediaProxiedImagePath(_ document: RenderDocument) -> String? {
    guard documentMediaImageHash(document) == nil else { return nil }
    return renderDocumentProxiedImages(document: document).first?.path
}

/// The nest-relative path of the document's first folded `ProxiedVideo` block — a bridged
/// post's video (render-model.md § D6c → *Proxied video*) — or `nil` when the post has a blob
/// `Video`, which takes the one `video-thumbnail` slot first. The strict twin of
/// ``documentMediaProxiedImagePath`` and of `RenderDocument::proxied_post_video`, over the shared
/// `renderDocumentProxiedVideos` face (which recurses, like its siblings). The slot paints the
/// play glyph + this path as text; nothing fetches or parses it.
public func documentMediaProxiedVideoPath(_ document: RenderDocument) -> String? {
    guard documentMediaVideoHash(document) == nil else { return nil }
    return renderDocumentProxiedVideos(document: document).first?.path
}

/// The content hash of the document's folded trusted-media `Video` block, or `nil` — the exact
/// twin of [`documentMediaImageHash`], one fold apart: `media_blocks` makes the image-vs-video
/// branch once, so a document never carries both for the same attachment (render-model.md §
/// D6b). The feed card paints it as `video-thumbnail` via `VideoThumbnailView` — **no poster
/// frame exists to paint** (`MediaItem.thumbnail`/`dimensions` are `None` from every writer,
/// deliberately, so a poster field would be dead on arrival), which is why the paint is a play
/// glyph + the hash as text, mirroring tui/linux/android rather than inventing one.
///
/// Wraps the shared `renderDocumentFirstVideoHash` UniFFI face
/// (`fauna_core::render::RenderDocument::first_video_hash`, render.rs) — top-level only, unlike
/// `first_image_hash`'s recursion into quotes/lists, because the feed fold never nests a `Video`
/// block. Named `…Hash` to match web's `mediaVideoHash` / android's `documentMediaVideoHash`
/// (priority #3 — one concept, one name on all 7 apps).
public func documentMediaVideoHash(_ document: RenderDocument) -> String? {
    renderDocumentFirstVideoHash(document: document)
}

/// The document's folded `QuotedPost` embed (`post_id`/`author`/`body`/`verification`/
/// `authoring_origin`), or `nil` — the D6 projection the feed manager folds in after the body once
/// `resolve_quoted_post` resolves the quote (render-model.md § D6). The feed card paints the
/// `quoted-post` card from it, replacing the sibling `post.quotedPostId` + client-side `resolve`
/// decode. Mirrors windows `DocumentRenderer.QuotedPost` / linux `has_quoted_post`.
/// Wraps the shared `renderDocumentQuotedPost` UniFFI face
/// (`fauna_core::render::RenderDocument::quoted_post`, render.rs), returning the generated
/// `QuotedPostEmbedOwned` record — the local tuple mirror this replaced is deleted, exactly as
/// android dropped its hand-rolled `ResolvedLinkPreview` mirror (priority #2/#4). The record's
/// fields carry the same meanings the tuple did:
///
/// - `verification` — the *quoted* post's, so `QuotedPostCard` paints `unverified-source-badge`
///   iff it is `Failed` (security.md § App display of unverified content; the feed manager
///   folds it onto `RenderBlock::QuotedPost` from the resolved `QuotedPostView::verification`).
/// - `authoringOrigin` (D10, `atproto-pds-full.md` § Problem 1 -> D10 -> Audit) — drives the
///   embed's `delegated-origin-badge`, painted iff the *quoted* post is `.delegated`,
///   independently of the focal card's own origin.
/// - `legalTakedownRef` — set when the quoted post was taken down under a legal obligation
///   (moderation.md § Categories & enforcement item 1); the card paints the shared tombstone in
///   place of the withheld body.
public func documentQuotedPost(_ document: RenderDocument) -> QuotedPostEmbedOwned? {
    renderDocumentQuotedPost(document: document)
}

/// The document's folded `QuotedMessage` embed (`author_display`/`snippet`), or `nil` — the D2b
/// reply-quote projection the conversations manager prepends at read time
/// (`ConversationsManager::thread_detail` `fold_reply_quotes`) when this message replies AND its
/// parent is loaded in the thread (render-model.md § D2 QuotedMessage). The bubble
/// (`DmMessageBubble`) paints the `dm-message-quote` card above the body from it; hidden (nil) when
/// the parent isn't loaded. Mirrors `documentQuotedPost`; the body walker skips the block.
public func documentQuotedMessage(_ document: RenderDocument) -> (author: String, snippet: String)? {
    for block in document.blocks {
        if case let .quotedMessage(author, snippet) = block { return (author, snippet) }
    }
    return nil
}

/// The document's **Resolved** link previews, in body order — the D4 projection the producer emits
/// for each standalone bare-url paragraph (render-model.md § D4), once the manager has folded
/// `PreviewState::Resolved` onto the block via `resolve_link_preview`. **Unlike** the singular
/// `documentQuotedPost` / `documentMediaImageHash`, a post/message can carry SEVERAL bare-url
/// previews, so this returns a LIST. The feed card / conversation bubble paints one
/// `LinkPreviewCard` per entry (a `Resolving`/`Failed` block yields nothing here and paints no
/// card — the inline body link already shows) and fires `resolveLinkPreview(url)` fire-once for each
/// still-resolving one (see `resolvingLinkPreviewUrls`). The body walker skips the block (rendered
/// off the text flow, like `documentQuotedPost`).
///
/// Wraps the shared `renderDocumentResolvedLinkPreviews` UniFFI face
/// (`fauna_core::render::RenderDocument::resolved_link_previews`, render.rs), returning the
/// generated `ResolvedLinkPreviewOwned` records. This replaces a twin that returned **every**
/// preview with its raw `PreviewState`, leaving each of its call sites to re-derive the
/// `case .resolved` match — the same per-call-site re-derivation web deleted in its own adoption
/// (render-model.md § Implementation status). The og:image stays reveal-gated at the call site:
/// paint `imageHash` only when `revealed` (the D3 twin).
public func documentResolvedLinkPreviews(_ document: RenderDocument) -> [ResolvedLinkPreviewOwned] {
    renderDocumentResolvedLinkPreviews(document: document)
}

/// The urls of the document's still-`Resolving` link previews — the fire-once resolve set the feed
/// card / conversation bubble drives through `resolveLinkPreview` (the manager resolves once per url,
/// cached, and folds `Resolved`/`Failed` onto the block; the snapshot re-emits and the card paints).
/// Empty once every preview has resolved, so a `.task(id:)` keyed on it fires the resolves once and
/// settles. Mirrors the `documentMediaImageHash == nil` / `documentQuotedPost == nil` fire-once
/// guards.
///
/// Wraps the shared `renderDocumentResolvingLinkPreviewUrls` UniFFI face
/// (`fauna_core::render::RenderDocument::resolving_link_preview_urls`, render.rs) — fire-once by
/// construction there too, since a resolved block no longer yields its url.
public func resolvingLinkPreviewUrls(_ document: RenderDocument) -> [String] {
    renderDocumentResolvingLinkPreviewUrls(document: document)
}

/// Whether the document carries ≥1 **blocked** (not-yet-revealed) remote `![](url)` image —
/// gates the `load-remote-content-button` (shown only when something is still blocked). The reveal
/// flag lives on each block, projected by the manager (D3 — render-model.md § D3), so the gate
/// reads `revealed == false` per block rather than a client-side flag; tapping the button
/// dispatches `revealRemoteImages` to the manager, which re-emits with `revealed = true` and the
/// button disappears. Wraps the shared `renderDocumentHasBlockedRemoteImages` UniFFI face
/// (`fauna_core::render::RenderDocument::has_blocked_remote_images`, render.rs) — the single source
/// of truth for the gate walk (recursing into block quotes / list items / task lists, and the D4
/// og:image arm), so a new blocked-content class can't drift per-app (priority #2/#4; windows
/// already consumes the same face).
public func hasBlockedRemoteImages(_ document: RenderDocument) -> Bool {
    renderDocumentHasBlockedRemoteImages(document: document)
}
