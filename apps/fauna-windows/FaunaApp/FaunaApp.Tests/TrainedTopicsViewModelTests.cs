using FaunaApp.Core.ViewModels;
using uniffi.fauna_feed;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The Personalization home's Trained-topics facet VM (topic-factors.md
/// § Authoring surface &amp; picker): list/create/rename/delete over
/// <c>trained_topics_{list,create,rename,delete}</c>
/// (INestRpcClient.TrainedTopics{List,Create,Rename,Delete}Async).
/// </summary>
public class TrainedTopicsViewModelTests
{
    private static FfiTrainedTopicRow Row(
        byte[] id, string name, string? factorKey, uint count, bool learnFromEngagement = false) =>
        new(id, name, factorKey, count, learnFromEngagement);

    /// Widening <see cref="FfiPublishedList"/>/<see cref="FfiPublishEntry"/> touches
    /// exactly these two factories instead of every
    /// hand-rolled positional call site.
    private static FfiPublishedList PublishedList(byte[] labelerId, ulong version, uint entryCount) =>
        new(labelerId, version, entryCount);

    private static FfiPublishEntry PublishEntry(string postId, long score) =>
        new(postId, score);

    [Fact]
    public async Task LoadAsync_PopulatesTopics()
    {
        var rows = new[] { Row(new byte[16], "Cats", "topic:aa", 3) };
        var rpc = new MockNestRpcClient { NextTrainedTopics = rows };
        var vm = new TrainedTopicsViewModel(rpc);

        await vm.LoadAsync();

        Assert.Contains("TrainedTopicsList", rpc.Calls);
        Assert.Equal(rows, vm.Topics);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task CreateAsync_TrimsName_AndPersists()
    {
        var rpc = new MockNestRpcClient
        {
            NextTrainedTopics = new[] { Row(new byte[16], "Cats", "topic:aa", 0) },
        };
        var vm = new TrainedTopicsViewModel(rpc);

        var ok = await vm.CreateAsync("  Cats  ");

        Assert.True(ok);
        Assert.Contains("TrainedTopicsCreate", rpc.Calls);
        Assert.Equal("Cats", rpc.LastTrainedTopicsCreateName);
        Assert.Single(vm.Topics);
    }

    [Fact]
    public async Task CreateAsync_BlankName_NoRpcCall()
    {
        var rpc = new MockNestRpcClient();
        var vm = new TrainedTopicsViewModel(rpc);

        var ok = await vm.CreateAsync("   ");

        Assert.False(ok);
        Assert.DoesNotContain("TrainedTopicsCreate", rpc.Calls);
    }

    [Fact]
    public async Task RenameAsync_KeepsId_PersistsNewName()
    {
        var id = new byte[16] { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16 };
        var rpc = new MockNestRpcClient
        {
            NextTrainedTopics = new[] { Row(id, "Kittens", "topic:aa", 0) },
        };
        var vm = new TrainedTopicsViewModel(rpc);

        var ok = await vm.RenameAsync(id, "Kittens");

        Assert.True(ok);
        Assert.Equal((id, "Kittens"), rpc.LastTrainedTopicsRename);
        Assert.Equal("Kittens", vm.Topics[0].name);
    }

    [Fact]
    public async Task DeleteAsync_RemovesRow()
    {
        var id = new byte[16];
        var rpc = new MockNestRpcClient { NextTrainedTopics = Array.Empty<FfiTrainedTopicRow>() };
        var vm = new TrainedTopicsViewModel(rpc);

        var ok = await vm.DeleteAsync(id);

        Assert.True(ok);
        Assert.Equal(id, rpc.LastTrainedTopicsDeleteId);
        Assert.Empty(vm.Topics);
    }

    [Fact]
    public async Task SetLearnFromEngagementAsync_Persists()
    {
        var id = new byte[16] { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16 };
        var rpc = new MockNestRpcClient
        {
            NextTrainedTopics = new[]
            {
                Row(id, "Cats", "topic:aa", 0, learnFromEngagement: true),
            },
        };
        var vm = new TrainedTopicsViewModel(rpc);

        var ok = await vm.SetLearnFromEngagementAsync(id, true);

        Assert.True(ok);
        Assert.Equal("TrainedTopicsSetLearnFromEngagement", Assert.Single(rpc.Calls));
        Assert.Equal((id, true), rpc.LastTrainedTopicsSetLearnFromEngagement);
        Assert.True(vm.Topics[0].learnFromEngagement);
    }

    [Fact]
    public async Task LoadAsync_Failure_RoutesToError()
    {
        var rpc = new MockNestRpcClient { NextError = "boom" };
        var vm = new TrainedTopicsViewModel(rpc);

        await vm.LoadAsync();

        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
        Assert.Empty(vm.Topics);
    }

    // ── Staleness guard — the windows leg of tui's fix for the same
    // clobber. Nothing serializes the nav-edge/login-time LoadAsync against a
    // gesture: both are independent async-void fire points on
    // PersonalizationPage, so a slower load can resolve AFTER a faster
    // create/rename/delete/toggle and must not clobber the fresher rows (or,
    // on failure, the fresher success's cleared error) with a stale result.

    [Fact]
    public async Task StaleLoad_ResolvingAfterAFasterDelete_NeverOverwritesTheFresherRows()
    {
        var id = new byte[16] { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16 };
        var staleRow = Row(id, "Cats", "topic:aa", 3);
        var rpc = new MockNestRpcClient
        {
            TrainedTopicsListGate = new TaskCompletionSource<FfiTrainedTopicRow[]>(),
            NextTrainedTopics = Array.Empty<FfiTrainedTopicRow>(), // what the delete below returns
        };
        var vm = new TrainedTopicsViewModel(rpc);

        // Dispatch the nav-edge load FIRST (seq 1) — it parks on the gate, so
        // this does not complete yet.
        var loadTask = vm.LoadAsync();

        // A faster gesture dispatches SECOND (seq 2) and resolves immediately,
        // reflecting the post-delete state the switcher must end up showing.
        var deleteOk = await vm.DeleteAsync(id);
        Assert.True(deleteOk);
        Assert.Empty(vm.Topics);

        // The stale load finally resolves, carrying the PRE-delete row list.
        // It must be dropped as a no-op — reintroducing it would resurrect a
        // row the user just deleted.
        rpc.TrainedTopicsListGate!.SetResult(new[] { staleRow });
        await loadTask;

        Assert.Empty(vm.Topics);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task StaleLoadFailure_ResolvingAfterAFasterDeleteSucceeds_NeverSurfacesTheStaleError()
    {
        var id = new byte[16] { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16 };
        var rpc = new MockNestRpcClient
        {
            TrainedTopicsListGate = new TaskCompletionSource<FfiTrainedTopicRow[]>(),
            NextTrainedTopics = Array.Empty<FfiTrainedTopicRow>(),
        };
        var vm = new TrainedTopicsViewModel(rpc);

        var loadTask = vm.LoadAsync();

        var deleteOk = await vm.DeleteAsync(id);
        Assert.True(deleteOk);
        Assert.Null(vm.ErrorMessage);

        // The stale load finally fails. A failure this old must not clobber
        // the fresher delete's success by surfacing an error over it.
        rpc.TrainedTopicsListGate!.SetException(new FfiTrainedTopicsException.General("boom"));
        await loadTask;

        Assert.Null(vm.ErrorMessage);
        Assert.Empty(vm.Topics);
    }

    // ── Publishing a trained factor as a List (topic-factors.md § Publishing
    // a trained factor; frame D8) — bundled into this VM (§ Authoring surface
    // & picker's own claim: android/linux/web all bundle the sheet's state
    // into the trained-topics facet's own holder, not a second VM).

    [Fact]
    public async Task ScoreCorpusForFactorAsync_ReturnsExemplars_AndPassesTheFactorKeyThrough()
    {
        var exemplars = new[] { new ScoredExemplar("aa", "tabby kitten", 900) };
        var rpc = new MockNestRpcClient { NextScoredExemplars = exemplars };
        var vm = new TrainedTopicsViewModel(rpc);

        var result = await vm.ScoreCorpusForFactorAsync("topic:aa");

        Assert.Equal(exemplars, result);
        Assert.Equal("topic:aa", rpc.LastScoreCorpusForFactorArg);
        Assert.Contains("ScoreCorpusForFactor", rpc.Calls);
    }

    [Fact]
    public async Task ScoreCorpusForFactorAsync_Failure_PropagatesException_NeverTouchesErrorMessage()
    {
        // The sheet can be the FIRST call the page makes — a failure here must
        // not blank ErrorMessage, which the Topics list's own render reads
        // (topic-factors.md § Publishing; mirrors linux's error_label being the
        // sheet's own surface, not the facet's).
        var rpc = new MockNestRpcClient { NextError = "corpus read failed" };
        var vm = new TrainedTopicsViewModel(rpc);

        await Assert.ThrowsAsync<InvalidOperationException>(() => vm.ScoreCorpusForFactorAsync("topic:aa"));

        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task TrainedTopicPublishListAsync_PublishesTheKeptEntries()
    {
        var id = new byte[16] { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16 };
        var published = PublishedList(new byte[] { 9, 9 }, 1, 1);
        var rpc = new MockNestRpcClient { NextPublishedList = published };
        var vm = new TrainedTopicsViewModel(rpc);
        var entries = new[] { PublishEntry("aa", 900) };

        var result = await vm.TrainedTopicPublishListAsync(id, "Small orange cats", entries);

        Assert.Equal(published, result);
        Assert.Equal((id, "Small orange cats", (IReadOnlyList<FfiPublishEntry>)entries), rpc.LastTrainedTopicPublishList);
        Assert.Contains("TrainedTopicPublishList", rpc.Calls);
    }

    [Fact]
    public void LocalizePublish_General_ReturnsTheBoundarySuppliedMessage()
    {
        var ex = new FfiPublishListException.General("nest: nest down");

        Assert.Equal("nest: nest down", TrainedTopicsViewModel.LocalizePublish(ex));
    }

    [Fact]
    public void LocalizePublish_NameTooLong_FallsBackToExceptionMessage()
    {
        // No dedicated i18n copy is ratified for this case yet (mirrors
        // apple/android's raw-message fallback) — just assert it doesn't
        // throw and returns SOME non-empty text, not a specific phrase.
        var ex = new FfiPublishListException.NameTooLong(500);

        var text = TrainedTopicsViewModel.LocalizePublish(ex);

        Assert.False(string.IsNullOrEmpty(text));
    }
}
