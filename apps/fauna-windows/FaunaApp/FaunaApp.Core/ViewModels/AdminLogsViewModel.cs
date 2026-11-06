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
///
/// <para>Below the log lines, the admin audit log section (<c>admin.md</c> § The admin
/// audit log on <c>admin-logs</c>): <see cref="LoadAuditAsync"/> /
/// <see cref="MoreAuditAsync"/> over the shared <c>FfiAuditLog</c>, whose view hands
/// back every row's text, the chain check's line and the more cursor — this VM only
/// prefixes each row with the app's absolute local time (the shared
/// <c>FormatUnixLocalMs</c> door), as tui's <c>audit_elements</c> does. A failed read
/// lands on <see cref="Error"/> and keeps the rows already shown.</para>
/// </summary>
public partial class AdminLogsViewModel : ObservableObject
{
    private readonly INestRpcClient _rpc;

    /// <summary>The section's entries-so-far, held in Rust; one per page visit.</summary>
    private uniffi.fauna_ffi.IFfiAuditLog? _audit;

    /// <summary>The <c>admin-audit-entry</c> rows, newest first: local time, then the
    /// shared row text.</summary>
    public ObservableCollection<AuditRowItem> AuditRows { get; } = new();

    /// <summary>The <c>admin-audit-integrity-status</c> line (empty until a read lands).</summary>
    [ObservableProperty] private string _auditIntegrityText = "";

    /// <summary>Whether <c>admin-audit-more-button</c> can page older entries — the
    /// shared view's more cursor is set.</summary>
    [ObservableProperty] private bool _auditMoreEnabled;

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

    /// <summary>Read the audit section's newest page — a fresh section on every
    /// entry to the page, replacing what was held.</summary>
    public async Task LoadAuditAsync()
    {
        try
        {
            _audit = await _rpc.AdminAuditLogAsync();
            RenderAudit(await _audit.Load());
        }
        catch (Exception ex)
        {
            Error = Strings.Error(ex);
        }
    }

    /// <summary><c>admin-audit-more-button</c>: page in the entries older than the
    /// oldest held, below the newer ones.</summary>
    public async Task MoreAuditAsync()
    {
        if (_audit is null) return;
        try
        {
            RenderAudit(await _audit.More());
        }
        catch (Exception ex)
        {
            Error = Strings.Error(ex);
        }
    }

    private void RenderAudit(uniffi.fauna_ffi.FfiAuditLogView view)
    {
        AuditRows.Clear();
        foreach (var row in view.rows)
        {
            AuditRows.Add(new AuditRowItem(
                $"{uniffi.fauna_ffi.FaunaFfiMethods.FormatUnixLocalMs(row.tsMs)}  {Strings.Resolve(row.text)}"));
        }
        AuditIntegrityText = Strings.Resolve(view.integrityText);
        AuditMoreEnabled = view.moreBeforeId is not null;
    }

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

/// <summary>One painted <c>admin-audit-entry</c> row.</summary>
public sealed record AuditRowItem(string Line);
