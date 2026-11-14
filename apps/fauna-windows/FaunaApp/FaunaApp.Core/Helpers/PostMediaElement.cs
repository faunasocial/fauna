using uniffi.fauna_core;

namespace FaunaApp.Core.Helpers;

/// <summary>Which of the four media blocks a <see cref="PostMediaElement"/> paints — a
/// picture or a video, each by content hash (<c>Image</c> / <c>Video</c>) or by
/// nest-relative path (<c>ProxiedImage</c> / <c>ProxiedVideo</c>, render-model.md § D6c).</summary>
public enum PostMediaKind
{
    Image,
    ProxiedImage,
    Video,
    ProxiedVideo,
}

/// <summary>
/// One painted media element of a post: what one <c>post-image</c> or one
/// <c>video-thumbnail</c> slot paints, with the label it carries already settled
/// (render-model.md § D6c → <i>Proxied video</i>, the <c>alt</c> paragraph: the block's own
/// <c>alt</c> when the author wrote one, the shared fixed text otherwise). The list card and
/// the post detail paint one element per entry of <see cref="DocumentRenderer.MediaElements"/>,
/// in body order (§ D6b <i>Accessors</i>) — the windows twin of apple's
/// <c>DocumentMediaElement</c> and tui's <c>feed::media_elements</c>.
/// <para>Public, with string and <c>object</c> members only, because the card binds it with
/// x:Bind and the UniFFI-generated <c>RenderBlock</c> / <c>FfiPostBlobHome</c> are
/// <c>internal</c>. A record, so a card's <c>ContentEquals</c> sees a changed alt or an extra
/// picture as a changed row.</para>
/// </summary>
public sealed record PostMediaElement
{
    internal PostMediaElement(PostMediaKind kind, string key, string alt, RenderBlock? block)
    {
        Kind = kind;
        Key = key;
        Alt = alt;
        _block = block;
    }

    private readonly RenderBlock? _block;

    public PostMediaKind Kind { get; }

    /// <summary>The content hash (<c>Image</c> / <c>Video</c>) or the nest-relative path
    /// (<c>ProxiedImage</c> / <c>ProxiedVideo</c>).</summary>
    public string Key { get; }

    /// <summary>The element's label — published as its <c>alt</c> attribute.</summary>
    public string Alt { get; }

    public bool IsVideo => Kind is PostMediaKind.Video or PostMediaKind.ProxiedVideo;

    /// <summary>What the picture loader fetches by hash — <c>""</c> unless a blob picture.</summary>
    public string Hash => Kind == PostMediaKind.Image ? Key : "";

    /// <summary>What the picture loader fetches by path — <c>""</c> unless a bridged picture.</summary>
    public string ProxiedPath => Kind == PostMediaKind.ProxiedImage ? Key : "";

    /// <summary>The block a video element's <c>video-thumbnail</c> plays (a tap hands it to
    /// the shared <c>playback_source</c>, render-model.md § D6c → <i>Inline playback</i>), or
    /// <c>null</c> for a picture. Typed <c>object</c> because <c>RenderBlock</c> is
    /// <c>internal</c>, which the XAML compiler cannot bind.</summary>
    public object? Block => IsVideo ? _block : null;

    /// <summary>The post this element belongs to — a list card's video slot keeps its
    /// player under it (<see cref="InlinePlayerKeep{TPlayer}"/>). <c>null</c> outside a feed item.</summary>
    public string? PostId { get; init; }

    /// <summary>The post's shared <c>post_blob_home</c> (<c>FfiPostBlobHome</c>, typed
    /// <c>object</c> for the reason <see cref="Block"/> is), handed unchanged with every hash;
    /// <c>null</c> = this nest.</summary>
    public object? Home { get; init; }
}
