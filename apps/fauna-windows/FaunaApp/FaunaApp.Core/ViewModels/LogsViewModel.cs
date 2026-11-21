using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
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

    /// <summary>The text the last confirmed copy put on the clipboard — paints
    /// <c>log-copy-button</c> as "Copied!" with this as its <c>copied</c> attr (the
    /// <c>account-actor-id-copy-btn</c> contract); <c>null</c> before a copy, after a
    /// failed one, and after the ring is re-read.</summary>
    [ObservableProperty] private string? _copied;

    /// <summary>The i18n key of the page's own empty line — the reason
    /// <c>log-copy-button</c> / <c>log-clear-button</c> are blocked on an empty ring.</summary>
    internal const string EmptyReasonKey = "logs.empty";

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
        Copied = null;
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
        if (!RingAction().IsPossible) return;
        _ring.Clear();
        Load();
    }

    /// <summary>The currently-rendered lines as one newest-first block — the
    /// clipboard payload for <c>log-copy-button</c>.</summary>
    public string CopyText() => LogsFormat.RenderedText(_current);

    /// <summary>Whether <c>log-copy-button</c> / <c>log-clear-button</c> can do anything
    /// with the entries the active filter shows — the shared
    /// <c>fauna_log::format::ring_action_possibility</c> (blocked by the page's own
    /// empty line, <c>logs.empty</c>, when none are). The buttons paint from it and
    /// both actions ask it again.</summary>
    internal PossibilityPaint RingAction() =>
        PossibilityPaint.Of(uniffi.fauna_ffi.FaunaFfiMethods.LogRingActionPossibility((uint)_current.Count, EmptyReasonKey));

    /// <summary>The clipboard payload for a copy the answer allows, or <c>null</c>
    /// when <see cref="RingAction"/> refuses it.</summary>
    internal string? CopyPayload() => RingAction().IsPossible ? CopyText() : null;

    /// <summary>Record a copy's outcome (<c>docs/goal/ui/README.md</c> § Offer only what
    /// is possible, rule 7): a write that reached the clipboard confirms on
    /// <c>log-copy-button</c> (<see cref="Copied"/>) and clears a prior failure; one
    /// that threw says so on <c>error-message</c> (<c>common.copy_failed</c>).</summary>
    internal void RecordCopyOutcome(string text, Exception? failure)
    {
        if (failure is null)
        {
            Error = null;
            Copied = text;
        }
        else
        {
            Copied = null;
            Error = Strings.Format("common/copy_failed", failure.Message);
        }
    }

    private void RenderEntries()
    {
        Entries.Clear();
        foreach (var row in LogsFormat.Rows(_current))
        {
            Entries.Add(row);
        }
    }
}
