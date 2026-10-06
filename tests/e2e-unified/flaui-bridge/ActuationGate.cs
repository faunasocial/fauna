namespace FauiBridge;

/// <summary>
/// An actuation route must not drive a control the UI has DISABLED
/// (<c>docs/goal/architecture/e2e-conventions.md</c> § convention 11, one layer
/// down from the command table).
///
/// <para>This is the C# half of the gate the Rust hosts share as
/// <c>fauna_e2e_agent::gate_actuation</c> (tui, linux) and apple's Swift
/// <c>InProcessAutomationServer</c> implements natively. windows' automation
/// server is this out-of-process FlaUI bridge rather than an in-app agent, so it
/// cannot CALL the shared crate — it mirrors its shape instead: the same three
/// environment variables, the same marker text, the same 409 refusal wording.
/// That is deliberate and is the whole reason the constants are copied rather
/// than invented: one sweep invocation spans every app, and one grep spans their
/// logs.</para>
///
/// <para><b>Why the bridge and not the app.</b> Every windows actuation is a UIA
/// call this process makes against the app's automation tree, and UIA's
/// <c>IsEnabled</c> is already the effective, ancestor-inclusive read that apple
/// had to assemble by hand as <c>AutomationRegistry.folding</c> — a control greyed
/// only by a disabled ancestor reads disabled here too. So windows needs no
/// registration surface for the predicate, for the same reason GTK's
/// <c>is_sensitive()</c> spared linux one.</para>
///
/// <para><b>Everything here is pure except <see cref="Record"/>.</b> The verdict,
/// the marker text and the refusal wording are functions of their arguments, so
/// <see cref="SelfTest.ActuationGateChecks"/> pins them with no UIA element, no
/// window and no app.</para>
/// </summary>
internal sealed class ActuationGate
{
    /// <summary>Environment opt-OUT of refusal, for a host whose gate is already
    /// the default. Set by pytest's <c>--permissive-actuation</c>.</summary>
    public const string PermissiveEnv = "FAUNA_E2E_PERMISSIVE_ACTUATION";

    /// <summary>Environment opt-IN to refusal, for a host still STAGING its gate.
    /// windows staged until 2026-09-14 and refuses by default now, so here the flag
    /// only changes a gate built with a non-default <c>defaultStrict</c>; it stays
    /// because the three names are the shared cross-app set, not windows'
    /// own.</summary>
    public const string StrictEnv = "FAUNA_E2E_STRICT_ACTUATION";

    /// <summary>Run-scoped file every launch appends its violation markers to. The
    /// bridge's stderr cannot serve this alone: it is a bounded deque in the driver
    /// and the app relaunches at every module boundary
    /// (<c>helpers/module_relaunch.py</c>), so stderr markers do not survive a
    /// sweep. Set by pytest's <c>--actuation-log</c>.</summary>
    public const string LogEnv = "FAUNA_E2E_ACTUATION_LOG";

    /// <summary>The greppable marker a permissive-mode violation writes.
    /// Deliberately the same text apple and the Rust hosts log, so ONE grep spans
    /// every app's sweep. Do not prefix it per app.</summary>
    public const string Marker = "DISABLED-ACTUATION";

    /// <summary>Does windows REFUSE a disabled actuation by default? <b>Yes, since
    /// 2026-09-14.</b>
    ///
    /// <para>windows staged first, in convention 11's order: land the refusal,
    /// sweep the whole suite PERMISSIVELY so one run enumerates every offender with
    /// no new red, triage that list to empty, <i>"and only then make refusal the
    /// default"</i>. A strict sweep reds its test, and a red test stops, so it
    /// reports at most the first offender per test and hides the rest. The
    /// 2026-09-11 chunked sweep (11 of 11 chunks, its known-positive markers
    /// present) marked three calls: the probe's own two, and
    /// <c>test_conversation_room_roles.py</c> asserting that a plain member's
    /// Remove is refused — a deliberate drive, not an offender.</para>
    ///
    /// <para><b>The permissive mode is kept</b> (<c>--permissive-actuation</c>): it
    /// is the instrument the next broad change to this app re-measures itself with,
    /// not staging scaffolding to delete. <see cref="SelfTest.ActuationGateChecks"/>
    /// pins this polarity, because <see cref="ShouldRefuse"/> cannot.</para></summary>
    public const bool WindowsRefusesDisabledActuationByDefault = true;

    /// <summary>Is refusal ON for this session?</summary>
    public bool Strict { get; }

    /// <summary>The run-scoped marker sink, or null when no sweep is harvesting.</summary>
    public string? LogPath { get; }

    /// <summary>Where a marker also goes so it reaches the driver's captured
    /// bridge stderr. Wired by <c>Program</c>; null off a bridge run, which is what
    /// lets the self-test construct a gate with no console at all.</summary>
    public Action<string>? Sink { get; init; }

    public ActuationGate(bool strict, string? logPath = null)
    {
        Strict = strict;
        LogPath = logPath;
    }

    /// <summary>The gate a session runs under, read from the launch environment the
    /// driver posted to <c>/session</c>.
    ///
    /// <para>Reading the SESSION env rather than this process's own is what makes
    /// windows' wiring identical to every other app's: pytest's
    /// <c>_apply_actuation_mode_env</c> writes the flags into the launch env dict,
    /// and <c>SessionManager.Launch</c> already picks its own keys
    /// (<c>FAUNA_E2E_DATA_DIR</c>, <c>FAUNA_E2E_SESSION_EPOCH</c>) out of the same
    /// dictionary. A bridge-process env would have needed a second, windows-only
    /// plumbing path that no conftest hook feeds.</para></summary>
    public static ActuationGate FromEnvironment(
        IReadOnlyDictionary<string, string>? environment,
        Action<string>? sink = null,
        bool defaultStrict = WindowsRefusesDisabledActuationByDefault)
    {
        string? logPath = null;
        environment?.TryGetValue(LogEnv, out logPath);
        return new ActuationGate(
            StrictFor(environment, defaultStrict),
            string.IsNullOrEmpty(logPath) ? null : logPath)
        {
            Sink = sink,
        };
    }

    /// <summary>Refusal on or off, given the environment and the HOST's stance.
    ///
    /// <para><b>Permissive wins when both are set</b>, exactly as in the Rust half:
    /// the enumerating run must never be the run that turns red, or the measurement
    /// is lost along with the offender list.</para></summary>
    public static bool StrictFor(IReadOnlyDictionary<string, string>? environment, bool defaultStrict)
    {
        if (NonEmpty(environment, PermissiveEnv)) return false;
        if (NonEmpty(environment, StrictEnv)) return true;
        return defaultStrict;
    }

    private static bool NonEmpty(IReadOnlyDictionary<string, string>? environment, string key)
    {
        if (environment is null) return false;
        return environment.TryGetValue(key, out var value) && !string.IsNullOrEmpty(value);
    }

    /// <summary>The marker line one violation writes, in both the log file and the
    /// bridge's stderr. Pure, so the format is pinned without touching a file.
    /// Byte-identical to <c>fauna_e2e_agent::disabled_actuation_marker</c>.</summary>
    public static string MarkerLine(string route, string id, int index)
        => $"[InProcessAutomation] {Marker} {route} id={id} index={index}";

    /// <summary>The refusal message for driving a control the UI has disabled.
    ///
    /// <para><b>409, never 404</b> — the element WAS found; its state is the
    /// problem. A 404 sends <c>drivers/http_bridge.py</c> into its scroll-retry
    /// loop and the test dies on a <c>LookupError</c> reading "not rendered yet",
    /// which is the opposite diagnosis. The message names the element, the index
    /// and the route so the failure diagnoses itself (convention 6) and one grep
    /// finds every instance. Word-for-word
    /// <c>fauna_e2e_agent::disabled_actuation_refusal</c>, because
    /// <c>http_bridge.py</c> discriminates this 409 from <c>select</c>'s
    /// option-not-offered 409 on the text alone.</para></summary>
    public static string RefusalMessage(string route, string id, int index)
        => $"element is disabled: {id}[{index}] — {route} refused (convention 11: an "
         + "actuation route must not drive a control the UI has disabled)";

    /// <summary>The verdict, with no I/O of any kind: true to refuse, false to
    /// drive.
    ///
    /// <para>Permissive mode still DRIVES the control — that is what makes ONE
    /// sweep enumerate EVERY offender.</para></summary>
    public static bool ShouldRefuse(bool enabled, bool strict) => !enabled && strict;

    /// <summary>The one entry point an actuation route calls before driving:
    /// record any violation, then refuse or return.
    ///
    /// <para>Call it AFTER resolving the element and the route's own structural
    /// check, and BEFORE actuating. Gating click alone is not compliance — typing
    /// into a disabled field is the same illegal act. Read routes
    /// (<c>text</c>/<c>visible</c>/<c>count</c>/<c>enabled</c>/<c>attr</c>) must NOT
    /// be gated: reading a disabled control is exactly how a test asserts that it
    /// <i>is</i> disabled. <c>scroll-into-view</c> is viewport positioning, not
    /// actuation, and stays ungated for the same reason.</para>
    ///
    /// <para><paramref name="enabled"/> must be read LIVE, per request, off the
    /// element this call is about to drive.</para></summary>
    /// <exception cref="DisabledActuationException">in strict mode, for a disabled
    /// control. <c>Program</c> answers it 409.</exception>
    public void Check(string route, string id, int index, bool enabled)
    {
        if (enabled) return;
        Record(route, id, index);
        if (ShouldRefuse(enabled, Strict))
            throw new DisabledActuationException(route, id, index);
    }

    /// <summary>Write one marker to the run-scoped log and the bridge's stderr.
    ///
    /// <para>Best-effort is deliberate and load-bearing: an unwritable sink must
    /// never fail the run under test. The measurement is worth less than the
    /// run.</para></summary>
    private void Record(string route, string id, int index)
    {
        var line = MarkerLine(route, id, index);
        try { Sink?.Invoke(line); } catch { /* a marker never breaks an action */ }
        if (LogPath is null) return;
        try
        {
            File.AppendAllText(LogPath, line + Environment.NewLine);
        }
        catch { /* ditto */ }
    }
}

/// <summary>An actuation route was asked to drive a control the UI has disabled,
/// and this session refuses (<see cref="ActuationGate"/>). Carries the route, id
/// and index so <c>Program</c> can answer the same 409 body every other app's
/// agent does.</summary>
internal sealed class DisabledActuationException : Exception
{
    public string Route { get; }
    public string Id { get; }
    public int Index { get; }

    public DisabledActuationException(string route, string id, int index)
        : base(ActuationGate.RefusalMessage(route, id, index))
    {
        Route = route;
        Id = id;
        Index = index;
    }
}
