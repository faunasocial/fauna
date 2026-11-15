using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;

namespace FaunaApp.Feed;

/// <summary>
/// The mark a feed card's post body carries: only a card that shows the post is a cue
/// subject (<c>docs/goal/behavior/engagement-cues.md</c> § Cue vocabulary &amp;
/// derivation — a muted, blocked or collapsed placeholder hides what the post says, so
/// lingering on it is no exposure to the post at all). <c>PostCardTemplates.xaml</c>
/// sets <see cref="IsSubjectProperty"/> on the one grid that paints the post — the arm
/// no placeholder (region, nest, content-policy block, muted collapse, content-policy
/// collapse) shows — and <see cref="CueViewportObserver"/> hands the shared tracker only
/// rows whose marked body is laid out visible (<see cref="ShowsPost"/>). The twin of
/// linux's <c>cue-subject</c> class, android's <c>CueSubjectMark</c>, apple's
/// <c>cueSubject(postId:)</c>, web's <c>data-cue-post</c> and tui's subject-less
/// placeholder rows.
/// </summary>
public static class CueSubject
{
    /// <summary>How deep under a list row the mark is looked for — the row's own
    /// presenter, the card's root button and its template, the card grid and its arms
    /// sit well inside it.</summary>
    private const int MaxDepth = 8;

    public static readonly DependencyProperty IsSubjectProperty =
        DependencyProperty.RegisterAttached(
            "IsSubject", typeof(bool), typeof(CueSubject), new PropertyMetadata(false));

    public static void SetIsSubject(DependencyObject d, bool value) => d.SetValue(IsSubjectProperty, value);
    public static bool GetIsSubject(DependencyObject d) => (bool)d.GetValue(IsSubjectProperty);

    /// <summary>Whether the realized list row <paramref name="row"/> shows its post right
    /// now: its marked body is found and laid out visible. A card turned into a
    /// placeholder (a muted word added, a rule arriving) collapses that body and stops
    /// being measured at once; one revealed again is measured again. Breadth-first, so
    /// the body is met at its own level before any arm's children are walked. UI thread
    /// only.</summary>
    internal static bool ShowsPost(DependencyObject row)
    {
        var level = new System.Collections.Generic.List<DependencyObject> { row };
        for (var depth = 0; depth <= MaxDepth && level.Count > 0; depth++)
        {
            var next = new System.Collections.Generic.List<DependencyObject>();
            foreach (var node in level)
            {
                if (GetIsSubject(node))
                {
                    return node is UIElement { Visibility: Visibility.Visible };
                }
                var count = VisualTreeHelper.GetChildrenCount(node);
                for (var i = 0; i < count; i++) next.Add(VisualTreeHelper.GetChild(node, i));
            }
            level = next;
        }
        return false;
    }
}
