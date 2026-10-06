using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Helpers;

namespace FaunaApp.Helpers;

/// <summary>
/// Attached property that paints a feed post's <see cref="FaunaApp.Core.ViewModels.FeedPostItem.RemoteImages"/>
/// (render-model.md § D3 + § Implementation status) into a host <see cref="Panel"/> via
/// <see cref="DocumentPainter.ApplyRemoteImages"/> — the feed analogue of
/// <see cref="FeedPostBodyBind"/>: a ListView DataTemplate can't imperatively reach a row's
/// panel, so this is declarative in XAML, painted in code.
///
/// Usage (FeedPage.xaml post-card, just under the body):
///   &lt;StackPanel helpers:RemoteImagesBind.Item="{x:Bind}" /&gt;
/// </summary>
public static class RemoteImagesBind
{
    public static readonly DependencyProperty ItemProperty =
        DependencyProperty.RegisterAttached(
            "Item", typeof(object), typeof(RemoteImagesBind),
            new PropertyMetadata(null, OnChanged));

    public static void SetItem(DependencyObject d, object? value) => d.SetValue(ItemProperty, value);
    public static object? GetItem(DependencyObject d) => d.GetValue(ItemProperty);

    private static void OnChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        if (d is not Panel panel) return;
        if (GetItem(panel) is not FaunaApp.Core.ViewModels.FeedPostItem item)
        {
            // Container recycled onto a non-post item (or cleared): drop any stale widgets.
            panel.Children.Clear();
            return;
        }
        DocumentPainter.ApplyRemoteImages(panel, item.RemoteImages);
    }
}
