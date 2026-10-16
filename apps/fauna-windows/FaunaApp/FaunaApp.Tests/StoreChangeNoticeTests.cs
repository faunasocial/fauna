using System;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The store-change notice on windows (<c>account-runtime.md</c> § Multi-instance
/// concurrency → <i>A runtime's own pump is a source of the notice too</i>, part 4):
/// the UniFFI face's payload-free <c>store_changed</c> reaches the app as
/// <see cref="INestRpcClient.StoreChanged"/>, and the page that is OPEN re-runs its
/// own store-backed load — paints only what differs, never discards an edit in
/// progress, and a page that has been left listens no more.
///
/// <para>Serialized with the other tests that touch the process-global caches
/// (<see cref="MutedKeywordsCache"/>, <see cref="InheritedFilterMarks"/>,
/// <see cref="ContentPolicyCache"/>).</para>
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class StoreChangeNoticeTests : IDisposable
{
    public StoreChangeNoticeTests()
    {
        MutedKeywordsCache.Reset();
        InheritedFilterMarks.Reset();
        ContentPolicyCache.SetHiddenContent(null);
    }

    public void Dispose()
    {
        MutedKeywordsCache.Reset();
        InheritedFilterMarks.Reset();
        ContentPolicyCache.SetHiddenContent(null);
    }

    // ── the relay: the UniFFI face → the app's event ────────────────────────

    [Fact]
    public void TheRelayRaisesStoreChangedOnTheClient()
    {
        var rpc = new NestRpcClient("https://nest.example", new MockCryptoService());
        var raised = 0;
        rpc.StoreChanged += () => raised++;

        // The Rust relay calls this on a runtime thread; with no UI context
        // captured (a unit test) the handler runs inline.
        FfiStoreChangeListener listener = new NestRpcClient.StoreChangeRelay(rpc);
        listener.StoreChanged();
        listener.StoreChanged();

        Assert.Equal(2, raised);
    }

    [Fact]
    public void ANoticeWithNoSubscriberIsDroppedNotAFault()
    {
        var rpc = new NestRpcClient("https://nest.example", new MockCryptoService());

        FfiStoreChangeListener listener = new NestRpcClient.StoreChangeRelay(rpc);

        listener.StoreChanged(); // nobody listening: the next visit's own load reads fresh state
    }

    // ── Muted words ─────────────────────────────────────────────────────────

    [Fact]
    public async Task MutedWords_ANoticeRepaintsTheOpenPageWithAnotherDevicesWord()
    {
        var rpc = new MockNestRpcClient { NextMutedKeywords = new[] { "spam" } };
        var vm = new MutedWordsViewModel(rpc);
        await vm.LoadAsync();
        var resynced = 0;
        vm.StoreResynced += () => resynced++;
        vm.WatchStore();

        rpc.NextMutedKeywords = new[] { "spam", "lottery" };
        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();

        Assert.Equal(new[] { "spam", "lottery" }, vm.Words);
        Assert.Equal(new[] { "spam", "lottery" }, MutedKeywordsCache.Words);
        Assert.Equal(1, resynced);
    }

    [Fact]
    public async Task MutedWords_AnUnchangedAnswerPaintsNothing()
    {
        var rpc = new MockNestRpcClient { NextMutedKeywords = new[] { "spam" } };
        var vm = new MutedWordsViewModel(rpc);
        await vm.LoadAsync();
        var resynced = 0;
        var propertyChanges = 0;
        vm.StoreResynced += () => resynced++;
        vm.PropertyChanged += (_, _) => propertyChanges++;
        vm.WatchStore();

        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();

        Assert.Equal(0, resynced);
        Assert.Equal(0, propertyChanges);
        Assert.Equal(new[] { "spam" }, vm.Words);
    }

    [Fact]
    public async Task MutedWords_AFailedReReadKeepsTheListAndRaisesNoError()
    {
        var rpc = new MockNestRpcClient { NextMutedKeywords = new[] { "spam" } };
        var vm = new MutedWordsViewModel(rpc);
        await vm.LoadAsync();
        vm.WatchStore();

        rpc.NextError = "offline";
        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();

        Assert.Equal(new[] { "spam" }, vm.Words);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task MutedWords_APageThatWasLeftHearsNothingMore()
    {
        var rpc = new MockNestRpcClient { NextMutedKeywords = new[] { "spam" } };
        var vm = new MutedWordsViewModel(rpc);
        await vm.LoadAsync();
        vm.WatchStore();
        Assert.Equal(1, rpc.StoreChangedSubscribers);

        vm.CleanupStoreChange();
        rpc.NextMutedKeywords = new[] { "spam", "lottery" };
        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();

        Assert.Equal(0, rpc.StoreChangedSubscribers);
        Assert.Equal(new[] { "spam" }, vm.Words);
    }

    [Fact]
    public async Task MutedWords_WatchingTwiceSubscribesOnce()
    {
        var rpc = new MockNestRpcClient();
        var vm = new MutedWordsViewModel(rpc);

        vm.WatchStore();
        vm.WatchStore();

        Assert.Equal(1, rpc.StoreChangedSubscribers);
        vm.CleanupStoreChange();
        Assert.Equal(0, rpc.StoreChangedSubscribers);
        await Task.CompletedTask;
    }

    [Fact]
    public async Task MutedWords_ANoticeDuringAGestureNeverOvertakesTheGesturesAnswer()
    {
        var rpc = new MockNestRpcClient { NextMutedKeywords = new[] { "spam" } };
        var vm = new MutedWordsViewModel(rpc);
        await vm.LoadAsync();
        vm.WatchStore();

        // The re-read answered before the gesture's write landed; the gesture
        // started after the read began, so its answer is the fresher one.
        var staleRead = vm.StoreResyncAsync();
        rpc.NextMutedKeywords = new[] { "spam", "lottery" };
        await vm.AddAsync("lottery");
        await staleRead;

        Assert.Equal(new[] { "spam", "lottery" }, vm.Words);
    }

    // ── Trained topics ──────────────────────────────────────────────────────

    private static FfiTrainedTopicRow Topic(string name, byte seed = 1) =>
        new(Enumerable.Repeat(seed, 16).ToArray(), name, $"topic:{seed:x2}", 0, false);

    [Fact]
    public async Task TrainedTopics_ANoticeRepaintsTheOpenPage_AndAnEqualReadPaintsNothing()
    {
        var rpc = new MockNestRpcClient { NextTrainedTopics = new[] { Topic("jazz") } };
        var vm = new TrainedTopicsViewModel(rpc);
        await vm.LoadAsync();
        var resynced = 0;
        vm.StoreResynced += () => resynced++;
        vm.WatchStore();

        // A fresh array of the same content: the generated record would compare
        // the id byte array by reference and call it a change.
        rpc.NextTrainedTopics = new[] { Topic("jazz") };
        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();
        Assert.Equal(0, resynced);

        rpc.NextTrainedTopics = new[] { Topic("jazz"), Topic("chess", 2) };
        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();
        Assert.Equal(1, resynced);
        Assert.Equal(new[] { "jazz", "chess" }, vm.Topics.Select(t => t.name));
    }

    [Fact]
    public async Task TrainedTopics_ARetiresOnlyTheErrorAFailedLoadPutUp()
    {
        var rpc = new MockNestRpcClient { NextError = "offline" };
        var vm = new TrainedTopicsViewModel(rpc);
        await vm.LoadAsync();
        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
        vm.WatchStore();

        rpc.NextError = null;
        rpc.NextTrainedTopics = new[] { Topic("jazz") };
        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();

        Assert.Null(vm.ErrorMessage);
        Assert.Single(vm.Topics);
    }

    [Fact]
    public async Task TrainedTopics_AGesturesErrorSurvivesANotice()
    {
        var rpc = new MockNestRpcClient { NextTrainedTopics = new[] { Topic("jazz") } };
        var vm = new TrainedTopicsViewModel(rpc);
        await vm.LoadAsync();
        vm.WatchStore();
        rpc.NextError = "create refused";
        await vm.CreateAsync("chess");
        var gestureError = vm.ErrorMessage;
        Assert.False(string.IsNullOrEmpty(gestureError));

        rpc.NextError = null;
        rpc.NextTrainedTopics = new[] { Topic("jazz"), Topic("chess", 2) };
        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();

        Assert.Equal(gestureError, vm.ErrorMessage);
    }

    // ── Task delegation ─────────────────────────────────────────────────────

    [Fact]
    public async Task TaskDelegation_AFailedReReadKeepsThePaintedRowsAndRaisesNoError()
    {
        var rpc = new MockNestRpcClient();
        var vm = new TaskDelegationViewModel(rpc, "device-a");
        await vm.LoadAsync();
        var before = vm.Rows.Count;
        vm.WatchStore();

        rpc.NextError = "offline";
        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();

        Assert.Equal(before, vm.Rows.Count);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task TaskDelegation_ANoticeReReadsTheRowsOfTheOpenPageOnly()
    {
        var rpc = new MockNestRpcClient();
        var vm = new TaskDelegationViewModel(rpc, "device-a");
        await vm.LoadAsync();
        vm.WatchStore();
        var listsBefore = rpc.Calls.Count(c => c == "TaskDelegationList");

        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();
        Assert.Equal(listsBefore + 1, rpc.Calls.Count(c => c == "TaskDelegationList"));

        vm.CleanupStoreChange();
        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();
        Assert.Equal(listsBefore + 1, rpc.Calls.Count(c => c == "TaskDelegationList"));
    }

    // ── Backups destinations ────────────────────────────────────────────────

    [Fact]
    public async Task Backups_ANoticeSwapsInAChangedList_AndAnEqualOneCostsNoFurtherRoundTrip()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new[] { MockNestRpcClient.MakeDestination("d1", "https://a.example", "A") },
        };
        var vm = new BackupsViewModel(rpc);
        await vm.LoadDestinationsAsync();
        var resynced = 0;
        vm.StoreResynced += () => resynced++;
        vm.WatchStore();

        // Equal content, fresh instances.
        rpc.NextDestinations = new[] { MockNestRpcClient.MakeDestination("d1", "https://a.example", "A") };
        var statusReads = rpc.Calls.Count(c => c == "BackupDestinationStatus");
        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();
        Assert.Equal(0, resynced);
        Assert.Equal(statusReads, rpc.Calls.Count(c => c == "BackupDestinationStatus"));

        rpc.NextDestinations = new[]
        {
            MockNestRpcClient.MakeDestination("d1", "https://a.example", "A"),
            MockNestRpcClient.MakeDestination("d2", "https://b.example", "B"),
        };
        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();
        Assert.Equal(1, resynced);
        Assert.Equal(2, vm.Destinations.Count);
    }

    [Fact]
    public async Task Backups_AFailedReReadKeepsTheListAndTheErrorBannerClear()
    {
        var rpc = new MockNestRpcClient
        {
            NextDestinations = new[] { MockNestRpcClient.MakeDestination("d1", "https://a.example", "A") },
        };
        var vm = new BackupsViewModel(rpc);
        await vm.LoadDestinationsAsync();
        vm.WatchStore();

        rpc.NextError = "offline";
        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();

        Assert.Single(vm.Destinations);
        Assert.Null(vm.ErrorMessage);
    }

    // ── Member review ───────────────────────────────────────────────────────

    [Fact]
    public async Task MemberReview_ANoticeRepaintsTheRoster_AndAnEqualReadPaintsNothing()
    {
        var rpc = new MockNestRpcClient();
        var vm = new MemberReviewViewModel(rpc);
        await vm.LoadAsync(null);
        var resynced = 0;
        vm.StoreResynced += () => resynced++;
        vm.WatchStore(() => null);

        rpc.NextMemberReviews = new[] { MockNestRpcClient.MakeMemberReview(Enumerable.Repeat((byte)7, 32).ToArray()) };
        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();
        Assert.Equal(1, resynced);
        Assert.Single(vm.Reviews);

        // The same person, a fresh array: nothing to repaint.
        rpc.NextMemberReviews = new[] { MockNestRpcClient.MakeMemberReview(Enumerable.Repeat((byte)7, 32).ToArray()) };
        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();
        Assert.Equal(1, resynced);
    }

    [Fact]
    public async Task Contacts_ANoticeMovesTheBadgeRosterOnly()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ContactsViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        var resynced = 0;
        vm.StoreResynced += () => resynced++;
        vm.WatchStore();
        var contactReads = rpc.Calls.Count(c => c == "ContactsList");

        rpc.NextMemberReviews = new[] { MockNestRpcClient.MakeMemberReview() };
        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();

        Assert.Equal(1, resynced);
        Assert.Single(vm.MemberReviewRoster);
        Assert.Equal(contactReads, rpc.Calls.Count(c => c == "ContactsList"));
    }

    // ── App-wide: the hide list and the ledger's filter marks ───────────────

    [Fact]
    public async Task TheHideListReReadsOnANotice_AndKeepsItsCacheWhenTheReadFails()
    {
        var rpc = new MockNestRpcClient();
        rpc.HiddenContent.Add("cid-1");
        var preloader = new ContentPolicyPreloader(rpc);

        await preloader.RefreshHiddenContentAsync();
        Assert.True(ContentPolicyCache.HiddenContentEquals(new[] { "cid-1" }));

        rpc.HiddenContent.Add("cid-2");
        await preloader.RefreshHiddenContentAsync();
        Assert.True(ContentPolicyCache.HiddenContentEquals(new[] { "cid-1", "cid-2" }));

        rpc.NextError = "offline";
        await preloader.RefreshHiddenContentAsync();
        Assert.True(
            ContentPolicyCache.HiddenContentEquals(new[] { "cid-1", "cid-2" }),
            "a failed read under-enforces nothing it already knew: the cached list stays");
    }

    [Fact]
    public async Task TheFilterMarksRaiseChangedOnlyWhenTheSetMoved()
    {
        var rpc = new MockNestRpcClient { NextFilterMarks = new long[] { 7 } };
        var changed = 0;
        void OnChanged() => changed++;
        InheritedFilterMarks.Changed += OnChanged;
        try
        {
            await InheritedFilterMarks.RefreshAsync(rpc);
            Assert.Equal(1, changed);

            await InheritedFilterMarks.RefreshAsync(rpc);
            Assert.Equal(1, changed);

            rpc.NextFilterMarks = new long[] { 7, 9 };
            await InheritedFilterMarks.RefreshAsync(rpc);
            Assert.Equal(2, changed);
        }
        finally
        {
            InheritedFilterMarks.Changed -= OnChanged;
        }
    }
}
