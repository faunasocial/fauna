using Microsoft.UI.Xaml.Data;

namespace FaunaApp.Helpers;

/// <summary>
/// Windows's platform-specific post-source color map for the card's
/// <c>protocol-badge</c>, keyed off the stable SourceKind id from the shared fauna-feed
/// classifier (<c>PostSourceBadge.Id</c>) — the label text comes from the shared
/// classifier, this only picks a fill (docs/goal/ui/feed.md § Where logic lives).
/// Used by the shared post-card template (<c>Controls/PostCardTemplates.xaml</c>).
/// </summary>
public class SourceToColorConverter : IValueConverter
{
    public object Convert(object value, System.Type targetType, object parameter, string language)
    {
        return (value as string) switch
        {
            "fauna" => Brush(45, 184, 75),         // 🌿 leaf green (matches web's fauna badge)
            "bluesky" => Brush(0, 133, 255),       // Bluesky blue
            "nostr" => Brush(139, 92, 246),        // Nostr purple
            "activitypub" => Brush(99, 100, 255),  // Fediverse indigo (#6364FF)
            "email" => Brush(245, 158, 11),        // amber
            _ => Brush(0, 0, 0, 0),                // unknown ("other"): transparent, label only
        };
    }

    private static Microsoft.UI.Xaml.Media.SolidColorBrush Brush(byte r, byte g, byte b, byte a = 255)
        => new(Windows.UI.Color.FromArgb(a, r, g, b));

    public object ConvertBack(object value, System.Type targetType, object parameter, string language)
        => throw new System.NotSupportedException();
}
