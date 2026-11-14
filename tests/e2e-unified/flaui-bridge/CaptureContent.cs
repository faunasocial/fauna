namespace FauiBridge;

/// <summary>
/// Whether a window capture shows anything the app painted — the guard
/// <see cref="Actions.CaptureWindow"/> applies before it keeps a store-listing shot
/// (installers/windows.md § Store distribution, the screenshots paragraph).
///
/// <para>The question is asked of the window's CLIENT area, never the whole frame. A
/// session with no attached desktop (<c>query session</c> shows it <c>Disc</c>) renders no
/// app composition, yet <c>PrintWindow</c> still draws the non-client title bar — its
/// caption, icon and caption buttons — so the frame as a whole is never one colour while the
/// client area underneath is flat white. Measured on <c>win</c> 2026-11-12: both listing
/// shots came back as a bare title bar over a white client area, and a whole-frame check let
/// them through, so the run wrote them over the committed pair.</para>
///
/// <para>Pure, so <c>SelfTest.CaptureContentChecks</c> (<c>--self-test-capture-content</c>)
/// grades it with no window and no app.</para>
/// </summary>
static class CaptureContent
{
    /// <summary>True when every pixel of <paramref name="region"/>, clipped to the frame, is
    /// one colour — an empty region (after the clip) included, since it shows nothing.
    /// <paramref name="pixels"/> is tightly packed 24-bit rows, <c>width * 3</c> bytes each
    /// (<c>Actions.PixelBytes</c>).</summary>
    public static bool RegionIsUniform(byte[] pixels, int width, int height, System.Drawing.Rectangle region)
    {
        region.Intersect(new System.Drawing.Rectangle(0, 0, width, height));
        if (region.Width <= 0 || region.Height <= 0 || pixels.Length < width * height * 3) return true;
        var row = width * 3;
        var first = region.Top * row + region.Left * 3;
        for (var y = region.Top; y < region.Bottom; y++)
            for (var i = y * row + region.Left * 3; i < y * row + region.Right * 3; i += 3)
                if (pixels[i] != pixels[first] || pixels[i + 1] != pixels[first + 1] || pixels[i + 2] != pixels[first + 2])
                    return false;
        return true;
    }
}
