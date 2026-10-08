using FaunaApp.Core.Media;
using Xunit;

namespace FaunaApp.Tests;

// ── The post-image paint-read contract ──
//
// get_attr(post-image, "state") must answer painted / placeholder off the
// element's OWN current picture, never a marker written beside the call that
// assigns it — ImageHashBind (WinUI, untestable from here) reads
// Image.Source back AFTER assigning it and asks this pure rule what that
// means. Pinned here, not in ImageHashBind, because FaunaApp.Tests
// references FaunaApp.Core alone (the UploadPathGuard precedent).

public class ImagePaintStateTests
{
    [Fact]
    public void A_picture_present_reads_painted()
    {
        Assert.Equal("painted", ImagePaintState.From(hasPicture: true));
        Assert.Equal(ImagePaintState.Painted, ImagePaintState.From(hasPicture: true));
    }

    [Fact]
    public void No_picture_reads_placeholder()
    {
        Assert.Equal("placeholder", ImagePaintState.From(hasPicture: false));
        Assert.Equal(ImagePaintState.Placeholder, ImagePaintState.From(hasPicture: false));
    }

    // ── The bridged-picture placeholder's readable text (render-model.md § D6c) ──
    //
    // get_text(post-image) answers the nest-relative path while a bridged post's
    // picture has no bytes on screen — the address tui paints as its label and
    // apple's PostImageSource.placeholderText answers — and nothing once it has.

    private const string ProxiedPicture =
        "/api/v1/bluesky/media?url=https%3A%2F%2Fcdn.bsky.app%2Fimg%2Ffeed_fullsize%2Fplain%2Fa%40jpeg";

    [Fact]
    public void A_proxied_placeholder_reads_its_path()
    {
        Assert.Equal(ProxiedPicture, ImagePaintState.PlaceholderText(ProxiedPicture, hasPicture: false));
    }

    [Fact]
    public void A_painted_proxied_picture_reads_no_placeholder_text()
    {
        Assert.Equal("", ImagePaintState.PlaceholderText(ProxiedPicture, hasPicture: true));
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    public void A_picture_that_is_not_proxied_never_reads_placeholder_text(string? proxiedPath)
    {
        Assert.Equal("", ImagePaintState.PlaceholderText(proxiedPath, hasPicture: false));
        Assert.Equal("", ImagePaintState.PlaceholderText(proxiedPath, hasPicture: true));
    }
}
