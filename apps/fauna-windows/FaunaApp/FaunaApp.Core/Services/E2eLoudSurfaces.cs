#if DEBUG || FAUNA_E2E_AGENT
using System.Collections.Generic;
using System.Linq;
#endif
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The loud-surface logs windows publishes for convention 14 —
/// <c>state.connection_reports</c> (<c>fauna_e2e_agent::CONNECTION_REPORTS_KEY</c>),
/// <c>state.painted_errors</c> (<c>fauna_e2e_agent::PAINTED_ERRORS_KEY</c>) and
/// <c>state.knock_notices</c> (<c>fauna_e2e_agent::KNOCK_NOTICES_KEY</c>), which own
/// the cross-app contracts. All counting is shared Rust: each is ONE process-wide
/// instance of the <c>fauna-ffi</c> <c>test-helpers</c> wrapper
/// (<c>FfiConnectionReportsForTest</c> / <c>FfiPaintedErrorTallyForTest</c> /
/// <c>FfiKnockNoticesForTest</c>, over <c>fauna-e2e-contract</c>'s
/// <c>ConnectionReports</c> / <c>PaintedErrorTally</c> / <c>KnockNotices</c>), so
/// windows counts with the same code as tui, linux, android, web and apple.
///
/// <para><b>Same convention-15 shape as <see cref="E2eSessionCounters"/></b>: a
/// gated real plus a same-signature production twin for each member production
/// code calls — <see cref="ObserveConnectionReport"/>, fed by the connection
/// indicator's handler, and <see cref="ObserveKnockNotice"/> /
/// <see cref="RecordKnockNotice"/>, fed by the knock toast. The wrapper types exist only in the
/// <c>windows-ffi-test</c> flavor's bindings, so an ungated reference would not
/// compile in Release at all; every other member is called only from gated code
/// and so has no twin.</para>
///
/// <para>Both wrappers lock internally (a Rust <c>Mutex</c>), so the feeds (UI
/// thread) and the reads (the agent's poll thread) need no lock here.</para>
/// </summary>
internal static class E2eLoudSurfaces
{
#if DEBUG || FAUNA_E2E_AGENT

    private static readonly FfiConnectionReportsForTest ConnectionReports = new();
    private static readonly FfiPaintedErrorTallyForTest PaintedErrors = new();

    /// <summary>
    /// Count one connection-state value the indicator received — EVERY value,
    /// repeats included: a repeat is a report and never a transition, which is
    /// exactly how "further failed attempts left 'Cannot connect' standing" is
    /// told apart from "nothing happened" (<c>transport-connection.md</c> §
    /// <c>Unreachable</c>).
    /// </summary>
    internal static void ObserveConnectionReport(FfiConnectionState state) =>
        ConnectionReports.Observe(state);

    /// <summary>The <c>connection_reports</c> state value, as JSON text.</summary>
    internal static string ConnectionReportsJson() => ConnectionReports.Json();

    /// <summary>
    /// Record one painted frame's elements as (test id, visible text). Pass every
    /// id-bearing element or only the error-shaped ones — the shared tally
    /// re-applies the error-surface predicate and drops empty text itself.
    /// </summary>
    internal static void ObservePaintedFrame(IEnumerable<(string Id, string Text)> frame) =>
        PaintedErrors.Observe(frame.Select(e => new FfiPaintedElement(e.Id, e.Text)).ToArray());

    /// <summary>The <c>painted_errors</c> state value, as JSON text.</summary>
    internal static string PaintedErrorsJson() => PaintedErrors.Json();

    private static readonly FfiKnockNoticesForTest KnockNotices = new();

    /// <summary>
    /// A knock push reached the knock-toast arm — called BEFORE every gate (the
    /// notification host, the muted-word gate), so <c>seen</c> counts every push
    /// the arm received (<c>fauna_e2e_agent::KNOCK_NOTICES_KEY</c>,
    /// <c>notifications</c> outcome 15).
    /// </summary>
    internal static void ObserveKnockNotice() => KnockNotices.Observe();

    /// <summary>
    /// A knock toast was handed to the platform: the catalog key its text resolved
    /// from (<c>null</c> for a verbatim text), the painted text, and the shared id
    /// its tag is spelled from. Called only at the firing site, after every
    /// suppression and only once <c>Show</c> returned, so an entry means "the user
    /// was shown this".
    /// </summary>
    internal static void RecordKnockNotice(string? key, string text, uint replaceId) =>
        KnockNotices.Record(key, text, replaceId);

    /// <summary>The <c>knock_notices</c> state value, as JSON text.</summary>
    internal static string KnockNoticesJson() => KnockNotices.Json();

#else

    // The production twins of the members a production path feeds (the connection
    // indicator's handler, MainViewModel; the knock toast, NotificationService).
    // Same signatures, no-ops — the agent that would publish the counts is itself
    // compiled out of a Release build. The readers and the painted-frame feed have
    // no twin: their only callers (the agent's state serializer,
    // Testing/PaintedErrorObserver) are gated with them.
    internal static void ObserveConnectionReport(FfiConnectionState state) { }

    internal static void ObserveKnockNotice() { }

    internal static void RecordKnockNotice(string? key, string text, uint replaceId) { }

#endif
}
