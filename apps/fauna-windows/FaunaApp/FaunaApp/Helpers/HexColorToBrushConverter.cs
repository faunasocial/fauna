using System;
using System.Globalization;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Media;

namespace FaunaApp.Helpers;

/// <summary>
/// Maps a <c>#RRGGBB</c> hex string (e.g. the shared <c>content_label_style</c>
/// tint/accent colour) to a <see cref="SolidColorBrush"/> — so a badge colour comes
/// from a shared Rust map, not a hard-coded per-app swatch. Used both as an
/// x:Bind/Binding <see cref="IValueConverter"/> (moderation queue, feed post-card) and
/// via <see cref="FromHex"/> directly from imperative code-behind (the DM bubble,
/// which renders without x:Bind). An optional integer <c>ConverterParameter</c>
/// (0-255) sets the alpha byte, so a <c>tint</c> background can apply windows' own
/// low-alpha treatment (default opaque) — the SAME per-app low-alpha convention
/// web (<c>rgba(.., 0.15)</c>) and android (<c>copy(alpha=.15)</c>) already apply
/// (<c>fauna_core::content_category</c> doc comment on <c>tint()</c>).
/// </summary>
public class HexColorToBrushConverter : IValueConverter
{
    public static SolidColorBrush FromHex(string hex, byte alpha = 0xFF)
    {
        if (hex.Length == 7 && hex[0] == '#'
            && byte.TryParse(hex.AsSpan(1, 2), NumberStyles.HexNumber, CultureInfo.InvariantCulture, out var r)
            && byte.TryParse(hex.AsSpan(3, 2), NumberStyles.HexNumber, CultureInfo.InvariantCulture, out var g)
            && byte.TryParse(hex.AsSpan(5, 2), NumberStyles.HexNumber, CultureInfo.InvariantCulture, out var b))
        {
            return new SolidColorBrush(global::Windows.UI.Color.FromArgb(alpha, r, g, b));
        }
        return new SolidColorBrush(global::Microsoft.UI.Colors.Gray);
    }

    public object Convert(object value, Type targetType, object parameter, string language)
    {
        var alpha = parameter is string alphaStr && byte.TryParse(alphaStr, out var a) ? a : (byte)0xFF;
        return value is string hex ? FromHex(hex, alpha) : new SolidColorBrush(global::Microsoft.UI.Colors.Gray);
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
        => throw new NotSupportedException();
}
