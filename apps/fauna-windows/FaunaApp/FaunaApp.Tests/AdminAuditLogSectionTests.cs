using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_core;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// A scripted <see cref="IFfiAuditLog"/>: <see cref="Load"/> answers the first view,
/// <see cref="More"/> the next; <see cref="Fail"/> makes the next read throw.
/// </summary>
internal sealed class FakeAuditLog : FfiAuditLogFakeBase
{
    public Queue<FfiAuditLogView> Views { get; } = new();
    public List<string> Calls { get; } = new();
    public bool Fail { get; set; }

    private Task<FfiAuditLogView> Next(string call)
    {
        Calls.Add(call);
        if (Fail) throw new InvalidOperationException("audit read failed");
        return Task.FromResult(Views.Dequeue());
    }

    public override Task<FfiAuditLogView> Load() => Next("load");
    public override Task<FfiAuditLogView> More() => Next("more");
}

/// <summary>
/// The admin audit log section on <c>admin-logs</c> (<c>admin.md</c> § The admin
/// audit log on <c>admin-logs</c>) in <see cref="AdminLogsViewModel"/>: every word is
/// the shared view's; the VM prefixes the local time, follows the more cursor and
/// keeps the rows across a failed read.
/// </summary>
public class AdminAuditLogSectionTests
{
    private static LocalizedText Text(string key) => new(key, new Dictionary<string, string>());

    private static FfiAuditLogView View(long? moreBefore, params (long Id, string Key)[] rows)
        => new(
            rows.Select(r => new FfiAuditLogRow(r.Id, 1_700_000_000_000 + r.Id, Text(r.Key))).ToArray(),
            new FfiAuditIntegrity.Verified((uint)rows.Length),
            Text("admin.logs_page.audit_title"),
            moreBefore);

    private static (AdminLogsViewModel Vm, FakeAuditLog Log, MockNestRpcClient Rpc) Make()
    {
        var log = new FakeAuditLog();
        var rpc = new MockNestRpcClient { NextAuditLog = log };
        return (new AdminLogsViewModel(rpc), log, rpc);
    }

    [Fact]
    public async Task Load_paints_one_row_per_entry_newest_first_with_the_local_time_and_the_shared_text()
    {
        var (vm, log, rpc) = Make();
        log.Views.Enqueue(View(7, (9, "admin.logs_page.audit_more"), (8, "admin.logs_page.title")));

        await vm.LoadAuditAsync();

        Assert.Contains("AdminAuditLog", rpc.Calls);
        Assert.Equal(new[] { "load" }, log.Calls);
        Assert.Equal(2, vm.AuditRows.Count);
        var first = vm.AuditRows[0].Line;
        Assert.StartsWith(FaunaFfiMethods.FormatUnixLocalMs(1_700_000_000_009) + "  ", first);
        Assert.EndsWith(Core.Services.Strings.Resolve(Text("admin.logs_page.audit_more")), first);
        Assert.Equal(Core.Services.Strings.Resolve(Text("admin.logs_page.audit_title")), vm.AuditIntegrityText);
        Assert.True(vm.AuditMoreEnabled, "a more cursor means older entries are unread");
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task More_pages_older_entries_in_below_and_disables_itself_at_the_end()
    {
        var (vm, log, _) = Make();
        log.Views.Enqueue(View(8, (9, "a"), (8, "b")));
        log.Views.Enqueue(View(null, (9, "a"), (8, "b"), (7, "c")));

        await vm.LoadAuditAsync();
        await vm.MoreAuditAsync();

        Assert.Equal(new[] { "load", "more" }, log.Calls);
        Assert.Equal(3, vm.AuditRows.Count);
        Assert.False(vm.AuditMoreEnabled, "the whole record is shown");
    }

    [Fact]
    public async Task A_failed_read_lands_on_the_error_and_keeps_the_rows_shown()
    {
        var (vm, log, _) = Make();
        log.Views.Enqueue(View(8, (9, "a")));
        await vm.LoadAuditAsync();

        log.Fail = true;
        await vm.MoreAuditAsync();

        Assert.Single(vm.AuditRows);
        Assert.False(string.IsNullOrEmpty(vm.Error));
    }

    [Fact]
    public async Task More_before_any_load_reads_nothing()
    {
        var (vm, log, _) = Make();
        await vm.MoreAuditAsync();
        Assert.Empty(log.Calls);
    }
}
