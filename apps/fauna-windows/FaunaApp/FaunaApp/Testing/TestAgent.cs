// testing.md § Cross-app e2e conventions, point 15 — the automation surface is
// compiled out of release artifacts. This whole file is the windows automation
// surface: a Release build compiles NONE of it, so no `strings` of the shipped
// FaunaApp.dll can find the agent, its command table, or the bridge protocol.
// The FAUNA_E2E_BRIDGE env var stays the inner runtime switch *within* a Debug
// build (convention 15's "runtime gates stay" rule); the `#if DEBUG` is the
// outer security boundary. Two production call sites reach in — App.ArmNavLoad
// and App's agent-start block — and both are gated at their own site.
#if DEBUG || FAUNA_E2E_AGENT
using System.Text.Json;
using System.Text.Json.Serialization;

namespace FaunaApp.Testing;

/// <summary>
/// Test agent for E2E state protocol. Activated by FAUNA_E2E_BRIDGE env var.
/// Polls the bridge for commands and pushes app state back.
/// </summary>
public class TestAgent
{
    public static TestAgent? Instance { get; private set; }

    private readonly string _bridgeUrl;
    private readonly HttpClient _http = new() { Timeout = TimeSpan.FromSeconds(10) };
    private string _lastCommandId = "";
    private volatile bool _ready = true;
    private bool _running;
    // The agent's SECOND deferral rail: work that must run off the poll thread
    // AND off the UI thread. Set by `ProcessCommand` (which returns null), taken
    // and cleared by `PollLoopAsync`, which holds `ready=false` across it exactly
    // as it does for a post-action. Written and read on the poll thread only.
    //
    // The post-action rail marshals to the DispatcherQueue because its work IS
    // UI work (navigation, deferred render steps) — the ack must land after the
    // UI has applied it. A machine-method call is the opposite: it has never
    // touched the UI thread here (the pre-2026-08-29 code ran the SYNC
    // dispatcher inline on this poll thread), and putting one on the UI thread
    // puts a UI round trip in front of every reader poll. But it cannot stay ON
    // the poll thread either — see the `call_machine_method` arm below.
    private Func<Task>? _backgroundAction;
    // Upper bound (ms) the agent holds ready=false waiting for a navigated
    // IAsyncLoadedPage to finish its async Page_Loaded (App.PendingNavLoad). A page that
    // never signals degrades to the old fire-and-forget behaviour instead of hanging every
    // nav; kept under the set_state poll timeout (drivers/http_bridge.py, 10s).
    private const int NavReadyBudgetMs = 8000;

    // Session epoch injected by the bridge on POST /session. Echoed on /app/commands
    // + /app/state so the bridge can fence a stale (pre-recover()) app instance whose
    // epoch no longer matches the current session — it then gets no commands and its
    // state pushes are ignored. Empty when unset (the
    // bridge then serves this agent normally).
    private static readonly string _epochQuery =
        Environment.GetEnvironmentVariable("FAUNA_E2E_SESSION_EPOCH") is { Length: > 0 } ep
            ? $"?epoch={ep}"
            : "";

    private Func<Dictionary<string, object?>>? _stateProvider;
    private Func<Dictionary<string, object?>, Func<Task>?>? _commandHandler;
    private Microsoft.UI.Dispatching.DispatcherQueue? _dispatcherQueue;

    private static readonly JsonSerializerOptions JsonOpts = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

    private TestAgent(string bridgeUrl)
    {
        _bridgeUrl = bridgeUrl;
    }

    public static TestAgent Start(string bridgeUrl)
    {
        var agent = new TestAgent(bridgeUrl);
        Instance = agent;
        return agent;
    }

    /// <summary>
    /// Configure the agent with a state provider and command handler.
    /// The command handler receives a command dict and returns an optional
    /// post-action (e.g. navigation) that runs AFTER state is pushed.
    /// This prevents slow UI operations from blocking the state acknowledgment.
    ///
    /// <para>The post-action is a <c>Func&lt;Task&gt;</c>, not an <c>Action</c>: one
    /// command may compose several deferred steps (see
    /// <see cref="FaunaApp.Core.Helpers.PostActionChain"/>) and they must run in
    /// order, each awaited to completion — an <c>Action</c>-typed chain makes every
    /// async step <c>async void</c> and silently interleaves them.</para>
    /// </summary>
    public void Configure(
        Func<Dictionary<string, object?>> stateProvider,
        Func<Dictionary<string, object?>, Func<Task>?> commandHandler,
        Microsoft.UI.Dispatching.DispatcherQueue dispatcherQueue)
    {
        _stateProvider = stateProvider;
        _commandHandler = commandHandler;
        _dispatcherQueue = dispatcherQueue;
        _running = true;
        StartPoolWatchdog();
        StartUiLatencyWatchdog(dispatcherQueue);
        // Run on thread pool to avoid capturing the UI SynchronizationContext.
        // This ensures the poll loop never blocks on the UI thread.
        _ = Task.Run(PollLoopAsync);
    }

    public void Stop() => _running = false;

    /// <summary>
    /// A DEDICATED OS thread that measures how long the UI thread takes to run a no-op
    /// it enqueues — the instrument that sees UI-thread time spent OUTSIDE any handler
    /// this app wrote (a layout/measure pass, a UIA provider call, a render tick). Stage
    /// timers inside a handler cannot see that time at all: it happens after the handler
    /// returns. Logs every dispatch that waited ≥100 ms, plus a "still stalled" line each
    /// second while one is in progress.
    ///
    /// <para>Why a second instrument beside <see cref="FaunaApp.Core.Helpers.DispatcherLatencyWatch"/>
    /// (the onboarding page's UI-thread timer): a timer that runs ON the UI thread can only
    /// report a stall after the thread comes back, so a stall still in progress when the
    /// app is torn down leaves no line at all. This one reports from off the thread while
    /// the stall is happening — which is how a large mail's conversations open was
    /// measured stalling for 109 s, and how a stall cut short by teardown was told apart
    /// from no stall.</para>
    /// </summary>
    private static void StartUiLatencyWatchdog(Microsoft.UI.Dispatching.DispatcherQueue dispatcherQueue)
    {
        if (!FaunaApp.Core.Logs.E2eTrace.Enabled) return;
        var t = new Thread(() =>
        {
            while (true)
            {
                var sw = System.Diagnostics.Stopwatch.StartNew();
                using var ran = new ManualResetEventSlim(false);
                if (!dispatcherQueue.TryEnqueue(() => ran.Set()))
                {
                    Thread.Sleep(250);
                    continue;
                }
                var stalled = false;
                while (!ran.Wait(1000))
                {
                    stalled = true;
                    FaunaApp.Core.Logs.E2eTrace.Write($"[ui] still stalled {sw.ElapsedMilliseconds}ms");
                }
                var ms = sw.ElapsedMilliseconds;
                if (ms >= 100 || stalled)
                    FaunaApp.Core.Logs.E2eTrace.Write($"[ui] dispatch waited {ms}ms");
                Thread.Sleep(250);
            }
        })
        { IsBackground = true, Name = "e2e-ui-latency-watchdog" };
        t.Start();
    }

    // Routed through the shared, LOCKED writer rather than appending here: the poll
    // loop and the UI thread both trace, and two unsynchronized AppendAllText calls
    // over one file drop lines under load — precisely when a wedge is being chased.
    // See FaunaApp.Core.Logs.E2eTrace for why a thinning trace is an actively
    // misleading instrument.
    private static void Log(string msg) => FaunaApp.Core.Logs.E2eTrace.Write(msg);

    /// <summary>
    /// The refusal sentence <c>conversations_accept_recipient</c> reports when the
    /// accept commits no chip — windows' hand-carried copy of shared Rust's
    /// <c>fauna_conversations::manager::ACCEPT_RECIPIENT_NO_CHIP_REASON</c>.
    ///
    /// <para>Carried by hand rather than exported over UniFFI <b>on that constant's
    /// own instruction</b>: <i>"Rust consumers (tui, linux) use this constant
    /// directly; web's TypeScript arm and the still-owed windows/apple arms carry
    /// the same sentence by hand, since no FFI export is warranted for a debug-only
    /// string."</i> web's TypeScript arm and apple's
    /// <c>acceptRecipientNoChipReason</c> carry it the same way.</para>
    ///
    /// <para>⚠ It must stay byte-identical to the Rust constant: the cross-app pin
    /// <c>tests/e2e-unified/tests/test_agent_refuses_declining_arm.py</c> asserts
    /// text every app agrees on, so a re-wording on one side unassertable-ifies
    /// it.</para>
    /// </summary>
    internal const string AcceptRecipientNoChipReason =
        "nothing committed — the active picker had no resolvable recipient "
        + "(empty input, or an address that resolved to no chip)";

    /// <summary>
    /// Stamp the nav-independent agent-failure slot for a command windows
    /// <b>NEVER TRIED</b> — an action with no arm, a payload it rejected, or a
    /// collaborator that is not wired up. The linux twin is
    /// <c>report_refused_agent_command</c>; tui's is
    /// <c>App::report_refused_agent_command</c>.
    ///
    /// <para>Keep the action name in the text: the cross-app floor pin
    /// (<c>test_agent_refuses_unknown_command.py</c>) asserts it, and a walk driving
    /// many commands needs to know <i>which</i> one was refused (convention 6 —
    /// failures diagnose themselves).</para>
    /// </summary>
    private static void ReportRefusedCommand(string action, string reason)
        => StampAgentFailure($"test agent refused command \"{action}\": {reason}");

    /// <summary>
    /// Stamp the same slot for a command windows <b>DID try</b> and which then threw
    /// or declined. The linux twin is <c>report_agent_command_failure</c>.
    ///
    /// <para>The distinction from <see cref="ReportRefusedCommand"/> is diagnostic
    /// only — both are equally loud, and convention 11 forbids silence rather than
    /// prescribing a wording — but "refused" vs "failed" is the first thing a session
    /// reading the text needs to know: whether the arm exists at all.</para>
    /// </summary>
    private static void ReportCommandFailure(string action, string reason)
        => StampAgentFailure($"test agent command \"{action}\" failed: {reason}");

    /// <summary>
    /// The single write to <see cref="FaunaApp.App.AgentCommandFailure"/>.
    ///
    /// <para>⚠ Do NOT stamp <c>App.CurrentErrorMessage</c> instead — that is the
    /// page mirror, which <c>HandleTestCommand</c> clears on every login and every
    /// <c>navigate_to</c>, so a refusal written there is wiped microseconds later and
    /// the driver reads <c>error=''</c>. That wrong slot is the one tui, android and
    /// apple each shipped first, and windows shipped it too until 2026-08-29
    /// (<c>e2e-conventions.md</c> § convention 11's build-out record). The trace line
    /// is kept as well — it is free, and a log line beside the state field is how a
    /// wedge that never reaches a state push is still diagnosable.</para>
    /// </summary>
    private static void StampAgentFailure(string text)
    {
        Log($"[TestAgent] {text}");
        System.Diagnostics.Debug.WriteLine($"[TestAgent] {text}");
        FaunaApp.App.AgentCommandFailure = text;
    }

    /// <summary>
    /// Run one <c>conversations_real_*</c> command and report, never swallow, its
    /// failure: a throw goes to <see cref="ReportCommandFailure"/> (the refusal
    /// slot the action layer's <c>_assert_command_honoured</c> reads) and to
    /// ShellLog (the fauna_log ring + the daily file under
    /// <c>%LocalAppData%\Fauna\logs\</c>).
    ///
    /// <para>These arms logged and acked green until 2026-09-14. The wire ops fail
    /// for a dozen reasons (no key package for the peer, a refused commit,
    /// transport), and a failure the driver never sees surfaces two assertions
    /// later as a missing effect: a remove that posted no Commit read as
    /// <c>assert 6 &gt; 6</c> on an envelope count, not as the refusal it was.
    /// <c>e2e-conventions.md</c> § convention 11. The gestures that report through the
    /// manager's page error rather than a throw are turned into one by
    /// <c>ConversationsCommands.ThrowOnPageError</c>. Twins: linux's and tui's
    /// <c>conv_backend.rs</c> <c>e2e_*</c> returning <c>Err</c> to their agent.</para>
    /// </summary>
    private static void RunRealConversationsCommand(string action, Action run)
    {
        try
        {
            run();
        }
        catch (Exception ex)
        {
            var reason = $"{ex.GetType().Name}: {ex.Message}";
            FaunaApp.Core.Logs.ShellLog.Error("TestAgent", $"{action} threw: {reason}");
            ReportCommandFailure(action, reason);
        }
    }

    /// <summary>
    /// A DEDICATED OS thread (never a thread-pool thread) that samples the pool
    /// while the app runs. It exists to answer one question the rest of the trace
    /// structurally cannot: when every other line stops at the same instant, is the
    /// process dead, or alive with a starved/deadlocked thread pool?
    ///
    /// <para>Every other producer — the agent poll loop, task continuations, the UI
    /// dispatcher's async work — runs on the pool, so pool exhaustion silences all
    /// of them at once and looks exactly like a crash. A watchdog that itself needs
    /// a pool thread would go silent too and prove nothing; that is why this is a
    /// raw <see cref="Thread"/>, and why it never awaits.</para>
    /// </summary>
    private static void StartPoolWatchdog()
    {
        if (!FaunaApp.Core.Logs.E2eTrace.Enabled) return;
        var t = new Thread(() =>
        {
            while (true)
            {
                ThreadPool.GetAvailableThreads(out var availWorker, out var availIo);
                ThreadPool.GetMaxThreads(out var maxWorker, out var maxIo);
                FaunaApp.Core.Logs.E2eTrace.Write(
                    $"[pool] threads={ThreadPool.ThreadCount} pending={ThreadPool.PendingWorkItemCount} "
                        + $"completed={ThreadPool.CompletedWorkItemCount} "
                        + $"availWorker={availWorker}/{maxWorker} availIo={availIo}/{maxIo}");
                Thread.Sleep(500);
            }
        })
        { IsBackground = true, Name = "e2e-pool-watchdog" };
        t.Start();
    }

    private async Task PollLoopAsync()
    {
        Log($"PollLoop started on thread {Environment.CurrentManagedThreadId}");
        var pushCounter = 0;
        var iteration = 0;
        while (_running)
        {
            iteration++;
            try
            {
                var fetchSw = System.Diagnostics.Stopwatch.StartNew();
                var command = await FetchCommandAsync();
                fetchSw.Stop();
                if (fetchSw.ElapsedMilliseconds > 50)
                    Log($"[{iteration}] FetchCommandAsync took {fetchSw.ElapsedMilliseconds}ms");
                if (command != null)
                {
                    // Name the command. When the app dies mid-run the trace's last
                    // line is the only witness to what it was doing, and "Got
                    // command, processing" names nothing — three paid live runs
                    // were graded against a trace that could not say which command
                    // preceded the death.
                    var commandName = command.TryGetValue("action", out var actionValue)
                        ? actionValue?.ToString() ?? "patch"
                        : "patch";
                    var methodName = command.TryGetValue("method", out var methodValue)
                        ? methodValue?.ToString()
                        : null;
                    Log($"[{iteration}] Got command {commandName}"
                        + (methodName is null ? "" : $" ({methodName})") + ", processing");
                    var postAction = ProcessCommand(command);
                    Log($"[{iteration}] Command processed, lastCmdId={_lastCommandId}");
                    // Take-and-clear BEFORE either rail runs: the arm sets it, this
                    // loop owns its lifetime, and a stale one would re-run next command.
                    var backgroundAction = _backgroundAction;
                    _backgroundAction = null;
                    if (backgroundAction != null)
                    {
                        // The off-UI-thread rail. Same ack contract as the post-action
                        // below — `ready=false` here, `ready=true` in the continuation's
                        // `finally` — but the work runs on the thread pool, so THIS loop
                        // stays free to push `/app/state`. The push is the ack; a command
                        // that occupies this thread starves its own acknowledgement.
                        //
                        // ⚠ `ready=false` MUST be set before the task is launched. Set it
                        // after and a call that completes first has its `ready=true`
                        // clobbered here, and the driver waits out its whole budget on an
                        // ack that already happened.
                        _ready = false;
                        var bgIteration = iteration;
                        var backgroundOf = command.TryGetValue("action", out var ba)
                            ? ba?.ToString() ?? "patch"
                            : "patch";
                        _ = Task.Run(async () =>
                        {
                            Log($"[{bgIteration}] backgroundAction ENTER (thread {Environment.CurrentManagedThreadId})");
                            try
                            {
                                await backgroundAction();
                                Log($"[{bgIteration}] backgroundAction EXIT (thread {Environment.CurrentManagedThreadId})");
                            }
                            // Convention 11: a throw here means the command's whole effect
                            // never happened, so it must reach the slot the driver can see
                            // — not the trace file alone.
                            catch (Exception ex)
                            {
                                Log($"backgroundAction threw: {ex.Message}");
                                ReportCommandFailure(
                                    backgroundOf,
                                    $"background action threw: {ex.GetType().Name}: {ex.Message}");
                            }
                            finally
                            {
                                Log($"[{bgIteration}] backgroundAction FINALLY -> ready=true");
                                _ready = true;
                                // Push at once so the driver sees ready=true without
                                // waiting for the next idle push (~1s).
                                _ = Task.Run(PushStateAsync);
                            }
                        });
                    }
                    else if (postAction != null)
                    {
                        _ready = false;
                        var cmdIteration = iteration;
                        // Captured for the catch below: `command` is the only place the
                        // action name still exists by the time the post-action runs.
                        var postActionOf = command.TryGetValue("action", out var pa)
                            ? pa?.ToString() ?? "patch"
                            : "patch";
                        RunOnUiThread(async () =>
                        {
                            // Proves the enqueued lambda actually STARTED. A successful
                            // TryEnqueue only proves it was queued: if the UI thread is
                            // blocked, the queue never drains and no marker appears here.
                            Log($"[{cmdIteration}] postAction ENTER (thread {Environment.CurrentManagedThreadId})");
                            try
                            {
                                // AWAIT it: the post-action chain is ordered, and the
                                // nav-load barrier below must be taken AFTER the
                                // navigation it is meant to observe has actually
                                // happened. Fire-and-forget here would take the barrier
                                // at the chain's first suspension point, before any
                                // Navigate ran, and flip ready=true early.
                                await postAction();
                                // Separates "the post-action chain hung" from "it finished
                                // and the nav-load barrier hung".
                                Log($"[{cmdIteration}] postAction EXIT (thread {Environment.CurrentManagedThreadId})");
                                // A nav to an IAsyncLoadedPage armed App.PendingNavLoad with
                                // the page's initial-load task; await it (bounded) so ready=true
                                // means the target page finished its async Page_Loaded — not
                                // merely that the frame-nav was kicked off. Fixes the folder
                                // wizard-render race. Null (and no wait) for a nav to a page
                                // without the barrier.
                                var navLoad = App.TakePendingNavLoad();
                                if (navLoad is not null)
                                    await Task.WhenAny(navLoad, Task.Delay(NavReadyBudgetMs));
                            }
                            // Convention 11: the post-action is where NAVIGATION and every
                            // deferred UI step run, so a throw here means the command's
                            // whole visible effect never happened — and it used to reach
                            // the trace file only, while `finally` set ready=true and the
                            // driver read a green ack. The trace line is kept (it carries
                            // the iteration and is the wedge-diagnosis channel); the slot
                            // is what the driver can actually see.
                            catch (Exception ex)
                            {
                                Log($"postAction/navLoad threw: {ex.Message}");
                                ReportCommandFailure(
                                    postActionOf,
                                    $"post-action threw: {ex.GetType().Name}: {ex.Message}");
                            }
                            finally
                            {
                                Log($"[{cmdIteration}] postAction FINALLY -> ready=true");
                                _ready = true;
                                // Push state immediately so Python sees ready=true
                                // without waiting for the next idle push (~1s delay).
                                _ = Task.Run(PushStateAsync);
                            }
                        });
                    }
                    Log($"[{iteration}] Pushing state (ready={_ready})");
                    await PushStateAsync();
                    Log($"[{iteration}] State pushed");
                    pushCounter = 0;
                }
                else
                {
                    pushCounter++;
                    if (pushCounter >= 5)
                    {
                        await PushStateAsync();
                        pushCounter = 0;
                    }
                }
            }
            catch (Exception ex)
            {
                Log($"[{iteration}] POLL ERROR: {ex.GetType().Name}: {ex.Message}");
                await Task.Delay(1000);
                continue;
            }
            var delaySw = System.Diagnostics.Stopwatch.StartNew();
            await Task.Delay(200);
            delaySw.Stop();
            if (delaySw.ElapsedMilliseconds > 300)
                Log($"[{iteration}] Task.Delay(200) actually took {delaySw.ElapsedMilliseconds}ms");
        }
        Log("PollLoop exited");
    }

    private async Task<Dictionary<string, object?>?> FetchCommandAsync()
    {
        var resp = await _http.GetAsync($"{_bridgeUrl}/app/commands{_epochQuery}");
        if (resp.StatusCode == System.Net.HttpStatusCode.NoContent)
            return null;
        if (!resp.IsSuccessStatusCode)
            return null;
        var json = await resp.Content.ReadAsStringAsync();
        return JsonSerializer.Deserialize<Dictionary<string, object?>>(json);
    }

    private async Task PushStateAsync()
    {
        if (_stateProvider == null) return;

        // Phase timings, not just a completion line. `PushStateAsync` is the whole
        // ack path — a command is acknowledged when, and only when, one of these
        // POSTs lands — so "the push was slow" is the diagnosis for every
        // `App did not acknowledge command … within Ns` timeout. The old single
        // trailing line could not say WHICH phase was slow, and the three phases
        // have nothing to do with each other: the provider walks app state (locks,
        // credential-store reads), the serializer is pure CPU, and the POST is the
        // bridge round trip. Splitting them is what turned
        // row 60 from "not root-caused" into a measurement.
        var sw = FaunaApp.Core.Logs.E2eTrace.Enabled
            ? System.Diagnostics.Stopwatch.StartNew()
            : null;

        // State provider reads plain fields (no UI access), safe to call from any thread
        Dictionary<string, object?> state;
        try { state = _stateProvider(); }
        catch (Exception ex)
        {
            Log($"PushState stateProvider THREW: {ex.GetType().Name}: {ex.Message}\n{ex.StackTrace}");
            return;
        }
        var providerMs = sw?.ElapsedMilliseconds ?? 0;

        var payload = new Dictionary<string, object?>
        {
            ["last_command_id"] = _lastCommandId,
            ["ready"] = _ready,
            ["state"] = state,
        };
        var json = JsonSerializer.Serialize(payload, JsonOpts);
        var serializeMs = (sw?.ElapsedMilliseconds ?? 0) - providerMs;
        var content = new StringContent(json, System.Text.Encoding.UTF8, "application/json");
        try
        {
            var resp = await _http.PostAsync($"{_bridgeUrl}/app/state{_epochQuery}", content);
            var postMs = (sw?.ElapsedMilliseconds ?? 0) - providerMs - serializeMs;
            // ⚠ `_lastCommandId` is read HERE, after the await — it is NOT necessarily
            // the id this push carried. The payload snapshotted it above, so a push
            // slow enough to overlap the next command logs the NEWER id and reads as
            // an ack that was never sent. Both are printed for that reason.
            Log($"PushState POST {resp.StatusCode}, sentCmdId={payload["last_command_id"]}, "
                + $"nowCmdId={_lastCommandId}, bytes={json.Length}, "
                + $"provider={providerMs}ms serialize={serializeMs}ms post={postMs}ms");
        }
        catch (Exception ex)
        {
            Log($"PushState POST FAILED: {ex.GetType().Name}: {ex.Message}");
        }
    }

    /// <summary>
    /// Process a command: apply state changes (no UI thread needed), then return
    /// an optional post-action for deferred execution (e.g. navigation).
    /// </summary>
    /// <summary>Read a top-level string field off a command payload, tolerating both
    /// the <see cref="JsonElement"/> form (the wire) and a plain string (hand-built
    /// commands in unit tests). Empty string when absent or not a string — callers
    /// that require it must fail loudly, never silently no-op (testing.md § conventions
    /// point 11).</summary>
    private static string CommandString(Dictionary<string, object?> command, string key) =>
        command.TryGetValue(key, out var v)
            ? v switch
            {
                JsonElement je when je.ValueKind == JsonValueKind.String => je.GetString() ?? "",
                string s => s,
                _ => "",
            }
            : "";

    /// <summary>Mirrors <c>fauna_e2e_agent::FOCUS_MOVE_MAX_TIMES</c> — see the
    /// <c>focus_move</c> case's payload validation.</summary>
    private const int FocusMoveMaxTimes = 256;

    /// <summary>Render a command payload field for a refusal message — `absent`
    /// when the key is missing, `null` when it is present but JSON-null, else its
    /// raw text — mirroring `fauna_e2e_agent::describe` (and apple's/android's
    /// `describe`/`describeCommandField` twins) so a failing walk names the exact
    /// mistake. Pass `null` for an absent field (callers already distinguish
    /// presence via `TryGetValue`'s bool).</summary>
    private static string DescribeCommandField(object? raw) => raw switch
    {
        null => "absent",
        JsonElement je when je.ValueKind == JsonValueKind.Null => "null",
        JsonElement je => je.GetRawText(),
        _ => raw.ToString() ?? "null",
    };

    /// <summary>A signed integer out of a command payload (0 when absent or
    /// unparseable) — the numeric twin of <see cref="CommandString"/>. Signed
    /// deliberately: <c>atproto_delegation_advance_clock</c> takes an OFFSET, and 0
    /// is its documented reset value.</summary>
    private static long CommandLong(Dictionary<string, object?> command, string key) =>
        command.TryGetValue(key, out var v)
            ? v switch
            {
                JsonElement je when je.ValueKind == JsonValueKind.Number
                    && je.TryGetInt64(out var parsed) => parsed,
                long l => l,
                int i => i,
                _ => 0L,
            }
            : 0L;

    /// <summary>A boolean out of a command payload, defaulting to
    /// <paramref name="fallback"/> when absent or not a boolean — the boolean
    /// twin of <see cref="CommandLong"/>. <c>serve_enable_folder</c>'s
    /// <c>create</c> field is the first caller: default <c>true</c>, matching
    /// linux's <c>cmd.payload.get("create")...unwrap_or(true)</c>
    /// (<c>apps/fauna-linux/src/main.rs</c>).</summary>
    private static bool CommandBool(Dictionary<string, object?> command, string key, bool fallback) =>
        command.TryGetValue(key, out var v)
            ? v switch
            {
                JsonElement je when je.ValueKind == JsonValueKind.True => true,
                JsonElement je when je.ValueKind == JsonValueKind.False => false,
                bool b => b,
                _ => fallback,
            }
            : fallback;

    private Func<Task>? ProcessCommand(Dictionary<string, object?> command)
    {
        var cmdId = command.TryGetValue("id", out var id) ? id?.ToString() ?? "unknown" : "unknown";
        var action = command.TryGetValue("action", out var act) ? act?.ToString() ?? "patch" : "patch";
        _lastCommandId = cmdId;

        System.Diagnostics.Debug.WriteLine($"[TestAgent] Processing {cmdId} (action: {action})");

        Dictionary<string, object?>? payload = null;
        switch (action)
        {
            case "patch":
                if (command.TryGetValue("state", out var stateObj) && stateObj is JsonElement stateEl)
                    payload = JsonSerializer.Deserialize<Dictionary<string, object?>>(stateEl.GetRawText());
                break;
            case "reset":
                payload = new Dictionary<string, object?> { ["__action"] = "reset" };
                break;
            case "logout":
                payload = new Dictionary<string, object?> { ["__action"] = "logout" };
                break;
            case "call_machine_method":
                // Cross-app E2E bridge (design tracked internally,
                // §"E2E bridge contract"). Routed directly here (not through
                // the command-handler payload path) because the target is
                // the OnboardingMachine instance held by OnboardingViewModel,
                // not App-level state. Tests fixture wizard snapshots via
                // `set_handle_check_snapshot_for_test` /
                // `set_invite_request_snapshot_for_test` to verify per-app
                // rendering across all 7 apps.
                {
                    var method = command.TryGetValue("method", out var m) ? m?.ToString() ?? "" : "";
                    var jsonArg = "";
                    if (command.TryGetValue("json_arg", out var ja))
                    {
                        jsonArg = ja switch
                        {
                            JsonElement je when je.ValueKind == JsonValueKind.String => je.GetString() ?? "",
                            JsonElement je => je.GetRawText(),
                            string s => s,
                            _ => ja?.ToString() ?? "",
                        };
                    }
                    var vm = FaunaApp.Core.ViewModels.OnboardingViewModel.Current;
                    // Clear any prior reader's value before the ack, so a setter
                    // call after a reader call doesn't leave a stale result behind
                    // (onboarding.md § E2E bridge contract § Return values).
                    FaunaApp.App.MachineMethodResult = null;

                    // Two names touch only process-global state (the nest-identity
                    // TOFU pin seed/read — security.md § Post-auth surfacing) and need
                    // no machine at all in shared Rust
                    // (fauna-onboarding-machine::call_machine_free_method,
                    // machine.rs:6842 — every OnboardingMachine's async dispatcher
                    // tries these two names FIRST regardless of instance, which is
                    // what makes a throwaway instance below dispatch them identically
                    // to the real one). This is the real blocker closed here: post-auth (the
                    // identity-changed e2e re-seeds the pin on a LIVE session,
                    // no OnboardingPage on screen),
                    // OnboardingViewModel.Current is null, so these two silently
                    // no-op'd before this. Every OTHER name still requires the real
                    // vm below — dispatching one of those on a throwaway machine
                    // would process against blank state and falsely ack green having
                    // done nothing (convention 11), so the fallback is this narrow
                    // allowlist, not "any name with vm null".
                    // ── The registry half of the bridge, tried FIRST ──
                    // onboarding.md § E2E bridge contract: a name the shared registry
                    // dispatcher owns (`fauna_client_accounts::call_registry_method_for_test`
                    // — `refuse_secret_writes_for_test`, …) runs against this app's own
                    // registry, exactly as tui's automation and apple's
                    // RegistryTestBridge do; windows contributes only the registry.
                    // Before the vm check: these names need no wizard, and the
                    // outcome-17 journey arms its fault on a signed-in session with no
                    // OnboardingPage on screen. A fresh registry per call is fine — the
                    // one stateful arm keeps its fault in the store's backing, which
                    // every view shares.
                    {
                        using var bridgeRegistry = Services.CredentialStore.Registry();
                        if (bridgeRegistry.CallRegistryMethodForTest(method, jsonArg)
                            is uniffi.fauna_ffi.FfiRegistryMethodOutcome.Handled handled)
                        {
                            if (handled.@resultJson is { } registryResult)
                            {
                                using var doc = JsonDocument.Parse(registryResult);
                                FaunaApp.App.MachineMethodResult = doc.RootElement.Clone();
                            }
                            return null;
                        }
                    }

                    var isFreeDispatchName =
                        method is "set_nest_identity_pin_for_test" or "nest_identity_pin_for_test";
                    if (vm is null && !isFreeDispatchName)
                    {
                        // testing.md § Cross-app e2e conventions, convention 11: a
                        // test agent must HONOUR a command or FAIL LOUDLY — never
                        // silently drop one. A dropped call yields no error AND no
                        // effect, so the test fails later on an unrelated-looking read
                        // ("the wizard never advanced") that is indistinguishable from
                        // a real product bug — the exact shape that cost apple multiple
                        // sessions twice. Surface it on the app's own error-message
                        // element, as the `default:` arm below already does.
                        var msg = $"[TestAgent] call_machine_method({method}): no OnboardingViewModel.Current";
                        StampAgentFailure(msg);
                        return null;
                    }

                    // ── The one arm the shared dispatcher deliberately excludes ──
                    // Provisioning spawns work that outlives the call (minutes), so
                    // runtime ownership is per-app and the bridge must ack AT ONCE —
                    // the driver polls `provisioning_snapshot` instead of blocking.
                    // Returning a post-action here would be wrong: the poll loop
                    // AWAITS it before setting ready=true, so the ack would stall for
                    // the whole provisioning run.
                    //
                    // ⚠ `RunProvisioningForTestAsync` routes to `RunProvisioning()`,
                    // NOT the same-named `StartProvisioning()`/`RetryProvisioning()`.
                    // Those have bare Rust-side `tokio::spawn` bodies needing an
                    // ambient runtime on the CALLING thread, which the WinUI UI thread
                    // does not have — the panic class linux/android/macOS/iOS already
                    // hit for this exact orchestrator (OnboardingViewModel
                    // § ProvisioningStartAsync). `run_provisioning_inner` resets the
                    // snapshot + cancel flag at entry, so one arm serves both names.
                    if (method is "start_provisioning" or "retry_provisioning")
                    {
                        // vm is guaranteed non-null here: neither provisioning name is
                        // in the free-dispatch allowlist above, so a null vm already
                        // returned before this line was reached.
                        RunOnUiThread(async () =>
                        {
                            try { await vm!.RunProvisioningForTestAsync(); }
                            catch (Exception ex)
                            {
                                // Convention 11 again: a provisioning run that never
                                // started must not look like one that is merely slow.
                                var msg = $"[TestAgent] call_machine_method({method}) threw: "
                                    + $"{ex.GetType().Name}: {ex.Message}";
                                StampAgentFailure(msg);
                            }
                        });
                        return null;
                    }

                    // Everything else goes through the ASYNC dispatcher. Through the
                    // SYNC one the async names (`verify_dns`,
                    // `wizard_submit_claim_code`, `submit_nat_mode_choice`, …) fall
                    // into its silent `_` arm and ack green having done nothing — how
                    // the live Hetzner drive sat at `overall: 'Idle'` on macOS
                    // (2026-08-29) and why windows was kept out of `LIVE_DRIVE_APPS`.
                    // Non-async names delegate to the sync dispatcher inside Rust, so
                    // this stays ONE name table for every name.
                    //
                    // ⚠ THE THREAD THIS RUNS ON IS THE WHOLE PROBLEM. Two shapes are
                    // already measured wrong; this is the third, and the comment is
                    // kept long so a fourth session does not re-pick one of them.
                    //
                    // (1) BLOCKING HERE, on the poll thread — what shipped
                    //     2026-08-29 (`.GetAwaiter().GetResult()`). `ProcessCommand`
                    //     runs inline on `PollLoopAsync`'s thread, and that loop is
                    //     also what pushes `/app/state` — the ack the driver waits
                    //     on, and the only thing that publishes `_lastCommandId`. So
                    //     a call that occupies this thread starves its own ack by
                    //     construction. Wrong on its own terms, which is why it is
                    //     gone — but ⚠ do NOT read the live `provisioning_snapshot`
                    //     ack timeout it was blamed for as evidence for it. That was
                    //     re-measured 2026-08-30 with this rail in place and the
                    //     timeout is unchanged: the app PROCESS dies a few seconds
                    //     into a live run and the timeout is the driver noticing the
                    //     corpse. See `live_provision.py`'s `LIVE_DRIVE_APPS` note.
                    // (2) A RETURNED POST-ACTION, on the UI thread. That frees the
                    //     poll thread, but the post-action rail is the DispatcherQueue,
                    //     so it puts a UI-thread round trip in front of EVERY name —
                    //     including the reader polls a live drive spins on for minutes.
                    //     Measured 2026-08-29: crashed the app (exit 0xC0000409, .NET
                    //     fail-fast) in `test_provisioning_progress.py` in 2 of 2 runs
                    //     where the same tests at baseline crashed 0 times. Never
                    //     root-caused, so treat the UI thread as out of bounds here.
                    // (3) THIS — the `_backgroundAction` rail: `await`ed on the thread
                    //     pool, so no thread is blocked and the UI thread is not
                    //     involved at all. The ack still waits for the call (the rail
                    //     holds `ready=false` across it), which every setter name needs.
                    //     It is also what macOS gets for free: its handler is `async`
                    //     and simply `await`s `callMachineMethod` (FaunaMacApp.swift).
                    //     Measured healthy under the 2026-08-30 agent trace: enter and
                    //     exit ~1 ms on pool threads, pushes 1–8 ms, pool idle.
                    //
                    // Touching the machine off the UI thread is not new or unproven:
                    // the pre-2026-08-29 code called the SYNC dispatcher inline on this
                    // very poll thread. What changes is only that we no longer BLOCK a
                    // thread while the call runs.
                    _backgroundAction = async () =>
                    {
                        // Caught HERE, not by the rail's own backstop, so the stamp
                        // still names the METHOD. The rail only knows the action
                        // (`call_machine_method`), and "which name threw" is the whole
                        // diagnostic value of convention 11's loud failure.
                        try
                        {
                            string? resultJson;
                            if (vm is not null)
                            {
                                resultJson = await vm.CallMachineMethodAsync(method, jsonArg);
                            }
                            else
                            {
                                // vm is null and isFreeDispatchName is true (the only
                                // way past the early-return above) — a throwaway,
                                // disposed-right-after machine dispatches
                                // set_nest_identity_pin_for_test / nest_identity_pin_for_test
                                // identically to a real one: both ignore `self` entirely
                                // (machine.rs:6842's doc comment).
                                using var throwaway = new uniffi.fauna_onboarding_machine.OnboardingMachine(
                                    new NoOpOnboardingObserver());
                                resultJson = await throwaway.CallMachineMethodAsync(method, jsonArg);
                            }
                            if (resultJson is not null)
                            {
                                using var doc = System.Text.Json.JsonDocument.Parse(resultJson);
                                FaunaApp.App.MachineMethodResult = doc.RootElement.Clone();
                            }
                        }
                        catch (Exception ex)
                        {
                            var msg = $"[TestAgent] call_machine_method({method}) threw: {ex.GetType().Name}: {ex.Message}";
                            StampAgentFailure(msg);
                        }
                    };
                    return null;
                }
            case "silent_sign_in":
                // Trigger the production background silent-challenge refresh on the
                // LIVE authenticated session (security.md § Post-auth surfacing) — the same
                // App.RunTtlRefreshLoopAsync tick already runs on its own near-expiry
                // schedule, exposed here so the post-auth identity-changed e2e can run
                // it AFTER re-seeding a bogus pin via call_machine_method above. This
                // drives the real path — LaunchMachine.RefreshToken() ->
                // fauna.auth.handshake -> classify_silent_challenge ->
                // LaunchPhase.IdentityChanged -> LaunchIdentityChangedPage — NOT a
                // shortcut that fakes the verdict (mirrors linux/macOS's
                // silent_sign_in/performPostAuthSilentSignIn bridge commands).
                //
                // DispatchLaunchSnapshotAsync navigates the root Frame, so this runs
                // on the UI thread (mirrors the start_provisioning/retry_provisioning
                // arm above) — fire-and-forget from the poll thread's perspective; the
                // driver polls nest-identity-changed-warning afterward rather than
                // waiting on this ack, the same shape provisioning's snapshot poll
                // already establishes.
                RunOnUiThread(async () =>
                {
                    try
                    {
                        if (!await FaunaApp.App.TriggerSilentSignInForTestAsync())
                        {
                            // Convention 11: honour or fail loudly, never silently drop.
                            var msg = "[TestAgent] silent_sign_in with no live authenticated session";
                            StampAgentFailure(msg);
                        }
                    }
                    catch (Exception ex)
                    {
                        var msg = $"[TestAgent] silent_sign_in threw: {ex.GetType().Name}: {ex.Message}";
                        StampAgentFailure(msg);
                    }
                    finally
                    {
                        // Push at once so the driver sees the fresh session_generation
                        // bump and nav state without waiting for the poll loop's next
                        // idle push (~1s) — the same rail the call_machine_method
                        // background/post-action arms use. Without this, a driver read
                        // immediately after the UI-visible page transition (which FlaUI
                        // observes directly, no push involved) can race a still-stale
                        // /app/state snapshot — measured: nest-identity-changed-warning
                        // visible while state.session_generation still read 0.
                        _ = Task.Run(PushStateAsync);
                    }
                });
                return null;
            case "backup_audit_run_now":
                // e2e-only forced re-run of the client-side audit loop (backups.md
                // § Audit-alert surface): sets the process-wide e2e clock offset,
                // then re-runs the pass and re-renders — mirrors linux's
                // set_clock_offset_secs + poke_rerun. Payload: {now_offset_secs}
                // (epoch-seconds offset). See test_backups.py.
                {
                    long offset = 0;
                    if (command.TryGetValue("now_offset_secs", out var offEl)
                        && offEl is JsonElement oe && oe.ValueKind == JsonValueKind.Number)
                    {
                        oe.TryGetInt64(out offset);
                    }
                    var page = FaunaApp.Views.BackupsPage.Current;
                    if (page is null)
                    {
                        // testing.md § Cross-app e2e conventions, convention 11: a test
                        // agent must HONOUR a command or FAIL LOUDLY — never silently
                        // drop one (mirrors the call_machine_method arm above / linux's
                        // "the Backups page is not built" case).
                        var msg = "[TestAgent] backup_audit_run_now: no BackupsPage.Current";
                        StampAgentFailure(msg);
                        return null;
                    }
                    return async () =>
                    {
                        try { await page.RunAuditNowAsync(offset); }
                        catch (Exception ex)
                        {
                            var msg = $"[TestAgent] backup_audit_run_now threw: {ex.GetType().Name}: {ex.Message}";
                            StampAgentFailure(msg);
                        }
                    };
                }
            case "custodian_pull_run_now":
                // Client-custodian e2e (tests/e2e-unified/tests/test_backups.py::
                // test_a_hosted_custodian_pulls_checks_in_and_the_owners_row_reports_it).
                // Runs ONE custodian pull pass on the sync agent's hosted replica and
                // returns what it did, as App.MachineMethodResult — the windows twin
                // of linux's (apps/fauna-linux/src/main.rs) and tui's
                // (apps/fauna-tui/src/automation.rs) `custodian_pull_run_now` arms.
                // Payload: {now_offset_secs} (convention 14's fake clock, same
                // spelling as backup_audit_run_now above). Genuinely async:
                // FfiSyncAgentProvisioner.CustodianRunPassNow is a UniFFI async
                // export, so awaiting it below yields the UI thread for the whole
                // pass (up to CUSTODIAN_PASS_WAIT_S=120s in test_backups.py) instead
                // of blocking it — never a block_on_tokio-style call on this thread.
                {
                    long offset = 0;
                    if (command.TryGetValue("now_offset_secs", out var custOffEl)
                        && custOffEl is JsonElement coe && coe.ValueKind == JsonValueKind.Number)
                    {
                        coe.TryGetInt64(out offset);
                    }
                    var channel = FaunaApp.App.CurrentSyncAgent?.Channel;
                    if (channel is null)
                    {
                        // testing.md convention 11: an honest "no agent on this
                        // platform" refusal, distinct from an agent that answered
                        // and refused — mirrors linux's/tui's "drives no sync
                        // agent" arm.
                        FaunaApp.App.MachineMethodResult = null;
                        var msg = "[TestAgent] custodian_pull_run_now: windows drives no sync agent on this platform";
                        StampAgentFailure(msg);
                        return null;
                    }
                    return async () =>
                    {
                        try
                        {
                            var report = await channel.CustodianRunPassNow(offset);
                            var json = JsonSerializer.Serialize(new
                            {
                                hosting = report.hosting,
                                kinds_run = report.kindsRun,
                                held_bytes = report.heldBytes,
                                cap_state = report.capState,
                                audit_state = report.auditState,
                                checked_in = report.checkedIn,
                            });
                            using var doc = JsonDocument.Parse(json);
                            FaunaApp.App.MachineMethodResult = doc.RootElement.Clone();
                        }
                        catch (Exception ex)
                        {
                            FaunaApp.App.MachineMethodResult = null;
                            var msg = $"[TestAgent] custodian_pull_run_now threw: {ex.GetType().Name}: {ex.Message}";
                            StampAgentFailure(msg);
                        }
                    };
                }
            case "conversations_inject_inbound":
                // Cross-app E2E bridge for the unified conversations page: the
                // payload goes whole to the shared inject parser
                // (inject_inbound_from_test_json, test-helpers feature). A refused
                // payload is reported, never swallowed (convention 11). See
                // actions/conversations.py.
                RunRealConversationsCommand(action,
                    () => FaunaApp.Conversations.ConversationsCommands.InjectInbound(command));
                // Returning an empty postAction forces a UI-thread sync
                // before the agent signals ready=true. The manager's
                // notify() inside InjectInbound enqueued a Refresh on the
                // dispatcher; FIFO ordering means that Refresh runs
                // BEFORE this empty action. Without this, PushStateAsync
                // would signal ready=true while the dispatcher still has
                // a queued Refresh that hadn't yet realized the new
                // ListView rows — the test would race ahead and see
                // `conversation-item[0]` missing from the UIA tree.
                return () => Task.CompletedTask;
            case "conversations_evict_attachment":
                // Drop a thread's cached attachment bytes the way the store's budget
                // eviction does (the shared evict_thread_attachments_for_test), so a
                // test reaches the re-fetch of an evicted attachment and the declared
                // placeholder of one with nowhere to be fetched from, without filling
                // the 128 MiB store. {thread_id, filename}; nothing evicted is a
                // FAILED command, never an ack (convention 11).
                RunRealConversationsCommand(action,
                    () => FaunaApp.Conversations.ConversationsCommands.EvictAttachment(command));
                // Same FIFO-ordering rationale as conversations_inject_inbound: the
                // evict's notify() enqueued the Refresh that repaints the bubble.
                return () => Task.CompletedTask;
            case "conversations_create_mls_group":
                {
                    try
                    {
                        FaunaApp.Conversations.ConversationsCommands.CreateMlsGroup(command);
                    }
                    catch (Exception ex)
                    {
                        System.Diagnostics.Debug.WriteLine(
                            $"[TestAgent] conversations_create_mls_group threw: {ex.GetType().Name}: {ex.Message}");
                    }
                }
                return () => Task.CompletedTask;
            case "conversations_inject_send_failure":
                // Stamp a thread's compose into send_state = Failed { reason } via
                // the shared manager's inject_send_failure_for_test (test-helpers),
                // so the page-level error-message surfaces the reason. Expects
                // {thread_id, reason}. See actions/conversations.py.
                {
                    try
                    {
                        FaunaApp.Conversations.ConversationsCommands.InjectSendFailure(command);
                    }
                    catch (Exception ex)
                    {
                        System.Diagnostics.Debug.WriteLine(
                            $"[TestAgent] conversations_inject_send_failure threw: {ex.GetType().Name}: {ex.Message}");
                    }
                }
                // Empty postAction forces a UI-thread sync before ready=true: the
                // manager's notify() enqueued a Refresh that renders the error;
                // FIFO ordering runs it before this action (same race fix as
                // conversations_inject_inbound).
                return () => Task.CompletedTask;
            case "conversations_inject_page_error":
                // Stamp ConversationsSnapshot.error via inject_page_error_for_test
                // (test-helpers), so the page-level error-message surfaces a failed
                // membership/label op's reason. Expects {key, message}. See
                // actions/conversations.py::inject_page_error_for_test.
                {
                    try
                    {
                        FaunaApp.Conversations.ConversationsCommands.InjectPageError(command);
                    }
                    catch (Exception ex)
                    {
                        System.Diagnostics.Debug.WriteLine(
                            $"[TestAgent] conversations_inject_page_error threw: {ex.GetType().Name}: {ex.Message}");
                    }
                }
                // Same FIFO-ordering rationale as conversations_inject_send_failure.
                return () => Task.CompletedTask;
            case "conversations_enable_real_faunamls":
                // No-op probe: the real wire-backed manager is registered at login
                // under FAUNA_E2E_REAL_CONVERSATIONS. The action layer polls
                // data.conv_real_backend_active for readiness rather than reading a
                // result here. Twin of linux conv_backend::request_e2e_activation.
                {
                    try
                    {
                        FaunaApp.Conversations.ConversationsCommands.EnableRealFaunaMls(command);
                    }
                    catch (Exception ex)
                    {
                        System.Diagnostics.Debug.WriteLine(
                            $"[TestAgent] conversations_enable_real_faunamls threw: {ex.GetType().Name}: {ex.Message}");
                    }
                }
                return () => Task.CompletedTask;
            case "conversations_seed_resolved_link_preview":
                // Stamp a pre-resolved link-preview card for a url so an injected
                // bubble paints `link-preview-card` (render-model.md § D4):
                // {url, title, description, image_hash}. The card is built here
                // already; this seam is what lets the shared witness speak for
                // windows. Reported, never swallowed (convention 11) — the
                // handler THROWS on a missing `url`.
                RunRealConversationsCommand(action,
                    () => FaunaApp.Conversations.ConversationsCommands.SeedResolvedLinkPreview(command));
                // Empty postAction forces a UI-thread sync before ready=true:
                // the manager's notify() enqueued a Refresh that re-folds the
                // bubble, and FIFO ordering runs it before this action (the same
                // race fix as conversations_inject_inbound).
                return () => Task.CompletedTask;
            case "conversations_select_message":
                // Select a thread AND a message inside it — the same call a
                // Search `Mail` row activation makes: {thread_id, message_id}.
                // The paint and the scroll-into-view are the detail view's own
                // reaction to the manager's notify; this only makes the call.
                RunRealConversationsCommand(action,
                    () => FaunaApp.Conversations.ConversationsCommands.SelectMessage(command));
                return () => Task.CompletedTask;
            case "conversations_real_resolve_send_new":
                // Drive a REAL FaunaMls send into a brand-new conversation:
                // {recipient, body}. Blocks this thread-pool thread on the async
                // manager calls (never the UI thread) — see ConversationsCommands.
                // A failure is reported, never swallowed: see RunRealConversationsCommand.
                RunRealConversationsCommand(action,
                    () => FaunaApp.Conversations.ConversationsCommands.RealResolveSendNew(command));
                return () => Task.CompletedTask;
            case "conversations_real_send":
                // Drive a REAL FaunaMls send into an existing thread: {thread_id, body}.
                RunRealConversationsCommand(action,
                    () => FaunaApp.Conversations.ConversationsCommands.RealSend(command));
                return () => Task.CompletedTask;
            case "conversations_real_add":
                // Add a peer to a REAL FaunaMls thread/group:
                // {thread_id, peer_actor_id_hex, peer_handle}.
                RunRealConversationsCommand(action,
                    () => FaunaApp.Conversations.ConversationsCommands.RealAdd(command));
                return () => Task.CompletedTask;
            case "conversations_real_remove":
                // Remove a peer from a bound REAL FaunaMls group:
                // {thread_id, peer_actor_id_hex, peer_handle}.
                RunRealConversationsCommand(action,
                    () => FaunaApp.Conversations.ConversationsCommands.RealRemove(command));
                return () => Task.CompletedTask;
            case "conversations_real_rename":
                // Rename a bound REAL FaunaMls group: {thread_id, label}.
                RunRealConversationsCommand(action,
                    () => FaunaApp.Conversations.ConversationsCommands.RealRename(command));
                return () => Task.CompletedTask;
            case "conv_receive_now":
                // Convention 14 (D9): poke the receive loop's cycle NOW instead
                // of waiting out its 30s ticker. Fire-and-forget by contract — the
                // ack is deliberately NOT the barrier (an awaited reply hangs
                // when no loop is running); `conv_receive_cycles` in the state
                // payload is what a consumer deadline-polls. "No session yet" is
                // a legitimate quiet no-op, handled inside the callee.
                FaunaApp.App.ConvReceiveNowForTest();
                return () => Task.CompletedTask;
            case "account_pump_now":
                // The account plane's run-one-pass-now poke (convention 14's
                // `run_now`), windows' twin of android/macOS/tui/linux's arm
                // (`fauna_e2e_agent::ACCOUNT_PUMP_NOW`). Fire-and-forget, same
                // shape as `conv_receive_now` directly above: the barrier is
                // `account_pump_cycles` in the state payload, not this ack.
                // No runtime yet (pre-auth) is a legitimate quiet no-op,
                // honoured inside the callee.
                FaunaApp.App.AccountPumpNowForTest();
                return () => Task.CompletedTask;
            case "device_set_state":
                // The fleet-removal convergence reader (account-data-taxonomy.md § The
                // generation machinery → Fleet-scope reclamation, clause (4)): whether
                // `device_id_hex`'s `fauna.state.device-set` row reads Removed/Enrolled from
                // THIS app's own account runtime, stashed as App.MachineMethodResult — the
                // windows twin of tui's, linux's, android's and apple's arms.
                // test_crash_recovery_journeys._require_device_set_reader treats ANY non-null
                // answer as "the reader is built", so the two edges below matter.
                // Payload: {device_id_hex}. Async-returning like custodian_pull_run_now
                // above (not the fire-and-forget account_pump_now): the reader's value IS the
                // result, so the ack must follow it.
                {
                    string? deviceIdHex = null;
                    if (command.TryGetValue("device_id_hex", out var devIdEl)
                        && devIdEl is JsonElement die && die.ValueKind == JsonValueKind.String)
                    {
                        deviceIdHex = die.GetString();
                    }
                    if (deviceIdHex is null)
                    {
                        // Convention 11's bad-payload clause: a LOUD refusal naming the field,
                        // never folded into "not found" — a journey asserting "the row is NOT
                        // there" would otherwise pass on a typo'd key (a vacuous green).
                        // Matches apple's DeviceSetStateTestCommand.
                        FaunaApp.App.MachineMethodResult = null;
                        StampAgentFailure("[TestAgent] device_set_state: needs a `device_id_hex` string");
                        return null;
                    }
                    string deviceId = deviceIdHex;
                    return async () =>
                    {
                        try
                        {
                            // No connected client (pre-auth) is a quiet not-found REPORT, never
                            // null: null is the wire signal for "reader unbuilt", which the
                            // journey's probe reads as a skip. Same answer tui/linux/android/
                            // apple give.
                            var devJson = await FaunaApp.App.DeviceSetStateForTest(deviceId)
                                ?? "{\"found\":false}";
                            using var devDoc = JsonDocument.Parse(devJson);
                            FaunaApp.App.MachineMethodResult = devDoc.RootElement.Clone();
                        }
                        catch (Exception ex)
                        {
                            FaunaApp.App.MachineMethodResult = null;
                            var msg = $"[TestAgent] device_set_state threw: {ex.GetType().Name}: {ex.Message}";
                            StampAgentFailure(msg);
                        }
                    };
                }
            case "open_route":
                // The `fauna://` route's e2e seam (windows.md § Shell Extension → The
                // Share hand-off, step 4): feeds a URI to the same door the argv leg
                // takes (`App.ApplyRoute`), so a journey needs no relaunch inside PAR's
                // 90-second window — the windows twin of tui's and apple's arm.
                // Payload: {uri}. The ack waits for the route's page work (a `consent`
                // route's `open_handoff`), so the card is painted when it returns.
                // Signed out the door only HOLDS the route (nothing to await). An
                // unparseable URI is a refusal — the argv leg drops one silently, but a
                // driver that sent one must hear about it (convention 11).
                {
                    string? routeUri = null;
                    if (command.TryGetValue("uri", out var routeEl)
                        && routeEl is JsonElement re && re.ValueKind == JsonValueKind.String)
                    {
                        routeUri = re.GetString();
                    }
                    if (routeUri is null
                        || uniffi.fauna_ffi.FaunaFfiMethods.ParseAppRoute(routeUri) is null)
                    {
                        StampAgentFailure("[TestAgent] open_route: needs a `uri` string the shared route parser accepts");
                        return null;
                    }
                    string route = routeUri;
                    return async () =>
                    {
                        try
                        {
                            var opened = FaunaApp.App.ApplyRouteAndWaitAsync(route);
                            // Named generous budget (convention 14): the open is one nest
                            // round trip plus the page's re-read, never a settle-sleep.
                            if (await Task.WhenAny(opened, Task.Delay(TimeSpan.FromSeconds(30))) != opened)
                            {
                                ReportCommandFailure(action, "the route's page work did not finish within 30 s");
                            }
                        }
                        catch (Exception ex)
                        {
                            ReportCommandFailure(action, $"{ex.GetType().Name}: {ex.Message}");
                        }
                    };
                }
            case "compose_text_runs":
                // The active compose field's applied styling — what
                // get_attr(dm-text-field, "text-runs") answers on windows
                // (drivers/windows.py routes that one attribute here: the element's
                // UIA HelpText already carries the `visible` channel). Read off the
                // live RichEdit document on the UI thread, in linux's JSON shape.
                // No conversations page mounted is a refusal, never an empty read.
                return () =>
                {
                    FaunaApp.App.MachineMethodResult = null;
                    var page = FaunaApp.Views.ConversationsPage.Current;
                    if (page is null)
                    {
                        ReportCommandFailure(action, "the conversations page is not mounted");
                        return Task.CompletedTask;
                    }
                    try
                    {
                        FaunaApp.App.MachineMethodResult =
                            JsonSerializer.SerializeToElement(page.ComposeTextRuns());
                    }
                    catch (Exception ex)
                    {
                        ReportCommandFailure(action, $"{ex.GetType().Name}: {ex.Message}");
                    }
                    return Task.CompletedTask;
                };
            case "conversations_accept_recipient":
                // Test-only escape hatch around the Windows 11 SendInput
                // sandbox: the bridge can't deliver keyboard events to a
                // non-foreground app, so press_key("recipient-picker-input",
                // "Enter") returns "Access is denied". Fire AcceptRequested
                // on whichever recipient picker is currently visible
                // (NewThread or AddParticipant) on the UI thread instead.
                //
                // Driven against the process-wide manager rather than
                // `ConversationsPage.Current` (which this arm used until 2026-08-29).
                // The page hop added nothing — `AcceptVisibleRecipientPicker()` is
                // literally `_vm?.AcceptCurrentRecipientChip()`, and the VM's manager
                // IS `ConversationsManagerHost.Instance` — while its `?.` turned "the
                // conversations page is not mounted" into a SILENT no-op, which is the
                // same convention-11 drop by another route. The manager itself decides
                // which picker is active (add-participant overlay > new-thread compose),
                // exactly as it does for tui, linux and apple, so nothing about the
                // visible-picker choice is lost. Still a post-action on the UI thread:
                // the accept mutates manager state whose observers repaint.
                return async () =>
                {
                    var m = FaunaApp.Conversations.ConversationsManagerHost.Instance;
                    try
                    {
                        // PROBE, then commit — the order the GUI's own on-Enter handler
                        // drives and every sibling app settled on (web
                        // `onRecipientKeydown`, tui, linux `e2e_accept_recipient`, apple
                        // 2026-08-28). Without the probe the commit can only ever use the
                        // format-only `try_parse_typed_address`, which CANNOT produce
                        // `TypedAddress::Fauna` by design (`libs/fauna-conversations/
                        // src/address.rs`), so a typed Fauna handle or 64-hex actor id
                        // commits no chip over the agent while working fine for a real
                        // user. `resolve_recipient` returns immediately when no picker is
                        // open or the input is empty (`manager.rs`), so it is safe
                        // unconditionally.
                        await m.ResolveRecipient();
                        // Convention 11's declining-arm clause. The boolean was DISCARDED
                        // here until 2026-08-29, so an accept against an untouched picker
                        // acked green and surfaced ~5 s later as the action layer's
                        // generic "chip not added", naming neither the command nor the
                        // reason. web, tui, linux and both apple targets all had the same
                        // gap; the sentence is shared so the cross-app pin can assert text
                        // every app agrees on.
                        if (!m.AcceptCurrentRecipientChip())
                        {
                            ReportRefusedCommand(
                                "conversations_accept_recipient", AcceptRecipientNoChipReason);
                        }
                    }
                    catch (Exception ex)
                    {
                        // Was a `System.Diagnostics.Debug.WriteLine` + `return` — the
                        // literal "never in a `.debug` log" convention 11 forbids, and the
                        // one this row was opened on: the driver read a green ack for a
                        // command that threw.
                        ReportCommandFailure(
                            "conversations_accept_recipient", $"{ex.GetType().Name}: {ex.Message}");
                    }
                };
            case "sync_inject_locations":
                // Cross-app E2E bridge for the Settings → Folders page's nested
                // local-folder binding: seed the bound-folder list (the native folder
                // picker can't be driven) by swapping the page VM onto an in-memory pipe
                // fake. Expects {folders:[{path, folder?, mode?}]}. Inject precedes the
                // test's navigate, so the fresh page reads TestPipeOverride in Page_Loaded.
                // See actions/sync_locations.py / file-sync.md § On-Demand Files.
                //
                // The binding moved onto FoldersPage by the 2026-06-28 unification
                // (the standalone SyncFoldersPage was retired).
                {
                    try
                    {
                        if (command.TryGetValue("locations", out var raw) && raw is JsonElement arr)
                        {
                            FaunaApp.Views.FoldersPage.TestLocationChannelOverride =
                                FaunaApp.Core.Services.InMemoryLocationControlChannel.FromJson(arr);
                        }
                        else
                        {
                            // point 11: never silently drop a command. A malformed inject
                            // that left the page on the real channel would read downstream
                            // as "the folder list didn't render" — a product bug that isn't.
                            StampAgentFailure(
                                "[TestAgent] sync_inject_locations: missing or non-array 'folders'");
                        }
                    }
                    catch (Exception ex)
                    {
                        StampAgentFailure(
                            $"[TestAgent] sync_inject_locations threw: {ex.GetType().Name}: {ex.Message}");
                    }
                }
                // Empty postAction forces a UI-thread pass (a barrier for the static
                // write) before ready=true, mirroring the conversations inject cases.
                return () => Task.CompletedTask;
            case "sync_add_location":
            case "sync_remove_location":
                // Bind (or unbind) a local folder to a nest folder over the REAL
                // control channel — the windows leg of linux's identically-named
                // commands (`apps/fauna-linux/src/main.rs`). Unlike the
                // `sync_inject_locations` render fixture above, this drives the agent
                // that actually hosts engines, which is what makes a real-agent
                // "the engine is serving" assertion (`data.sync.running`) reachable
                // on windows at all.
                //
                // The verb pair IS the contract: AddLocation then the ref-keyed SetLocationFolder,
                // exactly what the shared client's `bind_location` sends
                // (`fauna_client_sync::agent`) — the agent hosts an engine per BOUND
                // set, so an add without the bind starts nothing.
                //
                // Always the REAL client, never the page's TestPipeOverride fake: this
                // command's whole purpose is to move the agent that hosts engines, and
                // binding into the render fake would make `data.sync.running` assert
                // against a box with no agent (see AppDataSnapshot.GetSyncForState).
                {
                    var path = CommandString(command, "path");
                    var folder = CommandString(command, "folder");
                    // The set's `FolderRef` wire form — the bind is keyed by it alone,
                    // exactly as the Folders UI's gesture is (`FolderRefForRow`).
                    var folderId = CommandString(command, "folder_id");
                    var removing = action == "sync_remove_location";
                    return async () =>
                    {
                        // testing.md § conventions point 11: HONOUR or FAIL LOUDLY.
                        //
                        // ⚠ It is the CONTROL PLANE this asks for, never `App.CurrentSyncAgent`.
                        // Gating on the agent session is what made this command drop binds:
                        // the session installs ~10 s after login (two nest round-trips inside
                        // SyncAgentSession.CreateAsync), the command is one-shot, and a refusal
                        // taken in that window is never retried. The controller exists from the
                        // first instant of login and records the binding optimistically; the
                        // push follows when the agent attaches. A null here therefore means
                        // genuinely "not signed in", which IS a refusal.
                        var bindings = FaunaApp.App.CurrentLocationBindings;
                        if (bindings is null)
                        {
                            StampAgentFailure(
                                $"[TestAgent] {action}: no authenticated session on this actor "
                                + "— nothing to drive");
                            return;
                        }

                        try
                        {
                            if (removing)
                            {
                                // Linux keys removal by `folder` (its binding model is
                                // set-keyed); windows' UI keys by path. Accept either, so
                                // the shared action layer stays uniform — the model now
                                // offers both selectors.
                                var (paths, _) = !string.IsNullOrEmpty(path)
                                    ? await bindings.RemoveByPathAsync(path)
                                    : await bindings.RemoveBySetAsync(folder ?? "");
                                if (paths.Length == 0)
                                {
                                    StampAgentFailure(
                                        "[TestAgent] sync_remove_location: no bound folder for "
                                        + $"folder='{folder}' path='{path}'");
                                }
                                return;
                            }
                            if (string.IsNullOrEmpty(path))
                            {
                                StampAgentFailure(
                                    $"[TestAgent] {action}: missing 'path'");
                                return;
                            }
                            if (string.IsNullOrEmpty(folder))
                            {
                                // The agent hosts an engine per BOUND set, so an unbound
                                // add starts nothing — and the binding model holds only
                                // bindings. Refusing loudly beats a row that renders but
                                // can never make `running` flip.
                                StampAgentFailure(
                                    $"[TestAgent] sync_add_location({path}): missing 'folder' "
                                    + "— an unbound folder hosts no engine");
                                return;
                            }

                            // Move the LOGIN's model, not a local one: `data.sync.locations`
                            // reports its rendered union (AppDataSnapshot.GetSyncForState),
                            // exactly as linux's `sync_agent::current_locations()` does — which
                            // is the line that separates this real command from the
                            // `sync_inject_locations` render fixture above. The push to the
                            // agent is the controller's reconcile, so a not-yet-installed
                            // session delays it rather than losing it.
                            if (string.IsNullOrEmpty(folderId))
                            {
                                StampAgentFailure(
                                    $"[TestAgent] sync_add_location({path}): missing 'folder_id' "
                                    + "— a binding is keyed by the set's ref, never its name");
                                return;
                            }
                            await bindings.AddAsync(path, folder, folderId);
                        }
                        catch (Exception ex)
                        {
                            StampAgentFailure(
                                $"[TestAgent] {action} threw: {ex.GetType().Name}: {ex.Message}");
                        }
                    };
                }
            case "alert_sweep_wake":
                // Contract: `fauna_e2e_agent::ALERT_SWEEP_WAKE` — end the current
                // identity's re-sweep WAIT so the production loop sweeps again; the
                // caller's barrier is `alert_sweep_passes`, never this ack. Wakes the
                // LOOP (`CriticalAlertsSweep.StartForIdentity`), never a one-shot
                // pass: a one-shot would pass the "announced without a restart"
                // journey with the loop deleted. The tui arm is the reference.
                if (!uniffi.fauna_ffi.FaunaFfiMethods.CriticalAlertSweepWakeForTest())
                {
                    // Convention 11: no loop runs for this identity, so the wake
                    // would be acked and read by nobody.
                    ReportRefusedCommand(action,
                        "no authenticated session, so no sweep loop to wake");
                }
                return null;
            case "reconnect_backoff":
                // Contract: `fauna_e2e_agent::RECONNECT_BACKOFF` — pace this
                // session's reconnect retries (never the `Unreachable` threshold), or
                // restore them with `{}`. The payload is the command's own fields,
                // handed to shared Rust verbatim, which parses and validates them.
                {
                    var fields = new Dictionary<string, object?>(command);
                    fields.Remove("id");
                    fields.Remove("action");
                    try
                    {
                        if (!FaunaApp.App.SetReconnectBackoffForTest(JsonSerializer.Serialize(fields)))
                        {
                            // Convention 11: a pace that did not land leaves the
                            // production one in force, and the journey would spend its
                            // budget waiting.
                            ReportRefusedCommand(action, "no fauna client, so no connection to pace");
                        }
                    }
                    catch (Exception ex)
                    {
                        ReportRefusedCommand(action, $"{ex.GetType().Name}: {ex.Message}");
                    }
                    return null;
                }
            case "family_notify_check_now":
                // Force an immediate Guardian Notify due-check (family-safety.md §
                // Guardian Notify), bypassing the flush timer's real wall-clock wait
                // — testing.md convention 14's run_now poke, the windows twin of
                // web's family-notify-e2e.ts. The real cadence gate
                // (notify_report_min_interval_secs) still applies inside
                // CheckNowAsync; only the tick-interval wait is skipped.
                return async () =>
                {
                    var rpc = FaunaApp.App.CurrentRpc;
                    if (rpc is null)
                    {
                        StampAgentFailure(
                            "[TestAgent] family_notify_check_now: no authenticated session "
                            + "— nothing to drive");
                        return;
                    }
                    try
                    {
                        await FaunaApp.Core.Services.GuardianNotifyCache.CheckNowAsync(rpc);
                    }
                    catch (Exception ex)
                    {
                        StampAgentFailure(
                            $"[TestAgent] family_notify_check_now threw: {ex.GetType().Name}: {ex.Message}");
                    }
                };
            case "screen_time_heartbeat":
                // testing.md convention 14's fake clock + `run_now` poke for the
                // screen-time usage heartbeat (family-safety.md § Screen time,
                // Slice E) — the windows twin of linux `screen_time_heartbeat`
                // (main.rs) / web `$lib/screen-time-e2e.ts` / android
                // `advanceTestClockAndTick`. Advances the ward client's clock by
                // `minutes` of foreground use and runs one production heartbeat
                // step, so a tier_3 journey can prove the budget half WITHOUT
                // waiting on wall-clock time — a test that slept for a real
                // heartbeat would be DEFUNC under § point 14, not merely slow.
                // The cadence and accrual rules themselves are pure and already
                // proven at tier_1 (`fauna_core::screen_time::tests`); this
                // exercises the WIRING — that the client really calls
                // `fauna.family.usage_report` and feeds the reply back into the
                // lock.
                return async () =>
                {
                    var minutes = (int)CommandLong(command, "minutes");
                    var rpc = FaunaApp.App.CurrentRpc;
                    if (rpc is null)
                    {
                        StampAgentFailure(
                            "[TestAgent] screen_time_heartbeat: no authenticated session "
                            + "— nothing to drive");
                        return;
                    }
                    try
                    {
                        await FaunaApp.Core.Services.ScreenTimeCache.AdvanceTestClockAndTickAsync(minutes, rpc);
                    }
                    catch (Exception ex)
                    {
                        StampAgentFailure(
                            $"[TestAgent] screen_time_heartbeat threw: {ex.GetType().Name}: {ex.Message}");
                    }
                };
            case "feed_inject_posts":
                // Cross-app E2E bridge for the Feed page: seed the post list via
                // the shared manager's inject_posts_for_test (test-helpers) so the
                // unverified-source-badge `Failed` arm is reachable (a real nest
                // serves only Unchecked/Verified). Expects {posts:[TestPostSpec...]}.
                // See actions/feed.py / fauna_feed::test_support.
                {
                    try
                    {
                        FaunaApp.Feed.FeedCommands.InjectPosts(command);
                    }
                    catch (Exception ex)
                    {
                        System.Diagnostics.Debug.WriteLine(
                            $"[TestAgent] feed_inject_posts threw: {ex.GetType().Name}: {ex.Message}");
                    }
                }
                // Empty postAction forces a UI-thread pass before ready=true: the
                // manager's set_feed_snapshot_for_test notify() enqueued a snapshot
                // Refresh; FIFO ordering runs it before this action, so the test
                // doesn't race ahead of the new post-card rows (same fix as
                // conversations_inject_inbound).
                return () => Task.CompletedTask;
            case "feed_inject_error":
                // The feed twin of conversations_inject_page_error: stamp
                // FeedSnapshot.error through the shared manager's
                // inject_error_for_test, so `error-message` carries a REAL
                // localized failure carrier rather than a painted string. No
                // product path fails a feed fetch on demand. Expects
                // {key, message}. See actions/feed.py::inject_error_for_test.
                {
                    try
                    {
                        FaunaApp.Feed.FeedCommands.InjectError(command);
                    }
                    catch (Exception ex)
                    {
                        System.Diagnostics.Debug.WriteLine(
                            $"[TestAgent] feed_inject_error threw: {ex.GetType().Name}: {ex.Message}");
                    }
                }
                // Same empty postAction rationale as feed_inject_posts above: the
                // inject's notify() enqueued a snapshot Refresh, and FIFO ordering
                // runs it before this action, so the test cannot poll
                // `error-message` ahead of the paint that fills it.
                return () => Task.CompletedTask;
            case "feed_hold_next_reload":
            case "feed_release_held_reload":
                // Arm / release the feed manager's one-shot reload hold — see
                // FeedCommands.HoldReload. Runs on the poll thread (a plain manager
                // call, no UI touch). Convention 11: a missing manager is said on
                // error-message, never acked as a hold that was armed.
                if (!FaunaApp.Feed.FeedCommands.HoldReload(action == "feed_hold_next_reload"))
                {
                    StampAgentFailure($"[TestAgent] {action}: no feed manager yet (pre-auth, or the Feed page never built one)");
                    return null;
                }
                return () => Task.CompletedTask;
            case "atproto_delegation_advance_clock":
                // Move the delegation row's RENDER clock (never the mint clock —
                // authorize always mints against the real wall clock), so
                // `expiring_soon` / `expired` are reachable without waiting out the
                // real ~90-day window: convention 14's fake clock, never a sleep.
                //
                // Two steps, and BOTH are required. The setter is a process-wide
                // static in the test-helpers-flavoured FFI, but liveness is computed
                // when the machine refreshes — so the offset alone changes nothing
                // until the SAME machine instance the page observes re-reads. The
                // rehydrate hook below is that re-read (the windows twin of linux's
                // `notify_atproto_rehydrate`); without it the row keeps reporting the
                // pre-advance state and `test_lapse_reads_as_reauthorize_here` goes
                // red at ~76 days for a reason no one would find.
                //
                // The setter runs here on the poll thread (a static, no UI touch); the
                // repaint rides the postAction, which is UI-thread and awaited — so
                // the ack lands only after the row has actually re-rendered, and the
                // test's `state`-attr poll cannot read a stale value it then blames on
                // latency.
                {
                    var offsetSecs = CommandLong(command, "now_offset_secs");
                    try
                    {
                        uniffi.fauna_ffi.FaunaFfiMethods.SetDelegationClockOffsetSecs(offsetSecs);
                    }
                    catch (Exception ex)
                    {
                        // Convention 11: never a silent drop. A build whose FFI lacks
                        // the test-helpers setter must say so on error-message rather
                        // than ack a clock that never moved.
                        StampAgentFailure(
                            "[TestAgent] atproto_delegation_advance_clock: "
                            + $"{ex.GetType().Name}: {ex.Message}");
                        return null;
                    }
                    return async () =>
                    {
                        var rehydrate = FaunaApp.Views.AtprotoPage.RehydrateForTest;
                        if (rehydrate is null)
                        {
                            // The AT Protocol page is not mounted, so nothing observes the
                            // clock. Loud, not silent: a test that advanced the clock
                            // while looking at another page would otherwise poll a row
                            // that was never going to move.
                            StampAgentFailure(
                                "[TestAgent] atproto_delegation_advance_clock: the AT Protocol "
                                + "page is not mounted, so no delegation row can rehydrate");
                            return;
                        }
                        await rehydrate();
                    };
                }
            // Moves the co-present ceremony's ADMISSION clock — the `now` a
            // receive-act expectation is minted and judged against
            // (`fauna_sync_engine::ceremony_clock`, read per call by the seat's
            // own listener). The window is a 15-minute Rust constant and never a
            // knob, so a journey can only witness "someone arriving after it has
            // lapsed is refused like a stranger" (`p2p.md` § Offline share
            // initiation) by moving the clock — convention 14's fake clock,
            // never a sleep. `now_offset_secs: 0` resets it; the offset is
            // process-wide and nothing auto-resets it, so a leftover value would
            // lapse the next expectation this process mints. tui's
            // `automation.rs` / linux's `main.rs` carry this arm's full
            // rationale.
            //
            // Nothing to repaint: unlike the delegation clock above, the offset
            // is read at admission time by the seat's own listener, so this
            // command owes no rehydrate.
#if P2P_SHARE
            // Both ceremony arms are the p2p-share member's glue: a store-safe
            // build has no ceremony, so it answers them as unknown commands.
            case "offline_share_advance_clock":
                {
                    var offsetSecs = CommandLong(command, "now_offset_secs");
                    try
                    {
                        uniffi.fauna_ffi.FaunaFfiMethods.OfflineShareAdvanceClock(offsetSecs);
                    }
                    catch (Exception ex)
                    {
                        // Convention 11: never a silent drop. A build whose FFI
                        // lacks the test-helpers setter must say so on
                        // error-message rather than ack a clock that never moved.
                        ReportCommandFailure(action, $"{ex.GetType().Name}: {ex.Message}");
                    }
                    return null;
                }
            // Drops every connection a counterpart has open to THIS seat's
            // ceremony listener, keeping the listener up — the link between two
            // devices failing part-way, which a journey cannot otherwise cause.
            // It is how "the share picks up again without either person entering
            // the code a second time" (`p2p.md` § Offline share initiation) gets
            // a witness. Returns how many connections were dropped, so the
            // journey can prove there was one to drop. With no seat bound it
            // fails loudly (convention 11) rather than reporting a drop that
            // never happened. tui's `automation.rs` / linux's `main.rs` carry
            // this arm's full rationale.
            case "offline_share_drop_connections":
                FaunaApp.App.MachineMethodResult = null;
                if (FaunaApp.App.CurrentOfflineShareSeat is not { } offlineShareSeat)
                {
                    ReportCommandFailure(action, "no ceremony seat is bound");
                    return null;
                }
                FaunaApp.App.MachineMethodResult =
                    JsonSerializer.SerializeToElement(offlineShareSeat.DropConnectionsForTest());
                return null;
#endif
            case "feed_seed_cue_rollup_for_test":
                // Seed a real `cues:v1` nest row (a real network round trip, unlike
                // feed_inject_posts above), so a capture-less test can reach
                // "Clear activity data" with something to actually delete. Awaited:
                // the caller needs the PUT to have landed before it clicks Clear.
                // Expects {content_ids:[string...]}. See actions/feed.py.
                return async () =>
                {
                    try
                    {
                        await FaunaApp.Feed.FeedCommands.SeedCueRollupForTest(command);
                    }
                    catch (Exception ex)
                    {
                        StampAgentFailure(
                            "[TestAgent] feed_seed_cue_rollup_for_test threw: "
                            + $"{ex.GetType().Name}: {ex.Message}");
                    }
                };
            case "focus_move":
                // Convention 17 layer (c), windows leg
                // (`docs/goal/architecture/e2e-systematic-ui-walks.md` § The
                // convention). Mirrors `fauna_e2e_agent::{FOCUS_MOVE,
                // focus_move_request}`: windows cannot link that Rust crate
                // (the same reason apple's FocusWalkTestCommand.swift and
                // android's TestAgent.kt re-spell it), so the payload
                // vocabulary is re-derived here against its one documented
                // home rather than re-invented — an app that re-derives the
                // parse is free to disagree about what `{"times": "3"}`
                // means, and the walk it feeds then measures that
                // disagreement instead of the app.
                //
                // ⚠ A present-but-malformed field is a refusal; only an
                // ABSENT `times` defaults (to 1) — never a silent fallback.
                {
                    var hasDirection = command.TryGetValue("direction", out var directionRaw);
                    var directionStr = directionRaw is JsonElement dje && dje.ValueKind == JsonValueKind.String
                        ? dje.GetString()
                        : null;
                    if (directionStr != "next" && directionStr != "prev")
                    {
                        FaunaApp.App.CurrentErrorMessage =
                            "focus_move: `direction` must be \"next\" or \"prev\", got "
                            + DescribeCommandField(hasDirection ? directionRaw : null);
                        return null;
                    }
                    var direction = directionStr == "next"
                        ? Microsoft.UI.Xaml.Input.FocusNavigationDirection.Next
                        : Microsoft.UI.Xaml.Input.FocusNavigationDirection.Previous;

                    long times = 1;
                    if (command.TryGetValue("times", out var timesRaw))
                    {
                        if (timesRaw is JsonElement tje && tje.ValueKind == JsonValueKind.Number
                            && tje.TryGetInt64(out var parsedTimes) && parsedTimes >= 0)
                        {
                            times = parsedTimes;
                        }
                        else
                        {
                            FaunaApp.App.CurrentErrorMessage =
                                "focus_move: `times` must be a non-negative integer, got "
                                + DescribeCommandField(timesRaw);
                            return null;
                        }
                    }
                    if (times > FocusMoveMaxTimes)
                    {
                        FaunaApp.App.CurrentErrorMessage =
                            $"focus_move: `times` is {times}, above the {FocusMoveMaxTimes} "
                            + "cap — the step loop runs on the thread that serves this agent, "
                            + "so a count that large stalls every later command rather than "
                            + "just this one";
                        return null;
                    }

                    return () =>
                    {
                        // The SAME call WinUI's own Tab/Shift-Tab key handling makes
                        // (`FocusManager.TryMoveFocus`) — never a private seam that sets
                        // a focus index directly, the same door linux's `child_focus`,
                        // apple's `selectNextKeyView` and android's `FocusManager.
                        // moveFocus` also use. A `false` return (nothing to move to) is
                        // a legitimate quiet state, not a refusal — mirrors every other
                        // leg, none of which check this call's return value either.
                        //
                        // ⚠ The parameterless overload throws `COMException:
                        // Catastrophic failure` in a WinUI DESKTOP app (unlike UWP,
                        // there is no implicit XamlRoot context) — the two-arg
                        // overload with an explicit, loaded `SearchRoot` is required
                        // (measured 2026-09-06, walk-sweep's first `focus_move` step).
                        if (FaunaApp.App.MainWindow?.Content is Microsoft.UI.Xaml.DependencyObject searchRoot)
                        {
                            var options = new Microsoft.UI.Xaml.Input.FindNextElementOptions
                            {
                                SearchRoot = searchRoot,
                            };
                            for (var i = 0L; i < times; i++)
                            {
                                Microsoft.UI.Xaml.Input.FocusManager.TryMoveFocus(direction, options);
                            }
                        }
                        return Task.CompletedTask;
                    };
                }
            case "switch_pane":
                // Convention 17 layer (c), windows leg — the other half of the
                // walk vocabulary. Mirrors `fauna_e2e_agent::{SWITCH_PANE,
                // switch_pane_target}`. **windows' own design call** (the item
                // this leg closes flagged it open): `Views/MainPage.xaml`
                // mounts a `NavigationView` — a PERMANENT nav-pane-beside-content
                // split, the same shape tui/linux/macOS/web already implement
                // this command for — so windows implements it too, rather than
                // declaring an absence the way iOS/android's `TabView`/
                // `ModalNavigationDrawer` (no permanent two-region split) do.
                //
                // The mechanism lives on MainPage itself
                // (`MainPage.FindFocusPaneCandidate`) since it needs direct
                // references to the named `NavView`/`ContentFrame` XAML
                // elements; see that method's doc comment for the scoping
                // rules and the landmark-exclusion ruling.
                {
                    var hasPane = command.TryGetValue("pane", out var paneRaw);
                    var paneStr = paneRaw is JsonElement pje && pje.ValueKind == JsonValueKind.String
                        ? pje.GetString()
                        : null;
                    if (paneStr != "page" && paneStr != "sidebar")
                    {
                        FaunaApp.App.CurrentErrorMessage =
                            "switch_pane: `pane` must be \"page\" or \"sidebar\", got "
                            + DescribeCommandField(hasPane ? paneRaw : null);
                        return null;
                    }

                    return async () =>
                    {
                        var mainPage = FaunaApp.Views.MainPage.Current;
                        if (mainPage is null)
                        {
                            FaunaApp.App.CurrentErrorMessage =
                                $"switch_pane: no `{paneStr}` region is mounted on the page "
                                + "currently on screen";
                            return;
                        }
                        var candidate = mainPage.FindFocusPaneCandidate(paneStr!);
                        if (candidate is null)
                        {
                            // No focusable descendant is a legitimate, silent no-op — not
                            // a refusal. It means this region's current CONTENT has
                            // nothing to focus, not that the region or the command is
                            // unimplemented (mirrors apple's applySwitchPane ruling).
                            return;
                        }
                        var result = await Microsoft.UI.Xaml.Input.FocusManager.TryFocusAsync(
                            candidate, Microsoft.UI.Xaml.FocusState.Keyboard);
                        if (!result.Succeeded)
                        {
                            // A candidate the real traversal engine found, but that then
                            // refused first responder — mirrors apple's
                            // `guard window.makeFirstResponder(target) else return refusal`.
                            FaunaApp.App.CurrentErrorMessage =
                                $"switch_pane: the `{paneStr}` region's focus candidate "
                                + "refused to take keyboard focus";
                        }
                    };
                }
            case "barrier_probe":
                // The barrier's self-test probe (`fauna_e2e_agent::BARRIER_PROBE`):
                // queue UI work on the **DispatcherQueue** — the same queue real
                // deferred UI work rides — and let the normal ack fire WITHOUT
                // waiting for it (hence the `return null` below, which leaves
                // `_ready` true so PollLoopAsync acks on this pass). The early ack
                // is the point: only a correct `barrier` can make the token
                // observable.
                {
                    var token = CommandString(command, "token");
                    if (token.Length == 0)
                    {
                        // Convention 11: a token-less probe would ack green and
                        // prove nothing — the silent drop wearing a disguise.
                        StampAgentFailure(
                            "[TestAgent] barrier_probe: payload needs a non-empty `token`");
                        return null;
                    }
                    if (_dispatcherQueue is null)
                    {
                        // Without a queue, RunOnUiThread runs the items INLINE on
                        // this poll thread — they would already be applied when the
                        // probe acks, and the self-test would pass against a
                        // `barrier` that does nothing. Refuse loudly instead.
                        StampAgentFailure(
                            "[TestAgent] barrier_probe: no DispatcherQueue — the probe cannot enqueue");
                        return null;
                    }
                    // Mirrors `fauna_e2e_agent::BARRIER_PROBE_DEFAULT_COUNT`. A BATCH,
                    // not one item: against a single queued item a do-nothing barrier
                    // is a coin flip rather than a proof.
                    var count = 64;
                    if (command.TryGetValue("count", out var countEl)
                        && countEl is JsonElement ce && ce.ValueKind == JsonValueKind.Number
                        && ce.TryGetInt32(out var parsedCount) && parsedCount > 0)
                    {
                        count = parsedCount;
                    }
                    for (var i = 0; i < count; i++)
                    {
                        // `fauna_e2e_agent::barrier_probe_value` — the ONE place the
                        // "<token>#<i>" shape is spelled, mirrored here because the
                        // C# app has no seam onto that crate's constants.
                        var value = $"{token}#{i}";
                        RunOnUiThread(() => FaunaApp.App.RecordBarrierProbe(value));
                    }

                    // The FUSED form (`fauna_e2e_agent::BARRIER_PROBE_FUSE_FIELD`,
                    // payload `{"barrier": true}`): enqueue the batch and then run
                    // this app's own BARRIER mechanism, all before acking THIS one
                    // command. This is the form that actually GRADES the barrier —
                    // the two-command shape leaves a driver round trip in which most
                    // apps' queues drain unaided, so a do-nothing barrier passes it.
                    // Reaching the barrier here is just returning the same postAction
                    // the `barrier` arm returns: PollLoopAsync will clear `ready`,
                    // enqueue it behind the items above, and ack only once it has run.
                    if (command.TryGetValue(
                            "barrier", out var fuseEl)
                        && fuseEl is JsonElement fe
                        && fe.ValueKind == JsonValueKind.True)
                    {
                        return () =>
                        {
                            FaunaApp.App.FreezeBarrierAckProbe();
                            return Task.CompletedTask;
                        };
                    }
                }
                // Un-fused: ack EARLY by construction (`_ready` is still true, so
                // PollLoopAsync acks on this pass while the batch is still queued).
                // That asymmetry is the two-command test's whole premise.
                //
                // ⚠ It is also a TRAP for whoever debugs this arm next, and it caught
                // the session that built it. When `test_barrier_waits_for_work_
                // enqueued_before_it[windows]` was red while its fused twin passed,
                // this asymmetry was the obvious suspect — the un-fused arm is the one
                // whose ack rides the poll loop's own push instead of a post-action's.
                // It was not the cause. Both arms were fine; EVERY push was taking 6 s
                // in the state provider (a blocking sync-agent IPC probe — since fixed,
                // see `AppDataSnapshot.GetSyncForState` and
                // e2e-latency-independent-assertions.md § Implementation status today),
                // so only the arm with a second push to ride ever landed inside the 5 s
                // ack budget. The fused
                // twin passing did not mean this arm was broken; it meant the fused
                // twin had a spare push. Before touching either arm, phase-time
                // `PushStateAsync` — it prints provider/serialize/post separately for
                // exactly this reason.
                return null;
            case "barrier":
                // Convention 14's causal anchor (`fauna_e2e_agent::BARRIER`): ack
                // only once all UI-thread work enqueued BEFORE this command has run.
                //
                // The windows mechanism is a DispatcherQueue round trip, and the
                // agent's existing post-action rail already is one: a non-null
                // postAction makes PollLoopAsync clear `_ready`, `TryEnqueue` the
                // continuation, and flip `_ready` back only in that continuation's
                // `finally` — so the ack lands strictly after every item the probe
                // (or any real UI work) enqueued earlier, the queue being FIFO.
                //
                // ⚠ The freeze MUST happen here, inside the continuation, and NOT in
                // this switch body: `ProcessCommand` runs on the agent poll thread,
                // where a "what has the UI applied so far" read is both a
                // cross-thread read and, worse, taken before the queue has drained.
                //
                // ⚠ What grades this, and what does not. The TWO-COMMAND test
                // (`test_barrier_waits_for_work_enqueued_before_it`) does NOT pin this
                // mechanism on windows: the probe and the barrier arrive as separate
                // commands a whole ≥200 ms poll-loop iteration plus two HTTP hops
                // apart, and the DispatcherQueue drains in that gap unaided — the same
                // structural reason linux's (M4) and web's (M5) mutants survived
                // (`e2e-conventions.md` § convention 14). The FUSED probe above is what
                // pins it: same rail, no inter-command gap, so an early ack freezes
                // `None` and reds the assertion.
                // So do NOT "simplify" either site to an immediate ack — and if you are
                // grading this leg, the fused test is the one that can tell.
                return () =>
                {
                    FaunaApp.App.FreezeBarrierAckProbe();
                    return Task.CompletedTask;
                };
            case "serve_enable_folder":
                // WebDAV read+write tier_3 e2e
                // (tests/e2e-unified/tests/test_webdav_read_write_roundtrip.py):
                // arrange the served-set precondition for the currently
                // logged-in actor — windows twin of linux's
                // `serve_enable_folder` (apps/fauna-linux/src/main.rs), wired
                // over the SAME production slice-6b composition the Folders
                // page's serve toggle drives
                // (App.WebdavServeEnableFolderForTest ->
                // INestRpcClient.FoldersServeSetAsync -> the shared
                // FoldersAuthor::serve_set), never the raw FFI unseal door.
                // Writes the outcome into App.WebdavServeReply; clears any
                // prior reply first so the driver detects THIS run's
                // completion (same shape as `custodian_pull_run_now` above).
                // Payload: {folder, create} — `create` defaults true, the e2e
                // wants a fresh empty served set.
                {
                    FaunaApp.App.WebdavServeReply = null;
                    var folder = CommandString(command, "folder");
                    var create = CommandBool(command, "create", true);
                    if (string.IsNullOrEmpty(folder))
                    {
                        FaunaApp.App.WebdavServeReply = new Dictionary<string, object?>
                        {
                            ["ok"] = false,
                            ["error"] = "serve_enable_folder requires a non-empty folder",
                        };
                        return null;
                    }
                    return async () =>
                    {
                        var (ok, servedSets, error) =
                            await FaunaApp.App.WebdavServeEnableFolderForTest(folder, create);
                        FaunaApp.App.WebdavServeReply = ok
                            ? new Dictionary<string, object?>
                            {
                                ["ok"] = true,
                                ["served_sets"] = (long)servedSets,
                            }
                            : new Dictionary<string, object?>
                            {
                                ["ok"] = false,
                                ["error"] = error,
                            };
                    };
                }
            case "enable_caldav_mailbox":
                // Slice C (test_caldav_autoschedule_mailbox_less.py): mint the
                // currently logged-in actor's shared MSEK via the CalDAV-enable
                // recipe so a mailbox-less GUI attendee's `NestSchedulingSink`
                // can materialize a server-side auto-schedule invite. Windows
                // twin of linux's `enable_caldav_mailbox` case
                // (apps/fauna-linux/src/main.rs). Writes the outcome into
                // App.CaldavMailboxReply; clears any prior reply first so the
                // driver detects THIS run's completion (same shape as
                // `serve_enable_folder` above). Payload: {password} — omitted
                // generates one.
                {
                    FaunaApp.App.CaldavMailboxReply = null;
                    var password = CommandString(command, "password");
                    return async () =>
                    {
                        var (ok, error) = await FaunaApp.App.EnableCaldavMailboxForTest(
                            string.IsNullOrEmpty(password) ? null : password);
                        FaunaApp.App.CaldavMailboxReply = ok
                            ? new Dictionary<string, object?> { ["ok"] = true }
                            : new Dictionary<string, object?>
                            {
                                ["ok"] = false,
                                ["error"] = error,
                            };
                    };
                }
            default:
                // e2e-conventions.md § convention 11: a test agent must HONOUR a
                // command or FAIL LOUDLY — never silently drop one. An unrecognized
                // action here (a new/renamed bridge command this app's switch hasn't
                // been taught yet) once fell through with no error AND no effect,
                // which reads exactly like a downstream product bug (e.g. "the bubble
                // never rendered") rather than the harness gap it actually is. The
                // catch-all refusal is pinned cross-app by
                // `tests/test_agent_refuses_unknown_command.py`, which asserts the
                // text NAMES the action — keep the interpolation.
                ReportRefusedCommand(action, $"no arm for this action (command {cmdId})");
                break;
        }

        if (payload == null) return null;

        // Command handler modifies plain fields + credential store — no UI thread needed.
        // It returns an optional Action for deferred navigation (needs UI thread).
        try { return _commandHandler?.Invoke(payload); }
        catch (Exception ex)
        {
            // Convention 11, and the widest-blast-radius instance of it in this file:
            // `_commandHandler` is App.HandleTestCommand, so EVERY state-protocol
            // `patch`/`reset`/`logout` runs through here. A throw used to reach a
            // `.debug` line and then `return null`, which acks green — so a set_state
            // that half-applied surfaced as whatever the next assertion happened to
            // read. Name the action, not just the message: `patch` is the commonest
            // one and says nothing on its own about which block threw.
            ReportCommandFailure(action, $"{ex.GetType().Name}: {ex.Message}");
            return null;
        }
    }

    private void RunOnUiThread(Action action)
    {
        if (_dispatcherQueue != null)
            _dispatcherQueue.TryEnqueue(() => action());
        else
            action();
    }

    /// <summary>A throwaway <c>OnboardingMachine</c>'s observer for the
    /// call_machine_method free-dispatch fallback above — nothing ever calls
    /// <c>AddObserver</c> on that instance, so <c>OnChanged</c> is unreachable; this
    /// exists only because the constructor requires SOME observer.</summary>
    private sealed class NoOpOnboardingObserver : uniffi.fauna_onboarding_machine.OnboardingObserver
    {
        public void OnChanged() { }
    }
}
#endif
