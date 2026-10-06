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
}
