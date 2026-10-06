using FaunaApp.Core.Helpers;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Pins the ordering contract of <see cref="PostActionChain.Then"/>.
///
/// <para>These are the regression guard for the second-login disposed-client bug: the
/// <c>nav</c> block of a <c>set_state</c> used to <b>overwrite</b> the <c>session</c>
/// block's post-action rather than sequence after it, because the old
/// <c>Action</c>-typed chain could not await across a suspension point. A FIFO argument
/// ("the earlier action was invoked first, so its effects are visible") is <b>wrong</b>
/// for <c>async void</c> — <see cref="Then_AwaitsFirstToCompletion_BeforeStartingNext"/>
/// is the test that falsifies it.</para>
/// </summary>
public class PostActionChainTests
{
    [Fact]
    public async Task Then_RunsFirstThenNext_InOrder()
    {
        var order = new List<string>();
        Func<Task>? chain = null;

        chain = chain.Then(() => { order.Add("first"); return Task.CompletedTask; });
        chain = chain.Then(() => { order.Add("second"); return Task.CompletedTask; });
        chain = chain.Then(() => { order.Add("third"); return Task.CompletedTask; });

        await chain();

        Assert.Equal(new[] { "first", "second", "third" }, order);
    }

    /// <summary>
    /// The load-bearing case. The first action suspends (as the real session
    /// post-action does — it awaits the e2e conversations session before navigating);
    /// the second must not observe the world until the first has fully finished.
    /// Under the old <c>Action</c> chain this failed: invoking an <c>async void</c>
    /// lambda returns at its first <c>await</c>, so "second" ran while "first" was
    /// still mid-flight.
    /// </summary>
    [Fact]
    public async Task Then_AwaitsFirstToCompletion_BeforeStartingNext()
    {
        var order = new List<string>();
        Func<Task>? chain = null;

        chain = chain.Then(async () =>
        {
            order.Add("first:start");
            await Task.Delay(50);
            order.Add("first:end");
        });
        chain = chain.Then(() => { order.Add("second"); return Task.CompletedTask; });

        await chain();

        Assert.Equal(new[] { "first:start", "first:end", "second" }, order);
    }

    [Fact]
    public async Task Then_NullFirst_ReturnsNextAlone()
    {
        var ran = 0;
        Func<Task>? chain = null;

        chain = chain.Then(() => { ran++; return Task.CompletedTask; });
        await chain();

        Assert.Equal(1, ran);
    }

    /// <summary>
    /// A throwing link surfaces to the caller (the agent's post-action runner logs it)
    /// and stops the chain — it must not be swallowed into a silently half-applied
    /// command.
    /// </summary>
    [Fact]
    public async Task Then_FirstThrows_PropagatesAndSkipsNext()
    {
        var nextRan = false;
        Func<Task>? chain = null;

        chain = chain.Then(() => throw new InvalidOperationException("boom"));
        chain = chain.Then(() => { nextRan = true; return Task.CompletedTask; });

        await Assert.ThrowsAsync<InvalidOperationException>(() => chain());
        Assert.False(nextRan);
    }
}
