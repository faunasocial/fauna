using System.Diagnostics;
using System.Net;

namespace FauiBridge;

/// <summary>
/// Harness self-proof: the bridge keeps a usable handle to the app it launched for
/// that session's whole life, so <see cref="SessionManager.Quit"/> kills THROUGH the
/// handle rather than depending on the data-dir sweep to find a survivor.
///
/// <para> The bug, in one sentence: <c>Launch</c> handed FlaUI the bridge's own
/// <see cref="Process"/> object, and <c>Application.GetMainWindow</c> — which every
/// element find goes through — calls <c>Dispose()</c> on the process it holds
/// whenever the main window handle is missing. One shared object, disposed from
/// under the bridge, so every later <c>Refresh()</c>/<c>Id</c>/<c>HasExited</c> threw
/// <see cref="InvalidOperationException"/>. <c>Quit</c>'s kill loop read that as
/// "already gone" and the live app survived; mid-session it broke every subsequent
/// find, costing real feature coverage (2-3 of test_backups.py's 15 cases per run).</para>
///
/// <para>The reproduction needs no GUI app and no e2e run: any WINDOWLESS process is
/// permanently "main window handle missing", which is exactly the condition that
/// makes GetMainWindow dispose. <c>ping</c> stands in for the app.</para>
///
/// <para>Run: <c>FauiBridge.exe --self-test-handle-lifetime</c>; exit code = failures.</para>
/// </summary>
static class SelfTest
{
    public static int HandleLifetime()
    {
        var failures = 0;
        var session = new SessionManager();
        // A witness handle of our own, independent of both the bridge's and FlaUI's,
        // so "did the app actually die" is answered by neither party to the bug.
        Process? witness = null;
        var pid = -1;
        var quitRan = false;
        try
        {
            session.Launch("ping.exe", "-n 120 127.0.0.1");
            pid = session.AppPid;
            witness = Process.GetProcessById(pid);
            Console.WriteLine($"[self-test] launched stand-in app pid={pid}");

            // The disposing door: one element find. RootElement is what every
            // find goes through, and it is where GetMainWindow is called.
            try
            {
                _ = session.GetMainWindow(TimeSpan.FromSeconds(2));
                Console.WriteLine("[self-test] GetMainWindow returned (no main window expected)");
            }
            catch (Exception ex)
            {
                Console.WriteLine($"[self-test] GetMainWindow threw {ex.GetType().Name} (expected — windowless)");
            }

            // ASSERTION 1 — the bridge's handle still answers after that find.
            var answers = session.AppHandleAnswers(out var why);
            Console.WriteLine($"[self-test] handle answers after a find: {answers} ({why})");
            if (!answers)
            {
                Console.WriteLine("[self-test] FAIL: the tracked handle was disassociated by an element find.");
                failures++;
            }

            // ASSERTION 2 — Quit kills through the handle. No data dir is configured,
            // so the sweep cannot mask a handle that lost its process: Quit's verdict
            // here IS the handle's verdict.
            var closed = session.Quit();
            quitRan = true;
            witness.Refresh();
            Console.WriteLine($"[self-test] Quit reported closed={closed}; witness says HasExited={witness.HasExited}");
            if (!witness.HasExited)
            {
                Console.WriteLine("[self-test] FAIL: the app survived Quit — this is the harness leak.");
                failures++;
            }
            if (closed && !witness.HasExited)
            {
                Console.WriteLine("[self-test] FAIL: Quit reported the session closed while the app was still alive.");
                failures++;
            }
        }
        finally
        {
            // Dispose() is Quit(); calling it after a Quit that already ran only
            // prints the "no tracked app process" warning at a passing run.
            if (!quitRan) { try { session.Dispose(); } catch { } }
            if (pid > 0)
            {
                try { using var k = Process.GetProcessById(pid); k.Kill(entireProcessTree: true); k.WaitForExit(5000); }
                catch { /* already dead — the passing case */ }
            }
            witness?.Dispose();
        }

        Console.WriteLine(failures == 0
            ? "[self-test] PASS — handle survived the session and Quit killed through it."
            : $"[self-test] {failures} FAILURE(S)");
        return failures;
    }

    /// <summary>A CPU stand-in that outlives any budget here: <see cref="SessionManager.Quit"/>
    /// ends it. <c>cmd</c>, not <c>ping</c>, which spends no CPU to read.</summary>
    private const string SpinArgs = "/c for /L %i in (1,1,2000000000) do @rem";

    /// <summary>How long the spinning stand-in gets to show its first CPU time. Named
    /// and generous (convention 14): the poll returns the moment it does, and only a
    /// bridge that reads nothing pays this.</summary>
    private static readonly TimeSpan CpuReadingBudget = TimeSpan.FromSeconds(60);

    /// <summary>
    /// Pins what <c>/session/status</c> reports for a launch's timeout report
    /// (convention 6's launch-timeout rider): the app's own CPU time from the tracked
    /// handle, the instant it was started, a CPU pin read back, a CPU reading that
    /// survives the app's exit — a launch that died still says what it ran — and the
    /// launch's CPU wait, sampled once a second from the thread table: a stand-in that
    /// waits reads no thread Ready at any sample, one that spins is sampled at all.
    ///
    /// <para>Run: <c>FauiBridge.exe --self-test-launch-status</c>; exit code = failures.</para>
    /// </summary>
    public static int LaunchStatus()
    {
        var failures = 0;
        void Check(bool condition, string description)
        {
            Console.WriteLine(condition
                ? $"[self-test] PASS: {description}"
                : $"[self-test] FAIL: {description}");
            if (!condition) failures++;
        }

        // 1. The CPU the app actually got, and when it was started.
        var session = new SessionManager();
        var before = DateTime.UtcNow;
        try
        {
            session.Launch("cmd.exe", SpinArgs);
            var after = DateTime.UtcNow;
            Check(session.LaunchedAt >= before && session.LaunchedAt <= after,
                $"the launch instant is the launch's (got {session.LaunchedAt:o}, between {before:o} and {after:o})");
            var deadline = DateTime.UtcNow + CpuReadingBudget;
            long? cpu = session.AppCpuMs();
            while (DateTime.UtcNow < deadline && (cpu ?? 0) <= 0)
            {
                Thread.Sleep(50);
                cpu = session.AppCpuMs();
            }
            Check(cpu > 0, $"a spinning app reports its own CPU time (got {cpu?.ToString() ?? "null"} ms)");
            var spinning = session.AppThreadsReady();
            Check(spinning is { total: > 0 },
                $"a spinning app's thread table is read (got {spinning?.ready.ToString() ?? "null"} Ready of {spinning?.total.ToString() ?? "null"})");
            Check(session.WaitSamples().samples > 0,
                $"a launch's CPU wait is sampled from the launch on (got {session.WaitSamples().samples} sample(s))");
        }
        finally
        {
            session.Quit();
        }
        Check(session.AppCpuMs() is null, "no tracked app reports no CPU time, rather than a zero");
        Check(session.AppThreadsReady() is null, "no tracked app reports no thread table, rather than zeros");

        // 2. A CPU pin reads back. CPU 1: a mask that is not a lowest-n one, which is
        // what the fence's sweep treats as a deliberate pin.
        var pinned = new SessionManager();
        const long CpuOne = 0b10;
        try
        {
            pinned.Launch("ping.exe", "-n 30 127.0.0.1", cpuAffinityMask: CpuOne);
            Check(pinned.AppAffinityMask() == CpuOne,
                $"a launch's CPU pin reads back (got {pinned.AppAffinityMask()?.ToString() ?? "null"})");
            // A stand-in that waits (ping sleeps between its echoes) asks for no CPU:
            // its threads are Waiting, none Ready — the blocked launch's reading, at
            // one read and across the one-a-second samples alike.
            Thread.Sleep(2500);
            var waiting = pinned.AppThreadsReady();
            Check(waiting is { ready: 0, total: > 0 },
                $"a waiting app has no thread Ready (got {waiting?.ready.ToString() ?? "null"} Ready of {waiting?.total.ToString() ?? "null"})");
            var sampled = pinned.WaitSamples();
            Check(sampled is { samples: >= 2, ready: 0 },
                $"a waiting app's CPU wait samples read none Ready (got {sampled.ready} Ready at {sampled.samples} sample(s))");
        }
        finally
        {
            pinned.Quit();
        }

        // 3. The reading survives the exit, and the status says exited — and when.
        var exiting = new SessionManager();
        try
        {
            Check(exiting.AppExitedAt() is null, "no tracked app reports no exit instant");
            exiting.Launch("ping.exe", "-n 3 127.0.0.1");
            Check(exiting.AppExitedAt() is null, "a running app reports no exit instant");
            using var witness = Process.GetProcessById(exiting.AppPid);
            witness.WaitForExit(30_000);
            var (running, exitCode) = exiting.AppStatus();
            Check(!running && exitCode == 0, $"the stand-in has exited (running={running}, exit={exitCode})");
            Check(exiting.AppCpuMs() is not null, "an exited app still reports the CPU it ran");
            var exitedAt = exiting.AppExitedAt();
            Check(exitedAt > exiting.LaunchedAt && exitedAt <= DateTime.UtcNow,
                $"an exited app reports when it exited, after its launch (got {exitedAt:o}, launched {exiting.LaunchedAt:o})");
        }
        finally
        {
            exiting.Quit();
        }
        // The close drops the handle: what the app died of is unread after it, so a
        // driver reads it BEFORE its `DELETE /session` (convention 6's close rider).
        var (afterRunning, afterExit) = exiting.AppStatus();
        Check(!afterRunning && afterExit is null && exiting.AppExitedAt() is null,
            $"after the close no exit code or instant is left to read (exit={afterExit?.ToString() ?? "null"})");

        Console.WriteLine(failures == 0
            ? "[self-test] PASS — /session/status reports the app's CPU time, launch instant, exit instant, pin and sampled CPU wait."
            : $"[self-test] {failures} FAILURE(S)");
        return failures;
    }

    /// <summary>
    /// Pins <see cref="ScrollPolicy"/> — the arithmetic <c>Actions.cs::ScrollIntoView</c>'s
    /// sweep and <c>Actions.cs::Scroll</c>'s blind-fallback container choice run on — with
    /// no real UIA element, window, or app.
    ///
    /// <para>Six checks, run against the SAME constants production uses
    /// (<see cref="ScrollPolicy.SweepStepViewportFraction"/> etc. — never a hardcoded
    /// duplicate, so a future edit to the constant is graded by this test rather than
    /// silently drifting from it):</para>
    /// <list type="number">
    /// <item>Step-COUNT budget for a representative container (25%-viewport): must stay
    /// at or under 6 steps. RED at the pre-fix 0.5 fraction (9 steps), GREEN at 1.0 (5) —
    /// the one check this row's fix is actually for.</item>
    /// <item>The regression guard: a container whose step does NOT divide 100 evenly
    /// (18%-viewport) must still reach exactly 100 as its last target.</item>
    /// <item>An element in the sweep's FINAL slice (94–97% down the content) is found.</item>
    /// <item>A thin element sitting exactly at the TIGHTEST overlap between two
    /// consecutive steps is still found — the geometry proof that a full-viewport step
    /// cannot let an element fall through, computed from <see cref="ScrollPolicy.ViewportStart"/>'s
    /// scrollable-range scaling rather than assumed.</item>
    /// <item>The container-chooser: given synthetic bounding-rect areas with the
    /// NARROWEST one enumerated FIRST (mirroring a settings page's nav rail preceding its
    /// content frame in UIA tree order), the WIDEST one's index wins.</item>
    /// <item>Where a sweep stops: <see cref="ScrollPolicy.ReachedBottom"/> reads the
    /// position, never whether it moved, because a scroll that has not landed yet does
    /// not move either.</item>
    /// <item>7–12, bringing an element in WHOLE (<c>Actions.MakeWhole</c>), on the
    /// 28 %-visible post-card UIA's clipped rect reported whole: the honest share is the
    /// shown part over the true height; one reading proves wholeness only clear of both
    /// edges; a move's percent walks the scrollable range; two readings classify an
    /// element as whole, spanning the viewport, or unproven; an end of the range proves a
    /// near-half reading whole; a small centring move is not read as landed before it
    /// began.</item>
    /// <item>13–18, reading "in view" WITHOUT moving the list (the <c>in-viewport</c>
    /// attribute, <see cref="ScrollPolicy.ReadInViewport"/>), on the same geometry: the
    /// 28 % sliver is Undecided, never in view from its clipped centre; a centred card, a
    /// text whose neighbour reads past its edge, and a card flush with an edge at that end
    /// of the range are whole and in view; nothing shown is out of view.</item>
    /// </list>
    ///
    /// <para>Run: <c>FauiBridge.exe --self-test-scroll-policy</c>; exit code = failures.</para>
    /// </summary>
    public static int ScrollPolicyChecks()
    {
        var failures = 0;
        void Check(bool condition, string description)
        {
            Console.WriteLine(condition
                ? $"[self-test] PASS: {description}"
                : $"[self-test] FAIL: {description}");
            if (!condition) failures++;
        }

        // 1. Step-count budget — the check this row's fix is for.
        var budgetTargets = ScrollPolicy.ComputeSweepTargets(
            25.0, ScrollPolicy.SweepStepViewportFraction, ScrollPolicy.SweepStepMin, ScrollPolicy.SweepStepMax);
        Check(budgetTargets.Count <= 6,
            $"a 25%-viewport sweep takes at most 6 steps (got {budgetTargets.Count} at fraction={ScrollPolicy.SweepStepViewportFraction})");

        // 2. The regression guard: non-dividing step still reaches exactly 100.
        var oddTargets = ScrollPolicy.ComputeSweepTargets(
            18.0, ScrollPolicy.SweepStepViewportFraction, ScrollPolicy.SweepStepMin, ScrollPolicy.SweepStepMax);
        Check(oddTargets.Count > 0 && oddTargets[^1] == 100.0,
            $"an 18%-viewport sweep (step does not divide 100) still ends at exactly 100% (got {(oddTargets.Count > 0 ? oddTargets[^1] : double.NaN)})");

        // 3. An element in the sweep's final slice is found.
        Check(ScrollPolicy.SweepFinds(oddTargets, 0.94, 0.97, 18.0),
            "an element in the sweep's final slice (94-97% down the content) is found");

        // 4. A thin element at the tightest inter-step overlap is found — this is the
        // check that "decides the overlap question" for the CURRENT fraction: it proves
        // a full-viewport step cannot let an element fall through, rather than assuming it.
        if (oddTargets.Count >= 2)
        {
            var viewEndOfFirst = ScrollPolicy.ViewportStart(oddTargets[0], 18.0) + 18.0 / 100.0;
            var viewStartOfSecond = ScrollPolicy.ViewportStart(oddTargets[1], 18.0);
            var midpoint = (viewEndOfFirst + viewStartOfSecond) / 2.0;
            Check(viewStartOfSecond <= viewEndOfFirst,
                $"consecutive sweep steps overlap, not gap (step0 end={viewEndOfFirst:0.####}, step1 start={viewStartOfSecond:0.####})");
            Check(ScrollPolicy.SweepFinds(oddTargets, midpoint - 0.0001, midpoint + 0.0001, 18.0),
                $"a thin element straddling the tightest inter-step overlap (~{midpoint:0.####} of content) is found");
        }
        else
        {
            Check(false, "sweep produced fewer than 2 targets — cannot test inter-step overlap");
        }

        // 5. Container chooser: the widest candidate wins regardless of enumeration order.
        // Narrow-first mirrors a settings page's nav rail (small area) preceding its
        // content frame (large area) in UIA tree order.
        var narrowRailThenWideContent = new[] { 24_000.0, 560_000.0 };
        Check(ScrollPolicy.WidestIndex(narrowRailThenWideContent) == 1,
            "the widest scrollable candidate wins even when a narrower one is enumerated first");
        var wideContentThenNarrowRail = new[] { 560_000.0, 24_000.0 };
        Check(ScrollPolicy.WidestIndex(wideContentThenNarrowRail) == 0,
            "the widest scrollable candidate wins when enumerated first too");

        // 6. Where a sweep stops: only the position says the bottom. A page's first two
        // targets are 0 and one viewport, so a scroll that has not landed reads 0 twice.
        Check(!ScrollPolicy.ReachedBottom(0.0),
            "a position that stayed at the top is not the bottom (a scroll that has not landed reads the same)");
        Check(!ScrollPolicy.ReachedBottom(75.0), "a mid-page position is not the bottom");
        Check(ScrollPolicy.ReachedBottom(100.0), "the end of the range is the bottom");
        Check(ScrollPolicy.Landed(25.0, 25.0), "a position at its target has landed");
        Check(!ScrollPolicy.Landed(0.0, 25.0), "a position short of its target has not landed");

        // 7–12. Bringing an element in WHOLE (Actions.MakeWhole), on the geometry measured
        // 2026-11-04: a 189 px post-card whose top sat at 286.3 in a 338.5 px
        // viewport — 28 % of it on screen, which UIA's clipped rect reported as a 52.2 px
        // element wholly inside the viewport.
        var band = new ScrollPolicy.Span(0, 338.5);
        const double cardHeight = 189.0;
        var sliver = new ScrollPolicy.Span(286.3, band.Bottom - 286.3);

        // 7. The honest share is the shown part over the TRUE height; the clipped rect over
        // itself (the pre-fix VisibleFraction) answered 1.0 for this sliver.
        Check(Math.Abs(ScrollPolicy.VisibleShare(sliver.Height, cardHeight) - 0.276) < 0.005,
            $"the measured sliver shows {ScrollPolicy.VisibleShare(sliver.Height, cardHeight):0.000} of the card, not {sliver.Height / sliver.Height:0.0}");
        Check(ScrollPolicy.VisibleShare(cardHeight, cardHeight) == 1.0
              && ScrollPolicy.VisibleShare(10, 0) == 0.0,
            "a whole reading shows all of the card; no true height measures nothing");

        // 8. One reading proves wholeness only clear of both edges: the sliver is flush with
        // the bottom edge, and so is a whole card that merely touches it; a centred card is
        // clear; an edge one ScrollViewer padding (24 px) inside the rect proves nothing.
        Check(!ScrollPolicy.ClearOfEdges(sliver, band),
            "a reading flush with the viewport's bottom edge does not prove the card whole");
        var centred = new ScrollPolicy.Span(band.Centre - cardHeight / 2, cardHeight);
        Check(ScrollPolicy.ClearOfEdges(centred, band), "a centred card is proven whole by one reading");
        Check(!ScrollPolicy.ClearOfEdges(new ScrollPolicy.Span(band.Bottom - 24 - 60, 60), band),
            "a reading whose edge sits one padding inside the container's rect does not prove the card whole");

        // 9. A move's percent: one viewport of travel on a 25 % view size is a third of the
        // range (the range is content minus viewport: three viewports), clamped at both ends.
        var oneViewport = ScrollPolicy.PercentAfterMove(0, 25.0, 100.0, 100.0);
        Check(Math.Abs(oneViewport - 100.0 / 3.0) < 1e-9,
            $"moving one viewport down on a 25% view size lands at 33.3% (got {oneViewport:0.###})");
        Check(ScrollPolicy.PercentAfterMove(10, 25.0, 100.0, -1000) == 0.0
              && ScrollPolicy.PercentAfterMove(90, 25.0, 100.0, 1000) == 100.0,
            "a move past either end of the range clamps to it");
        Check(ScrollPolicy.PercentAfterMove(40, 0.0, 100.0, 50) == 40
              && ScrollPolicy.PercentAfterMove(40, 100.0, 100.0, 50) == 40,
            "an unreadable or non-scrolling view size moves nothing");

        // 10. Two readings classify: the sliver, centred on what shows, grows (proves
        // nothing yet); the card then keeps its height as it moves (whole); a reading that
        // fills the band and does not move spans the viewport; a small one that does not
        // move proves nothing.
        var afterFirstMove = new ScrollPolicy.Span(band.Centre - sliver.Height / 2, cardHeight);
        Check(ScrollPolicy.ClassifyMove(sliver, afterFirstMove, band) == ScrollPolicy.WholeVerdict.Unknown,
            "a reading that grew as the list moved was clipped — nothing proven yet");
        Check(ScrollPolicy.ClassifyMove(afterFirstMove, centred, band) == ScrollPolicy.WholeVerdict.Whole,
            "a reading that kept its height as both edges moved is the whole card");
        var fill = new ScrollPolicy.Span(band.Top + 16, band.Height - 32);
        Check(ScrollPolicy.ClassifyMove(fill, fill, band) == ScrollPolicy.WholeVerdict.SpansViewport,
            "a reading that fills the band and did not move spans the viewport");
        Check(ScrollPolicy.ClassifyMove(centred, centred, band) == ScrollPolicy.WholeVerdict.Unknown,
            "a small reading that did not move proves nothing");

        // 11. At an end of the range the list cannot move toward centring: a reading in the
        // upper half at the top, or the lower half at the bottom, cannot be clipped.
        var upper = new ScrollPolicy.Span(10, 60);
        var lower = new ScrollPolicy.Span(band.Bottom - 70, 60);
        Check(ScrollPolicy.AtRangeEnd(upper, band, 0.0) == ScrollPolicy.WholeVerdict.Whole
              && ScrollPolicy.AtRangeEnd(lower, band, 100.0) == ScrollPolicy.WholeVerdict.Whole,
            "a reading on the near half at an end of the range is whole");
        Check(ScrollPolicy.AtRangeEnd(sliver, band, 0.0) == ScrollPolicy.WholeVerdict.Unknown
              && ScrollPolicy.AtRangeEnd(upper, band, 50.0) == ScrollPolicy.WholeVerdict.Unknown,
            "a reading on the far half, or mid-range, proves nothing");

        // 12. A centring move smaller than Landed's fixed half percent has not landed
        // before it began.
        Check(!ScrollPolicy.MoveLanded(40.0, 40.0, 40.26) && ScrollPolicy.MoveLanded(40.0, 40.25, 40.26),
            "a 0.26% move reads landed only once the position has moved");

        // 13–18. Reading "in view" WITHOUT moving the list (the `in-viewport` attribute), on the same measured geometry: the true centre answers only once both
        // of the reading's edges are proven the element's own.
        const ScrollPolicy.ViewportVerdict inView = ScrollPolicy.ViewportVerdict.InView;
        const ScrollPolicy.ViewportVerdict outOfView = ScrollPolicy.ViewportVerdict.OutOfView;
        const ScrollPolicy.ViewportVerdict undecided = ScrollPolicy.ViewportVerdict.Undecided;
        // Mid-list on a 25 % view size: neither end of the range is near.
        var midRange = ScrollPolicy.RangeOffsetsPx(40.0, 25.0, band.Height);

        // 13. Where the list sits in its range, in pixels: nothing above it at 0 %, nothing
        // below it at 100 %, both far mid-list; an unreadable view size knows nothing.
        var atTop = ScrollPolicy.RangeOffsetsPx(0.0, 25.0, band.Height);
        var atBottom = ScrollPolicy.RangeOffsetsPx(100.0, 25.0, band.Height);
        Check(atTop is { FromTop: 0.0 } && atBottom is { FromBottom: 0.0 }
              && midRange is { FromTop: > 100, FromBottom: > 100 },
            $"the range offsets read 0 at each end and far mid-list (mid: {midRange})");
        Check(ScrollPolicy.RangeOffsetsPx(40.0, 0.0, band.Height) is null
              && ScrollPolicy.RangeOffsetsPx(40.0, 100.0, band.Height) is null
              && ScrollPolicy.RangeOffsetsPx(double.NaN, 25.0, band.Height) is null,
            "an unreadable view size or position, or a container that does not scroll, gives no range");

        // 14. THE gap: the 28 % sliver flush with the bottom edge, mid-list, nothing reading
        // below it. Its clipped rect's centre (312.4) lies inside the band, which is what read
        // it in view; its true centre (380.8) lies below the band. One reading cannot tell the
        // two cards apart, so the answer is Undecided — never InView.
        var (sliverVerdict, sliverWhy) = ScrollPolicy.ReadInViewport(sliver, false, band, midRange, null, null);
        Check(sliverVerdict == undecided && sliverWhy.Contains("bottom edge"),
            $"the measured sliver is not read in view from its clipped centre (got {sliverVerdict}: {sliverWhy})");
        var topSliver = new ScrollPolicy.Span(band.Top, sliver.Height);
        Check(ScrollPolicy.ReadInViewport(topSliver, false, band, midRange, null, null).Verdict == undecided,
            "a sliver flush with the top edge is Undecided too");
        // Its clipped neighbours share its clip line, so they prove nothing.
        Check(ScrollPolicy.ReadInViewport(sliver, false, band, midRange, 10.0, sliver.Bottom + 1.0).Verdict == undecided,
            "a neighbour reading to the same clip line does not prove the edge");

        // 15. A centred card is clear of both edges: whole, its centre in the band.
        Check(ScrollPolicy.ReadInViewport(centred, false, band, midRange, null, null).Verdict == inView,
            "a centred card reads in view with no evidence needed");

        // 16. A message text a bring-into-view left near the bottom edge: within the edge
        // allowance, but its own bubble's reply row reads below it, so the clip line is
        // below it too — whole, in view. The same text with nothing read below it is not proven.
        var text = new ScrollPolicy.Span(band.Bottom - 36 - 40, 40);
        Check(ScrollPolicy.ReadInViewport(text, false, band, midRange, text.Top - 30, band.Bottom - 6).Verdict == inView,
            "a text near the edge whose neighbour reads past it is whole and in view");
        Check(ScrollPolicy.ReadInViewport(text, false, band, midRange, null, null).Verdict == undecided,
            "the same text with no neighbour read past it is not proven whole");

        // 17. At an end of the range the content ends inside the viewport: a whole card
        // flush with the bottom at 100 %, or with the top at 0 %, is the whole card.
        var flushBottom = new ScrollPolicy.Span(band.Bottom - cardHeight, cardHeight);
        var flushTop = new ScrollPolicy.Span(band.Top, cardHeight);
        Check(ScrollPolicy.ReadInViewport(flushBottom, false, band, atBottom, null, null).Verdict == inView
              && ScrollPolicy.ReadInViewport(flushTop, false, band, atTop, null, null).Verdict == inView,
            "a card flush with an edge at that end of the range reads in view");
        Check(ScrollPolicy.ReadInViewport(flushBottom, false, band, atTop, null, null).Verdict == undecided,
            "a card flush with the bottom edge at the TOP of the range is not proven whole");

        // 18. Nothing shows: out of view, whatever else is known. One that fills the band
        // mid-list may run past both edges.
        Check(ScrollPolicy.ReadInViewport(centred, true, band, midRange, null, null).Verdict == outOfView
              && ScrollPolicy.ReadInViewport(new ScrollPolicy.Span(0, 0), false, band, midRange, null, null).Verdict == outOfView
              && ScrollPolicy.ReadInViewport(centred, false, new ScrollPolicy.Span(0, 0), midRange, null, null).Verdict == outOfView,
            "an offscreen element, an empty reading or an empty band reads out of view");
        var (fillVerdict, fillWhy) = ScrollPolicy.ReadInViewport(
            new ScrollPolicy.Span(band.Top, band.Height), false, band, midRange, null, null);
        Check(fillVerdict == undecided && fillWhy.Contains("top and bottom edges"),
            $"a reading filling the band mid-list is Undecided at both edges (got {fillVerdict}: {fillWhy})");

        Console.WriteLine(failures == 0
            ? "[self-test] PASS — sweep step policy and container choice are gap-free and cheap; an element is brought in whole from honest readings, and read in view only once proven whole."
            : $"[self-test] {failures} FAILURE(S)");
        return failures;
    }

    /// <summary>
    /// Pins <see cref="ActuationGate"/> — the decision every windows actuation route
    /// runs before it drives a control (`e2e-conventions.md` § convention 11) — with
    /// no UIA element, window, app, or environment of its own.
    ///
    /// <para><b>The polarity check is the load-bearing one, and it is not
    /// paranoia.</b> <see cref="ActuationGate.ShouldRefuse"/> is polarity-AGNOSTIC by
    /// construction: it takes <c>strict</c> as an argument, so it answers correctly
    /// whichever way the host's default points and can never catch a default that
    /// silently flips. apple learned this and pins its own polarity for the same
    /// reason (<c>apps/apple-e2e-automation.md</c> § The actuation gate). windows
    /// staged with refusal OFF until its 2026-09-11 sweep came back clean, and
    /// flipped on 2026-09-14; this check flipped in the same change, so a default
    /// that ever moves back reds here instead of passing unnoticed.</para>
    ///
    /// <para>Run: <c>FauiBridge.exe --self-test-actuation-gate</c>; exit code = failures.</para>
    /// </summary>
    /// <summary>
    /// The windows app's stall verdict (<see cref="StallVerdict"/>), every corner,
    /// with no app and no stall: the half of convention 11's stall stack that decides
    /// whether a stack is taken at all, so it must be assertable without one.
    /// </summary>
    public static int StallVerdictChecks()
    {
        var failures = 0;
        void Check(bool condition, string description)
        {
            Console.WriteLine(condition
                ? $"[self-test] PASS: {description}"
                : $"[self-test] FAIL: {description}");
            if (!condition) failures++;
        }

        var parked = new StallReadings(
            StallSeconds: 25, CpuStartMs: 1000, CpuNowMs: 1180, UiTid: 4242,
            UiState: 5, UiWaitReason: "ExecutionDelay", NotResponding: true);

        var (kind, clause) = StallVerdict.Decide(parked);
        Check(kind == StallKind.AppSide,
            "CPU moved and the UI thread is Waiting: the app's own code, so the stack is taken");
        Check(clause.Contains("ON THE UI THREAD") && clause.Contains("+180 ms")
              && clause.Contains("Waiting (ExecutionDelay)") && clause.Contains("Not Responding"),
            $"the app-side clause names the CPU, the state and the window: {clause}");

        Check(StallVerdict.Decide(parked with { UiState = 2, UiWaitReason = null }).Kind == StallKind.AppSide,
            "a Running UI thread in an app that ran is app-side too (computing, not starved)");

        foreach (var ready in new[] { 1, 3, 7 })
        {
            var (k, c) = StallVerdict.Decide(parked with { UiState = ready, UiWaitReason = null });
            Check(k == StallKind.Box && c.Contains("not an app finding") && c.Contains("no stack taken"),
                $"a UI thread in state {ready} waits for a core: the box, never a stack ({c})");
        }

        var (idleKind, idleClause) = StallVerdict.Decide(parked with { CpuNowMs = 1000 });
        Check(idleKind == StallKind.Box && idleClause.Contains("ran no CPU"),
            $"an app that ran no CPU across the stall was starved whole: the box ({idleClause})");

        // Cycles decide over milliseconds when both were read: a ~2 s window (a UIA
        // timeout) can show +0 ms for a process that ran, never +0 cycles.
        var (tickKind, tickClause) = StallVerdict.Decide(
            parked with { CpuNowMs = 1000, CyclesStart = 5_000_000, CyclesNow = 9_000_000 });
        Check(tickKind == StallKind.AppSide && tickClause.Contains("M cycles"),
            $"+0 ms but +4 M cycles: the app ran, so it is app-side ({tickClause})");
        Check(StallVerdict.Decide(
                parked with { CyclesStart = 5_000_000, CyclesNow = 5_000_000 }).Kind == StallKind.Box,
            "+180 ms but +0 cycles: the cycles decide, and nothing ran");

        Check(StallVerdict.Decide(parked with { UiTid = 0 }).Kind == StallKind.Unmeasured,
            "no UI thread found: UNMEASURED, never a verdict");
        Check(StallVerdict.Decide(parked with { CpuNowMs = null }).Kind == StallKind.Unmeasured,
            "no CPU reading: UNMEASURED, never a verdict");
        Check(StallVerdict.Decide(parked with { UiState = null }).Kind == StallKind.Unmeasured,
            "no thread state: UNMEASURED, never a verdict");
        Check(StallVerdict.Decide(parked with { UiState = null }).Clause.Contains("UNMEASURED"),
            "the unmeasured clause says so");

        // The faked starved reading a test uses to prove the box gets no stack.
        var (fakeKind, _) = StallVerdict.Decide(StallVerdict.Starved(parked));
        Check(fakeKind == StallKind.Box, "the faked starved reading decides the box");
        Check(StallVerdict.Decide(StallVerdict.Starved(parked with { UiTid = 0 })).Kind == StallKind.Box,
            "the faked starved reading decides the box even before a UI thread is found");

        // Which routes are watched: the app's UI-thread asks, never the control routes.
        Check(StallWatch.IsAppRoute("/element/text") && StallWatch.IsAppRoute("/registry")
              && StallWatch.IsAppRoute("/window/close"),
            "element, registry and window routes are watched");
        Check(!StallWatch.IsAppRoute("/session") && !StallWatch.IsAppRoute("/health")
              && !StallWatch.IsAppRoute("/session/status") && !StallWatch.IsAppRoute("/nest/start"),
            "control routes are not watched");

        Check(StallWatch.ReplyBudgetSeconds < 120,
            "the reply budget lands under the driver's 120 s BRIDGE_RPC_TIMEOUT_S, so the verdict is delivered");
        Check(StallWatch.ExamineAfterSeconds + 5 < StallWatch.ReplyBudgetSeconds,
            "an op is examined well before it is answered");

        Check(StallWatch.LooksLikeUiaTimeout(new TimeoutException()),
            "a TimeoutException is a UIA timeout");
        Check(StallWatch.LooksLikeUiaTimeout(
                new InvalidOperationException("wrapped", new System.Runtime.InteropServices.COMException(
                    "uia", unchecked((int)0x80131505)))),
            "a wrapped UIA_E_TIMEOUT HRESULT is a UIA timeout");
        Check(StallWatch.LooksLikeUiaTimeout(new System.ComponentModel.Win32Exception(1460)),
            "the main-window re-resolve's ERROR_TIMEOUT Win32Exception is a UIA timeout");
        Check(!StallWatch.LooksLikeUiaTimeout(new System.ComponentModel.Win32Exception(5)),
            "another Win32 error is not");
        Check(!StallWatch.LooksLikeUiaTimeout(new InvalidOperationException("other")),
            "any other failure is not");

        // A catch that answers an app read with "absent" answers for the element,
        // never for the app: a held UI thread or a crashed app is let through.
        Check(!UiaSwallow.MayAnswerAbsent(new TimeoutException("UIA Timeout")),
            "a UIA timeout is never answered as an absent element");
        Check(!UiaSwallow.MayAnswerAbsent(new System.ComponentModel.Win32Exception(1460)),
            "the main-window re-resolve's ERROR_TIMEOUT is never answered as an absent element");
        Check(!UiaSwallow.MayAnswerAbsent(
                new InvalidOperationException("wrapped", new TimeoutException("UIA Timeout"))),
            "a wrapped UIA timeout is never answered as an absent element");
        Check(!UiaSwallow.MayAnswerAbsent(new AppExitedException("the app exited")),
            "a crashed app is never answered as an absent element");
        Check(UiaSwallow.MayAnswerAbsent(new ElementNotFoundException("x", 0, 0))
              && UiaSwallow.MayAnswerAbsent(new InvalidOperationException("property not supported"))
              && UiaSwallow.MayAnswerAbsent(new System.ComponentModel.Win32Exception(5)),
            "an element's own failure (not found, a property it lacks, a refused call) may still answer absent");

        // A transient UIA timeout is waited out within the budget (convention 14); a
        // lasting one escapes UNCHANGED so the stall verdict still rides the reply.
        var attempts = 0;
        var traced = new List<string>();
        var got = UiaRetry.Run("probe", () =>
        {
            if (++attempts == 1) throw new TimeoutException("UIA Timeout");
            return "found";
        }, TimeSpan.FromSeconds(5), traced.Add, pauseMs: 1);
        Check(got == "found" && attempts == 2, "a UIA timeout once is retried and the read returns");
        Check(traced.Count == 1 && traced[0].Contains("absorbed") && traced[0].Contains("ms"),
            "an absorbed timeout is traced with its elapsed time");

        attempts = 0;
        var rootRetry = UiaRetry.Run("root", () =>
        {
            if (++attempts == 1) throw new System.ComponentModel.Win32Exception(1460);
            return 7;
        }, TimeSpan.FromSeconds(5), null, pauseMs: 1);
        Check(rootRetry == 7, "the root acquisition's ERROR_TIMEOUT is retried too");

        attempts = 0;
        var lasting = new TimeoutException("UIA Timeout");
        Exception? escaped = null;
        try
        {
            UiaRetry.Run<int>("lasting", () => { attempts++; throw lasting; },
                TimeSpan.FromMilliseconds(150), null, pauseMs: 20);
        }
        catch (Exception ex) { escaped = ex; }
        Check(ReferenceEquals(escaped, lasting) && attempts > 1,
            "a lasting UIA timeout escapes unchanged at the budget, never an answer");

        attempts = 0;
        escaped = null;
        try
        {
            UiaRetry.Run<int>("notfound", () => { attempts++; throw new ElementNotFoundException("x", 0, 0); },
                TimeSpan.FromSeconds(5), null, pauseMs: 1);
        }
        catch (Exception ex) { escaped = ex; }
        Check(escaped is ElementNotFoundException && attempts == 1,
            "a not-found is not a timeout: no retry here");

        Console.WriteLine(failures == 0
            ? "[self-test] stall verdict: all checks passed"
            : $"[self-test] stall verdict: {failures} check(s) FAILED");
        return failures == 0 ? 0 : 1;
    }

    public static int ActuationGateChecks()
    {
        var failures = 0;
        void Check(bool condition, string description)
        {
            Console.WriteLine(condition
                ? $"[self-test] PASS: {description}"
                : $"[self-test] FAIL: {description}");
            if (!condition) failures++;
        }

        // 1. The verdict, all four corners. An ENABLED control is driven in both
        // modes — the gate must be invisible on the hot path it now sits on.
        Check(!ActuationGate.ShouldRefuse(enabled: true, strict: true),
            "an enabled control is driven under strict");
        Check(!ActuationGate.ShouldRefuse(enabled: true, strict: false),
            "an enabled control is driven under permissive");
        Check(ActuationGate.ShouldRefuse(enabled: false, strict: true),
            "a disabled control is refused under strict");
        Check(!ActuationGate.ShouldRefuse(enabled: false, strict: false),
            "a disabled control is still DRIVEN under permissive — that is what makes ONE sweep enumerate EVERY offender");

        // 2. Polarity: windows REFUSES by default (since 2026-09-14). Read from the
        // constant production reads, never a duplicated literal, so a default that
        // moves is graded here instead of drifting past.
        Check(ActuationGate.WindowsRefusesDisabledActuationByDefault == true,
            "windows refuses a disabled actuation by default — --permissive-actuation is the way out");
        Check(ActuationGate.StrictFor(new Dictionary<string, string>(),
                ActuationGate.WindowsRefusesDisabledActuationByDefault) == true,
            "an unflagged run inherits windows' refusing default");

        // 3. Flag precedence. PERMISSIVE WINS when both are set: the enumerating run
        // must never be the run that turns red, or the measurement is lost along with
        // the offender list.
        Check(ActuationGate.StrictFor(
                new Dictionary<string, string> { [ActuationGate.StrictEnv] = "1" }, false),
            "--strict opts a staging host IN to refusal");
        Check(!ActuationGate.StrictFor(
                new Dictionary<string, string> { [ActuationGate.PermissiveEnv] = "1" }, true),
            "--permissive opts a refusing host OUT of refusal");
        Check(!ActuationGate.StrictFor(
                new Dictionary<string, string>
                {
                    [ActuationGate.PermissiveEnv] = "1",
                    [ActuationGate.StrictEnv] = "1",
                }, false),
            "permissive WINS over strict when both are set");
        // An EMPTY value is not a flag — the Rust half reads `is_some_and(|v| !v.is_empty())`,
        // and a shell that exports an unset variable would otherwise silently arm refusal.
        Check(!ActuationGate.StrictFor(
                new Dictionary<string, string> { [ActuationGate.StrictEnv] = "" }, false),
            "an EMPTY flag value is not set");

        // 4. The marker line, byte-for-byte as `fauna_e2e_agent::disabled_actuation_marker`
        // writes it. This is the cross-app grep contract: one sweep, one grep, every app.
        var marker = ActuationGate.MarkerLine("click", "restore-confirm-button", 0);
        Check(marker == "[InProcessAutomation] DISABLED-ACTUATION click id=restore-confirm-button index=0",
            $"the marker line matches the shared cross-app format (got: {marker})");

        // 5. The refusal message. `drivers/http_bridge.py::select` discriminates this
        // 409 from the option-not-offered 409 on the substring "element is disabled"
        // ALONE, so this wording is a driver-side contract, not prose.
        var refusal = ActuationGate.RefusalMessage("select", "kind-select", 2);
        Check(refusal.StartsWith("element is disabled: kind-select[2]"),
            $"the refusal opens with the driver's discriminator and names element+index (got: {refusal})");
        Check(refusal.Contains("select refused"),
            $"the refusal names the route so a log is triageable per route (got: {refusal})");
        // The WHOLE string, em-dash included, so a tail edit here cannot drift away
        // from `fauna_e2e_agent::disabled_actuation_refusal` unnoticed. The Rust
        // wording is the authority; this literal is the only thing that can hold a
        // hand-ported copy to it, since no C# test can read the crate.
        Check(refusal == "element is disabled: kind-select[2] — select refused "
                       + "(convention 11: an actuation route must not drive a control "
                       + "the UI has disabled)",
            $"the refusal is word-for-word the shared cross-app text (got: {refusal})");

        // 6. The side-effecting entry point: strict refuses by exception, permissive
        // records and returns, and an ENABLED control records NOTHING in either mode
        // (a marker per ordinary click would drown the sweep's own signal).
        var recorded = new List<string>();
        var permissive = new ActuationGate(strict: false) { Sink = recorded.Add };
        permissive.Check("type", "confirm-input", 0, enabled: true);
        Check(recorded.Count == 0, "an enabled control leaves NO marker");
        // try/catch, not a bare call: a regression that makes permissive REFUSE
        // (dropping `strict` from the verdict is the obvious one, and it is exactly
        // what would silently red an enumerating sweep) must arrive here as a named
        // failure, not as an unhandled exception that aborts the transcript before
        // the remaining checks run.
        try { permissive.Check("type", "confirm-input", 0, enabled: false); }
        catch (DisabledActuationException ex)
        {
            Check(false, $"permissive mode must MARK and return, never refuse: {ex.Message}");
        }
        Check(recorded.Count == 1 && recorded[0].Contains("type id=confirm-input"),
            $"a permissive violation leaves exactly one marker naming route and element (got {recorded.Count}: {string.Join(" | ", recorded)})");

        var strictRecorded = new List<string>();
        var strict = new ActuationGate(strict: true) { Sink = strictRecorded.Add };
        var threw = false;
        try { strict.Check("click", "confirm-button", 1, enabled: false); }
        catch (DisabledActuationException ex)
        {
            threw = true;
            Check(ex.Id == "confirm-button" && ex.Index == 1 && ex.Route == "click",
                "the refusal carries route/id/index so Program can answer the shared 409 body");
        }
        Check(threw, "strict mode REFUSES a disabled control by exception");
        // The marker is written in BOTH modes: a strict run's log is still the record
        // of what it refused.
        Check(strictRecorded.Count == 1, "a strict refusal is marked too, not only thrown");

        // 7. The OTHER select 409: an enabled picker asked for an option it never
        // painted. `http_bridge.py::select` raises `SelectOptionNotOffered` for a 409
        // whose text lacks "element is disabled", so this wording must never carry
        // it, and it must name the value, the picker and what was painted.
        var notOffered = new SelectOptionNotOfferedException(
            "compose-gate-tier-select", 0, "tier-sold", "Everyone, Followers");
        Check(!notOffered.Message.Contains("element is disabled"),
            $"the not-offered refusal never reads as the disabled one (got: {notOffered.Message})");
        Check(notOffered.Message.Contains("\"tier-sold\"")
              && notOffered.Message.Contains("\"compose-gate-tier-select\"[0]")
              && notOffered.Message.Contains("[Everyone, Followers]"),
            $"the not-offered refusal names value, picker and painted options (got: {notOffered.Message})");
        Check(notOffered.Id == "compose-gate-tier-select" && notOffered.Index == 0,
            "the not-offered refusal carries id/index so Program can answer the shared 409 body");

        Console.WriteLine(failures == 0
            ? "[self-test] PASS — the actuation gate's verdict, flags, marker and both select refusals are pinned."
            : $"[self-test] {failures} FAILURE(S)");
        return failures;
    }

    /// <summary>
    /// Pins <see cref="PortPicker"/> — the retry policy that replaced ``Program``'s
    /// single ``new Random().Next(18000, 19000)`` + one-shot ``Start()``
    /// (`e2e-conventions.md` § convention 10, "any harness child that picks its own
    /// listening port…") — with an injected draw and an injected bind attempt: no
    /// real listener, no port, no app.
    ///
    /// <para><b>Why two conflict codes, not one.</b> The measured production
    /// collision (two FlaUI bridges racing the same 18000-19000 draw) throws
    /// <c>HttpListenerException(183)</c> — ERROR_ALREADY_EXISTS, another
    /// <c>HttpListener</c>/http.sys registration already owns the port. A port a
    /// PLAIN socket owns instead (the shape <c>web-bridge/server.py</c> would
    /// collide in, drawing from the identical range) throws <c>32</c> instead —
    /// ERROR_SHARING_VIOLATION — confirmed experimentally 2026-09-22. Hard-coding
    /// 183 alone would silently stop retrying on the 32 case.</para>
    ///
    /// <para>Run: <c>FauiBridge.exe --self-test-port-retry</c>; exit code = failures.</para>
    /// </summary>
    public static int PortRetryChecks()
    {
        var failures = 0;
        void Check(bool condition, string description)
        {
            Console.WriteLine(condition
                ? $"[self-test] PASS: {description}"
                : $"[self-test] FAIL: {description}");
            if (!condition) failures++;
        }

        // 1. The predicate itself: both measured conflict codes are retried…
        Check(PortPicker.IsBindConflict(183), "183 (ERROR_ALREADY_EXISTS, another HttpListener) is a bind conflict");
        Check(PortPicker.IsBindConflict(32), "32 (ERROR_SHARING_VIOLATION, a plain socket) is a bind conflict");
        // …and an unrelated code is NOT — a conflict predicate that matches everything
        // would swallow a real bug (e.g. access denied) as a harmless retry.
        Check(!PortPicker.IsBindConflict(5), "5 (ERROR_ACCESS_DENIED) is NOT a bind conflict");

        // 2. [taken, free] against the measured 183 shape: two draws, lands on the
        // free port, never retries the SAME port twice.
        {
            var draws = new Queue<int>(new[] { 18001, 18002 });
            var attempted = new List<int>();
            var (port, result) = PortPicker.BindFreshPort(
                () => draws.Dequeue(),
                p =>
                {
                    attempted.Add(p);
                    if (p == 18001) throw new HttpListenerException(183);
                    return "bound";
                });
            Check(port == 18002 && result == "bound",
                $"a taken-then-free draw lands on the free port (got port={port}, result={result})");
            Check(attempted.Count == 2 && attempted[0] == 18001 && attempted[1] == 18002,
                $"the retry draws a FRESH port, never re-attempting the taken one (got [{string.Join(",", attempted)}])");
        }

        // 3. Same shape, the 32 (plain-socket) code — proves the family is retried,
        // not only the one code the production incident measured.
        {
            var draws = new Queue<int>(new[] { 18003, 18004 });
            var (port, result) = PortPicker.BindFreshPort(
                () => draws.Dequeue(),
                p => p == 18003 ? throw new HttpListenerException(32) : "bound");
            Check(port == 18004 && result == "bound",
                $"a 32 (sharing-violation) conflict is retried onto the next free port (got port={port})");
        }

        // 4. A NON-conflict exception must propagate, never be swallowed as a retry —
        // this is the "let any other exception propagate" half of the fix.
        {
            var threw = false;
            try
            {
                PortPicker.BindFreshPort<object>(
                    () => 18005,
                    _ => throw new HttpListenerException(5));
            }
            catch (HttpListenerException ex) when (ex.NativeErrorCode == 5)
            {
                threw = true;
            }
            Check(threw, "a non-conflict HttpListenerException (5, access denied) propagates instead of being retried");
        }

        // 5. Exhausting the bound raises rather than retrying forever — every draw
        // conflicts, so this proves the retry is BOUNDED (tens of attempts, not
        // unbounded), per the row's own fix shape.
        {
            var attempts = 0;
            var threw = false;
            try
            {
                PortPicker.BindFreshPort<object>(
                    () => { attempts++; return 19000 + attempts; },
                    _ => throw new HttpListenerException(183));
            }
            catch (InvalidOperationException)
            {
                threw = true;
            }
            Check(threw, "exhausting every attempt raises rather than retrying forever");
            Check(attempts == PortPicker.MaxAttempts,
                $"exactly MaxAttempts draws are made, no more (got {attempts}, MaxAttempts={PortPicker.MaxAttempts})");
        }

        Console.WriteLine(failures == 0
            ? "[self-test] PASS — the port picker retries the bind-conflict family onto a fresh port, bounded."
            : $"[self-test] {failures} FAILURE(S)");
        return failures;
    }

    /// <summary>
    /// Pins <see cref="PackedAttr"/> — the reading of a HelpText that carries SEVERAL
    /// attributes (<c>state=playing;position=1.250;source=…</c>, the
    /// <c>video-thumbnail</c> player's three) as one value per key, so a test reads
    /// <c>get_attr("video-thumbnail", "position")</c> on windows exactly as it does on
    /// every other app. UIA gives an element one application-supplied string; until this
    /// existed every attribute read answered that whole string, which no shared test could
    /// compare against.
    ///
    /// <para>The load-bearing checks are the NEGATIVE ones: a HelpText that is not a pack
    /// must keep answering verbatim, because dozens of elements publish a single bare value
    /// there (<c>painted</c>, <c>true</c>, a backup id that may end in base64 <c>=</c>) and
    /// a reader that mistook one for a pack would quietly answer <c>null</c> for it.</para>
    ///
    /// <para>Run: <c>FauiBridge.exe --self-test-packed-attr</c>; exit code = failures.</para>
    /// </summary>
    public static int PackedAttrChecks()
    {
        var failures = 0;
        void Check(bool condition, string description)
        {
            Console.WriteLine(condition
                ? $"[self-test] PASS: {description}"
                : $"[self-test] FAIL: {description}");
            if (!condition) failures++;
        }

        const string Player = "state=playing;position=1.250;source=http://127.0.0.1:3000/api/v1/blob/ab12";

        // 1. Each key of a pack answers its own value.
        Check(PackedAttr.TryRead(Player, "state", out var state) && state == "playing",
            $"state reads its own value off the pack (got {state ?? "null"})");
        Check(PackedAttr.TryRead(Player, "position", out var position) && position == "1.250",
            $"position reads its own value off the pack (got {position ?? "null"})");
        Check(PackedAttr.TryRead(Player, "source", out var source)
                && source == "http://127.0.0.1:3000/api/v1/blob/ab12",
            $"source keeps every character after its first '=' (got {source ?? "null"})");

        // 2. A key the pack does not name is absent — never another key's value.
        Check(PackedAttr.TryRead(Player, "selected", out var missing) && missing is null,
            $"a key the pack lacks answers null (got {missing ?? "null"})");

        // 3. An empty value is a value.
        Check(PackedAttr.TryRead("state=idle;source=", "source", out var empty) && empty == "",
            $"an empty value reads as \"\" (got {empty ?? "null"})");

        // 4. The packer's escapes come back out.
        Check(PackedAttr.TryRead("a=x%3By;b=50%25", "a", out var semi) && semi == "x;y",
            $"%3B unescapes to ';' (got {semi ?? "null"})");
        Check(PackedAttr.TryRead("a=x%3By;b=50%25", "b", out var pct) && pct == "50%",
            $"%25 unescapes to '%' (got {pct ?? "null"})");

        // 5. A key may be snake case: `oracle_ops` is the attribute's name on every app, and
        //    one such key must not turn the whole pack back into a bare value.
        const string Roster = "revoked=;status=connected;oracle_ops=3";
        Check(PackedAttr.TryRead(Roster, "oracle_ops", out var snake) && snake == "3",
            $"a snake-case key reads its own value (got {snake ?? "null"})");
        Check(PackedAttr.TryRead(Roster, "status", out var beside) && beside == "connected",
            $"a snake-case key leaves its neighbours readable (got {beside ?? "null"})");

        // 6. Not a pack → the caller keeps the HelpText verbatim.
        Check(!PackedAttr.TryRead("painted", "state", out _), "a bare value is not a pack");
        Check(!PackedAttr.TryRead("true", "checked", out _), "a bare true is not a pack");
        Check(!PackedAttr.TryRead("YWJjZA==", "state", out _),
            "a base64 id ending in '=' is not a pack");
        Check(!PackedAttr.TryRead("state=idle", "state", out _),
            "a single pair is not a pack (one value needs no packing)");
        Check(!PackedAttr.TryRead("Waiting for x=1; y=2", "state", out _),
            "free text holding '=' and ';' is not a pack");
        Check(!PackedAttr.TryRead("state=idle;", "state", out _),
            "a trailing ';' is not a pack (the packer never writes one)");
        Check(!PackedAttr.TryRead("State=idle;position=0", "state", out _),
            "an upper-case key is not a pack key");
        Check(!PackedAttr.TryRead("_state=idle;position=0", "state", out _),
            "a key starts with a letter");
        Check(!PackedAttr.TryRead("", "state", out _) && !PackedAttr.TryRead(null, "state", out _),
            "no HelpText is not a pack");

        Console.WriteLine(failures == 0
            ? "[self-test] PASS — a packed HelpText reads one value per key, and nothing else is mistaken for one."
            : $"[self-test] {failures} FAILURE(S)");
        return failures;
    }
}
