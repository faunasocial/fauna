import SwiftUI

/// The D4 link-preview card (`link-preview-card`) — a clickable embedded preview of a standalone
/// bare URL in a post/message body (render-model.md § D4). Painted **below** the body whose
/// `RenderBlock::LinkPreview` block resolved, off the text flow (the walker no-ops the block; the
/// caller pulls Resolved blocks via `documentResolvedLinkPreviews` and paints one card each — the same
/// extract-then-paint shape as `documentQuotedPost`/`QuotedPostCard`). A `Resolving`/`Failed` block
/// paints **no** card — the inline body link stays in the paragraph and already shows the URL, so a
/// skeleton would be a worse perpetual-loading state (render-model.md § D4).
///
/// Shared by the macOS + iOS feed cards/detail **and** the conversation bubble (priority #1/#2 — one
/// leaf for both pages, both apps; mirrors the web `LinkPreviewCard.svelte`, linux
/// `build_link_preview_card`, android `FeedLinkPreviewCards`). Takes only the `PreviewState::Resolved`
/// projection the shared manager (`FeedManager`/`ConversationsManager::resolve_link_preview`) folds
/// onto the document — never re-fetches the preview per client.
///
/// **og:image reveal-gated** (render-model.md § D4, user-ratified 2026-06-27). Even though the
/// og:image is a content-addressed blob served by *this* client's own nest (no third-party fetch), it
/// obeys the post's D3 remote-content reveal: title/description/domain always show; the image paints
/// only once revealed. The caller passes `imageURL` **only** when the block's `revealed == true` (and
/// it carries an `image_hash`), resolving the hash through `vm.blobURL` — the same blob path as
/// `post-image`, so this leaf holds no `vm`/`api` reference. The post's existing
/// `load-remote-content-button` (now counting the un-revealed og:image via `hasBlockedRemoteImages`)
/// reveals it; there is no card-local toggle.
public struct LinkPreviewCard: View {
    /// The previewed url — opened when the card is tapped (`link-preview-card` is clickable).
    public let url: String
    public let title: String
    public let description: String
    /// The resolved og:image blob URL, or `nil` when the image is still blocked (un-revealed) or the
    /// preview carries no image. Non-nil ⇒ paint `link-preview-image`.
    public let imageURL: URL?
    /// This card's position among its host's cards — the `link-preview-card[n]` a scoped query
    /// (`post-card[i]/link-preview-card[n]`) names.
    public let index: Int

    @Environment(\.openURL) private var openURL

    public init(url: String, title: String, description: String, imageURL: URL?, index: Int = 0) {
        self.url = url
        self.title = title
        self.description = description
        self.imageURL = imageURL
        self.index = index
    }

    public var body: some View {
        Button(action: open) {
            VStack(alignment: .leading, spacing: 4) {
                // og:image — painted only when the caller passed a revealed `imageURL` (the reveal
                // gate lives at the call site, off `PreviewState.Resolved.revealed`). `automationValue`
                // registers the image for the in-process e2e `count` (a bare `.accessibilityIdentifier`
                // is invisible to the driver); it appears once revealed and is absent while blocked.
                if let imageURL {
                    AsyncImage(url: imageURL) { image in
                        image.resizable().scaledToFit()
                    } placeholder: {
                        ProgressView()
                    }
                    .frame(maxHeight: 160)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .clipShape(RoundedRectangle(cornerRadius: 6))
                    .accessibilityIdentifier(Ids.linkPreviewImage)
                    .automationValue(Ids.linkPreviewImage, value: { imageURL.absoluteString })
                }
                if !title.isEmpty {
                    automationText(Ids.linkPreviewTitle, title)
                        .font(.caption.weight(.semibold))
                        .lineLimit(2)
                        .multilineTextAlignment(.leading)
                }
                if !description.isEmpty {
                    automationText(Ids.linkPreviewDescription, description)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                        .multilineTextAlignment(.leading)
                }
                // The domain (host only — no scheme/path/port) from the shared `url_host`; never a
                // per-app URL parse (priority #2/#3 — identical to windows/linux/web).
                automationText(Ids.linkPreviewDomain, ValueFormat.urlHost(url))
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .buttonStyle(.plain)
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary, in: RoundedRectangle(cornerRadius: 10))
        .accessibilityIdentifier(Ids.linkPreviewCard)
        // The bare container id would clobber the child ids in the a11y tree; `.contain` keeps BOTH
        // the card id AND the children queryable (the documented per-card pattern — quoted-post /
        // device-card / snapshot-item).
        .accessibilityElement(children: .contain)
        // Register the card's open-url action for the in-process driver so `is_visible` (count > 0)
        // resolves; the closure invokes the SAME `open()` the Button does (registry convention).
        .automationActivate(Ids.linkPreviewCard, perform: open)
        // OUTERMOST, so the title/description/domain register under
        // `(link-preview-card, index)`: without it `link-preview-card[1]/link-preview-title`
        // had no scope to resolve and a two-card body read neither card's children.
        .automationScope(Ids.linkPreviewCard, index: index)
    }

    private func open() {
        if let parsed = URL(string: url) { openURL(parsed) }
    }
}
