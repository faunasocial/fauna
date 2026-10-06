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

    /// <summary>
    /// Pins <see cref="ScrollPolicy"/> — the arithmetic <c>Actions.cs::ScrollIntoView</c>'s
    /// sweep and <c>Actions.cs::Scroll</c>'s blind-fallback container choice run on — with
    /// no real UIA element, window, or app.
    ///
    /// <para>Five checks, run against the SAME constants production uses
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

        Console.WriteLine(failures == 0
            ? "[self-test] PASS — sweep step policy and container choice are gap-free and cheap."
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

        Console.WriteLine(failures == 0
            ? "[self-test] PASS — the actuation gate's verdict, flags, marker and refusal are pinned."
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
}
