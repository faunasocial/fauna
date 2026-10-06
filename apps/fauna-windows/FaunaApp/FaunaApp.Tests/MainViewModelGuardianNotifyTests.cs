using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// <see cref="MainViewModel.CheckGuardianNotifyAsync"/> — the flush-tick's async
/// Core method (family-safety.md § Guardian Notify), mirroring
/// <see cref="MainViewModelSyncAgentStatusTests"/>'s split: the DispatcherTimer
/// lives in MainPage, this is the part testable without WinUI.
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class MainViewModelGuardianNotifyTests : IDisposable
{
    public MainViewModelGuardianNotifyTests() => GuardianNotifyCache.Reset();
    public void Dispose() => GuardianNotifyCache.Reset();

    [Fact]
    public async Task CheckGuardianNotifyAsync_NothingPending_DoesNotCallTheRpc()
    {
        var rpc = new MockNestRpcClient();
        var vm = new MainViewModel(rpc, new FakeAgentStatusProbe());

        await vm.CheckGuardianNotifyAsync();

        Assert.Null(rpc.LastFamilyNotifyReport);
    }

    [Fact]
    public async Task CheckGuardianNotifyAsync_ABatchIsDue_SendsItAndDrains()
    {
        GuardianNotifyCache.SetEnabled(true);
        GuardianNotifyCache.Record("post-1", ["spam"]);
        var rpc = new MockNestRpcClient();
        var vm = new MainViewModel(rpc, new FakeAgentStatusProbe());

        await vm.CheckGuardianNotifyAsync();

        Assert.NotNull(rpc.LastFamilyNotifyReport);
        var (entries, _offsetMinutes) = rpc.LastFamilyNotifyReport!.Value;
        Assert.Single(entries);
        Assert.Equal("spam", entries[0].@category);
        Assert.Equal(1u, entries[0].@count);

        // Drained — a second tick before the next interval sends nothing more.
        await vm.CheckGuardianNotifyAsync();
        Assert.Equal(1, rpc.FamilyNotifyReportCallCount);
    }

    [Fact]
    public async Task CheckGuardianNotifyAsync_RpcThrows_IsSwallowedBestEffort()
    {
        GuardianNotifyCache.SetEnabled(true);
        GuardianNotifyCache.Record("post-1", ["spam"]);
        var rpc = new MockNestRpcClient { NextError = "simulated failure" };
        var vm = new MainViewModel(rpc, new FakeAgentStatusProbe());

        // Must not throw — Guardian Notify is fire-and-forget best-effort.
        await vm.CheckGuardianNotifyAsync();
    }
}
