using System;
using Microsoft.UI.Xaml.Data;

namespace FaunaApp.Helpers;

/// <summary>
/// Prefixes a raw <c>PostSummary.tags</c> entry with <c>#</c> for chip display
/// (matches linux's <c>append_tag_chips</c> / the feed post-detail dialog's own
/// <c>$"#{tag}"</c>) — used as the item-template converter for the feed post-card's
/// tag-chip <c>ItemsControl</c> (an <c>x:DataType="x:String"</c> template has no
/// named property to feed a format function, so the transform is a converter
/// applied to the implicit whole-item bind).
/// </summary>
public class TagChipTextConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
        => value is string tag ? $"#{tag}" : string.Empty;

    public object ConvertBack(object value, Type targetType, object parameter, string language)
        => throw new NotSupportedException();
}
