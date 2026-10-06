using System;
using Microsoft.UI;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using uniffi.fauna_core;
using uniffi.fauna_ffi;

namespace FaunaApp.Helpers;

/// <summary>
/// Paints a shared <c>fauna_core::qr_matrix</c> grid onto a <see cref="Canvas"/> —
/// never a platform or NuGet QR library (priorities #1/#2: the matrix itself is
/// computed once in shared Rust and every app paints the same grid).
///
/// <para>Extracted from <c>SettingsAccountPage</c>'s identity-export QR when the
/// Nostr page's Nostr Connect reveal became its second consumer, exactly as linux
/// pulled <c>identity_export.rs</c>'s painter into <c>crate::qr_widget</c> for the
/// same pair of surfaces and android made <c>IdentityExportSection.kt</c>'s
/// <c>QrCanvas</c> <c>internal</c> to reuse it (nostr.md § Layout &amp; flow item 6).
/// Lift, don't copy.</para>
/// </summary>
internal static class QrPainter
{
    /// <summary>Encode <paramref name="text"/> and paint it into
    /// <paramref name="canvas"/>. Throws if the text cannot be encoded (too long for
    /// any QR version) — callers surface that on their error element rather than
    /// leaving a silently blank canvas.</summary>
    internal static void Paint(Canvas canvas, string text) =>
        Paint(canvas, FaunaFfiMethods.QrMatrix(text));

    /// <summary>Paint <paramref name="m"/> into <paramref name="canvas"/>,
    /// dark-on-light with the mandatory quiet zone (<c>qr_quiet_zone_modules</c> —
    /// never a hard-coded 4). Deliberately NOT theme-aware: a theme-inverted QR does
    /// not scan, so the light background (the canvas's own White <c>Background</c>,
    /// set once in XAML) is never swapped for a theme brush.</summary>
    internal static void Paint(Canvas canvas, QrMatrix m)
    {
        canvas.Children.Clear();
        var quiet = FaunaFfiMethods.QrQuietZoneModules();
        var modulesPerSide = m.size + 2 * quiet;
        var scale = Math.Min(canvas.Width, canvas.Height) / modulesPerSide;

        for (uint y = 0; y < m.size; y++)
        {
            for (uint x = 0; x < m.size; x++)
            {
                if (!m.modules[(int)(y * m.size + x)]) continue;

                var rect = new Rectangle
                {
                    Width = scale,
                    Height = scale,
                    Fill = new SolidColorBrush(Colors.Black),
                };
                Canvas.SetLeft(rect, (x + quiet) * scale);
                Canvas.SetTop(rect, (y + quiet) * scale);
                canvas.Children.Add(rect);
            }
        }
    }

    /// <summary>Drop a painted matrix — used when hiding a QR so its encoding is not
    /// retained in the visual tree longer than it is shown.</summary>
    internal static void Clear(Canvas canvas) => canvas.Children.Clear();
}
