using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The erase barrier over every account-runtime stop still in flight
/// (<c>account-scoping.md</c> § Erasure follows scope, the ⚠ <i>An OPEN store is an
/// unerasable store</i> note): an erase waits for ALL of them — the rule the shared
/// <c>StopQueue</c> applies on linux and tui — under one named budget, and erases
/// anyway once it lapses.
///
/// <para>The remove-account red this pins: the switch back started the outgoing
/// account's stop fire-and-forget, and the only record of it was one field the next
/// dispose overwrote, which the remove's erase never read at all. The erase met the
/// still-open store and failed with <c>os error 32</c>.</para>
/// </summary>
public class AccountRuntimeStopsTests
{
    private static readonly TimeSpan Generous = TimeSpan.FromSeconds(30);

    [Fact]
    public async Task Barrier_NothingInFlight_PassesAtOnce()
    {
        var stops = new AccountRuntimeStops();

        Assert.True(await stops.BarrierAsync("test", Generous));
    }

    /// <summary>Two switches in a row leave two stops running. Waiting for the newest
    /// alone — what one overwritten field amounted to — lets the erase run while the
    /// older account's store is still open.</summary>
    [Fact]
    public async Task Barrier_WaitsForEveryStopInFlight_NotOnlyTheNewest()
    {
        var stops = new AccountRuntimeStops();
        var older = new TaskCompletionSource();
        var newer = new TaskCompletionSource();
        stops.Track(older.Task);
        stops.Track(newer.Task);

        var barrier = stops.BarrierAsync("test", Generous);
        newer.SetResult();
        await Task.Yield();
        Assert.False(barrier.IsCompleted, "the barrier passed with the older stop still running");

        older.SetResult();
        Assert.True(await barrier);
    }

    /// <summary>A stop started while an erase is already waiting holds it back too:
    /// nothing runs while ANY stop is in flight (<c>StopQueue</c>'s first rule).</summary>
    [Fact]
    public async Task Barrier_HoldsForAStopStartedWhileItWaits()
    {
        var stops = new AccountRuntimeStops();
        var first = new TaskCompletionSource();
        var second = new TaskCompletionSource();
        stops.Track(first.Task);

        var barrier = stops.BarrierAsync("test", Generous);
        stops.Track(second.Task);
        first.SetResult();
        await Task.Yield();
        Assert.False(barrier.IsCompleted, "the barrier passed with a later stop still running");

        second.SetResult();
        Assert.True(await barrier);
    }

    /// <summary>A stop that failed has still finished: its store is as closed as it is
    /// going to get, and the erase must not wait the whole budget for it.</summary>
    [Fact]
    public async Task Barrier_AFaultedStopCountsAsFinished()
    {
        var stops = new AccountRuntimeStops();
        stops.Track(Task.FromException(new InvalidOperationException("dispose failed")));

        Assert.True(await stops.BarrierAsync("test", Generous));
    }

    /// <summary>Bounded, never unbounded: a wedged stop costs the budget, then the
    /// barrier reports the lapse and the erase goes ahead (loudly — the warning is
    /// the barrier's own).</summary>
    [Fact]
    public async Task Barrier_AWedgedStop_LapsesAtTheBudget()
    {
        var stops = new AccountRuntimeStops();
        stops.Track(new TaskCompletionSource().Task);

        Assert.False(await stops.BarrierAsync("test", TimeSpan.FromMilliseconds(50)));
    }

    /// <summary>A stop that finished before the erase asked is not waited on again.</summary>
    [Fact]
    public async Task Barrier_AFinishedStopIsNotWaitedOn()
    {
        var stops = new AccountRuntimeStops();
        var done = new TaskCompletionSource();
        stops.Track(done.Task);
        done.SetResult();

        Assert.True(await stops.BarrierAsync("test", TimeSpan.FromMilliseconds(1)));
    }
}
