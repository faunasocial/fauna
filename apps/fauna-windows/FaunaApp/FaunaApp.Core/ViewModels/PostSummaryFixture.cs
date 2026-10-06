using uniffi.fauna_core;
using uniffi.fauna_feed;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// Test-only convenience: mint a catalog-aligned <see cref="PostSummary"/> with named
/// overrides for just the fields a test cares about, instead of every call site
/// hand-listing the full positional record — the C# analogue of a Rust
/// <c>..Default::default()</c> fixture, so two branches growing the same record merge
/// cleanly instead of colliding on the grown field. Widening <see cref="PostSummary"/>
/// now touches exactly this one file;
/// this is the fix for the fixture breakage that recurred across
/// <c>FeedViewModel.ForSourceTest</c>, <c>FeedPostItemTests</c>, and
/// <c>ContentPolicyRenderTests</c> each time the record grew a field.
/// </summary>
internal static class PostSummaryFixture
{
    internal static PostSummary Make(
        string postId = "p",
        string author = "a",
        AuthorDisplayView? authorDisplay = null,
        string body = "",
        RenderDocument? document = null,
        long timestamp = 0,
        string[]? tags = null,
        bool hasMedia = false,
        bool isReply = false,
        string source = "fauna",
        string? quotedPostId = null,
        string? repostedPostId = null,
        string? viewerRepostId = null,
        bool viewerLiked = false,
        string? mediaHash = null,
        VerificationStatus verification = VerificationStatus.Unchecked,
        AuthoringOriginStatus authoringOrigin = AuthoringOriginStatus.Unknown,
        long likeCount = 0,
        long replyCount = 0,
        long repostCount = 0,
        long quoteCount = 0,
        string? gatedTier = null,
        string? gatedRoom = null,
        string? roomLabel = null,
        string? webSlug = null,
        bool gatedUnlocked = false,
        ContentLabelEntry[]? labels = null,
        UnlockOfferView? unlockOffer = null,
        string? legalTakedownRef = null,
        TipView? tips = null) =>
        new PostSummary(
            postId: postId,
            author: author,
            authorDisplay: authorDisplay,
            body: body,
            document: document ?? EmptyDocument(),
            timestamp: timestamp,
            tags: tags ?? System.Array.Empty<string>(),
            hasMedia: hasMedia,
            isReply: isReply,
            source: source,
            quotedPostId: quotedPostId,
            repostedPostId: repostedPostId,
            viewerRepostId: viewerRepostId,
            viewerLiked: viewerLiked,
            mediaHash: mediaHash,
            verification: verification,
            authoringOrigin: authoringOrigin,
            likeCount: likeCount,
            replyCount: replyCount,
            repostCount: repostCount,
            quoteCount: quoteCount,
            gatedTier: gatedTier,
            gatedRoom: gatedRoom,
            roomLabel: roomLabel,
            webSlug: webSlug,
            gatedUnlocked: gatedUnlocked,
            labels: labels ?? System.Array.Empty<ContentLabelEntry>(),
            unlockOffer: unlockOffer,
            legalTakedownRef: legalTakedownRef,
            tips: tips);

    private static RenderDocument EmptyDocument() =>
        new RenderDocument(System.Array.Empty<RenderBlock>());
}
