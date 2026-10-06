using System.Collections.Generic;
using System.Linq;
using FaunaApp.Core.Logs;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_log;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit tests for the Settings → Logs VM (<see cref="LogsViewModel"/>)
/// over a fake <see cref="ILogRing"/> seam (observability.md § Surfaces; no live FFI
/// ring, which is process-global + racy under parallel xUnit, and no FlaUI, which
/// flakes on win-arm64). Mirrors linux <c>settings/logs.rs</c>: load reads the ring,
/// the filter narrows via <c>snapshot_at_least</c>, clear empties the live view.
/// </summary>
public class LogsViewModelTests
{
    /// <summary>A fake process-global ring: holds a backing list, applies the
    /// severity threshold itself (the <c>snapshot_at_least</c> contract), and tracks
    /// clears — independent of the SUT's <see cref="LogsFormat"/>.</summary>
    private sealed class FakeLogRing : ILogRing
    {
        public readonly List<LogEntry> Backing = new();
        public int ClearCount;

        public IReadOnlyList<LogEntry> Snapshot(LogLevel? min)
            => min is LogLevel m ? Backing.Where(e => (int)e.level <= (int)m).ToList() : Backing.ToList();

        public void Clear()
        {
            ClearCount++;
            Backing.Clear();
        }
    }

    private static LogEntry Entry(ulong ts, LogLevel level, string message = "m")
        => new LogEntry(ts, level, "fauna_app", message);

    [Fact]
    public void Load_PopulatesEntriesFromTheRing()
    {
        var ring = new FakeLogRing();
        ring.Backing.Add(Entry(1, LogLevel.Info, "startup"));
        ring.Backing.Add(Entry(2, LogLevel.Error, "boom"));
        var vm = new LogsViewModel(ring);

        vm.Load();

        Assert.Equal(2, vm.Entries.Count);
        Assert.Null(vm.Error);
    }

    [Fact]
    public void Load_RendersNewestFirst()
    {
        var ring = new FakeLogRing();
        ring.Backing.Add(Entry(1, LogLevel.Info, "older"));
        ring.Backing.Add(Entry(2, LogLevel.Info, "newer"));
        var vm = new LogsViewModel(ring);

        vm.Load();

        Assert.Contains("newer", vm.Entries[0].Line);
        Assert.Contains("older", vm.Entries[1].Line);
    }

    [Fact]
    public void SetFilter_Error_IsASubsetOfAll()
    {
        var ring = new FakeLogRing();
        ring.Backing.Add(Entry(1, LogLevel.Error));
        ring.Backing.Add(Entry(2, LogLevel.Info));
        ring.Backing.Add(Entry(3, LogLevel.Debug));
        var vm = new LogsViewModel(ring);

        vm.Load();
        int nAll = vm.Entries.Count;

        vm.SetFilter(1); // Error
        int nErr = vm.Entries.Count;

        Assert.Equal(3, nAll);
        Assert.True(nErr <= nAll);
        Assert.Equal(1, nErr);
    }

    [Fact]
    public void Clear_EmptiesTheLiveViewViaTheRing()
    {
        var ring = new FakeLogRing();
        ring.Backing.Add(Entry(1, LogLevel.Info, "startup"));
        var vm = new LogsViewModel(ring);
        vm.Load();
        Assert.NotEmpty(vm.Entries);

        vm.Clear();

        Assert.Equal(1, ring.ClearCount);
        Assert.Empty(vm.Entries);
    }

    [Fact]
    public void CopyText_JoinsRenderedLines()
    {
        var ring = new FakeLogRing();
        ring.Backing.Add(Entry(1, LogLevel.Info, "alpha"));
        ring.Backing.Add(Entry(2, LogLevel.Info, "beta"));
        var vm = new LogsViewModel(ring);
        vm.Load();

        var text = vm.CopyText();

        Assert.Contains("alpha", text);
        Assert.Contains("beta", text);
        // Newest-first: beta precedes alpha in the copy payload.
        Assert.True(text.IndexOf("beta") < text.IndexOf("alpha"));
    }
}
