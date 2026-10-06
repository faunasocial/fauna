namespace FauiBridge;

/// <summary>
/// Pure geometry and selection logic factored out of <see cref="Actions"/> so it
/// can be red/green-verified without any real UIA element, window, or app — see
/// <see cref="SelfTest.ScrollPolicyChecks"/> (<c>--self-test-scroll-policy</c>).
/// Nothing here touches FlaUI or the OS: every method is plain percent/fraction
/// arithmetic and array scanning, testable with numbers alone.
/// </summary>
internal static class ScrollPolicy
{
    /// <summary>The scroll-percent step <see cref="Actions.ScrollIntoView"/>'s sweep
    /// advances by, as a fraction of the container's own viewport (<c>VerticalViewSize</c>).
    ///
    /// <para>A full 1.0 (one viewport per step) is safe for DISCOVERY (any part of the
    /// element on screen — <c>IsOffscreen == false</c>), not merely convenient: UIA's
    /// <c>SetScrollPercent</c> walks the container's SCROLLABLE RANGE (content minus
    /// viewport), not the raw content, so <see cref="ViewportStart"/> maps a step of
    /// exactly one viewport-size to an actual offset advance of LESS than one viewport —
    /// by <c>(viewSize/100)²</c> of the content, which is the guaranteed overlap between
    /// any two consecutive steps, always ≥ 0. Halving the step to 0.5 (the pre-fix value)
    /// bought no safety this arithmetic didn't already guarantee at 1.0 — only twice the
    /// UIA round trips, each ~2 s (<see cref="Actions.ScrollIntoView"/>'s own doc comment).
    /// <see cref="SelfTest.ScrollPolicyChecks"/> pins both the step-count budget (red at
    /// 0.5, green at 1.0) and the gap-free guarantee (holds at both).</para>
    /// </summary>
    public const double SweepStepViewportFraction = 1.0;

    /// <summary>Floor and ceiling on the step, in percent. The floor keeps a very long
    /// list (a tiny viewport) at a bounded step count rather than an unbounded one; the
    /// ceiling keeps a container whose viewport is most of its content from jumping
    /// straight to the end in one move.</summary>
    public const double SweepStepMin = 5.0;
    public const double SweepStepMax = 25.0;

    /// <summary>The <c>SetScrollPercent</c> targets a sweep visits, given the container's
    /// own <c>VerticalViewSize</c> (%). Always ends at exactly 100 regardless of whether
    /// <c>step</c> divides it evenly — the loop's bound is "the last target reached 100",
    /// not "the largest multiple of step below 100", which is what left a page's final
    /// slice permanently unreachable before it was fixed.</summary>
    public static IReadOnlyList<double> ComputeSweepTargets(double viewSize, double fraction, double min, double max)
    {
        var step = viewSize > 0 && viewSize <= 100
            ? Math.Clamp(viewSize * fraction, min, max)
            : min;
        var targets = new List<double>();
        for (double p = 0; ; p += step)
        {
            var target = Math.Min(p, 100.0);
            targets.Add(target);
            if (target >= 100.0) break;
        }
        return targets;
    }

    /// <summary>The absolute top-of-viewport offset a <c>SetScrollPercent(percent)</c>
    /// call produces, as a fraction (0..1) of the TOTAL content height. This is UIA's own
    /// mapping: <c>percent</c> walks the scrollable range (content minus viewport), not
    /// the content itself — the reason a step of exactly one viewport-size still leaves
    /// consecutive viewports overlapping instead of exactly abutting.</summary>
    public static double ViewportStart(double percent, double viewSizePercent)
    {
        var scrollableRange = 1.0 - viewSizePercent / 100.0;
        return percent / 100.0 * scrollableRange;
    }

    /// <summary>Whether an element occupying content-fraction range [elStart, elEnd]
    /// (0..1 of total content height) has any part onscreen when the container sits at
    /// <c>percent</c> — the same "any overlap" test UIA's <c>IsOffscreen</c> answers.</summary>
    public static bool ElementVisibleAt(double elStart, double elEnd, double percent, double viewSizePercent)
    {
        var viewStart = ViewportStart(percent, viewSizePercent);
        var viewEnd = viewStart + viewSizePercent / 100.0;
        return elEnd > viewStart && elStart < viewEnd;
    }

    /// <summary>Whether a sweep over <c>targets</c> ever lands the element (occupying
    /// [elStart, elEnd]) onscreen.</summary>
    public static bool SweepFinds(IReadOnlyList<double> targets, double elStart, double elEnd, double viewSizePercent)
    {
        foreach (var p in targets)
            if (ElementVisibleAt(elStart, elEnd, p, viewSizePercent)) return true;
        return false;
    }

    /// <summary>Index of the widest (largest-area) candidate in <c>areas</c>, or -1 when
    /// empty — the rule the blind page-scroll fallback (<see cref="Actions.Scroll"/>)
    /// uses to pick a scrollable container by SIZE rather than by UIA tree order, so a
    /// narrow navigation rail never wins over the actual page content just because it is
    /// enumerated first (a settings page's 24-item rail precedes its content frame in
    /// tree order.</summary>
    public static int WidestIndex(IReadOnlyList<double> areas)
    {
        var best = -1;
        var bestArea = double.NegativeInfinity;
        for (var i = 0; i < areas.Count; i++)
        {
            if (areas[i] > bestArea) { bestArea = areas[i]; best = i; }
        }
        return best;
    }
}
