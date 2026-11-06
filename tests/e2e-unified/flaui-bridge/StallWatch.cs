using System.Diagnostics;
using System.Net;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;

namespace FauiBridge;

/// <summary>What the bridge read of the app across one stalled op: the inputs of
/// <see cref="StallVerdict.Decide"/>, kept apart from the reading so the verdict is
/// assertable with no app and no stall (<c>--self-test-stall-verdict</c>).</summary>
/// <param name="StallSeconds">How long the op had been in flight.</param>
/// <param name="CpuStartMs">The app's user+kernel CPU when the op took the gate.</param>
/// <param name="CpuNowMs">The same, read now.</param>
/// <param name="UiTid">The app's UI thread (the owner of its main window); 0 unread.</param>
/// <param name="UiState">That thread's scheduler state, as
/// <see cref="System.Diagnostics.ThreadState"/>'s numbers (7 is the kernel's
/// DeferredReady, which .NET spells Unknown); null unread.</param>
/// <param name="UiWaitReason">Its wait reason, when it is Waiting.</param>
/// <param name="NotResponding">Whether Windows reads its window as Not Responding
/// (<c>IsHungAppWindow</c>: no message pumped for 5 s); null unread.</param>
/// <param name="CyclesStart">The app's CPU cycles (<c>QueryProcessCycleTime</c>) when
/// the op took the gate. What decides "the app ran": a UIA timeout comes ~2 s into a
/// stall, and over 2 s the millisecond figure, which the OS accrues a clock tick
/// (~15.6 ms) at a time, can read zero for a process that did run; the cycle count
/// is exact.</param>
/// <param name="CyclesNow">The same, read now.</param>
internal readonly record struct StallReadings(
    double StallSeconds,
    long? CpuStartMs,
    long? CpuNowMs,
    int UiTid,
    int? UiState,
    string? UiWaitReason,
    bool? NotResponding,
    long? CyclesStart = null,
    long? CyclesNow = null);

internal enum StallKind
{
    /// <summary>The app ran and its UI thread still did not answer: our code.</summary>
    AppSide,
    /// <summary>The UI thread waited for a core, or nothing in the app ran.</summary>
    Box,
    /// <summary>A reading the verdict needs could not be taken.</summary>
    Unmeasured,
}

/// <summary>
/// The windows app's UI-thread stall verdict — <c>e2e-conventions.md</c>
/// convention 11's split, judged from OUTSIDE the app.
///
/// <para>tui and linux decide it inside the app, from two heartbeats their agent
/// beats (<c>fauna_e2e_agent::thread_verdict</c>). The windows app is a C# process
/// that beats neither, so the bridge — which already reads the app's CPU and its
/// threads' scheduler states for the launch-timeout rider — decides it from those:
/// the app's CPU moved across the stall (something in it ran, so the box did not
/// starve it) AND its UI thread is not runnable-and-waiting-for-a-core. Only then is
/// the stall the app's own, and only then is its UI thread's stack taken: a stack
/// attached to a starved-box stall would send a reader hunting a blocking call that
/// does not exist. Never on a bare time bound — the time bound only says the op is
/// slow enough to look at; this decides whose it is.</para>
/// </summary>
internal static class StallVerdict
{
    /// <summary>The line that opens every report, so a reader (and a test) finds it.</summary>
    public const string Header = "UI-THREAD STALL";

    /// <summary>Runnable and not running: Ready, Standby, or DeferredReady.</summary>
    public static bool WaitingForACore(int state) => state is 1 or 3 or 7;

    public static string StateWord(int state, string? waitReason) => state switch
    {
        0 => "Initialized",
        1 => "Ready (runnable, given no core)",
        2 => "Running",
        3 => "Standby (chosen to run next)",
        4 => "Terminated",
        5 => waitReason is null ? "Waiting" : $"Waiting ({waitReason})",
        6 => "Transition",
        7 => "DeferredReady (runnable, given no core)",
        _ => $"state {state}",
    };

    public static (StallKind Kind, string Clause) Decide(StallReadings r)
    {
        var responding = r.NotResponding switch
        {
            true => "; Windows reads its window as Not Responding (no message pumped for 5 s)",
            false => "; Windows still reads its window as responding",
            null => "",
        };
        if (r.CpuStartMs is not long cpu0 || r.CpuNowMs is not long cpu1
            || r.UiTid == 0 || r.UiState is not int state)
        {
            return (StallKind.Unmeasured,
                $": the app's CPU or its UI thread's state could not be read (CPU "
                + $"{r.CpuStartMs?.ToString() ?? "?"}→{r.CpuNowMs?.ToString() ?? "?"} ms, UI thread "
                + $"{(r.UiTid == 0 ? "not found" : r.UiTid.ToString())}, state "
                + $"{(r.UiState is int s ? StateWord(s, r.UiWaitReason) : "unread")}), so whether "
                + $"the app or the box holds it is UNMEASURED; no stack taken{responding}");
        }
        var delta = cpu1 - cpu0;
        // Cycles decide when both were read (exact); the millisecond figure otherwise.
        long? cycles = r.CyclesStart is long c0 && r.CyclesNow is long c1 ? c1 - c0 : null;
        var ran = cycles is long cy ? cy > 0 : delta > 0;
        var cpu = cycles is long n
            ? $"+{delta} ms of CPU ({n / 1e6:N1} M cycles)"
            : $"+{delta} ms of CPU";
        var word = StateWord(state, r.UiWaitReason);
        if (WaitingForACore(state))
        {
            return (StallKind.Box,
                $": its UI thread (tid {r.UiTid}) is {word}, so the box starved it, and this is "
                + $"not an app finding (no stack taken; the app ran {cpu} across the "
                + $"stall){responding}");
        }
        if (!ran)
        {
            return (StallKind.Box,
                $": the app ran no CPU across the {r.StallSeconds:N0}s stall, so nothing in it ran "
                + $"— the box starved it, and this is not an app finding (no stack taken; its UI "
                + $"thread, tid {r.UiTid}, is {word}){responding}");
        }
        return (StallKind.AppSide,
            $": the app ran {cpu} across the {r.StallSeconds:N1}s stall and its UI "
            + $"thread (tid {r.UiTid}) is {word}, not waiting for a core — the process was on a "
            + "core and the UI thread still did not serve the op, so this is synchronous work ON "
            + $"THE UI THREAD, in the app (e2e-conventions.md convention 11), not the box{responding}");
    }

    /// <summary>The faked reading <c>FAUNA_E2E_BRIDGE_STALL_READINGS=starved</c> puts
    /// in place of the real one: a UI thread waiting for a core in an app that ran no
    /// CPU — the starved-box reading, so a test can prove it gets no stack.</summary>
    public static StallReadings Starved(StallReadings real) => real with
    {
        CpuNowMs = real.CpuStartMs ?? 0,
        CpuStartMs = real.CpuStartMs ?? 0,
        CyclesNow = real.CyclesStart ?? 0,
        CyclesStart = real.CyclesStart ?? 0,
        UiTid = real.UiTid == 0 ? -1 : real.UiTid,
        UiState = 1,
        UiWaitReason = null,
    };
}

/// <summary>
/// The one rule for a catch that answers an app read with "absent" — a count of 0,
/// <c>false</c>, <c>null</c>, an empty column, a skipped registry row, a dropped find
/// hit: such a catch answers for the ELEMENT, never for the app
/// (<c>e2e-conventions.md</c> convention 11). A crashed app
/// (<see cref="AppExitedException"/>) and a held UI thread (a UIA timeout,
/// <see cref="StallWatch.LooksLikeUiaTimeout"/>) are let through, so the op fails and
/// its reply carries the dead-peer report or the stall verdict.
///
/// <para>Why the timeout: UIA gives up on a held UI thread after ~2 s, and a catch-all
/// turned that into "absent". Measured 2026-11-01 against a UI thread parked for 60 s:
/// <c>/element/count</c> answered 0 in ~2 s for 13 of 15 ops, so
/// <c>drivers/windows.py::is_absent</c> (<c>count == 0</c>, the read a test makes while
/// it waits for something to disappear) answered True on a wedged app. Letting it
/// through cost nothing measurable: an ordinary heavy-polling run (folder read path and
/// message delete: 35 counts, 22 visibility reads, 8 scroll-into-views) swallowed no
/// UIA timeout at all, and every gesture route already failed on one.</para>
///
/// <para>Used as an exception FILTER — <c>catch (Exception ex) when
/// (UiaSwallow.MayAnswerAbsent(ex))</c> — so a let-through exception is never caught
/// and rethrown: it leaves the frame untouched, and the request thread's own filter
/// (<c>Program.HandleRequest</c>) judges the stall while the thread is still held.</para>
///
/// <para>One catch applies it only in part, on purpose: <c>ElementFinder.SearchPopups</c>
/// walks every top-level window on the desktop, other processes' included, and a hung
/// window of another app must never fail an op on ours — so only its calls into OUR
/// app's windows obey this rule.</para>
/// </summary>
internal static class UiaSwallow
{
    public static bool MayAnswerAbsent(Exception ex) =>
        ex is not AppExitedException && !StallWatch.LooksLikeUiaTimeout(ex);
}

/// <summary>
/// Re-runs a UIA read that gave up with a transient UIA timeout
/// (<see cref="StallWatch.LooksLikeUiaTimeout"/>: the element-read
/// <c>TimeoutException</c>, the 0x80131505 HRESULT, the main-window re-resolve's
/// ERROR_TIMEOUT) — the other half of <see cref="UiaSwallow"/>. A UI thread busy for
/// 2–4 s re-rendering a list makes UIA give up after its ~2 s connection timeout, and
/// that is app latency, not a dead app (convention 14): a read within its budget waits
/// it out.
///
/// <para>Never turns the timeout into an answer. When the budget is spent the
/// original exception escapes UNCHANGED, through an exception FILTER (no catch and
/// rethrow), so <c>Program.HandleRequest</c> still judges the stall while the thread is
/// held and the reply carries the verdict (convention 11). Every absorbed timeout is
/// traced with its elapsed time, so the re-render stays on record. Any other failure
/// (not found, a refused call, a dead app) is not retried here.</para>
/// </summary>
internal static class UiaRetry
{
    /// <summary>Three UIA timeouts' worth (~2 s each) for a read that carries no
    /// deadline of its own.</summary>
    public static readonly TimeSpan DefaultBudget = TimeSpan.FromSeconds(6);

    public static T Run<T>(string what, Func<T> op, TimeSpan? budget = null,
        Action<string>? trace = null, int pauseMs = 100)
    {
        var started = DateTime.UtcNow;
        var deadline = started + (budget ?? DefaultBudget);
        var absorbed = 0;
        while (true)
        {
            try
            {
                return op();
            }
            catch (Exception ex) when (StallWatch.LooksLikeUiaTimeout(ex)
                                       && DateTime.UtcNow < deadline
                                       && Absorb(what, ex, ++absorbed, started, trace))
            {
                Thread.Sleep(pauseMs);
            }
        }
    }

    private static bool Absorb(string what, Exception ex, int n, DateTime started, Action<string>? trace)
    {
        try
        {
            trace?.Invoke(
                $"UIA timeout absorbed in {what} (#{n}, {(DateTime.UtcNow - started).TotalMilliseconds:F0} ms in): "
                + $"{ex.GetType().Name}: {ex.Message}");
        }
        catch { /* diagnostics never break a read */ }
        return true;
    }
}

/// <summary>
/// Watches the one app op in flight (the UIA gate serializes them) and judges a
/// stall while it is still happening — the windows app's half of convention 11's
/// stall stack.
///
/// <para>Two moments, because a held UI thread fails an op in two shapes. The usual
/// one is FAST: UIA gives up on an unresponsive provider after ~2 s
/// (<see cref="LooksLikeUiaTimeout"/>), and the request thread judges the stall the
/// moment that timeout escapes its route — while the thread is still held, so the
/// stack shows where it sits, not where it went next. The other is SLOW: an op whose
/// every UIA call is answered, but slowly, stays in flight; a request thread inside a
/// UIA call can judge nothing (one <c>/element/visible</c> held the gate for ten
/// minutes), so a dedicated thread (never the pool, which a wedged run starves first:
/// <c>Program.StartMuteBridgeHeartbeat</c>) examines it at
/// <see cref="ExamineAfterSeconds"/> and, if it is still in flight at
/// <see cref="ReplyBudgetSeconds"/>, answers it with a 504 carrying the verdict —
/// before the driver's own 120 s budget expires into a bare socket timeout.</para>
///
/// <para>A read route that answers "absent" lets the timeout through too
/// (<see cref="UiaSwallow"/>), so a held UI thread never reads as an absent
/// element.</para>
/// </summary>
internal static class StallWatch
{
    /// <summary>When an op still in flight is examined. The same 25 s the Rust
    /// agents' <c>AGENT_REPLY_BUDGET</c> gives an op before judging it, so one stall
    /// reads alike on every app.</summary>
    public const double ExamineAfterSeconds = 25.0;

    /// <summary>When an op still in flight is answered with the verdict: under the
    /// driver's <c>BRIDGE_RPC_TIMEOUT_S</c> (120 s,
    /// <c>drivers/http_bridge.py</c>) by enough to deliver it, so no op that the driver
    /// would still have waited for is cut short by more than that margin. Mirrored as
    /// <c>drivers/windows.py::BRIDGE_STALL_REPLY_BUDGET_S</c>.</summary>
    public const double ReplyBudgetSeconds = 100.0;

    /// <summary>How long the capture helper gets: loading a debug PDB costs seconds.</summary>
    const int CaptureBudgetMs = 60_000;

    /// <summary>The session env naming the capture helper's path
    /// (<c>fauna-e2e-stall-capture</c>, the e2e helper-binary registry's entry).</summary>
    public const string CaptureBinEnv = "FAUNA_E2E_STALL_CAPTURE_BIN";

    /// <summary>The session env that fakes the starved-box reading (value
    /// <c>starved</c>): the test that proves a starved stall gets no stack.</summary>
    public const string FakeReadingsEnv = "FAUNA_E2E_BRIDGE_STALL_READINGS";

    /// <summary>The session env that SHORTENS the reply budget (seconds), for the
    /// forced-stall test, so each of its cases spends ~40 s rather than 100. Never
    /// longer than <see cref="ReplyBudgetSeconds"/>, never shorter than the
    /// examination plus a margin for the capture to start.</summary>
    public const string ReplyBudgetEnv = "FAUNA_E2E_BRIDGE_STALL_REPLY_S";

    /// <summary>The reply budget for this session's ops.</summary>
    public static double ReplyBudgetFor(SessionManager session) =>
        double.TryParse(session.LaunchEnv(ReplyBudgetEnv),
            System.Globalization.NumberStyles.Float,
            System.Globalization.CultureInfo.InvariantCulture, out var asked)
            ? Math.Clamp(asked, ExamineAfterSeconds + 5, ReplyBudgetSeconds)
            : ReplyBudgetSeconds;

    sealed class Inflight
    {
        public required string Route { get; init; }
        public required HttpListenerResponse Response { get; init; }
        public required SessionManager Session { get; init; }
        public required long? CpuStartMs { get; init; }
        public required long? CyclesStart { get; init; }
        /// <summary>Since the request ARRIVED, gate queueing included — the clock the
        /// driver's own budget runs on, so the reply budget is read off it.</summary>
        public required Stopwatch Arrived { get; init; }
        /// <summary>Since the op took the gate — the stall the readings span.</summary>
        public Stopwatch Held { get; } = Stopwatch.StartNew();
        /// <summary>0 until examined; the examination runs once.</summary>
        public int Examined;
        /// <summary>Set when the examination has finished and <see cref="Report"/> is final.</summary>
        public ManualResetEventSlim Done { get; } = new(false);
        public string? Report;
        /// <summary>The watcher answered the op: its own reply is dropped.</summary>
        public bool Replied;
    }

    /// <summary>How long an op that ends mid-examination waits for its report: the
    /// capture's own budget plus the reading.</summary>
    static readonly TimeSpan ReportWait = TimeSpan.FromMilliseconds(CaptureBudgetMs + 15_000);

    static Inflight? _current;
    static readonly ConditionalWeakTable<HttpListenerResponse, string> _reports = new();

    /// <summary>Writes a reply (Program.WriteJson); set at startup.</summary>
    public static Func<HttpListenerResponse, int, object, Task>? Reply;
    /// <summary>Narrates to the diag file (Program.DiagFile); set at startup.</summary>
    public static Action<string>? Trace;

    /// <summary>Does this route ask the app's UI thread something through UIA?</summary>
    public static bool IsAppRoute(string path) =>
        path.StartsWith("/element/", StringComparison.Ordinal)
        || path.StartsWith("/window/", StringComparison.Ordinal)
        || path is "/registry" or "/debug/tree" or "/scroll" or "/dismiss-dialogs"
            or "/clipboard/text" or "/screenshot";

    /// <summary>The op holding the gate starts; called by its request thread.
    /// <paramref name="arrived"/> has run since the request was accepted.</summary>
    public static void Begin(
        string route, HttpListenerResponse response, SessionManager session, Stopwatch arrived)
    {
        Volatile.Write(ref _current, new Inflight
        {
            Route = route,
            Response = response,
            Session = session,
            CpuStartMs = session.AppCpuMs(),
            CyclesStart = session.AppCycles(),
            Arrived = arrived,
        });
    }

    /// <summary>The op ends: keep its report for the failure reply that follows (if
    /// any), and narrate one that has no failure to ride on.</summary>
    public static void End(bool succeeded)
    {
        var cur = Interlocked.Exchange(ref _current, null);
        if (cur is null) return;
        // An op that failed while the watcher was still examining it must not go out
        // without the report the examination is about to finish.
        if (!succeeded && Volatile.Read(ref cur.Examined) == 1) cur.Done.Wait(ReportWait);
        if (cur.Report is not string report) return;
        if (succeeded || cur.Replied)
        {
            Trace?.Invoke($"STALL (op {(cur.Replied ? "answered at the budget" : "completed late")}): "
                + $"{cur.Route}{report}");
            return;
        }
        _reports.AddOrUpdate(cur.Response, report);
    }

    /// <summary>The report of a failed op, once; empty when it was never judged.</summary>
    public static string TakeReport(HttpListenerResponse response)
    {
        if (!_reports.TryGetValue(response, out var report)) return "";
        _reports.Remove(response);
        return report;
    }

    /// <summary>Judge the current op now, if it has not been: the request thread's
    /// path, for a UIA call that gave up (a UIA timeout) before the watcher looked.</summary>
    public static void ExamineNow()
    {
        if (Volatile.Read(ref _current) is not Inflight cur) return;
        Examine(cur);
        cur.Done.Wait(ReportWait);
    }

    /// <summary>A UIA call that gave up on an unresponsive provider: .NET maps its
    /// <c>UIA_E_TIMEOUT</c> (0x80131505, also <c>COR_E_TIMEOUT</c>) to a
    /// <see cref="TimeoutException"/>.</summary>
    ///
    /// <para>Measured 2026-11-01 against a UI thread parked inside <c>fauna_ffi</c>:
    /// UIA gives up on it after ~2 s, not the minutes a held thread lasts, in one of
    /// two shapes — an element read throws <c>TimeoutException: UIA Timeout</c>, and
    /// the main-window re-resolve (<c>UIA3Automation.FromHandle</c>) throws a
    /// <see cref="System.ComponentModel.Win32Exception"/> carrying
    /// <c>ERROR_TIMEOUT</c> (1460, 0x800705B4). So an op on a held UI thread FAILS
    /// FAST, and this timeout — not the watcher's 25 s — is the usual moment a stall
    /// is judged.</para>
    public static bool LooksLikeUiaTimeout(Exception ex)
    {
        for (var e = ex; e is not null; e = e.InnerException)
        {
            if (e is TimeoutException || e.HResult == unchecked((int)0x80131505)) return true;
            if (e is System.ComponentModel.Win32Exception w
                && (w.NativeErrorCode == 1460 || w.HResult == unchecked((int)0x800705B4)))
            {
                return true;
            }
        }
        return false;
    }

    public static void Start()
    {
        var t = new Thread(() =>
        {
            while (true)
            {
                Thread.Sleep(1000);
                try { Tick(); }
                catch (Exception ex) { Trace?.Invoke($"STALL watch tick threw: {ex}"); }
            }
        })
        { IsBackground = true, Name = "bridge-stall-watch" };
        t.Start();
    }

    static void Tick()
    {
        if (Volatile.Read(ref _current) is not Inflight cur) return;
        if (cur.Held.Elapsed.TotalSeconds >= ExamineAfterSeconds) Examine(cur);
        var budget = ReplyBudgetFor(cur.Session);
        if (cur.Arrived.Elapsed.TotalSeconds >= budget && !cur.Replied
            && cur.Report is string report && ReferenceEquals(Volatile.Read(ref _current), cur))
        {
            cur.Replied = true;
            var message =
                $"bridge op {cur.Route} did not complete within {budget:N0}s: the app's "
                + "UI thread has not answered UIA. The op is still in flight inside the app and holds "
                + "the UIA gate until it answers, so later ops queue behind it." + report;
            try
            {
                Reply?.Invoke(cur.Response, 504, new { error = message, status = 504 })
                    .GetAwaiter().GetResult();
            }
            catch (Exception ex)
            {
                Trace?.Invoke($"STALL reply to {cur.Route} failed: {ex.Message}");
            }
        }
    }

    static void Examine(Inflight cur)
    {
        if (Interlocked.Exchange(ref cur.Examined, 1) != 0) return;
        try { ExamineOnce(cur); }
        finally { cur.Done.Set(); }
    }

    static void ExamineOnce(Inflight cur)
    {
        var readings = Read(cur);
        var fake = cur.Session.LaunchEnv(FakeReadingsEnv);
        if (fake == "starved") readings = StallVerdict.Starved(readings);
        var (kind, clause) = StallVerdict.Decide(readings);
        var report = $"\n{StallVerdict.Header} (the windows app, judged from outside by the FlaUI "
            + $"bridge after {readings.StallSeconds:N0}s in {cur.Route}"
            + (fake == "starved" ? $"; readings FAKED by {FakeReadingsEnv}=starved" : "") + ")" + clause;
        if (kind == StallKind.AppSide)
        {
            report += Capture(cur.Session, readings.UiTid);
        }
        cur.Report = report;
        Trace?.Invoke($"STALL examined: {cur.Route}{report}");
    }

    static StallReadings Read(Inflight cur)
    {
        var pid = cur.Session.AppPid;
        var (tid, hwnd) = UiThreadOf(pid);
        int? state = null;
        string? reason = null;
        long? cpuNow = null;
        long? cyclesNow = null;
        // A Process of its own, never the session's: the request thread may be using
        // that one, and a Process is not safe to share across threads.
        try
        {
            using var process = Process.GetProcessById(pid);
            cpuNow = (long)process.TotalProcessorTime.TotalMilliseconds;
            cyclesNow = CyclesOf(process.Handle);
            foreach (ProcessThread thread in process.Threads)
            {
                if (thread.Id != tid) continue;
                state = (int)thread.ThreadState;
                if (state == 5)
                {
                    try { reason = thread.WaitReason.ToString(); } catch { }
                }
                break;
            }
        }
        catch { }
        bool? hung = hwnd == IntPtr.Zero ? null : IsHungAppWindow(hwnd);
        return new StallReadings(
            cur.Held.Elapsed.TotalSeconds, cur.CpuStartMs, cpuNow, (int)tid, state, reason, hung,
            cur.CyclesStart, cyclesNow);
    }

    /// <summary>A process's CPU cycles so far, exact (<c>QueryProcessCycleTime</c>),
    /// or null when the handle does not answer.</summary>
    public static long? CyclesOf(IntPtr process)
    {
        try
        {
            return QueryProcessCycleTime(process, out var cycles) ? (long)cycles : null;
        }
        catch
        {
            return null;
        }
    }

    [DllImport("kernel32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    static extern bool QueryProcessCycleTime(IntPtr process, out ulong cycles);

    /// <summary>
    /// The app's UI thread: the owner of its main window, found by
    /// <c>EnumWindows</c> + <c>GetWindowThreadProcessId</c>. Neither sends the window a
    /// message, so both answer while its thread is held — which a UIA read of the
    /// window handle would not. WinUI's main window class first, visible or not (a
    /// tray-resident launch has no visible window); any visible window of the app
    /// second.
    /// </summary>
    static (uint Tid, IntPtr Hwnd) UiThreadOf(int pid)
    {
        if (pid <= 0) return (0, IntPtr.Zero);
        (uint, IntPtr) winui = (0, IntPtr.Zero), visible = (0, IntPtr.Zero);
        var cls = new System.Text.StringBuilder(256);
        EnumWindows((hwnd, _) =>
        {
            var tid = GetWindowThreadProcessId(hwnd, out var owner);
            if (owner != (uint)pid) return true;
            cls.Clear();
            GetClassName(hwnd, cls, cls.Capacity);
            if (winui.Item1 == 0 && cls.ToString() == "WinUIDesktopWin32WindowClass") winui = (tid, hwnd);
            if (visible.Item1 == 0 && IsWindowVisible(hwnd)) visible = (tid, hwnd);
            return true;
        }, IntPtr.Zero);
        return winui.Item1 != 0 ? winui : visible;
    }

    /// <summary>Run the capture helper on the UI thread and return what it printed —
    /// or why the stack is UNMEASURED. Never throws.</summary>
    static string Capture(SessionManager session, int tid)
    {
        var bin = session.LaunchEnv(CaptureBinEnv);
        if (string.IsNullOrEmpty(bin) || !File.Exists(bin))
        {
            return $"\n  stack UNMEASURED: no stall-capture helper was handed to this bridge "
                + $"({CaptureBinEnv}={(string.IsNullOrEmpty(bin) ? "unset" : bin + ", which does not exist")}; "
                + "the run's collection-time prebuild builds it for every run that launches the windows app)";
        }
        try
        {
            var psi = new ProcessStartInfo(bin)
            {
                UseShellExecute = false,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                CreateNoWindow = true,
            };
            psi.ArgumentList.Add("--pid");
            psi.ArgumentList.Add(session.AppPid.ToString());
            psi.ArgumentList.Add("--tid");
            psi.ArgumentList.Add(tid.ToString());
            using var helper = Process.Start(psi)!;
            // Both pipes drained for the helper's whole life (convention 13).
            var stdout = helper.StandardOutput.ReadToEndAsync();
            var stderr = helper.StandardError.ReadToEndAsync();
            if (!helper.WaitForExit(CaptureBudgetMs))
            {
                try { helper.Kill(entireProcessTree: true); } catch { }
                return $"\n  stack NOT captured: the capture helper outran its {CaptureBudgetMs / 1000}s budget";
            }
            helper.WaitForExit();
            var text = stdout.Result.TrimEnd();
            if (helper.ExitCode != 0)
            {
                return $"\n  stack NOT captured: the capture helper exited {helper.ExitCode}: "
                    + stderr.Result.Trim();
            }
            return "\n" + text.TrimStart('\r', '\n');
        }
        catch (Exception ex)
        {
            return $"\n  stack NOT captured: could not run the capture helper ({ex.GetType().Name}: {ex.Message})";
        }
    }

    delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr lParam);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    static extern bool EnumWindows(EnumWindowsProc callback, IntPtr lParam);

    [DllImport("user32.dll")]
    static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    static extern int GetClassName(IntPtr hwnd, System.Text.StringBuilder name, int max);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    static extern bool IsWindowVisible(IntPtr hwnd);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    static extern bool IsHungAppWindow(IntPtr hwnd);
}
