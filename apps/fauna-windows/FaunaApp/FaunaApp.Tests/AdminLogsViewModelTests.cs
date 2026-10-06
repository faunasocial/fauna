using System.Linq;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_log;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit tests for the admin Logs page VM
/// (<see cref="AdminLogsViewModel"/>) over the <see cref="MockNestRpcClient"/> WS-RPC
/// seam (observability.md § Surfaces; no live nest / FlaUI, which flakes on
/// win-arm64). Mirrors linux <c>views/admin.rs build_admin_logs_page</c>: the nest
/// ring is fetched ONCE over <c>fauna.admin.logs</c> and the filter narrows the held
/// source in memory (no refetch); there is no clear.
/// </summary>
public class AdminLogsViewModelTests
{
    [Fact]
    public async Task Load_FetchesNestRingOverSeamAndPopulates()
    {
        var rpc = new MockNestRpcClient
        {
            NextAdminLogs = new[]
            {
                MockNestRpcClient.MakeLogEntry(1, LogLevel.Info, message: "nest up"),
                MockNestRpcClient.MakeLogEntry(2, LogLevel.Warn, message: "degraded"),
            },
        };
        var vm = new AdminLogsViewModel(rpc);

        await vm.LoadAsync();

        Assert.Equal(2, vm.Entries.Count);
        Assert.Null(vm.Error);
        Assert.False(vm.IsLoading);
        Assert.Contains("AdminLogs", rpc.Calls); // fetched over the WS-RPC seam
    }

    [Fact]
    public async Task Filter_NarrowsHeldSourceInMemory_WithoutRefetch()
    {
        var rpc = new MockNestRpcClient
        {
            NextAdminLogs = new[]
            {
                MockNestRpcClient.MakeLogEntry(1, LogLevel.Error, message: "e"),
                MockNestRpcClient.MakeLogEntry(2, LogLevel.Info, message: "i"),
            },
        };
        var vm = new AdminLogsViewModel(rpc);
        await vm.LoadAsync();
        int nAll = vm.Entries.Count;

        vm.SetFilter(1); // Error
        int nErr = vm.Entries.Count;

        vm.SetFilter(0); // All restores the full fetched set
        int nAllAgain = vm.Entries.Count;

        Assert.Equal(2, nAll);
        Assert.Equal(1, nErr);
        Assert.Equal(nAll, nAllAgain);
        // Filtering narrows the held source in memory — the nest is fetched exactly once.
        Assert.Equal(1, rpc.Calls.Count(c => c == "AdminLogs"));
    }

    [Fact]
    public async Task Load_FailureRoutesToPageError()
    {
        var rpc = new MockNestRpcClient { NextError = "boom" };
        var vm = new AdminLogsViewModel(rpc);

        await vm.LoadAsync();

        Assert.False(string.IsNullOrEmpty(vm.Error));
        Assert.False(vm.IsLoading);
        Assert.Empty(vm.Entries);
    }

    [Fact]
    public async Task CopyText_JoinsRenderedLinesNewestFirst()
    {
        var rpc = new MockNestRpcClient
        {
            NextAdminLogs = new[]
            {
                MockNestRpcClient.MakeLogEntry(1, LogLevel.Info, message: "alpha"),
                MockNestRpcClient.MakeLogEntry(2, LogLevel.Info, message: "beta"),
            },
        };
        var vm = new AdminLogsViewModel(rpc);
        await vm.LoadAsync();

        var text = vm.CopyText();

        Assert.Contains("alpha", text);
        Assert.Contains("beta", text);
        Assert.True(text.IndexOf("beta") < text.IndexOf("alpha"));
    }
}
