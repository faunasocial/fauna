using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Helpers;

/// <summary>
/// Attached property that paints a feed post's body <see cref="FeedPostItem.Document"/>
/// (the shared <c>RenderDocument</c> — render-model.md § D6) into a DataTemplate
/// <see cref="TextBlock"/>'s inline runs via <see cref="DocumentPainter"/> — the SAME
/// painter the conversations <c>DmMessageBubble</c> uses, so the feed body renders
/// structurally (bold/italic/headings/links/blockquote) instead of as the raw markdown
/// string (priority #1/#4). A ListView DataTemplate can't imperatively reach a row's
/// TextBlock and a <c>RenderDocument</c> can't be x:Bound to a TextBlock's Inlines, so
/// this is the feed analogue of <see cref="ImageHashBind"/>: declarative in XAML, painted
/// in code.
///
/// <para>D3: The painter reads <c>RemoteImage.revealed</c> directly from the manager-projected
/// document block — no per-card reveal flag. The item is rebuilt from the snapshot on every
/// observer tick, so the x:Bind OneWay on <c>Item</c> alone is sufficient to repaint after
/// a manager reveal (render-model.md § D3).</para>
///
/// Usage (FeedPage.xaml post-card):
///   &lt;TextBlock AutomationProperties.AutomationId="feed-post-text"
///              helpers:FeedPostBodyBind.Item="{x:Bind}"
///              TextWrapping="Wrap" MaxLines="6" /&gt;
/// </summary>
public static class FeedPostBodyBind
{
    public static readonly DependencyProperty ItemProperty =
        DependencyProperty.RegisterAttached(
            "Item", typeof(object), typeof(FeedPostBodyBind),
            new PropertyMetadata(null, OnChanged));

    public static void SetItem(DependencyObject d, object? value) => d.SetValue(ItemProperty, value);
    public static object? GetItem(DependencyObject d) => d.GetValue(ItemProperty);

    private static void OnChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is not TextBlock textBlock) return;
        if (GetItem(textBlock) is not FeedPostItem item)
        {
            // Container recycled onto a non-post item (or cleared): drop any stale runs.
            textBlock.Inlines.Clear();
            return;
        }
        // D3: revealed state is projected onto RenderBlock.RemoteImage.revealed by the
        // manager; DocumentPainter reads it per-block (no external revealRemote param).
        DocumentPainter.Apply(textBlock, item.Document);
    }
}
