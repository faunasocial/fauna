using System.Linq;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Media;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_core;
using Xunit;

namespace FaunaApp.Tests;

// ── Every media block of a post, painted in body order with its alt (render-model.md § D6b
// *Accessors*; § D6c → *Proxied video*, the `alt` paragraph) ──
// The list card and the post detail paint one `post-image` per picture element and one
// `video-thumbnail` per video element of DocumentRenderer.MediaElements, each publishing its
// settled alt — the windows twin of tui's `feed::media_elements` and apple's
// `documentMediaElements`. The witness is tests/e2e-unified/tests/test_feed_media_items.py.
public class PostMediaElementTests
{
    private const string ProxiedPicture =
        "/api/v1/bluesky/media?url=https%3A%2F%2Fcdn.bsky.app%2Fimg%2Ffeed_fullsize%2Fplain%2Fa%40jpeg";
    private const string ProxiedClip =
        "/api/v1/media/proxy?url=https%3A%2F%2Fr.example%2Fclip.mp4";

    private static RenderDocument Doc(params RenderBlock[] media) => new(
        new RenderBlock[] { new RenderBlock.Paragraph(new Inline[] { new Inline.Text("media") }) }
            .Concat(media).ToArray());

    private static string ImageFallback => Strings.Get("feed/post/image_alt_fallback");
    private static string VideoFallback => Strings.Get("feed/post/video_alt_fallback");

    [Fact]
    public void MediaElements_PaintsEveryBlockInBodyOrder()
    {
        var elements = DocumentRenderer.MediaElements(Doc(
            new RenderBlock.Image("aa01", "A red square at dusk"),
            new RenderBlock.Video("vv01", "One second of a grey frame"),
            new RenderBlock.Image("aa02", "The same square, at noon"),
            new RenderBlock.ProxiedImage(ProxiedPicture, ""),
            new RenderBlock.ProxiedVideo(ProxiedClip, "")));

        Assert.Equal(
            new[]
            {
                (PostMediaKind.Image, "aa01"),
                (PostMediaKind.Video, "vv01"),
                (PostMediaKind.Image, "aa02"),
                (PostMediaKind.ProxiedImage, ProxiedPicture),
                (PostMediaKind.ProxiedVideo, ProxiedClip),
            },
            elements.Select(e => (e.Kind, e.Key)).ToArray());
    }

    [Fact]
    public void MediaElements_CarryTheAuthorsAlt_OrTheSharedFallbackWhenNone()
    {
        var elements = DocumentRenderer.MediaElements(Doc(
            new RenderBlock.Image("aa01", "A red square at dusk"),
            new RenderBlock.Image("aa02", ""),
            new RenderBlock.Video("vv01", ""),
            new RenderBlock.ProxiedImage(ProxiedPicture, ""),
            new RenderBlock.ProxiedVideo(ProxiedClip, "A bridged clip")));

        Assert.Equal(
            new[] { "A red square at dusk", ImageFallback, VideoFallback, ImageFallback, "A bridged clip" },
            elements.Select(e => e.Alt).ToArray());
        Assert.All(elements, e => Assert.False(string.IsNullOrEmpty(e.Alt)));
    }

    [Fact]
    public void MediaElements_EmptyForAPostWithNoMedia()
    {
        Assert.Empty(DocumentRenderer.MediaElements(Doc()));
    }

    [Fact]
    public void PictureElement_FeedsTheImageLoaderEitherAHashOrAPath_NeverBoth()
    {
        var elements = DocumentRenderer.MediaElements(Doc(
            new RenderBlock.Image("aa01", ""),
            new RenderBlock.ProxiedImage(ProxiedPicture, "")));

        Assert.Equal(("aa01", ""), (elements[0].Hash, elements[0].ProxiedPath));
        Assert.Equal(("", ProxiedPicture), (elements[1].Hash, elements[1].ProxiedPath));
        Assert.All(elements, e => Assert.False(e.IsVideo));
        Assert.All(elements, e => Assert.Null(e.Block));
    }

    [Fact]
    public void VideoElement_IsTheBlockItsThumbnailPlays_KeyedByHashOrPath()
    {
        var elements = DocumentRenderer.MediaElements(Doc(
            new RenderBlock.Video("vv01", "a clip"),
            new RenderBlock.ProxiedVideo(ProxiedClip, "")));

        Assert.All(elements, e => Assert.True(e.IsVideo));
        Assert.Equal("vv01", InlineVideoKey.Of(elements[0].Block));
        Assert.Equal(ProxiedClip, InlineVideoKey.Of(elements[1].Block));
        // A video is never handed to the picture loader.
        Assert.All(elements, e => Assert.Equal(("", ""), (e.Hash, e.ProxiedPath)));
    }

    // ── The `alt` attribute rides post-image's HelpText beside its paint `state` ──

    [Fact]
    public void ImageHelpText_PacksStateAndAlt_SoEachKeyReadsOnItsOwn()
    {
        Assert.Equal("state=painted;alt=A red square", ImagePaintState.HelpText(true, "A red square"));
        Assert.Equal("state=placeholder;alt=Picture", ImagePaintState.HelpText(false, "Picture"));
    }

    [Fact]
    public void ImageHelpText_EscapesASemicolonInTheAlt()
    {
        Assert.Equal("state=painted;alt=dusk%3B noon", ImagePaintState.HelpText(true, "dusk; noon"));
    }

    [Fact]
    public void ImageHelpText_WithNoAlt_IsTheBareState()
    {
        // An image no media element names (link-preview-image, media-thumbnail) keeps the
        // bare value it always published.
        Assert.Equal(ImagePaintState.Painted, ImagePaintState.HelpText(true, null));
        Assert.Equal(ImagePaintState.Placeholder, ImagePaintState.HelpText(false, ""));
    }

    // ── The feed item hands its card one element per block, carrying the post's key ──

    [Fact]
    public void FeedPostItem_MediaElements_CarryThePostAndItsHome()
    {
        var item = new FeedPostItem(PostSummaryFixture.Make(document: Doc(
            new RenderBlock.Image("aa01", "first"),
            new RenderBlock.Image("aa02", "second"),
            new RenderBlock.Video("vv01", "clip"))));

        Assert.Equal(new[] { "first", "second", "clip" }, item.MediaElements.Select(e => e.Alt).ToArray());
        Assert.All(item.MediaElements, e => Assert.Equal(item.PostId, e.PostId));
        Assert.All(item.MediaElements, e => Assert.Equal(item.BlobHome, e.Home));
    }

    [Fact]
    public void FeedPostItem_ContentEquals_SeesAChangedAltOrASecondPicture()
    {
        FeedPostItem With(params RenderBlock[] media) => new(PostSummaryFixture.Make(document: Doc(media)));

        var one = With(new RenderBlock.Image("aa01", "first"));
        Assert.True(one.ContentEquals(With(new RenderBlock.Image("aa01", "first"))));
        Assert.False(one.ContentEquals(With(new RenderBlock.Image("aa01", "renamed"))),
            "a changed alt must rebuild the row, or the label stays stale");
        Assert.False(one.ContentEquals(With(new RenderBlock.Image("aa01", "first"), new RenderBlock.Image("aa02", ""))),
            "a second picture must rebuild the row, or it never paints");
    }
}
