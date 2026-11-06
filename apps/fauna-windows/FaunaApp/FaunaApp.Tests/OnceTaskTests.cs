using System;
using System.Threading.Tasks;
using FaunaApp.Core;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The once-until-it-succeeds gate behind <c>FeedManagerHost.HydrateCuesOnceAsync</c>
/// (engagement-cues.md § At rest). The feed manager lives for the session, and
/// <c>HydrateCues</c> replaces its live cue engine, so a Feed-page load that re-ran it
/// would discard every verdict folded since the first hydrate and not yet put.
/// </summary>
public class OnceTaskTests
{
    [Fact]
    public async Task A_run_that_landed_is_never_started_again()
    {
        var once = new OnceTask();
        var starts = 0;
        Task Start() { starts++; return Task.CompletedTask; }

        await once.RunAsync(Start);
        await once.RunAsync(Start);

        Assert.Equal(1, starts);
    }

    [Fact]
    public async Task A_call_while_the_run_is_in_flight_shares_it()
    {
        var once = new OnceTask();
        var gate = new TaskCompletionSource();
        var starts = 0;
        Task Start() { starts++; return gate.Task; }

        var first = once.RunAsync(Start);
        var second = once.RunAsync(Start);
        gate.SetResult();
        await Task.WhenAll(first, second);

        Assert.Same(first, second);
        Assert.Equal(1, starts);
    }

    [Fact]
    public async Task A_run_that_failed_is_reported_and_the_next_call_retries()
    {
        var once = new OnceTask();
        var starts = 0;
        Task Start()
        {
            starts++;
            return starts == 1
                ? Task.FromException(new InvalidOperationException("sealed model failed to decrypt"))
                : Task.CompletedTask;
        }

        await Assert.ThrowsAsync<InvalidOperationException>(() => once.RunAsync(Start));
        await once.RunAsync(Start);
        await once.RunAsync(Start);

        Assert.Equal(2, starts);
    }

    [Fact]
    public async Task Reset_forgets_a_run_that_landed()
    {
        var once = new OnceTask();
        var starts = 0;
        Task Start() { starts++; return Task.CompletedTask; }

        await once.RunAsync(Start);
        once.Reset();
        await once.RunAsync(Start);

        Assert.Equal(2, starts);
    }
}
