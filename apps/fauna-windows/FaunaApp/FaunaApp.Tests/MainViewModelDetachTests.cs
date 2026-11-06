using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The shell VM is per-LOGIN, like the <c>MainPage</c> that builds it
/// (<c>apps/windows.md</c> § Client lifetime, the third rule: a page is per-login, so
/// every page-scoped subscription is released in <c>OnNavigatedFrom</c>).
/// <see cref="MainViewModel.Detach"/> is that release: after it, the outgoing client's
/// events reach this VM no more — its connection-state handler is also the one writer of
/// the process-wide <c>OfflineGate.Shared</c>, so a detached VM left subscribed would let
/// a dead session's late <c>Disconnected</c> grey the next session's controls.
/// </summary>
public class MainViewModelDetachTests
{
    private static string Label(FfiConnectionState state) =>
        Strings.Resolve(FaunaFfiMethods.ConnectionStateLabel(state));

    [Fact]
    public void ADetachedVm_IgnoresTheOutgoingClientsConnectionState()
    {
        var rpc = new MockNestRpcClient();
        var vm = new MainViewModel(rpc, new FakeAgentStatusProbe());
        rpc.RaiseConnectionState(FfiConnectionState.Connected);

        vm.Detach();
        rpc.RaiseConnectionState(FfiConnectionState.Disconnected);

        Assert.Equal(Label(FfiConnectionState.Connected), vm.ConnectionStatus);
    }

    [Fact]
    public void ADetachedVm_NoLongerReReadsTheUnreadCountOnReconnect()
    {
        var rpc = new MockNestRpcClient();
        var vm = new MainViewModel(rpc, new FakeAgentStatusProbe());
        vm.Detach();
        var before = rpc.Calls.Count;

        rpc.RaiseReconnected();

        Assert.Equal(before, rpc.Calls.Count);
    }

    [Fact]
    public void Detach_IsIdempotent()
    {
        var rpc = new MockNestRpcClient();
        var vm = new MainViewModel(rpc, new FakeAgentStatusProbe());

        vm.Detach();
        vm.Detach();
    }
}
