using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Helpers;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The inherited-filter review cache
/// (<c>succession-aftermath.md</c> § Adjudicating what the aftermath carries
/// across): one read feeds both the Account line's count and the list's per-rule
/// marks; a failed read never reads as "nothing flagged"; the aftermath's
/// config-stage hook reads the raised marks back.
/// </summary>
[Collection(AftermathProgressCollection.Name)]
public sealed class InheritedFilterMarksTests
{
    [Fact]
    public async Task RefreshCachesTheOpenMarks()
    {
        InheritedFilterMarks.Reset();
        var rpc = new MockNestRpcClient { NextFilterMarks = new long[] { 7, 9 } };

        await InheritedFilterMarks.RefreshAsync(rpc);

        Assert.Equal(2, InheritedFilterMarks.Count);
        Assert.True(InheritedFilterMarks.Contains(7));
        Assert.False(InheritedFilterMarks.Contains(8));
    }

    /// <summary>Collapsing a failed read into "nothing flagged" would hide a
    /// flagged rule — the cache must survive it.</summary>
    [Fact]
    public async Task AFailedRefreshKeepsTheCachedMarks()
    {
        InheritedFilterMarks.Reset();
        await InheritedFilterMarks.RefreshAsync(
            new MockNestRpcClient { NextFilterMarks = new long[] { 3 } });

        await InheritedFilterMarks.RefreshAsync(new MockNestRpcClient { NextError = "offline" });

        Assert.True(InheritedFilterMarks.Contains(3));
    }

    [Fact]
    public async Task ConfigStageSettledReadsTheRaisedMarksBack()
    {
        InheritedFilterMarks.Reset();
        var rpc = new MockNestRpcClient { NextFilterMarks = new long[] { 42 } };
        var refreshed = new TaskCompletionSource();
        void OnChanged() { if (InheritedFilterMarks.Contains(42)) refreshed.TrySetResult(); }
        InheritedFilterMarks.Changed += OnChanged;
        try
        {
            new LoggingAftermathSink(rpc).ConfigStageSettled();
            await refreshed.Task.WaitAsync(System.TimeSpan.FromSeconds(10));
        }
        finally
        {
            InheritedFilterMarks.Changed -= OnChanged;
        }

        Assert.Contains("FilterMarksList", rpc.Calls);
    }

    /// <summary>A new pass (every sign-in, an account switch included) must not
    /// paint the previous identity's backlog.</summary>
    [Fact]
    public async Task ANewAftermathPassForgetsThePreviousIdentitysMarks()
    {
        await InheritedFilterMarks.RefreshAsync(
            new MockNestRpcClient { NextFilterMarks = new long[] { 5 } });

        await SuccessionAftermath.RunAsync(new MockNestRpcClient());

        Assert.Equal(0, InheritedFilterMarks.Count);
    }
}
