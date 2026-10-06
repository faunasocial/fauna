using FaunaApp.Core.Helpers;
using Xunit;

namespace FaunaApp.Tests;

public class MimeDetectTests
{
    [Theory]
    [InlineData("photo.png", "image/png")]
    [InlineData("PHOTO.PNG", "image/png")]
    [InlineData("c:\\tmp\\photo.jpg", "image/jpeg")]
    [InlineData("photo.jpeg", "image/jpeg")]
    [InlineData("anim.gif", "image/gif")]
    [InlineData("pic.webp", "image/webp")]
    public void FromExtension_KnownImageTypes(string path, string expected)
    {
        Assert.Equal(expected, MimeDetect.FromExtension(path));
    }

    // After the lift onto the shared `fauna_core::share::content_type_for_filename`
    // map (via the fauna-ffi `mime` export), MimeDetect resolves the full shared
    // catalog (text / application / audio / video / font), not just the feed
    // compose bar's image set. These are the canonical results every other app
    // gets from the one shared source of truth (priority #2/#4).
    [Theory]
    [InlineData("doc.txt", "text/plain")]
    [InlineData("report.pdf", "application/pdf")]
    [InlineData("notes.md", "text/markdown")]
    [InlineData("sheet.xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet")]
    public void FromExtension_SharedCatalogTypes(string path, string expected)
    {
        Assert.Equal(expected, MimeDetect.FromExtension(path));
    }

    [Theory]
    [InlineData("noext")]
    [InlineData("")]
    public void FromExtension_UnknownReturnsOctetStream(string path)
    {
        Assert.Equal("application/octet-stream", MimeDetect.FromExtension(path));
    }
}
