using Xunit;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// The shell VM's global <c>connection-status</c> indicator (transport.md
/// § Connection-status indicator). Drives <see cref="MainViewModel.ConnectionStatus"/>
/// off the seam's <c>ConnectionStateChanged</c> event — the unit-level peer of the
/// e2e <c>test_connection_status_flips_while_nest_down</c>. Asserts against
/// <see cref="Strings.Resolve"/> over <c>FaunaFfiMethods.ConnectionStateLabel</c> —
/// the same shared-Rust label resolution the VM itself now uses — so it holds
/// whether or not a localizer is installed.
/// </summary>
public class MainViewModelConnectionStatusTests
{
    private static string Label(FfiConnectionState state) =>
        Strings.Resolve(FaunaFfiMethods.ConnectionStateLabel(state));

    [Fact]
    public void ConnectionStatus_DefaultsToConnecting_BeforeAnyTransition()
    {
        var rpc = new MockNestRpcClient();
        var vm = new MainViewModel(rpc, new FakeAgentStatusProbe());
        Assert.Equal(Strings.Get("common/connecting"), vm.ConnectionStatus);
    }

    [Fact]
    public void ConnectionStatus_TracksTransportStateTransitions()
    {
        var rpc = new MockNestRpcClient();
        var vm = new MainViewModel(rpc, new FakeAgentStatusProbe());

        // Connected after (re)connect.
        rpc.RaiseConnectionState(FfiConnectionState.Connected);
        Assert.Equal(Label(FfiConnectionState.Connected), vm.ConnectionStatus);

        // The nest goes away → the indicator leaves Connected (never an error).
        rpc.RaiseConnectionState(FfiConnectionState.Disconnected);
        Assert.Equal(Label(FfiConnectionState.Disconnected), vm.ConnectionStatus);

        // Reconnecting shows the transient "Connecting…" state.
        rpc.RaiseConnectionState(FfiConnectionState.Connecting);
        Assert.Equal(Label(FfiConnectionState.Connecting), vm.ConnectionStatus);
    }

    [Fact]
    public void Construction_StartsConnectionStatePump()
    {
        var rpc = new MockNestRpcClient();
        _ = new MainViewModel(rpc, new FakeAgentStatusProbe());
        // The VM subscribes THEN starts the (idempotent) pump, so the initial
        // current-state raise is never missed.
        Assert.Contains("StartConnectionStatePump", rpc.Calls);
    }
}
