using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using FaunaApp.Core.Logs;
using uniffi.fauna_log;
// LogRow is now also a generated UniFFI type (uniffi.fauna_log.LogRow, the shared
// fauna_log::format::LogRow); pin the bare name to the XAML-bound C# record.
using LogRow = FaunaApp.Core.Logs.LogRow;
// (no System.Linq needed — newest-first row mapping lives in LogsFormat.Rows)

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The Settings → Logs sub-page VM (observability.md § Surfaces) — the client's own
/// durable log record. Renders the <b>process-global</b> <c>fauna_log</c> ring (read
/// over the <see cref="ILogRing"/> seam) newest-first with a severity filter, a
/// copy-to-clipboard payload, and a clear. Mirrors linux <c>settings/logs.rs</c>
/// (which reads the same ring); the bug-prone parts (filter↔level map, time format,
/// the one-line form) live in the shared <see cref="LogsFormat"/> both Logs surfaces
/// use. No nest handle — the ring is a process global the page self-wires.
/// </summary>
public partial class LogsViewModel : ObservableObject
{
    private readonly ILogRing _ring;

    /// <summary>The current filtered snapshot, oldest-first (as the ring returns it);
    /// <see cref="Entries"/> renders it newest-first and <see cref="CopyText"/> joins
    /// it newest-first.</summary>
    private IReadOnlyList<LogEntry> _current = Array.Empty<LogEntry>();

    [ObservableProperty] private string? _error;

    /// <summary>The selected severity-filter index (0 = "All"; 1..5 = Error..Trace).</summary>
    public int FilterIndex { get; private set; }

    /// <summary>The rendered <c>log-entry</c> rows, newest-first.</summary>
    public ObservableCollection<LogRow> Entries { get; } = new();

    internal LogsViewModel(ILogRing ring)
    {
        _ring = ring;
    }

    /// <summary>Read the ring at the active filter and re-render newest-first.</summary>
    public void Load()
    {
        Error = null;
        _current = _ring.Snapshot(LogsFormat.LevelForIndex(FilterIndex));
        RenderEntries();
    }

    /// <summary>Set the severity filter (re-reads the ring at the new threshold —
    /// <c>snapshot_at_least</c>).</summary>
    public void SetFilter(int index)
    {
        FilterIndex = index;
        Load();
    }

    /// <summary>Clear the in-memory ring (<c>fauna_log::clear</c>) and re-render
    /// (empty); the on-disk rolling file is untouched.</summary>
    public void Clear()
    {
        _ring.Clear();
        Load();
    }

    /// <summary>The currently-rendered lines as one newest-first block — the
    /// clipboard payload for <c>log-copy-button</c>.</summary>
    public string CopyText() => LogsFormat.RenderedText(_current);

    private void RenderEntries()
    {
        Entries.Clear();
        foreach (var row in LogsFormat.Rows(_current))
        {
            Entries.Add(row);
        }
    }
}
