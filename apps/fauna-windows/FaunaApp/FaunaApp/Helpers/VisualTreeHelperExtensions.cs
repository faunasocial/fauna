using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;

namespace FaunaApp.Helpers;

/// <summary>
/// Small visual-tree walk to find the first descendant of type
/// <typeparamref name="T"/> — used to reach a control's template-generated
/// internal parts that have no public API to reach directly. The engagement-
/// cue capture shell (<c>FaunaApp.Feed.CueViewportObserver</c>,
/// <c>docs/goal/behavior/engagement-cues.md</c> §§ Cue vocabulary &amp;
/// derivation / At rest, task-6 of the personalization-port plan) uses this
/// to find <c>FeedPage.PostsList</c>'s internal <c>ScrollViewer</c> — every
/// WinUI <c>ListView</c>'s default control template contains exactly one, but
/// nothing exposes it directly.
///
/// A depth-first walk of <see cref="VisualTreeHelper"/>'s children; returns
/// the first match, or <c>null</c> if the template hasn't been applied yet
/// (the control isn't loaded) or the subtree contains no match.
/// </summary>
internal static class VisualTreeHelperExtensions
{
    internal static T? FindDescendant<T>(DependencyObject root) where T : DependencyObject
    {
        int count = VisualTreeHelper.GetChildrenCount(root);
        for (int i = 0; i < count; i++)
        {
            var child = VisualTreeHelper.GetChild(root, i);
            if (child is T match) return match;
            var nested = FindDescendant<T>(child);
            if (nested is not null) return nested;
        }
        return null;
    }
}
