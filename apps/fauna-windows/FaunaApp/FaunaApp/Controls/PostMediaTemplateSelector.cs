using FaunaApp.Core.Helpers;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FaunaApp.Controls;

/// <summary>
/// Picks the list card's template for one <see cref="PostMediaElement"/>: a
/// <c>post-image</c> for a picture, a <c>video-thumbnail</c> for a video — so the card paints
/// every media block of a post, in body order, from one list (render-model.md § D6b
/// <i>Accessors</i>). The typed variants are why the choice is made here and never by a flag
/// on one template: a video is never painted into an image element (§ D6b).
/// </summary>
public sealed class PostMediaTemplateSelector : DataTemplateSelector
{
    public DataTemplate? Image { get; set; }

    public DataTemplate? Video { get; set; }

    protected override DataTemplate? SelectTemplateCore(object item)
        => item is PostMediaElement { IsVideo: true } ? Video : Image;

    protected override DataTemplate? SelectTemplateCore(object item, DependencyObject container)
        => SelectTemplateCore(item);
}
