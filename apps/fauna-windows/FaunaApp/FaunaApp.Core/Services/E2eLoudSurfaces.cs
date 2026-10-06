using System.Collections.Generic;
using System.Linq;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The two loud-surface counters windows publishes for convention 14 —
/// <c>state.connection_reports</c> (<c>fauna_e2e_agent::CONNECTION_REPORTS_KEY</c>)
/// and <c>state.painted_errors</c> (<c>fauna_e2e_agent::PAINTED_ERRORS_KEY</c>),
/// which own the cross-app contracts. All counting is shared Rust: each is ONE
/// process-wide instance of the <c>fauna-ffi</c> <c>test-helpers</c> wrapper
/// (<c>FfiConnectionReportsForTest</c> / <c>FfiPaintedErrorTallyForTest</c>, over
/// <c>fauna-e2e-contract</c>'s <c>ConnectionReports</c> / <c>PaintedErrorTally</c>),
/// so windows counts with the same code as tui, linux and web.
///
/// <para><b>Same convention-15 shape as <see cref="E2eSessionCounters"/></b>: a
/// gated real plus a same-signature production twin. The feed call sites are
/// production code (the connection indicator's handler), and the wrapper types
/// exist only in the <c>windows-ffi-test</c> flavor's bindings, so an ungated
/// reference would not compile in Release at all.</para>
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

#else

    // Production twins. Same signatures, no-ops — the agent that would publish
    // these is itself compiled out of a Release build.
    internal static void ObserveConnectionReport(FfiConnectionState state) { }
    internal static string ConnectionReportsJson() => "{}";
    internal static void ObservePaintedFrame(IEnumerable<(string Id, string Text)> frame) { }
    internal static string PaintedErrorsJson() => "{}";

#endif
}
