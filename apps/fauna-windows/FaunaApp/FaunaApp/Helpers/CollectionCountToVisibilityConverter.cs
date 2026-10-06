using System;
using System.Collections;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;

namespace FaunaApp.Helpers;

/// <summary>
/// Maps a bound <see cref="ICollection"/>'s emptiness to
/// <see cref="Visibility"/> — <c>Visible</c> iff at least one item. Used by the
/// global critical-alerts banner (critical-alerts.md § Rendering contract:
/// `critical-alerts` is present iff ≥1 alert is active), whose presence rule
/// is a collection-emptiness check rather than a single bool the ViewModel
/// would otherwise have to keep in lockstep with the collection itself.
/// </summary>
public class CollectionCountToVisibilityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language) =>
        value is ICollection { Count: > 0 } ? Visibility.Visible : Visibility.Collapsed;

    public object ConvertBack(object value, Type targetType, object parameter, string language)
        => throw new NotSupportedException();
}
