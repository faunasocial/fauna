using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_conversations;
using uniffi.fauna_core;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The Events page's refused-change list (docs/goal/ui/events.md § Refused
/// scheduling changes): every line is the shared <c>refused_change_view</c>'s
/// <c>LocalizedText</c> resolved through <see cref="Strings"/>; the list is
/// empty (so absent from the page) when nothing was refused; dismiss is a local
/// store write that clears the row; and the OPEN page re-reads through the
/// shared store-change notice — the witness stays on the page and never
/// re-enters, and the calendar poll bails on a refused CANCEL.
///
/// <para>[Collection("StringsGlobal")]: the localizer is process-global.</para>
/// </summary>
[Collection("StringsGlobal")]
public class EventsRefusedChangesTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["events/refused_changes/cancel_attempt"] = "Someone tried to cancel \"{title}\"",
            ["events/refused_changes/sender"] = "Sent by {who}",
            ["events/refused_changes/reason/not_the_organizer"] = "They are not the organizer of this event.",
            ["events/refused_changes/attempts"] = "Tried {count} times",
        };
        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public EventsRefusedChangesTests() => Strings.Initialize(new FakeLocalizer());

    private static ConversationsManager NewManager()
    {
        var m = new ConversationsManager();
        m.InstallMockBackendsForTest();
        return m;
    }

    private static LocalizedText Text(string key, params (string, string)[] args) =>
        new(key, args.ToDictionary(a => a.Item1, a => a.Item2));

    private static FfiRefusedChange Change(string key, bool repeated = false, long at = 1_700_000_000) => new(
        key,
        Text("events.refused_changes.cancel_attempt", ("title", "Standup")),
        Text("events.refused_changes.sender", ("who", "@bob")),
        Text("events.refused_changes.reason.not_the_organizer"),
        repeated ? Text("events.refused_changes.attempts", ("count", "3")) : null,
        at);

    [Fact]
    public async Task Load_NothingRefused_ListIsEmpty()
    {
        var vm = new EventsViewModel(new MockNestRpcClient());

        await vm.LoadRefusedChangesAsync(NewManager());

        Assert.Empty(vm.RefusedChanges);
    }

    [Fact]
    public async Task Load_RowsCarryTheSharedWordingAndTheAttemptsRideTheReasonLine()
    {
        var rpc = new MockNestRpcClient { NextRefusedChanges = new[] { Change("k1"), Change("k2", repeated: true) } };
        var vm = new EventsViewModel(rpc);

        await vm.LoadRefusedChangesAsync(NewManager());

        Assert.Equal(2, vm.RefusedChanges.Count);
        var first = vm.RefusedChanges[0];
        Assert.Equal("k1", first.Key);
        Assert.Equal("Someone tried to cancel \"Standup\"", first.Title);
        Assert.Equal("Sent by @bob", first.Author);
        Assert.Equal("They are not the organizer of this event.", first.Reason);
        Assert.False(string.IsNullOrEmpty(first.Time));
        Assert.Equal("They are not the organizer of this event. Tried 3 times", vm.RefusedChanges[1].Reason);
    }

    [Fact]
    public async Task Dismiss_ClearsTheRowThroughTheStoreWrite()
    {
        var rpc = new MockNestRpcClient { NextRefusedChanges = new[] { Change("k1"), Change("k2") } };
        var vm = new EventsViewModel(rpc);
        var manager = NewManager();
        await vm.LoadRefusedChangesAsync(manager);

        await vm.DismissRefusedChangeAsync("k1", manager);

        Assert.Equal("k1", rpc.LastRefusedChangeDismiss);
        Assert.Equal(new[] { "k2" }, vm.RefusedChanges.Select(r => r.Key));
    }

    [Fact]
    public async Task StoreChange_ARefusalLandingOnTheOpenPagePaintsWithNoReEntry()
    {
        var rpc = new MockNestRpcClient();
        var vm = new EventsViewModel(rpc);
        var manager = NewManager();
        await vm.LoadRefusedChangesAsync(manager);
        var resynced = 0;
        vm.StoreResynced += () => resynced++;
        vm.WatchStore(() => manager);

        rpc.NextRefusedChanges = new[] { Change("k1") };
        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();

        Assert.Equal(new[] { "k1" }, vm.RefusedChanges.Select(r => r.Key));
        Assert.Equal(1, resynced);
    }

    [Fact]
    public async Task StoreChange_AnUnchangedAnswerPaintsNothing()
    {
        var rpc = new MockNestRpcClient { NextRefusedChanges = new[] { Change("k1") } };
        var vm = new EventsViewModel(rpc);
        var manager = NewManager();
        await vm.LoadRefusedChangesAsync(manager);
        var resynced = 0;
        vm.StoreResynced += () => resynced++;
        vm.WatchStore(() => manager);

        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();

        Assert.Equal(0, resynced);
        Assert.Single(vm.RefusedChanges);
    }

    [Fact]
    public async Task StoreChange_ALeftPageStopsReReading()
    {
        var rpc = new MockNestRpcClient();
        var vm = new EventsViewModel(rpc);
        var manager = NewManager();
        vm.WatchStore(() => manager);
        Assert.Equal(1, rpc.StoreChangedSubscribers);

        vm.CleanupStoreChange();
        rpc.NextRefusedChanges = new[] { Change("k1") };
        rpc.RaiseStoreChanged();
        await vm.WhenStoreResyncedAsync();

        Assert.Equal(0, rpc.StoreChangedSubscribers);
        Assert.Empty(vm.RefusedChanges);
    }
}
