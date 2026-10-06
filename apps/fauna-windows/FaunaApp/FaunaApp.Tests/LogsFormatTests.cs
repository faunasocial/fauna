using System.Collections.Generic;
using System.Linq;
using FaunaApp.Core.Logs;
using uniffi.fauna_log;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Cross-language conformance tests for the Windows Logs adapter
/// (<see cref="LogsFormat"/>) — each method forwards to the shared
/// <c>fauna_log::format</c> Rust over UniFFI, and <c>dotnet test</c> loads the
/// <b>real</b> native <c>fauna_ffi</c> dll, so these assert the C# adapter and the
/// shared formatter agree (observability.md § Surfaces). The form/filter logic
/// itself is unit-tested once in Rust (<c>fauna-log</c>'s <c>format::tests</c>);
/// this pins the Windows surface to it. Both the Settings → Logs page and the
/// admin Logs page render through these.
/// </summary>
public class LogsFormatTests
{
    private static LogEntry Entry(ulong ts, LogLevel level, string target = "t", string message = "m")
        => new LogEntry(ts, level, target, message);

    [Fact]
    public void LevelForIndex_MapsSeverities()
    {
        // LogLevel is an internal UniFFI type, so it can't be a public [Theory]
        // parameter — assert each mapping in the body instead.
        Assert.Null(LogsFormat.LevelForIndex(0));      // "All" → no threshold
        Assert.Equal(LogLevel.Error, LogsFormat.LevelForIndex(1));
        Assert.Equal(LogLevel.Warn, LogsFormat.LevelForIndex(2));
        Assert.Equal(LogLevel.Info, LogsFormat.LevelForIndex(3));
        Assert.Equal(LogLevel.Debug, LogsFormat.LevelForIndex(4));
        Assert.Equal(LogLevel.Trace, LogsFormat.LevelForIndex(5));
        Assert.Null(LogsFormat.LevelForIndex(99));     // out of range → no threshold
    }

    [Fact]
    public void FilterEntries_IsASubsetKeyedOnSeverity()
    {
        var entries = new List<LogEntry>
        {
            Entry(1, LogLevel.Error, message: "e"),
            Entry(2, LogLevel.Info, message: "i"),
        };
        // No threshold ⇒ every entry.
        Assert.Equal(2, LogsFormat.FilterEntries(entries, null).Count);
        // Error threshold ⇒ only the Error line (Error is most severe).
        var errors = LogsFormat.FilterEntries(entries, LogLevel.Error);
        Assert.Single(errors);
        Assert.Equal("e", errors[0].message);
    }

    [Fact]
    public void FilterEntries_KeepsEntriesAtOrAboveThreshold()
    {
        var entries = new List<LogEntry>
        {
            Entry(1, LogLevel.Error, message: "err"),
            Entry(2, LogLevel.Warn, message: "warn"),
            Entry(3, LogLevel.Info, message: "info"),
            Entry(4, LogLevel.Debug, message: "debug"),
            Entry(5, LogLevel.Trace, message: "trace"),
        };
        // Info threshold keeps Error + Warn + Info (rank ≤ Info), drops Debug/Trace,
        // and preserves the oldest-first input order.
        var infoAndUp = LogsFormat.FilterEntries(entries, LogLevel.Info);
        Assert.Equal(new[] { "err", "warn", "info" }, infoAndUp.Select(e => e.message));
    }

    [Fact]
    public void FormatLine_CarriesLevelTargetAndMessage()
    {
        var line = LogsFormat.FormatLine(Entry(0, LogLevel.Warn, target: "fauna_app::sync", message: "hi"));
        Assert.Contains("WARN", line);
        Assert.Contains("fauna_app::sync", line);
        Assert.Contains("hi", line);
        Assert.Contains(" · ", line);
    }

    [Fact]
    public void RenderedText_IsNewestFirst()
    {
        // Input is oldest-first (as fauna_log returns it); the copy payload is newest-first.
        var entries = new List<LogEntry>
        {
            Entry(1, LogLevel.Info, message: "older"),
            Entry(2, LogLevel.Info, message: "newer"),
        };
        var text = LogsFormat.RenderedText(entries);
        var lines = text.Split('\n');
        Assert.Equal(2, lines.Length);
        Assert.Contains("newer", lines[0]);
        Assert.Contains("older", lines[1]);
    }
}
