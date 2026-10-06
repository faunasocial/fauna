using System.Collections.Generic;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// windows' leg of <c>account-scoping.md</c> § Concurrent instances (the agent
/// bullet) and <c>sync-agent.md</c> § Control plane split → <i>The
/// UnprovisionCapability reply is a receipt</i>: every windows path that erases account-scoped stores awaits the
/// sync agent's un-provision reply first, so an open handle under the agent's
/// still-mounted store cannot make the erase fail outright. <see cref="App.SignOutHandler"/>
/// and the e2e test agent's <c>reset</c>/<c>logout</c> arms cannot be
/// exercised directly from this project (<c>FaunaApp.Tests</c> references only
/// <c>FaunaApp.Core</c>, not the WinUI <c>FaunaApp</c> project), so the
/// ordering primitive they all now call through is pinned here instead.
/// </summary>
public class ErasePreconditionTests
{
    [Fact]
    public async Task EraseDoesNotRunUntilUnprovisionCompletes()
    {
        var order = new List<string>();
        var gate = new TaskCompletionSource();

        var task = ErasePrecondition.AwaitUnprovisionThenErase(
            shouldUnprovision: true,
            unprovision: async () =>
            {
                await gate.Task;
                order.Add("unprovision");
            },
            erase: () => order.Add("erase"));

        Assert.Empty(order);
        gate.SetResult();
        await task;
        Assert.Equal(new[] { "unprovision", "erase" }, order);
    }

    [Fact]
    public async Task EraseStillRunsWhenUnprovisionFails()
    {
        var order = new List<string>();

        await ErasePrecondition.AwaitUnprovisionThenErase(
            shouldUnprovision: true,
            unprovision: () =>
            {
                order.Add("unprovision");
                return Task.CompletedTask;
            },
            erase: () => order.Add("erase"));

        Assert.Equal(new[] { "unprovision", "erase" }, order);
    }

    /// <summary>
    /// The e2e <c>reset</c>/<c>logout</c> arms tear the clients down BEFORE they
    /// return the erase, and that teardown drops the sync-agent session
    /// (<c>StopSyncAgentSession</c>). Run first, it left the awaited un-provision
    /// nothing to tear down — "no live session to tear down" before every erase —
    /// so the agent kept its account runtime, and with it
    /// <c>account-store.db</c>, open through every reset: the erase failed with
    /// <c>os error 32</c> each time. The
    /// un-provision must therefore START (claiming the session synchronously)
    /// before the teardown runs, and the erase still waits for its reply.
    /// </summary>
    [Fact]
    public async Task UnprovisionStartsBeforeTheTeardownAndTheEraseWaitsForIt()
    {
        var order = new List<string>();
        var gate = new TaskCompletionSource();

        var eraseStep = ErasePrecondition.BeginUnprovisionThenTearDown(
            shouldUnprovision: true,
            unprovision: async () =>
            {
                order.Add("unprovision claimed");
                await gate.Task;
                order.Add("unprovision replied");
            },
            tearDown: () => order.Add("tear down"),
            erase: () => order.Add("erase"));

        Assert.Equal(new[] { "unprovision claimed", "tear down" }, order);
        var erased = eraseStep();
        Assert.DoesNotContain("erase", order);
        gate.SetResult();
        await erased;
        Assert.Equal(
            new[] { "unprovision claimed", "tear down", "unprovision replied", "erase" }, order);
    }

    [Fact]
    public async Task BeginUnprovisionThenTearDownSkipsTheUnprovisionWhenNotAsked()
    {
        var order = new List<string>();

        await ErasePrecondition.BeginUnprovisionThenTearDown(
            shouldUnprovision: false,
            unprovision: () =>
            {
                order.Add("unprovision");
                return Task.CompletedTask;
            },
            tearDown: () => order.Add("tear down"),
            erase: () => order.Add("erase"))();

        Assert.Equal(new[] { "tear down", "erase" }, order);
    }

    [Fact]
    public async Task UnprovisionIsSkippedWhenNotAsked()
    {
        var order = new List<string>();

        await ErasePrecondition.AwaitUnprovisionThenErase(
            shouldUnprovision: false,
            unprovision: () =>
            {
                order.Add("unprovision");
                return Task.CompletedTask;
            },
            erase: () => order.Add("erase"));

        Assert.Equal(new[] { "erase" }, order);
    }
}
