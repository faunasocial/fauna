import SwiftUI

/// Compact embedded display of a quoted or reposted post (`quoted-post`).
/// Takes the author + body projected by the shared `resolve_quoted_post`
/// (`fauna_feed`), rendered identically in the list card and `post_detail`.
///
/// Named `QuotedPostCard` (not `QuotedPostView`) to avoid colliding with the
/// UniFFI-generated `QuotedPostView` *data* type in the flat FaunaFFISwift module
/// (the snapshot projection this card renders) — the same flat-module collision
/// class that Feed-prefixed `FeedComposeState` / `FeedSnapshotObserver`.
public struct QuotedPostCard: View {
    public let author: String
    public let postBody: String
    public let verification: VerificationStatus
    /// The *quoted* post's own authoring origin, folded onto
    /// `RenderBlock::QuotedPost::authoring_origin` — read independently of the
    /// focal card's origin, exactly as `verification` above is (D10 § Audit).
    public let authoringOrigin: AuthoringOriginStatus
    /// Set when the quoted post was taken down under a legal obligation
    /// (`moderation.md` § Categories & enforcement item 1). The body is withheld
    /// (empty) in that case, so the card paints the shared tombstone instead —
    /// never a blank/broken embed.
    public let legalTakedownRef: String?
    /// Set when the quoted post is gone — its author deleted it (`ui/feed.md` §
    /// Post deletion: references to a deleted post dangle by design, and the embed
    /// renders the not-found state). The shared fold stamps it on
    /// `RenderBlock::QuotedPost::not_found`; the card paints "Post not found" in
    /// place of the author row and body.
    public let notFound: Bool

    public init(author: String, postBody: String, verification: VerificationStatus,
                authoringOrigin: AuthoringOriginStatus = .unknown,
                legalTakedownRef: String? = nil, notFound: Bool = false) {
        self.author = author
        self.postBody = postBody
        self.verification = verification
        self.authoringOrigin = authoringOrigin
        self.legalTakedownRef = legalTakedownRef
        self.notFound = notFound
    }

    /// The one line a quote with nothing to show paints in place of author + body:
    /// the legal-takedown tombstone, or the not-found state of a deleted post.
    /// `nil` for an ordinary quote. Mirrors linux `render_block_into`'s placeholder.
    private var placeholder: String? {
        if let reference = legalTakedownRef {
            return renderLocalizedText(legalTakedownTombstone(reference: reference))
        }
        return notFound ? L.feed.post.postNotFound : nil
    }

    /// What the embed paints, as the in-process driver reads it back
    /// (`quoted-post`'s text) — the placeholder line, or the author and body.
    private var paintedText: String {
        placeholder ?? "\(author)\n\(postBody)"
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let placeholder {
                // A taken-down or deleted quote carries no envelope, so there is no
                // author to show and no verification to assert — the one line replaces
                // the whole card body. Mirrors web `QuotedPost.svelte` / linux
                // `build_quoted_post_card` / android `QuotedPostEmbed`.
                Text(placeholder)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                HStack(spacing: 4) {
                    Text(author)
                        .font(.caption.weight(.semibold))
                    // Slice 2b: the quoted-embed unverified-source badge — `UnverifiedSourceBadge`
                    // self-gates to render iff the *quoted* post's `verification == .failed`
                    // (security.md § App display of unverified content). Mirrors linux
                    // `build_quoted_post_card` (author row, badge trailing) / web `QuotedPost.svelte`
                    // / android `QuotedPostEmbed`.
                    UnverifiedSourceBadge(verification: verification)
                    // D10 § Audit: the embed badge keys off the *quoted* post's own
                    // origin, independently of the focal card's — a self-authored post
                    // quoting an externally-authored one badges here and nowhere else.
                    // Mirrors tui's `.within("quoted-post", 0)` element (lead app).
                    DelegatedOriginBadge(authoringOrigin: authoringOrigin)
                }
                Text(postBody)
                    .font(.caption)
                    .lineLimit(3)
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.quaternary, in: RoundedRectangle(cornerRadius: 10))
        .accessibilityIdentifier(Ids.quotedPost)
        // Register the container itself for the in-process driver so
        // `count`/`is_visible("quoted-post")` resolve (the countable-container
        // pattern — ProviderRow / device-card / admin-stat-card): a bare
        // `.accessibilityIdentifier` populates only the a11y tree, NOT the
        // AutomationRegistry the in-process driver reads.
        .automationValue(Ids.quotedPost, text: { paintedText })
        // `.contain` keeps the child badge ids (`unverified-source-badge`,
        // `delegated-origin-badge`) queryable in the a11y tree (the bare container
        // id would otherwise clobber them — the documented per-card pattern).
        .accessibilityElement(children: .contain)
        // Mark this embed as a scoped container so the child badges capture
        // `post-card[i]/quoted-post` as their ancestor path. That's what lets
        // the in-process driver tell post-card[0]'s quoted badge (a Failed quote)
        // from post-card[1]'s (a Verified quote, no badge) — the negative scoped
        // assert `not is_visible("unverified-source-badge", scope="post-card[1]/quoted-post")`
        // in test_feed_unverified_source.py, and its D10 twin on
        // `delegated-origin-badge` in test_feed_delegated_origin.py.
        // Index 0: exactly one quoted-post per card.
        .automationScope(Ids.quotedPost)
    }
}
