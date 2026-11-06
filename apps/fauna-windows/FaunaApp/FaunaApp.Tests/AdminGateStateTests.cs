using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// <see cref="AdminGateState"/> — the admin gate re-read on every reconnect
/// (transport.md § Which surfaces a push invalidates, <c>admin_gate</c>).
/// Pins the two properties tui and linux pin: a FAILED re-read moves nothing,
/// and the once-per-session admin effects fire on the false→true transition
/// only.
/// </summary>
public class AdminGateStateTests
{
    [Fact]
    public async Task FailedReRead_KeepsConfirmedAdmin()
    {
        var rpc = new MockNestRpcClient { NextAmIAdmin = true };
        var gate = new AdminGateState();
        Assert.Equal(AdminGateRefresh.BecameAdmin, await gate.RefreshAsync(rpc));

        rpc.NextError = "nest unreachable";
        Assert.Equal(AdminGateRefresh.Unreachable, await gate.RefreshAsync(rpc));

        Assert.True(gate.IsAdmin);
    }

    [Fact]
    public async Task ConfirmingReRead_DoesNotRefireTheTransition()
    {
        var rpc = new MockNestRpcClient { NextAmIAdmin = true };
        var gate = new AdminGateState();

        Assert.Equal(AdminGateRefresh.BecameAdmin, await gate.RefreshAsync(rpc));
        Assert.Equal(AdminGateRefresh.Unchanged, await gate.RefreshAsync(rpc));
        Assert.Equal(AdminGateRefresh.Unchanged, await gate.RefreshAsync(rpc));
    }

    [Fact]
    public async Task LoginReadThatFailed_IsRepairedByTheNextReconnectRead()
    {
        var rpc = new MockNestRpcClient { NextAmIAdmin = true, NextError = "name not resolving" };
        var gate = new AdminGateState();

        Assert.Equal(AdminGateRefresh.Unreachable, await gate.RefreshAsync(rpc));
        Assert.False(gate.IsAdmin);

        rpc.NextError = null;
        Assert.Equal(AdminGateRefresh.BecameAdmin, await gate.RefreshAsync(rpc));
        Assert.True(gate.IsAdmin);
    }

    [Fact]
    public async Task NonAdmin_NeverTransitions()
    {
        var rpc = new MockNestRpcClient { NextAmIAdmin = false };
        var gate = new AdminGateState();

        Assert.Equal(AdminGateRefresh.Unchanged, await gate.RefreshAsync(rpc));
        Assert.False(gate.IsAdmin);
    }
}
