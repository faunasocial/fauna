using Xunit;
using FaunaApp.Core.Helpers;

namespace FaunaApp.Tests;

// Photo-backup ingress metadata-strip convergence
// (docs/goal/behavior/file-sync.md § Implementation status today): windows must
// use the shared lossless fauna_media::process::strip_metadata face
// (FaunaFfiMethods.StripMediaMetadata), not a lossy BitmapEncoder re-encode —
// re-encoding permanently degrades the only copy a backup restore returns.
public class ExifStripperTests
{
    [Fact]
    public void Strip_MetadataFreeImageBytes_RoundTripsByteIdentical()
    {
        // A well-formed PNG with no eXIf/tEXt/iTXt/zTXt chunks: strip_metadata only
        // removes known metadata chunks, so this must come back byte-identical. A
        // re-encode-based stripper (the old BitmapEncoder path) would NOT be.
        var png = MinimalPng();

        var stripped = ExifStripper.Strip(png);

        Assert.Equal(png, stripped);
    }

    [Fact]
    public void Strip_NonImageBytes_PassesThroughUnchanged()
    {
        var raw = System.Text.Encoding.UTF8.GetBytes("not an image, just backup bytes");

        var stripped = ExifStripper.Strip(raw);

        Assert.Equal(raw, stripped);
    }

    // 1x1 transparent PNG, no ancillary metadata chunks.
    private static byte[] MinimalPng() => System.Convert.FromBase64String(
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=");
}
