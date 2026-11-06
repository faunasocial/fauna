using Microsoft.UI.Xaml;
using FaunaApp.Helpers;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Feed;

/// <summary>
/// The static functions the shared <c>post-card</c> template binds through
/// (<c>Controls/PostCardTemplates.xaml</c>) and the feed page's imperative post
/// detail reuses. One home for both pages that paint the card — the feed and the
/// profile's Posts tab (`ui/profile.md` § Layout &amp; flow → <i>The Posts tab</i>:
/// "the post-card component with every child it has on the feed … in identical
/// paint"). Each is a single call, so it stays <c>x:Bind</c>-legal (no nested call).
/// </summary>
public static class PostCardBind
{
    // Singleton loader behind every card's post-image, link-preview image and the
    // detail's picture. Static so the template's `feed:PostCardBind.GetImageLoader()`
    // has a stable target whichever page hosts the row, and built on first use so a
    // profile opened before the feed page still loads its cards' pictures. It resolves
    // the CURRENT nest client and feed manager per call, never captured: the client is
    // disposed on the next re-login, and the manager is built later, rebuilt on
    // re-auth, and holds the per-post keys that open a tier-restricted post's
    // attachments (`apps/windows.md` § Client lifetime).
    private static BlobImageLoader? _imageLoader;

    /// <summary>The session's image loader, built on first use.</summary>
    public static BlobImageLoader GetImageLoader()
        => _imageLoader ??= new BlobImageLoader(
            () => App.CurrentNest, () => Core.FeedManagerHost.Current);

    public static Visibility BoolToVisibility(bool value)
        => value ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>Hide an interaction count when 0 (feed.md § Interaction bar — count
    /// hidden at 0, a clean icon-only button until the post has activity), uniform
    /// across all seven apps.</summary>
    public static Visibility CountToVisibility(long count)
        => count > 0 ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>Lit-state visual for the like toggle — the AccentButtonStyle swap idiom
    /// the feed rail's Trending entry uses for its selected state: a lit
    /// feed-like-button means the next tap un-likes (feed.md § Interaction bar → Repost).</summary>
    public static Style? LikedToButtonStyle(bool liked)
        => liked ? (Style)Application.Current.Resources["AccentButtonStyle"] : null;

    /// <summary>Lit-state visual for the repost toggle — the repost twin of
    /// <see cref="LikedToButtonStyle"/>: a lit <c>feed-repost-button</c> means the next tap
    /// un-reposts (feed.md § Interaction bar → Repost, ratified 2026-08-10), matching
    /// linux's <c>.reposted</c> CSS class and web's <c>viewer_reposted</c> lit state.</summary>
    public static Style? RepostedToButtonStyle(string? viewerRepostId)
        => viewerRepostId is { Length: > 0 } ? (Style)Application.Current.Resources["AccentButtonStyle"] : null;

    /// <summary>Suppress an element on a REPOST ROW — its own interaction bar renders
    /// nothing (a repost's own counters are structurally dark; feed.md § Interaction bar →
    /// Repost, ratified 2026-08-10), mirroring tui/linux/web's <c>if !is_repost_row</c>.</summary>
    public static Visibility HideOnRepostRow(bool isRepostRow)
        => isRepostRow ? Visibility.Collapsed : Visibility.Visible;

    // ── Post-card withholding arms: muted collapse (topic-factors.md § Scoring —
    //    a mute collapses everywhere; mirrors the dm-message-muted precedent) and
    //    content policy (family-safety.md § Content policy). WHICH arm wins is
    //    decided once in FaunaApp.Core (FeedPostItem.RenderArm → SocialRenderGate,
    //    shared with the conversation bubble and unit-tested there) — the card
    //    only paints the already-resolved gates. ──

    /// <summary>The post-card's accessible Name: the placeholder of whichever arm
    /// is withholding the body — NEVER the body it hides (a blocked or collapsed
    /// body must not leak into the accessibility tree) — else the body itself.
    /// Takes the resolved gates, so the block-beats-muted ordering is not
    /// re-derived here (it lives in <c>SocialRenderGate</c>).</summary>
    public static string PostCardAccessibleName(
        string regionNotice, string nestNotice, bool showContentBlockedNotice,
        string contentBlockedNotice, bool showMutedCollapse, bool showContentCollapse,
        string bodyText)
    {
        // The region arm paints ahead of every other (region-blocking.md § The
        // blocked render), so its frame is the Name whenever it is withholding.
        if (regionNotice.Length > 0) return regionNotice;
        // ...then the nest's (nest-rules.md § The nest source in the engine).
        if (nestNotice.Length > 0) return nestNotice;
        // The notice's own words: the family-policy sentence, or "You reported this"
        // when the viewer's report hid the post (FeedPostItem.ContentBlockedNoticeText).
        if (showContentBlockedNotice) return contentBlockedNotice;
        if (showMutedCollapse) return S.Get("muted_words/collapsed");
        if (showContentCollapse) return S.Get("family/content_collapsed_notice");
        return bodyText;
    }
}
