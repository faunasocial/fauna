using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using uniffi.fauna_log;
// LogRow is now also a generated UniFFI type (uniffi.fauna_log.LogRow, the shared
// fauna_log::format::LogRow); pin the bare name to the XAML-bound C# record.
using LogRow = FaunaApp.Core.Logs.LogRow;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The admin Logs page VM (observability.md § Surfaces) — the <b>nest's</b>
/// <c>fauna_log</c> ring fetched over the admin WS-RPC <c>fauna.admin.logs</c>
/// (<see cref="INestRpcClient.AdminLogsAsync"/> → <c>FfiAdminClient.Logs</c>) and
/// rendered with the <b>same</b> <see cref="LogsFormat"/>/<see cref="LogRow"/> shape
/// as the client's own Settings → Logs page. Mirrors linux <c>views/admin.rs</c>
/// <c>build_admin_logs_page</c>: the source is fetched once and the filter narrows
/// it <b>in memory</b> (no refetch); there is <b>no clear</b> (no admin RPC to wipe
/// the nest ring).
/// </summary>
public partial class AdminLogsViewModel : ObservableObject
{
    private readonly INestRpcClient _rpc;

    /// <summary>The nest ring as fetched once (oldest-first); the filter narrows this
    /// in memory.</summary>
    private IReadOnlyList<LogEntry> _source = Array.Empty<LogEntry>();

    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private string? _error;

    /// <summary>The selected severity-filter index (0 = "All"; 1..5 = Error..Trace).</summary>
    public int FilterIndex { get; private set; }

    /// <summary>The rendered <c>log-entry</c> rows, newest-first.</summary>
    public ObservableCollection<LogRow> Entries { get; } = new();

    internal AdminLogsViewModel(INestRpcClient rpc)
    {
        _rpc = rpc;
    }

    /// <summary>Fetch the nest ring over <c>fauna.admin.logs</c> into the held source
    /// and render at the active filter. Failures route to <see cref="Error"/>.</summary>
    public async Task LoadAsync()
    {
        IsLoading = true;
        Error = null;
        try
        {
            // No ConfigureAwait(false): a WinUI VM mutates bound state after the
            // await, which must stay on the UI thread (a COMException otherwise).
            _source = await _rpc.AdminLogsAsync();
            RenderEntries();
        }
        catch (Exception ex)
        {
            Error = Strings.Error(ex);
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// <summary>Set the severity filter — narrows the <b>held</b> source in memory
    /// (no refetch), so "All" restores the full fetched set.</summary>
    public void SetFilter(int index)
    {
        FilterIndex = index;
        RenderEntries();
    }

    /// <summary>The currently-rendered lines as one newest-first block — the
    /// clipboard payload for <c>log-copy-button</c>.</summary>
    public string CopyText()
        => LogsFormat.RenderedText(LogsFormat.FilterEntries(_source, LogsFormat.LevelForIndex(FilterIndex)));

    private void RenderEntries()
    {
        var filtered = LogsFormat.FilterEntries(_source, LogsFormat.LevelForIndex(FilterIndex));
        Entries.Clear();
        foreach (var row in LogsFormat.Rows(filtered))
        {
            Entries.Add(row);
        }
    }
}
