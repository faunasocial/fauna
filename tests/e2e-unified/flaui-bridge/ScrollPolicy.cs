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

    /// <summary>How long one sweep step waits for the container to reach its target
    /// before reading on anyway. The reads it polls with cost about a millisecond, and a
    /// healthy page lands on the first of them, so this is a ceiling a settled app never
    /// pays.</summary>
    public const int SweepSettleMs = 1000;

    /// <summary>Whether the container's position has reached a step's target.</summary>
    public static bool Landed(double achieved, double target) => Math.Abs(achieved - target) < 0.5;

    /// <summary>Whether the container is at the bottom of its range. Only the position
    /// itself says so: a position that did not change since the last step is also what a
    /// scroll that has not landed yet looks like, and on a loaded box reading it that way
    /// ended the sweep two steps in, every time.</summary>
    public static bool ReachedBottom(double achieved) => achieved >= 99.5;

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

    // ── Bringing an element in WHOLE (Actions.MakeWhole) ─────────────────────────
    //
    // WinUI clips an element's UIA BoundingRectangle to its ScrollViewer, so a card a
    // quarter on screen reads as a smaller card wholly on screen: one reading cannot
    // tell the two apart (28 % of a post-card measured whole on 2026-11-04).
    // Two readings at different scroll positions can: a whole element keeps its
    // height as it moves, a clipped one does not, and one taller than the viewport does
    // not move at all. Everything below is arithmetic over vertical rect spans (screen
    // pixels) and scroll percentages, pinned by SelfTest.ScrollPolicyChecks.

    /// <summary>A vertical span of a UIA rect, in screen pixels.</summary>
    public readonly record struct Span(double Top, double Height)
    {
        public double Bottom => Top + Height;
        public double Centre => Top + Height / 2.0;
    }

    /// <summary>What the readings so far prove about an element's true extent.</summary>
    public enum WholeVerdict
    {
        /// <summary>Nothing proven yet: the reading may be clipped.</summary>
        Unknown,
        /// <summary>The whole element shows; its reading is its true height.</summary>
        Whole,
        /// <summary>Clipped at both edges: taller than the viewport, true height unknown.</summary>
        SpansViewport,
    }

    /// <summary>How many scroll moves bringing one element in whole may take. A sliver
    /// takes two (centre what shows, then centre the whole), the rest are slack for a
    /// move the container's padding made fall short; the bound keeps a list that never
    /// settles from holding the bridge's UIA gate.</summary>
    public const int MakeWholeMaxMoves = 4;

    /// <summary>How far inside the scroll container's rect an element's edges must sit
    /// before one reading proves it whole. The clip is the container's content presenter,
    /// inset from the container's own rect by its padding (16–24 DIP across this app's
    /// pages), so an edge flush with the clip can sit that far inside the rect; the
    /// allowance stays above any such inset.</summary>
    public static double EdgeAllowance(double bandHeight) => Math.Max(48.0, bandHeight * 0.15);

    /// <summary>How far an element's centre may sit from the viewport's and still count
    /// as centred — loose enough to absorb a container whose padding is uneven, so the
    /// container's centre and the clip's differ by a few pixels.</summary>
    public static double CentreTolerance(double bandHeight) => Math.Max(4.0, bandHeight * 0.05);

    /// <summary>Two readings of one edge closer than this are the same edge.</summary>
    public const double SameEdgePx = 1.5;

    /// <summary>How long a move's re-read waits for the element's rect to follow the
    /// landed scroll position and settle. The reads cost about a millisecond; only an
    /// element that does not move at all — one spanning the viewport — pays it in full.</summary>
    public const int SpanSettleMs = 500;

    /// <summary>Whether ONE reading proves the element whole: both edges clear of the
    /// band's edges by more than any clip inset (<see cref="EdgeAllowance"/>). An edge
    /// nearer the band's edge may be the clip's, so such a reading proves nothing.</summary>
    public static bool ClearOfEdges(Span el, Span band)
    {
        if (el.Height <= 0) return false;
        var allowance = EdgeAllowance(band.Height);
        return el.Top >= band.Top + allowance && el.Bottom <= band.Bottom - allowance;
    }

    /// <summary>The scroll percent that moves the content up by <paramref name="deltaPx"/>
    /// screen pixels (negative: down), clamped to the range. <c>SetScrollPercent</c> walks
    /// the SCROLLABLE range (content minus viewport, <see cref="ViewportStart"/>), which is
    /// <c>viewportPx · (100 / viewSize − 1)</c> pixels. <paramref name="viewportPx"/> is the
    /// container's rect, which can exceed the true viewport by its padding, so a move can
    /// fall short by that share; the caller re-reads and moves again. An unreadable view
    /// size answers the current percent: no move.</summary>
    public static double PercentAfterMove(double percent, double viewSizePercent, double viewportPx, double deltaPx)
    {
        if (!(viewSizePercent > 0 && viewSizePercent < 100) || !(viewportPx > 0)) return percent;
        var rangePx = viewportPx * (100.0 / viewSizePercent - 1.0);
        return Math.Clamp(percent + deltaPx / rangePx * 100.0, 0.0, 100.0);
    }

    /// <summary>What two readings of one element, before and after the list moved, prove.
    /// Both edges moved and the height held: neither reading was clipped, so the element
    /// is whole. Neither edge moved and the reading fills the band: it is clipped at both
    /// edges, taller than the viewport. Anything else — the height changed (the move
    /// uncovered or hid part of it), or a small reading did not move — proves nothing.</summary>
    public static WholeVerdict ClassifyMove(Span before, Span after, Span band)
    {
        if (before.Height <= 0 || after.Height <= 0) return WholeVerdict.Unknown;
        var topMoved = Math.Abs(after.Top - before.Top) > SameEdgePx;
        var bottomMoved = Math.Abs(after.Bottom - before.Bottom) > SameEdgePx;
        var sameHeight = Math.Abs(after.Height - before.Height) <= SameEdgePx;
        if (topMoved && bottomMoved && sameHeight) return WholeVerdict.Whole;
        if (!topMoved && !bottomMoved && after.Height >= band.Height - 2 * EdgeAllowance(band.Height))
            return WholeVerdict.SpansViewport;
        return WholeVerdict.Unknown;
    }

    /// <summary>What one reading proves when the list cannot move any further toward
    /// centring it. At the top of the range the content starts at the viewport's top, so
    /// nothing is clipped above; a reading clipped below reaches the band's bottom edge,
    /// so its centre cannot sit above the band's centre. Hence a reading in the upper half
    /// at the top of the range is whole — and, mirrored, one in the lower half at the
    /// bottom.</summary>
    public static WholeVerdict AtRangeEnd(Span el, Span band, double percent)
    {
        if (el.Height <= 0) return WholeVerdict.Unknown;
        var tolerance = CentreTolerance(band.Height);
        if (percent <= 0.5 && el.Centre < band.Centre - tolerance) return WholeVerdict.Whole;
        if (percent >= 99.5 && el.Centre > band.Centre + tolerance) return WholeVerdict.Whole;
        return WholeVerdict.Unknown;
    }

    /// <summary>The share of an element's TRUE height that shows: what a reading shows
    /// (the clipped rect's height is exactly the part on screen) over the height a whole
    /// reading measured. Never a reading over itself, which answers 1.0 for any sliver.</summary>
    public static double VisibleShare(double shownPx, double trueHeightPx)
        => trueHeightPx > 0 ? Math.Clamp(shownPx / trueHeightPx, 0.0, 1.0) : 0.0;

    // ── Placing an element at a measured fraction above the bottom edge ─────────────
    //
    // `POST /element/scroll-to-bottom-fraction` (e2e-conventions.md § Driver primitives →
    // *Placing an element at a measured fraction*): leave `fraction` of an element's TRUE
    // height above its scroll container's bottom edge. MakeWhole cannot serve it: a video
    // card on the 600 × 600 window is taller than the feed's viewport, so no reading ever
    // shows it whole (measured 2026-11-06: 239 px growing to the band's full 339 px).
    // So the placement walks the element's EDGES instead, each proven its own one at a
    // time (TopOwn, BottomOwn — the in-viewport read's doctrine): its top, then a second
    // reading of its top after a move, which calibrates the screen pixels one scroll
    // percent moves (PxPerPercent); then its bottom, the height measured across the two
    // readings (EdgeAt). A short element shows both edges at once and needs no
    // calibration. The placement then moves the top to `fraction · trueHeight` above the
    // clip line. The clip line starts as the container rect's bottom, which can sit a
    // padding below the real clip; a reading whose top is its own but which is shorter
    // than the true height ends exactly at the clip (ClipBottomOf).

    /// <summary>How many moves a bottom-edge placement may take: up to three to walk the
    /// edges of an element taller than its viewport, one to place it and one to correct
    /// for a clip line the first placement measured; the rest are slack for a move the
    /// container's padding made fall short.</summary>
    public const int BottomPlacementMaxMoves = 8;

    /// <summary>Whether a reading's TOP is the element's own: clear of the band's top edge
    /// by more than any clip inset, or at the top of the scroll range, where the content
    /// starts inside the viewport and nothing is clipped above.</summary>
    public static bool TopOwn(Span reading, Span band, double percent)
        => reading.Height > 0 && (percent <= 0.5 || reading.Top >= band.Top + EdgeAllowance(band.Height));

    /// <summary>Whether a reading's BOTTOM is the element's own: clear of the band's bottom
    /// edge by more than any clip inset, or at the bottom of the scroll range.</summary>
    public static bool BottomOwn(Span reading, Span band, double percent)
        => reading.Height > 0 && (percent >= 99.5 || reading.Bottom <= band.Bottom - EdgeAllowance(band.Height));

    /// <summary>How far one edge-seeking move travels: the band less an edge allowance at
    /// each end, so an edge just out of view comes in clear of the far edge, and an element
    /// whose near edge was in view cannot leave the band in one move.</summary>
    public static double WalkStepPx(double bandHeight)
        => Math.Max(bandHeight / 3.0, bandHeight - 2 * EdgeAllowance(bandHeight));

    /// <summary>The least travel a calibrating move makes: enough that two readings of one
    /// edge, a pixel apart in rounding, still give the pixels-per-percent within a few
    /// percent.</summary>
    public const double MinCalibrationPx = 24.0;

    /// <summary>The screen pixels one scroll percent moves the content, from two readings
    /// of one edge known to be the element's own at two scroll positions. Null when the
    /// list did not move, or the edge moved the wrong way (one of the readings was not the
    /// edge it was taken for).</summary>
    public static double? PxPerPercent(double edgeBefore, double percentBefore, double edgeAfter, double percentAfter)
    {
        var dp = percentAfter - percentBefore;
        if (Math.Abs(dp) < 0.01) return null;
        var k = (edgeBefore - edgeAfter) / dp;
        return k > 0 ? k : null;
    }

    /// <summary>Where an edge read at <paramref name="refEdge"/> at
    /// <paramref name="refPercent"/> sits on screen at <paramref name="percent"/> — on
    /// screen or not: the content moves up <paramref name="pxPerPercent"/> pixels for every
    /// percent the position grows.</summary>
    public static double EdgeAt(double refEdge, double refPercent, double pxPerPercent, double percent)
        => refEdge - pxPerPercent * (percent - refPercent);

    /// <summary>How far, in screen pixels, to move the content up (negative: down) so that
    /// <paramref name="fraction"/> of an element of true height <paramref name="trueHeight"/>
    /// whose top reads at <paramref name="top"/> shows above <paramref name="clipBottom"/>.
    /// The fraction is clamped to 0–1.</summary>
    public static double BottomPlacementDelta(double top, double trueHeight, double clipBottom, double fraction)
        => top - (clipBottom - Math.Clamp(fraction, 0.0, 1.0) * trueHeight);

    /// <summary>The clip line one reading proves: a reading shorter than the element's true
    /// height is cut at the clip, so its bottom IS the clip line. A whole reading (or an
    /// empty one) says nothing about where the clip is: null.</summary>
    public static double? ClipBottomOf(Span reading, double trueHeight)
        => reading.Height > 0 && reading.Height < trueHeight - SameEdgePx ? reading.Bottom : null;

    /// <summary>Whether a reading shows the asked share of the element: its shown height
    /// within two edge widths, or a hundredth of the element, of
    /// <c>fraction · trueHeight</c>.</summary>
    public static bool PlacedAt(double shownPx, double trueHeight, double fraction)
        => Math.Abs(shownPx - Math.Clamp(fraction, 0.0, 1.0) * trueHeight)
           <= Math.Max(2 * SameEdgePx, 0.01 * trueHeight);

    /// <summary>Whether a move has landed: within a tenth of its own size (and 0.01 %).
    /// <see cref="Landed"/>'s fixed half percent suits the sweep's viewport-sized steps,
    /// but a centring move can be smaller than that, and read as landed before it began.</summary>
    public static bool MoveLanded(double start, double achieved, double target)
        => Math.Abs(achieved - target) <= Math.Max(0.01, 0.1 * Math.Abs(target - start));

    // ── Reading "in view" without moving the list (the `in-viewport` attribute) ──────
    //
    // The cross-driver "brought into view" read (`drivers/base.py::in_viewport`): true
    // when the element's vertical CENTRE lies inside its scroll container's visible band,
    // read from the element's WHOLE extent, as linux and tui read it. A read never scrolls,
    // so it cannot take MakeWhole's second reading, and the centre of a clipped reading
    // always lies inside the band: a sliver read as in view until 2026-11-06.
    //
    // One reading can still prove its edges one at a time. An edge clear of the band's
    // edge by more than any clip inset (EdgeAllowance) is the element's own. So is an edge
    // past which ANOTHER element of the same container reads: every reading there is cut
    // at the same clip line, so a reading beyond this edge puts the line beyond it too. So
    // is an edge at an end of the scroll range, where the content starts or ends inside the
    // viewport. Both edges proven: the reading is the whole element, and its centre
    // answers. Either one unproven: the element may run on past the viewport by any
    // amount, so the side of the band its centre lies on is unknown. That is Undecided,
    // named and never guessed, because a guess is exactly what read the sliver as in view.

    /// <summary>What one reading says about whether an element's centre is in view.</summary>
    public enum ViewportVerdict
    {
        /// <summary>Whole, and its centre lies inside the band.</summary>
        InView,
        /// <summary>No part of it shows, or it is whole and its centre lies outside.</summary>
        OutOfView,
        /// <summary>An edge may be the viewport's clip: its true centre is unknown.</summary>
        Undecided,
    }

    /// <summary>How many siblings on each side, and how many levels of ancestors, the read
    /// looks through for an element reading past an edge it could not otherwise prove. A
    /// thread's bubbles flatten into one long run of siblings in the UIA tree, and the
    /// elements that prove an edge are the element's near neighbours (a bubble's header
    /// above its text, its reply row below), so a bounded walk finds them in a few reads
    /// and a long thread never costs a read per element.</summary>
    public const int EvidenceSiblings = 8;
    public const int EvidenceLevels = 6;

    /// <summary>How far the content sits from the top and from the bottom of its scroll
    /// range, in screen pixels: the range is <c>viewportPx · (100 / viewSize − 1)</c>
    /// pixels (<see cref="PercentAfterMove"/>). Null when the view size or the position is
    /// unreadable, or the container does not scroll.</summary>
    public static (double FromTop, double FromBottom)? RangeOffsetsPx(
        double percent, double viewSizePercent, double viewportPx)
    {
        if (!(viewSizePercent > 0 && viewSizePercent < 100) || !(viewportPx > 0)) return null;
        if (double.IsNaN(percent) || percent < 0) return null;
        var rangePx = viewportPx * (100.0 / viewSizePercent - 1.0);
        var p = Math.Min(percent, 100.0);
        return (p / 100.0 * rangePx, (100.0 - p) / 100.0 * rangePx);
    }

    /// <summary>Whether a reading's TOP edge is the element's own rather than the clip's:
    /// clear of the band's top by more than any inset, or the list at the top of its range,
    /// or another element of the same container reading higher.</summary>
    public static bool TopProven(Span el, Span band, double? fromTopPx, double? highestOtherTop)
        => el.Top >= band.Top + EdgeAllowance(band.Height)
           || fromTopPx is double t && t <= SameEdgePx
           || highestOtherTop is double o && o < el.Top - SameEdgePx;

    /// <summary>The mirror of <see cref="TopProven"/> for the BOTTOM edge.</summary>
    public static bool BottomProven(Span el, Span band, double? fromBottomPx, double? lowestOtherBottom)
        => el.Bottom <= band.Bottom - EdgeAllowance(band.Height)
           || fromBottomPx is double b && b <= SameEdgePx
           || lowestOtherBottom is double o && o > el.Bottom + SameEdgePx;

    /// <summary>The <c>in-viewport</c> verdict on one reading. <paramref name="range"/> is
    /// <see cref="RangeOffsetsPx"/>; the two evidence values are the highest top and the
    /// lowest bottom any OTHER element of the same container reads (null: none read, or
    /// none looked at yet).</summary>
    public static (ViewportVerdict Verdict, string Why) ReadInViewport(
        Span el, bool offscreen, Span band, (double FromTop, double FromBottom)? range,
        double? highestOtherTop, double? lowestOtherBottom)
    {
        if (band.Height <= 0) return (ViewportVerdict.OutOfView, "its scroll container shows nothing");
        if (offscreen || el.Height <= 0) return (ViewportVerdict.OutOfView, "no part of it shows");
        var top = TopProven(el, band, range?.FromTop, highestOtherTop);
        var bottom = BottomProven(el, band, range?.FromBottom, lowestOtherBottom);
        var at = $"reading {el.Top:0.#}+{el.Height:0.#} in band {band.Top:0.#}+{band.Height:0.#}";
        if (top && bottom)
        {
            var centre = el.Centre;
            return centre >= band.Top && centre <= band.Bottom
                ? (ViewportVerdict.InView, $"whole, its centre inside the band ({at})")
                : (ViewportVerdict.OutOfView, $"whole, its centre outside the band ({at})");
        }
        var edges = top ? "bottom edge" : bottom ? "top edge" : "top and bottom edges";
        return (ViewportVerdict.Undecided,
            $"its {edges} may be the viewport's clip ({at}): within {EdgeAllowance(band.Height):0} px "
            + "of the band's edge, with no other element reading past it and the list not at that end "
            + "of its range, so the element may run on past the viewport and its centre's side of the "
            + "band is unknown without moving the list");
    }
}
