using System.Diagnostics;
using System.Net;
using System.Text.Json;

namespace FauiBridge;

class Program
{
    static readonly JsonSerializerOptions JsonOpts = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
    };

    static SessionManager? _session;
    static Actions? _actions;
    static List<Dictionary<string, object>> commandQueue = new();
    static Dictionary<string, object> cachedAppState = new();
    static Dictionary<int, Process> _nestProcesses = new();

    // Guards commandQueue/cachedAppState/_sessionEpoch/_fencedStatePushes/
    // _lastFencedEpoch — the small in-memory fields the app-channel fast path
    // (below) and every other handler both touch. Cheap: every critical section
    // is a plain field read/write, never I/O or UIA.
    static readonly object _stateLock = new();

    // Serializes every FlaUI/UIA-touching handler (window attach, element finds,
    // dialog dismissal, ...) — UIA/COM automation was never safe for concurrent
    // access from multiple threads, so this preserves the SAME one-at-a-time
    // guarantee the bridge always had for that work. What changed is
    // that the app's own command-ack channel (/app/commands, /app/state) no
    // longer queues behind it — see HandleRequest.
    static readonly SemaphoreSlim _uiaGate = new(1, 1);

    /// <summary>Where diagnostics go when stderr cannot carry them.
    ///
    /// Every instrument in this file writes to stderr, and stderr is a pipe SHARED
    /// with the app under test (SessionManager.Launch starts it UseShellExecute
    /// =false with no redirect of its own, so it inherits these handles). A pipe
    /// nobody drains fast enough fills, and a full pipe blocks its writer — which
    /// makes the diagnostics unreadable in exactly the scenario that needs them,
    /// and makes their ABSENCE uninterpretable: silence could mean "nothing to
    /// report" or "the report itself is blocked". Two instrumented runs of row 182
    /// produced five 210s stalls and total silence from instruments that should
    /// have fired every 10s, which is what this exists to disambiguate.
    ///
    /// A file share no writer can block on breaks the circularity. Path is printed
    /// on the launch line so a post-mortem can find it, and it deliberately does NOT
    /// live in the session data dir — that is deleted at teardown, and the runs
    /// worth reading are the ones that ended badly.</summary>
    static readonly string DiagLogPath = Path.Combine(
        Path.GetTempPath(), $"fauna-bridge-diag-{Environment.ProcessId}.log");

    /// <summary>Report to the file only. Never throws: a diagnostic that can take
    /// the bridge down is worse than no diagnostic.</summary>
    static void DiagFile(string message)
    {
        try
        {
            File.AppendAllText(
                DiagLogPath,
                $"[bridge] {DateTime.UtcNow:HH:mm:ss.fff} {message}{Environment.NewLine}");
        }
        catch { }
    }

    /// <summary>File first, THEN stderr — order is load-bearing. If stderr is full
    /// this call never returns, so anything written after it would be lost; the
    /// file write has to have already happened. Callers that must keep running
    /// (the heartbeat) use <see cref="DiagFile"/> instead, since a blocked stderr
    /// write would silence every later beat.</summary>
    static void Diag(string message)
    {
        DiagFile(message);
        try { Console.Error.WriteLine($"[bridge] {message}"); } catch { }
    }

    /// <summary>The request currently holding <see cref="_uiaGate"/>, and since when,
    /// so a waiter can name the request that is actually pegged. Deliberately plain
    /// fields read through <see cref="Volatile"/> rather than anything guarded by a
    /// lock: the whole point is to be readable BY A THREAD THAT IS BLOCKED, so this
    /// may never take a lock the holder could be sitting on.</summary>
    /// (UTC ticks rather than a DateTime: Volatile's non-generic overloads cover
    /// long, and its generic one refuses a struct outright.)
    static string _gateHolder = "(idle)";
    static long _gateHeldSinceTicks = DateTime.UtcNow.Ticks;

    /// <summary>How long a request waits for the gate before naming the holder.
    /// Well above normal queueing behind an ordinary UIA call, well under the
    /// driver's 120s HTTP budget so the line lands before the request is
    /// abandoned.</summary>
    const double SlowGateWaitSeconds = 10.0;

    /// <summary>How long a request may hold the gate before it is narrated on
    /// completion. The waiter-side line above is the one that survives a holder
    /// that never returns; this is its counterpart for one that finishes late.</summary>
    const double SlowRequestSeconds = 10.0;

    /// <summary>Requests accepted off the listener, and requests whose handler ran
    /// to completion. The GAP between them is the whole diagnosis: equal means the
    /// bridge is idle and the client's problem is elsewhere; a growing gap means
    /// work is arriving and not finishing.</summary>
    static long _accepted = 0;
    static long _completed = 0;
    static long _lastCompletedTicks = DateTime.UtcNow.Ticks;
    static long _lastAcceptTicks = DateTime.UtcNow.Ticks;

    /// <summary>Announce a mute bridge from a DEDICATED thread every this often.
    ///
    /// A bridge that stops answering currently says nothing at all — five separate
    /// 210s stalls produced not one line of output, which is indistinguishable from
    /// a bridge with nothing to say. The driver can only report which request TIMED
    /// OUT, and on a serialized server that is almost never the request at fault.
    ///
    /// This must not be a timer or a Task: if the thread pool is starved then pool
    /// work items — including the accept loop's own continuation — are precisely
    /// what stops running, so the one instrument that must still speak cannot be
    /// scheduled on it. A plain background Thread is immune, and reporting
    /// ThreadPool depth alongside the gate holder is what tells pool starvation
    /// apart from a genuinely slow UIA call.</summary>
    static readonly TimeSpan HeartbeatPeriod = TimeSpan.FromSeconds(15);

    /// <summary>How long without a completed request before the heartbeat speaks.
    /// An idle bridge between tests is normal and must stay quiet.</summary>
    static readonly TimeSpan HeartbeatSilenceBeforeReporting = TimeSpan.FromSeconds(20);

    static void StartMuteBridgeHeartbeat()
    {
        var t = new Thread(() =>
        {
            while (true)
            {
                Thread.Sleep(HeartbeatPeriod);
                var quietFor = DateTime.UtcNow
                    - new DateTime(Volatile.Read(ref _lastCompletedTicks), DateTimeKind.Utc);
                if (quietFor < HeartbeatSilenceBeforeReporting) continue;
                var accepted = Interlocked.Read(ref _accepted);
                var completed = Interlocked.Read(ref _completed);
                var sinceAccept = DateTime.UtcNow
                    - new DateTime(Volatile.Read(ref _lastAcceptTicks), DateTimeKind.Utc);

                // A frozen ACCEPT LOOP looks identical to an idle bridge from the
                // counters alone — both stop advancing — which is the blind spot
                // that would otherwise let the worst case pass unreported. The
                // idle watchdog resolves it: it exits the process after
                // IdleTimeout without an accept, so still being alive well past
                // that means the watchdog's own Task.Delay continuation never ran
                // either. Two pool work items stalled at once is not an idle
                // bridge; it is a pool that is not running work.
                if (sinceAccept > IdleTimeout + HeartbeatPeriod + HeartbeatPeriod)
                {
                    DiagFile(
                        $"HEARTBEAT: no request ACCEPTED for {sinceAccept.TotalSeconds:N0}s, " +
                        $"yet the {IdleTimeout.TotalSeconds:N0}s idle watchdog has not exited " +
                        "this process — the accept loop and the watchdog are both pool work " +
                        $"items, so neither is running. threads={ThreadPool.ThreadCount} " +
                        $"pending={ThreadPool.PendingWorkItemCount} " +
                        $"accepted={accepted} completed={completed}");
                    continue;
                }

                if (accepted == completed) continue;  // nothing in flight: idle, not stuck
                var holder = Volatile.Read(ref _gateHolder);
                var heldFor = DateTime.UtcNow
                    - new DateTime(Volatile.Read(ref _gateHeldSinceTicks), DateTimeKind.Utc);
                // The app's own liveness is the datum that decides this: a UIA call
                // that outlives the process it queries is a bridge defect (an
                // unbounded wait on a dead peer), while one against a live app is a
                // wedged UI thread in the app. AppStatus is a Process.Refresh plus
                // HasExited — no UIA, no lock — so it cannot itself block here.
                var app = "unknown";
                try
                {
                    var (running, exitCode) = _session?.AppStatus() ?? (false, (int?)null);
                    app = running ? "app RUNNING" : $"app EXITED (exit={exitCode?.ToString() ?? "?"})";
                }
                catch { }

                DiagFile(
                    $"HEARTBEAT: no request has completed for " +
                    $"{quietFor.TotalSeconds:N0}s — accepted={accepted} completed={completed} " +
                    $"(in flight {accepted - completed}), UIA gate held by '{holder}' for " +
                    $"{heldFor.TotalSeconds:N0}s, stuck in {UiaStage.Current}, {app}, " +
                    $"threadpool threads={ThreadPool.ThreadCount} " +
                    $"pending={ThreadPool.PendingWorkItemCount}");
            }
        })
        { IsBackground = true, Name = "bridge-heartbeat" };
        t.Start();
    }

    // Session epoch — incremented on every POST /session. A relaunched app process
    // (driver.recover()) gets a fresh epoch injected into its env
    // (FAUNA_E2E_SESSION_EPOCH); its TestAgent echoes the epoch on /app/commands +
    // /app/state. Under E2E single-instancing is disabled, so a pre-recover() app
    // instance can briefly outlive the relaunch and keep polling this (global)
    // command queue — STEALING the post-restart commands and mounting UI in the dead
    // window while FlaUI inspects the new one. Fencing by
    // epoch makes the stale agent's polls/pushes no-ops, so only the current session's
    // agent drives commands + state.
    static int _sessionEpoch = 0;

    // How many state pushes the fence above has DISCARDED, and the epoch the last
    // one carried. Reported on /session/status so a relaunch that never becomes
    // ready can distinguish "no agent is pushing at all" from "an agent is pushing
    // and being fenced" — see the /app/state POST handler.
    static int _fencedStatePushes = 0;
    static string? _lastFencedEpoch = null;

    // App instances that owned the session's data dir but were NOT the process this
    // bridge started, found and killed by the most recent Quit (SessionManager
    // .ClearDataDirOwners). Kept here rather than on the session because the session
    // object is dropped the moment it is quit, and this outlives it as the evidence
    // that the harness leaked an instance — reported on /session/status so a run can
    // name the leak instead of merely surviving it.
    static IReadOnlyList<string> _lastUntrackedSurvivors = Array.Empty<string>();

    // How many kills have had to sweep a survivor, ever. A COUNTER rather than a
    // flag so the python side can tell "this relaunch leaked" from "some earlier
    // relaunch did" by comparing before and after — otherwise the details below,
    // which are deliberately sticky, would be re-reported at every boundary for the
    // rest of the session and the actual culprit would be unfindable.
    static int _untrackedSweeps = 0;

    /// <summary>Quit a session and keep whatever it found out about untracked
    /// survivors, which is lost once the session object is dropped.</summary>
    static bool QuitAndRecord(SessionManager? session, bool sweepDataDir = true)
    {
        if (session is null) return true;
        var closed = session.Quit(sweepDataDir);
        if (session.LastUntrackedSurvivors.Count > 0)
        {
            _lastUntrackedSurvivors = session.LastUntrackedSurvivors;
            _untrackedSweeps++;
        }
        return closed;
    }

    static readonly TimeSpan IdleTimeout = TimeSpan.FromSeconds(60);
    static DateTime _lastRequestTime = DateTime.UtcNow;

    static async Task Main(string[] args)
    {
        if (args.Length > 0 && args[0] == "--self-test-handle-lifetime")
        {
            Environment.Exit(SelfTest.HandleLifetime());
            return;
        }
        if (args.Length > 0 && args[0] == "--self-test-scroll-policy")
        {
            Environment.Exit(SelfTest.ScrollPolicyChecks());
            return;
        }
        if (args.Length > 0 && args[0] == "--self-test-actuation-gate")
        {
            Environment.Exit(SelfTest.ActuationGateChecks());
            return;
        }
        if (args.Length > 0 && args[0] == "--self-test-port-retry")
        {
            Environment.Exit(SelfTest.PortRetryChecks());
            return;
        }
        // Retry on a bind conflict rather than the old one-shot draw+Start:
        // any two bridges racing the same 18000-19000 draw — or a plain-socket holder
        // sharing the range, like the web bridge — could otherwise die at startup.
        // See PortPicker's own doc comment for the measured collision rate and why
        // both native error codes (183, 32) matter.
        var random = new Random();
        var (port, listener) = PortPicker.BindFreshPort(
            () => random.Next(PortPicker.RangeStart, PortPicker.RangeEnd),
            p =>
            {
                var l = new HttpListener();
                l.Prefixes.Add($"http://127.0.0.1:{p}/");
                l.Start();
                return l;
            });

        Console.WriteLine($"BRIDGE_PORT={port}");
        Console.Out.Flush();
        // Announce the diag file BEFORE anything can wedge, and through Diag so
        // the file's own first line records where it lives — a post-mortem that
        // has the pytest log but not this line still has the path, and one that
        // has neither can glob the temp dir for fauna-bridge-diag-*.log.
        Diag($"diagnostics also appended to {DiagLogPath}");

        // Watchdog: exit if no requests arrive within IdleTimeout.
        // Prevents orphaned bridge processes when pytest crashes or is interrupted.
        _ = Task.Run(async () =>
        {
            while (true)
            {
                await Task.Delay(TimeSpan.FromSeconds(10));
                if (DateTime.UtcNow - _lastRequestTime > IdleTimeout)
                {
                    Console.Error.WriteLine($"[bridge] No requests for {IdleTimeout.TotalSeconds}s, exiting.");
                    Cleanup();
                    Environment.Exit(0);
                }
            }
        });

        StartMuteBridgeHeartbeat();

        // Per-phase action costs go to the diag FILE, never stderr — the same
        // reason DiagFile exists at all: an instrument that can block on a full
        // pipe goes quiet exactly when the run is busiest, which is when it is
        // needed. The gate-hold line names the request; this names the phase
        // inside it.
        Actions.Trace = DiagFile;

        while (true)
        {
            var ctx = await listener.GetContextAsync();
            _lastRequestTime = DateTime.UtcNow;
            Interlocked.Increment(ref _accepted);
            Volatile.Write(ref _lastAcceptTicks, DateTime.UtcNow.Ticks);
            // Dispatched, never awaited here: a slow FlaUI/UIA call
            // (window attach, a desktop dialog scan, an element find) must not
            // stop the listener from accepting the NEXT connection — in
            // particular the app's own TestAgent poll loop, whose command-ack
            // traffic (HandleRequest's app-channel fast path) has to keep
            // flowing while some OTHER request is still working through
            // _uiaGate. Per-request ordering/safety is unaffected: each ctx is
            // only ever touched by its own Task, and HandleRequest itself
            // still serializes all UIA work through _uiaGate exactly as
            // before — only the previously-accidental serialization of the
            // app-channel behind it is gone.
            _ = Task.Run(async () =>
            {
                try
                {
                    await HandleRequest(ctx);
                }
                catch (ElementNotFoundException ex)
                {
                    await WriteJson(ctx.Response, 404, new { error = ex.Message });
                }
                // Convention 11's disabled-actuation refusal: 409, NEVER 404. The
                // element was found; its state is the problem. A 404 sends
                // `drivers/http_bridge.py` into its scroll-retry loop and the test
                // dies on a LookupError reading "not rendered yet" — the opposite
                // diagnosis. Body shape and wording are shared with every other
                // app's agent (`ActuationGate.RefusalMessage`), because
                // `http_bridge.select()` discriminates this 409 from the
                // option-not-offered 409 on the message text alone.
                catch (DisabledActuationException ex)
                {
                    await WriteJson(ctx.Response, 409, new
                    {
                        error = ex.Message,
                        status = 409,
                        id = ex.Id,
                        index = ex.Index,
                    });
                }
                catch (Exception ex)
                {
                    await WriteJson(ctx.Response, 500, new { error = ex.ToString() });
                }
                finally
                {
                    Interlocked.Increment(ref _completed);
                    Volatile.Write(ref _lastCompletedTicks, DateTime.UtcNow.Ticks);
                }
            });
        }
    }

    static void Cleanup()
    {
        // Kill all managed nest processes
        foreach (var kv in _nestProcesses)
        {
            try { kv.Value.Kill(); kv.Value.WaitForExit(3000); } catch { }
            kv.Value.Dispose();
        }
        _nestProcesses.Clear();
        // Kill the app if still running
        _session?.Dispose();
        _session = null;
        _actions = null;
    }

    static async Task HandleRequest(HttpListenerContext ctx)
    {
        var path = ctx.Request.Url?.AbsolutePath ?? "";
        var method = ctx.Request.HttpMethod;

        // App channel fast path: TestAgent's own poll loop lives here
        // (GET/POST /app/commands, GET/POST /app/state) — it must never queue
        // behind a slow FlaUI/UIA call some OTHER request is making, since
        // set_state()/call_command() race this exact traffic against their own
        // 10s ack timeout. Guarded by _stateLock only (cheap, in-memory), never
        // _uiaGate. See HandleAppChannelRequest for the moved case bodies.
        if (path is "/app/commands" or "/app/state")
        {
            await HandleAppChannelRequest(path, method, ctx);
            return;
        }

        // Who is holding the gate, and since when. Every UIA route — /health
        // INCLUDED — queues here, which is why a pegged request makes the whole
        // bridge look mute and why the driver's own verdict can only ever say
        // "pegged, not dead" while naming the request that TIMED OUT rather than
        // the one that actually pegged. Those are almost never the same request.
        // Recording the holder is what lets a waiter name the culprit, and it has
        // to be reported from the WAITER because the holder may never finish.
        var gateWait = System.Diagnostics.Stopwatch.StartNew();
        if (!await _uiaGate.WaitAsync(TimeSpan.FromSeconds(SlowGateWaitSeconds)))
        {
            var holder = Volatile.Read(ref _gateHolder);
            var heldFor = (DateTime.UtcNow
                - new DateTime(Volatile.Read(ref _gateHeldSinceTicks), DateTimeKind.Utc)).TotalSeconds;
            // DiagFile, not Diag: a blocked stderr write here would delay THIS
            // waiter's own progress to the unbounded wait below, right when its
            // own diagnostic is most needed (same hazard as the
            // holder-side narration this call mirrors).
            DiagFile(
                $"SLOW GATE: {method} {path} has waited " +
                $"{gateWait.Elapsed.TotalSeconds:N0}s for the UIA gate, held by " +
                $"'{holder}' for {heldFor:N0}s — the holder is what is pegged, not " +
                "the waiter that eventually reports a timeout");
            await _uiaGate.WaitAsync();
        }
        gateWait.Stop();
        Volatile.Write(ref _gateHolder, $"{method} {path}");
        Volatile.Write(ref _gateHeldSinceTicks, DateTime.UtcNow.Ticks);
        var served = System.Diagnostics.Stopwatch.StartNew();
        try
        {
        switch (path)
        {
            case "/health":
                await WriteJson(ctx.Response, 200, new { ready = true, platform = "windows", version = "1.0.0" });
                break;

            case "/session" when method == "POST":
                var sessionReq = await ReadJson<SessionRequest>(ctx.Request);
                // NEVER launch on top of a survivor. A still-live instance holds the
                // per-account instance lock, so the new process would take the
                // `[launch-collision] … already served by a live instance` branch,
                // show the chooser and never start its TestAgent — wedging every
                // remaining test in the session (SessionManager.Quit owns the story).
                // Refusing loudly here turns that silent, session-ending wedge into
                // one legible failed relaunch.
                if (!QuitAndRecord(_session))
                {
                    await WriteJson(ctx.Response, 409, new
                    {
                        error = "previous app instance survived kill; refusing to launch a "
                              + "second instance against the same data dir",
                    });
                    break;
                }
                _session = null;
                // New session → bump the epoch and drop any stale queued commands /
                // cached state so a pre-recover() agent can't bleed into this session.
                // Locked: the app-channel fast path (HandleAppChannelRequest) reads/
                // writes these same fields concurrently now.
                lock (_stateLock)
                {
                    _sessionEpoch++;
                    commandQueue.Clear();
                    cachedAppState = new();
                }
                // Inject the epoch so the app's TestAgent echoes it on /app/commands +
                // /app/state; a surviving old-instance agent carries the OLD epoch and
                // is fenced out below (GAP 1 fix).
                var sessionEnv = sessionReq.Environment is null
                    ? new Dictionary<string, string>()
                    : new Dictionary<string, string>(sessionReq.Environment);
                sessionEnv["FAUNA_E2E_SESSION_EPOCH"] = _sessionEpoch.ToString();
                _session = new SessionManager();
                _session.Launch(sessionReq.App, sessionReq.Args ?? "", sessionEnv,
                                sessionReq.PackageFamilyName, sessionReq.PackageAppId);
                // The actuation gate is a per-SESSION value read from the launch
                // environment the driver just posted — the same dictionary
                // `SessionManager.Launch` already picks FAUNA_E2E_DATA_DIR and
                // FAUNA_E2E_SESSION_EPOCH out of, and the same one
                // `conftest._apply_actuation_mode_env` writes the two actuation
                // flags into for every other app. Reading THIS process's env
                // instead would have needed a second, windows-only plumbing path
                // that no conftest hook feeds — convention 11's own warning that a
                // flag which never reaches the app makes a sweep measure nothing
                // while reporting it clean.
                //
                // `Diag` as the sink, not `DiagFile`: a violation must reach the
                // driver's captured bridge stderr, which is what lets the probe
                // e2e witness a permissive marker in an ORDINARY run rather than
                // only inside a `--actuation-log` sweep (linux reads its app's
                // stderr for exactly this). Markers are rare by construction, so
                // this does not put the stderr-block hazard on any hot path.
                _actions = new Actions(_session, ActuationGate.FromEnvironment(sessionEnv, Diag));
                await WriteJson(ctx.Response, 201, new { session_id = _sessionEpoch.ToString() });
                break;

            // Liveness of the launched app. `running: false` with no session at all
            // is reported the same way as an exited one — both mean "no instance is
            // serving", which is what the single-instance guard's refusal contract
            // asserts. Never 404s, so a caller can poll it without special-casing.
            case "/session/status" when method == "GET":
            {
                var (running, exitCode) = _session?.AppStatus() ?? (false, (int?)null);
                int statusEpoch; int fencedPushes; string? lastFencedEpoch;
                lock (_stateLock)
                {
                    statusEpoch = _sessionEpoch;
                    fencedPushes = _fencedStatePushes;
                    lastFencedEpoch = _lastFencedEpoch;
                }
                await WriteJson(ctx.Response, 200, new {
                    running,
                    exit_code = exitCode,
                    package_full_name = _session?.AppPackageFullName(),
                    session_epoch = statusEpoch,
                    fenced_state_pushes = fencedPushes,
                    last_fenced_epoch = lastFencedEpoch,
                    untracked_survivors = _lastUntrackedSurvivors,
                    untracked_sweeps = _untrackedSweeps,
                });
                break;
            }

            // Every live app process owning this session's data dir, straight from
            // the OS. The relaunch contract's unstated half is "one app process at a
            // time" (drivers/base.py::live_app_instance_count); nothing could check
            // it before, because a second instance is invisible from inside the app
            // and only shows up as the NEXT launch misbehaving. Reported rather than
            // asserted here so a test names the leak, and so the same route serves
            // the post-mortem when a relaunch fails.
            case "/session/app-instances" when method == "GET":
            {
                var instances = _session?.DataDirOwners()
                    ?? (IReadOnlyList<ProcessScan.AppInstance>)Array.Empty<ProcessScan.AppInstance>();
                await WriteJson(ctx.Response, 200, new {
                    instances = instances
                        .Select(i => new { pid = i.Pid, parent_pid = i.ParentPid, epoch = i.Epoch })
                        .ToList(),
                });
                break;
            }

            // The OS's own view of the app's windows (title + offscreen flag each).
            // The `--autostart` tray-residency case corroborates the app's published
            // activation decision against this: a self-report proves the branch was
            // taken, this proves it had the effect. Never 404s and never throws —
            // "no windows" is a legitimate, asserted-on state there.
            case "/session/windows" when method == "GET":
            {
                var windows = _session?.TopLevelWindows()
                    ?? (IReadOnlyList<(string Title, bool IsOffscreen)>)Array.Empty<(string, bool)>();
                await WriteJson(ctx.Response, 200, new {
                    windows = windows
                        .Select(w => new { title = w.Title, is_offscreen = w.IsOffscreen })
                        .ToList(),
                });
                break;
            }

            // Who owns the desktop's foreground right now, and which gestures this
            // bridge has recorded as taking it (Actions.RecordForegroundTake) — e2e
            // convention 10's windows focus axis. `clear=1` drains the record, which
            // is how the harness attributes takes to the test that caused them.
            // Measured here rather than in pytest because the bridge knows the app's
            // pid; both run in the same interactive session, so the answer is the
            // one the person at this desktop sees. A disconnected session answers
            // foreground_hwnd 0 — "nobody", not "the app".
            case "/session/foreground" when method == "GET":
            {
                var clearTakes = ctx.Request.QueryString["clear"] == "1";
                var fg = Win32.GetForegroundWindow();
                uint fgPid = 0;
                if (fg != IntPtr.Zero) Win32.GetWindowThreadProcessId(fg, out fgPid);
                var appPid = _session?.AppPid ?? -1;
                await WriteJson(ctx.Response, 200, new {
                    foreground_hwnd = (long)fg,
                    foreground_pid = (long)fgPid,
                    app_pid = appPid,
                    app_owns_foreground = appPid > 0 && fgPid == (uint)appPid,
                    takes = Actions.ForegroundTakes(clearTakes),
                });
                break;
            }

            case "/session" when method == "DELETE":
                // Kill all managed nest processes
                foreach (var kv in _nestProcesses)
                {
                    try { kv.Value.Kill(); kv.Value.WaitForExit(5000); } catch { }
                    kv.Value.Dispose();
                }
                _nestProcesses.Clear();
                // Report the kill HONESTLY: an unverified "closed: true" over a
                // surviving instance is what let recover() relaunch on top of it
                // (SessionManager.Quit owns the full mechanism).
                // `?sweep=false`: stop only the tracked app, leaving data-dir peers
                // another bridge tracks (SessionManager.Quit).
                var closed = QuitAndRecord(
                    _session, ctx.Request.QueryString["sweep"] != "false");
                _session = null;
                _actions = null;
                // Drop queued commands / cached state so they can't leak into the next
                // session (the queues are process-global, not per-session). Locked —
                // same reason as the /session POST handler above.
                lock (_stateLock)
                {
                    commandQueue.Clear();
                    cachedAppState = new();
                }
                await WriteJson(ctx.Response, closed ? 200 : 409, new { closed });
                break;

            case "/quit" when method == "POST":
                await WriteJson(ctx.Response, 200, new { quitting = true });
                Cleanup();
                Environment.Exit(0);
                break;

            case "/element/click" when method == "POST":
                var clickReq = await ReadJson<ElementRequest>(ctx.Request);
                RequireActions().Click(clickReq.Id, clickReq.Index, clickReq.Scope);
                await WriteJson(ctx.Response, 200, new { });
                break;

            case "/element/double_click" when method == "POST":
                var dblClickReq = await ReadJson<ElementRequest>(ctx.Request);
                RequireActions().DoubleClick(dblClickReq.Id, dblClickReq.Index, dblClickReq.Scope);
                await WriteJson(ctx.Response, 200, new { });
                break;

            case "/element/type" when method == "POST":
                var typeReq = await ReadJson<TypeRequest>(ctx.Request);
                RequireActions().Type(typeReq.Id, typeReq.Text, typeReq.Index, typeReq.Scope);
                await WriteJson(ctx.Response, 200, new { });
                break;

            // Diagnostic sibling of /element/type — always physical SendInput, with an
            // optional hold that pins the interleaving of two bridges' input sections.
            // Sole consumer: tests/test_flaui_input_lock_windows.py (Actions.TypePhysical).
            case "/element/type_physical" when method == "POST":
                var physReq = await ReadJson<PhysicalTypeRequest>(ctx.Request);
                var physReport = RequireActions().TypePhysical(
                    physReq.Id, physReq.Text, physReq.Index, physReq.Scope, physReq.PreDelayMs);
                await WriteJson(ctx.Response, 200, physReport);
                break;

            case "/element/clear" when method == "POST":
                var clearReq = await ReadJson<ElementRequest>(ctx.Request);
                RequireActions().Clear(clearReq.Id, clearReq.Index, clearReq.Scope);
                await WriteJson(ctx.Response, 200, new { });
                break;

            case "/element/key" when method == "POST":
                var keyReq = await ReadJson<KeyRequest>(ctx.Request);
                RequireActions().PressKey(keyReq.Id, keyReq.Key, keyReq.Index, keyReq.Scope);
                await WriteJson(ctx.Response, 200, new { });
                break;

            case "/element/select" when method == "POST":
                var selectReq = await ReadJson<SelectRequest>(ctx.Request);
                RequireActions().Select(selectReq.Id, selectReq.Value, selectReq.Index, selectReq.Scope);
                await WriteJson(ctx.Response, 200, new { });
                break;

            case "/element/text":
                var textId = ctx.Request.QueryString["id"] ?? "";
                var textIdx = int.TryParse(ctx.Request.QueryString["index"], out var ti) ? ti : 0;
                var textScope = ParseScopeQuery(ctx.Request.QueryString["scope"]);
                var text = RequireActions().GetText(textId, textIdx, textScope);
                await WriteJson(ctx.Response, 200, new { text });
                break;

            case "/element/visible":
                var visId = ctx.Request.QueryString["id"] ?? "";
                var visScope = ParseScopeQuery(ctx.Request.QueryString["scope"]);
                var visible = RequireActions().IsVisible(visId, visScope);
                await WriteJson(ctx.Response, 200, new { visible });
                break;

            case "/element/enabled":
                var enId = ctx.Request.QueryString["id"] ?? "";
                var enIdx = int.TryParse(ctx.Request.QueryString["index"], out var ei) ? ei : 0;
                var enScope = ParseScopeQuery(ctx.Request.QueryString["scope"]);
                var enabled = RequireActions().IsEnabled(enId, enIdx, enScope);
                await WriteJson(ctx.Response, 200, new { enabled });
                break;

            case "/element/attr":
                var atId = ctx.Request.QueryString["id"] ?? "";
                var atName = ctx.Request.QueryString["attr"] ?? "";
                var atIdx = int.TryParse(ctx.Request.QueryString["index"], out var ai) ? ai : 0;
                var atScope = ParseScopeQuery(ctx.Request.QueryString["scope"]);
                var atValue = RequireActions().GetAttr(atId, atName, atIdx, atScope);
                await WriteJson(ctx.Response, 200, new { value = atValue });
                break;

            case "/element/count":
                var cntId = ctx.Request.QueryString["id"] ?? "";
                var cntScope = ParseScopeQuery(ctx.Request.QueryString["scope"]);
                var count = RequireActions().Count(cntId, cntScope);
                await WriteJson(ctx.Response, 200, new { count });
                break;

            // The BULK twins of /element/text and /element/attr: one find over
            // the frame, N property reads, one round trip — see
            // Actions.GetTexts/GetAttrs for why (a per-element loop over a long
            // list is O(N) requests x O(N) tree walks, and every walk is served
            // by the app's UI thread). Both answer an empty array for an id
            // that matches nothing and never 404, which is what lets
            // drivers/http_bridge.py::_bulk_read read a 404 as "bridge too old"
            // and fall back to the slow path instead of failing the test.
            case "/element/texts":
                var textsId = ctx.Request.QueryString["id"] ?? "";
                var textsScope = ParseScopeQuery(ctx.Request.QueryString["scope"]);
                var texts = RequireActions().GetTexts(textsId, textsScope);
                await WriteJson(ctx.Response, 200, new { texts });
                break;

            case "/element/attrs":
                var attrsId = ctx.Request.QueryString["id"] ?? "";
                var attrsName = ctx.Request.QueryString["attr"] ?? "";
                var attrsScope = ParseScopeQuery(ctx.Request.QueryString["scope"]);
                var values = RequireActions().GetAttrs(attrsId, attrsName, attrsScope);
                await WriteJson(ctx.Response, 200, new { values });
                break;

            // The structured twin of /debug/tree — every element the app
            // currently publishes, as records (Actions.RegistrySnapshot's own
            // doc comment is the cross-app contract; drivers/base.py::
            // registry_snapshot, e2e-conventions.md convention 17). Mirrors
            // web-bridge/server.py's /registry route field-for-field.
            case "/registry":
                var elements = RequireActions().RegistrySnapshot();
                await WriteJson(ctx.Response, 200, new { elements });
                break;

            case "/clipboard/text":
                var clipText = RequireActions().GetClipboardText();
                await WriteJson(ctx.Response, 200, new { text = clipText });
                break;

            case "/screenshot" when method == "POST":
                var ssReq = await ReadJson<ScreenshotRequest>(ctx.Request);
                var ssPath = RequireActions().Screenshot(ssReq.Name);
                await WriteJson(ctx.Response, 200, new { path = ssPath });
                break;

            case "/element/scroll-into-view" when method == "POST":
                var sivReq = await ReadJson<ElementRequest>(ctx.Request);
                var sivFound = RequireActions().ScrollIntoView(sivReq.Id, sivReq.Index, sivReq.Scope);
                await WriteJson(ctx.Response, 200, new { found = sivFound });
                break;

            // Targeted scroll to a MEASURED visibility fraction (Actions.
            // ScrollIntoViewFraction) — the precise sibling of
            // /element/scroll-into-view above, whose IsOffscreen-based "found"
            // flips as soon as any sliver is on screen.
            case "/element/scroll-into-view-fraction" when method == "POST":
                var sivfReq = await ReadJson<ScrollFractionRequest>(ctx.Request);
                var sivfFraction = RequireActions().ScrollIntoViewFraction(
                    sivfReq.Id, sivfReq.MinFraction, sivfReq.Index, sivfReq.Scope);
                await WriteJson(ctx.Response, 200, new {
                    found = sivfFraction.HasValue,
                    fraction = sivfFraction ?? 0.0,
                });
                break;

            case "/scroll" when method == "POST":
                var scrollReq = await ReadJson<ScrollRequest>(ctx.Request);
                var dir = scrollReq.Direction ?? "down";
                RequireActions().Scroll(dir);
                await Task.Delay(300);
                await WriteJson(ctx.Response, 200, new { scrolled = dir });
                break;

            case "/dismiss-dialogs" when method == "POST":
                var dismissed = RequireActions().DismissSystemDialogs();
                await WriteJson(ctx.Response, 200, new { dismissed });
                break;

            // Simulate the titlebar close button (Actions.WindowClose) — a real
            // WM_CLOSE, not SessionManager.Quit()'s force-kill. See Actions.cs for
            // why the two must stay separate.
            case "/window/close" when method == "POST":
                RequireActions().WindowClose();
                await WriteJson(ctx.Response, 200, new { });
                break;

            case "/debug/tree":
                // ?anchor=<AutomationId|ClassName> roots the dump; ?raw=1 walks the UIA
                // raw view (see Actions.DumpTree — the realized-vs-pruned discriminator).
                var treeRoot = RequireActions().DumpTree(
                    ctx.Request.QueryString["depth"] ?? "3",
                    ctx.Request.QueryString["anchor"],
                    ctx.Request.QueryString["raw"] == "1");
                await WriteJson(ctx.Response, 200, treeRoot);
                break;

            case "/nest/start" when method == "POST":
                var nestReq = await ReadJson<Dictionary<string, JsonElement>>(ctx.Request);
                var nestPort = nestReq["port"].GetInt32();
                var nestCmd = nestReq["command"].EnumerateArray().Select(e => e.GetString()!).ToList();
                if (_nestProcesses.ContainsKey(nestPort))
                {
                    await WriteJson(ctx.Response, 409, new { error = $"Nest already running on port {nestPort}" });
                    break;
                }
                var nestPsi = new ProcessStartInfo(nestCmd[0])
                {
                    UseShellExecute = false,
                    RedirectStandardOutput = true,
                    RedirectStandardError = true,
                };
                for (int i = 1; i < nestCmd.Count; i++)
                    nestPsi.ArgumentList.Add(nestCmd[i]);
                var nestProc = Process.Start(nestPsi)
                    ?? throw new InvalidOperationException($"Failed to start nest on port {nestPort}");
                // Redirecting a pipe obliges us to READ it. Unread, the ~64 KB OS
                // buffer fills and fauna-nest's next log write blocks forever — the
                // nest then stops serving WS-RPC while still looking alive. Same
                // failure the un-drained bridge pipes caused on the app side (see
                // drivers/windows.py's drain comment for the full mechanism).
                nestProc.OutputDataReceived += static (_, _) => { };
                nestProc.ErrorDataReceived += static (_, _) => { };
                nestProc.BeginOutputReadLine();
                nestProc.BeginErrorReadLine();
                _nestProcesses[nestPort] = nestProc;
                await WriteJson(ctx.Response, 201, new { port = nestPort, pid = nestProc.Id });
                break;

            case "/nest" when method == "DELETE":
                var delPortStr = ctx.Request.QueryString["port"];
                if (delPortStr == null || !int.TryParse(delPortStr, out var delPort))
                {
                    await WriteJson(ctx.Response, 400, new { error = "missing port query parameter" });
                    break;
                }
                if (_nestProcesses.TryGetValue(delPort, out var proc))
                {
                    try { proc.Kill(); proc.WaitForExit(5000); } catch { }
                    proc.Dispose();
                    _nestProcesses.Remove(delPort);
                    await WriteJson(ctx.Response, 200, new { stopped = true, port = delPort });
                }
                else
                {
                    await WriteJson(ctx.Response, 404, new { error = $"No nest on port {delPort}" });
                }
                break;

            case "/nest/list" when method == "GET":
                var nests = _nestProcesses.Select(kv => new { port = kv.Key, pid = kv.Value.Id }).ToArray();
                await WriteJson(ctx.Response, 200, new { nests });
                break;

            default:
                await WriteJson(ctx.Response, 404, new { error = $"Unknown route: {method} {path}" });
                break;
        }
        }
        finally
        {
            served.Stop();
            Volatile.Write(ref _gateHolder, "(idle)");
            // Release BEFORE narrating, and narrate to the file only — never
            // `Diag`/stderr here. `_gateHolder` above already tells any waiter
            // this holder is done; releasing promptly is what makes that true.
            // `Diag`'s `Console.Error.WriteLine` can block INDEFINITELY if the
            // pipe is jammed (`Diag`'s own doc comment: "if stderr is full this
            // call never returns" — see `DiagFile`'s doc comment for why the
            // heartbeat thread already avoids it for the identical reason). A
            // narration sandwiched between the `(idle)` write and `Release()`
            // used to hold the semaphore hostage to that blocking write, so a
            // waiter's own diagnostic ("held by '(idle)'") was actively lying:
            // the gate was still truly held, just not by anyone `_gateHolder`
            // could still name.
            _uiaGate.Release();
            if (served.Elapsed.TotalSeconds >= SlowRequestSeconds)
                DiagFile(
                    $"SLOW REQUEST: {method} {path} held the UIA gate for " +
                    $"{served.Elapsed.TotalSeconds:N0}s" +
                    (gateWait.Elapsed.TotalSeconds >= 1
                        ? $" (after queueing {gateWait.Elapsed.TotalSeconds:N0}s behind another)"
                        : ""));
        }
    }

    /// <summary>
    /// The app-channel fast path split out of <see cref="HandleRequest"/>:
    /// GET/POST /app/commands and GET/POST /app/state, the only traffic TestAgent's
    /// own poll loop makes. Touches nothing but the process-global queue/state
    /// fields, so it is guarded by <see cref="_stateLock"/> alone — deliberately
    /// NOT <see cref="_uiaGate"/>, whose whole point is that this channel must
    /// never wait on it.
    /// </summary>
    static async Task HandleAppChannelRequest(string path, string method, HttpListenerContext ctx)
    {
        switch (path, method)
        {
            case ("/app/commands", "GET"):
                // Fence stale agents (GAP 1): a pre-recover() app instance that
                // outlived the relaunch carries an OLD epoch — serve it nothing so it
                // can't steal this session's commands. No epoch param (legacy/other
                // agents) → serve normally.
                Dictionary<string, object>? cmd = null;
                lock (_stateLock)
                {
                    if (EpochMatches(ctx.Request) && commandQueue.Count > 0)
                    {
                        cmd = commandQueue[0];
                        commandQueue.RemoveAt(0);
                    }
                }
                if (cmd is null) await WriteJson(ctx.Response, 204, new { });
                else await WriteJson(ctx.Response, 200, cmd);
                break;

            case ("/app/state", "GET"):
                Dictionary<string, object> stateSnapshot;
                lock (_stateLock) { stateSnapshot = cachedAppState; }
                if (stateSnapshot.Count == 0) await WriteJson(ctx.Response, 204, new { });
                else await WriteJson(ctx.Response, 200, stateSnapshot);
                break;

            case ("/app/commands", "POST"):
                var cmdBody = await ReadJson<Dictionary<string, object>>(ctx.Request);
                lock (_stateLock) { commandQueue.Add(cmdBody); }
                await WriteJson(ctx.Response, 201, new { queued = true });
                break;

            case ("/app/state", "POST"):
                // Fence stale agents (GAP 1): ignore a pre-recover() instance's state
                // push (old epoch) so it can't overwrite the live session's snapshot
                // with its stale one. Still 200 so the old agent doesn't error-spin.
                var pushedState = await ReadJson<Dictionary<string, object>>(ctx.Request);
                lock (_stateLock)
                {
                    if (EpochMatches(ctx.Request))
                    {
                        // Normalize top-level keys from camelCase to snake_case
                        // so Python always sees snake_case regardless of what the app sends.
                        cachedAppState = NormalizeKeys(pushedState);
                    }
                    else
                    {
                        // COUNT the discard rather than only performing it. A fence that
                        // drops silently is indistinguishable from an agent that never
                        // started, and those two have opposite fixes — "the app is
                        // pushing but nobody is listening" vs "the app never came up".
                        // Row 39 burned four sessions on exactly that ambiguity, so the
                        // tally is surfaced on /session/status for recover()'s failure
                        // report to name.
                        _fencedStatePushes++;
                        _lastFencedEpoch = ctx.Request.QueryString["epoch"];
                    }
                }
                await WriteJson(ctx.Response, 200, new { received = true });
                break;

            default:
                await WriteJson(ctx.Response, 404, new { error = $"Unknown route: {method} {path}" });
                break;
        }
    }

    /// <summary>
    /// Normalize top-level dictionary keys from camelCase to snake_case.
    /// Ensures Python always sees snake_case regardless of what the app sends.
    /// </summary>
    static Dictionary<string, object> NormalizeKeys(Dictionary<string, object> dict)
    {
        var result = new Dictionary<string, object>(dict.Count);
        foreach (var kv in dict)
        {
            var key = CamelToSnake(kv.Key);
            result[key] = kv.Value;
        }
        return result;
    }

    static string CamelToSnake(string name)
    {
        if (name.Contains('_')) return name; // Already snake_case
        var sb = new System.Text.StringBuilder(name.Length + 4);
        for (int i = 0; i < name.Length; i++)
        {
            if (char.IsUpper(name[i]) && i > 0)
            {
                sb.Append('_');
                sb.Append(char.ToLowerInvariant(name[i]));
            }
            else
            {
                sb.Append(char.ToLowerInvariant(name[i]));
            }
        }
        return sb.ToString();
    }

    static Actions RequireActions()
    {
        return _actions ?? throw new InvalidOperationException("No active session. POST /session first.");
    }

    /// <summary>
    /// True if the request's <c>epoch</c> query param matches the current session
    /// epoch — i.e. the requesting app instance belongs to the current session.
    /// A MISSING param returns true (legacy agents / agents launched without an
    /// injected epoch are unaffected); a MISMATCH returns false (a pre-recover()
    /// app instance that outlived its relaunch is fenced out — GAP 1 fix).
    /// </summary>
    static bool EpochMatches(HttpListenerRequest req)
    {
        var raw = req.QueryString["epoch"];
        if (string.IsNullOrEmpty(raw)) return true;
        return int.TryParse(raw, out var e) && e == _sessionEpoch;
    }

    /// <summary>
    /// Parse the URL-encoded JSON <c>scope</c> query parameter (used by GET
    /// element endpoints) into a list of <see cref="ScopeStep"/>. Returns
    /// null when the param is missing or empty so callers fall through to
    /// the unscoped (root) search.
    /// </summary>
    static List<ScopeStep>? ParseScopeQuery(string? raw)
    {
        if (string.IsNullOrEmpty(raw)) return null;
        return JsonSerializer.Deserialize<List<ScopeStep>>(raw, JsonOpts);
    }

    static async Task<T> ReadJson<T>(HttpListenerRequest req)
    {
        using var reader = new StreamReader(req.InputStream);
        var body = await reader.ReadToEndAsync();
        return JsonSerializer.Deserialize<T>(body, JsonOpts)
            ?? throw new InvalidOperationException("Failed to parse request body");
    }

    static async Task WriteJson(HttpListenerResponse resp, int status, object data)
    {
        resp.StatusCode = status;
        resp.ContentType = "application/json";
        var json = JsonSerializer.Serialize(data, JsonOpts);
        var bytes = System.Text.Encoding.UTF8.GetBytes(json);
        resp.ContentLength64 = bytes.Length;
        await resp.OutputStream.WriteAsync(bytes);
        resp.Close();
    }
}

// PackageFamilyName/PackageAppId: launch in that registered package's context, so
// the app runs with its identity (SessionManager.StartInPackageContext).
record SessionRequest(string App, string? Args, Dictionary<string, string>? Environment,
                      string? PackageFamilyName = null, string? PackageAppId = null);
record ElementRequest(string Id, int Index = 0, List<ScopeStep>? Scope = null);
record TypeRequest(string Id, string Text, int Index = 0, List<ScopeStep>? Scope = null);
record PhysicalTypeRequest(
    string Id, string Text, int Index = 0, List<ScopeStep>? Scope = null, int PreDelayMs = 0);
record SelectRequest(string Id, string Value, int Index = 0, List<ScopeStep>? Scope = null);
record KeyRequest(string Id, string Key, int Index = 0, List<ScopeStep>? Scope = null);
record ScreenshotRequest(string Name);
record ScrollRequest(string? Direction = "down");
record ScrollFractionRequest(
    string Id, double MinFraction, int Index = 0, List<ScopeStep>? Scope = null);
