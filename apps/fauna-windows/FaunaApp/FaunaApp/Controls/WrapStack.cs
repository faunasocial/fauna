using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Foundation;

namespace FaunaApp.Controls;

/// <summary>
/// A horizontal panel that wraps onto a new line instead of overflowing —
/// the WinUI counterpart of web's <c>flex-wrap: wrap</c> on the same rows
/// (<c>apps/fauna-web/src/routes/onboarding/+page.svelte</c>'s provider row).
///
/// Why this exists: a plain horizontal <see cref="StackPanel"/> lays its
/// children out past the edge of a width-constrained parent, and WinUI
/// clips rather than scrolls. UIA then reports the overflowed child
/// <c>IsOffscreen=true</c> even though it is hit-testable and
/// <c>InvokePattern.Invoke()</c> still works — the exact shape the bundled
/// provider's DNS row hit: six DNS-capable providers with <c>bundled</c>
/// last and longest, inside a <c>MaxWidth="700"</c> page. The e2e bridge
/// cannot rescue that: <c>flaui-bridge</c>'s scroll-into-view sweep walks
/// only <c>VerticallyScrollable</c> ancestors (the pages scroll vertically),
/// so <c>is_visible_scrolled</c> is powerless against a HORIZONTAL overflow
/// by design. Wrapping removes the overflow instead of chasing it, and
/// converges on the layout web already had (priority #1).
///
/// Deliberately minimal — no <c>Orientation</c>, no item-size uniformity.
/// WinUI 3 ships no <c>WrapPanel</c>; <c>ItemsWrapGrid</c> is virtualizing
/// and silently fails to materialize children outside a scroll-aware host
/// (see <c>RecipientPicker.xaml</c>'s note), which would break AutomationId
/// queries — the whole point here.
/// </summary>
public sealed partial class WrapStack : Panel
{
    public static readonly DependencyProperty HorizontalSpacingProperty =
        DependencyProperty.Register(
            nameof(HorizontalSpacing),
            typeof(double),
            typeof(WrapStack),
            new PropertyMetadata(0.0, OnSpacingChanged));

    /// <summary>Gap between two children on the same line.</summary>
    public double HorizontalSpacing
    {
        get => (double)GetValue(HorizontalSpacingProperty);
        set => SetValue(HorizontalSpacingProperty, value);
    }

    public static readonly DependencyProperty VerticalSpacingProperty =
        DependencyProperty.Register(
            nameof(VerticalSpacing),
            typeof(double),
            typeof(WrapStack),
            new PropertyMetadata(0.0, OnSpacingChanged));

    /// <summary>Gap between two wrapped lines.</summary>
    public double VerticalSpacing
    {
        get => (double)GetValue(VerticalSpacingProperty);
        set => SetValue(VerticalSpacingProperty, value);
    }

    private static void OnSpacingChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
        => ((WrapStack)d).InvalidateMeasure();

    protected override Size MeasureOverride(Size availableSize)
    {
        // An unconstrained width (inside an auto-sizing parent) means there
        // is nothing to wrap against — degrade to a single row rather than
        // wrapping after every child.
        var limit = double.IsInfinity(availableSize.Width) ? double.MaxValue : availableSize.Width;
        var childBudget = new Size(limit, double.PositiveInfinity);

        double lineWidth = 0, lineHeight = 0, totalWidth = 0, totalHeight = 0;
        var first = true;

        foreach (var child in Children)
        {
            child.Measure(childBudget);
            var desired = child.DesiredSize;
            var advance = first ? desired.Width : desired.Width + HorizontalSpacing;

            if (!first && lineWidth + advance > limit)
            {
                totalWidth = Math.Max(totalWidth, lineWidth);
                totalHeight += lineHeight + VerticalSpacing;
                lineWidth = desired.Width;
                lineHeight = desired.Height;
            }
            else
            {
                lineWidth += advance;
                lineHeight = Math.Max(lineHeight, desired.Height);
            }
            first = false;
        }

        totalWidth = Math.Max(totalWidth, lineWidth);
        totalHeight += lineHeight;
        return new Size(totalWidth, totalHeight);
    }

    protected override Size ArrangeOverride(Size finalSize)
    {
        double x = 0, y = 0, lineHeight = 0;
        var first = true;

        foreach (var child in Children)
        {
            var desired = child.DesiredSize;
            var advance = first ? desired.Width : desired.Width + HorizontalSpacing;

            if (!first && x + advance > finalSize.Width)
            {
                x = 0;
                y += lineHeight + VerticalSpacing;
                lineHeight = 0;
            }
            else if (!first)
            {
                x += HorizontalSpacing;
            }

            child.Arrange(new Rect(x, y, desired.Width, desired.Height));
            x += desired.Width;
            lineHeight = Math.Max(lineHeight, desired.Height);
            first = false;
        }

        return finalSize;
    }
}
