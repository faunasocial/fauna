using FaunaApp.Core.Helpers;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_core;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The account page's STANDING unsent-changes section on windows
/// (<c>account-offline-mutation.md</c> § The offline-mutation contract →
/// <i>The unsent-changes surface</i>, point 1): pending actions' three-state shape, one
/// row per undrained intent with the shared describer's words, retry and a confirmed
/// discard on a not-sent row only. The doors are faked; the row words run through the
/// real shared Rust (<c>unsent_change_state_text</c> / <c>unsent_change_reason_text</c>),
/// so no state or reason word is decided here. tui's
/// <c>the_unsent_changes_section_is_standing_and_offers_retry_on_a_not_sent_row_only</c>
/// and android's <c>UnsentChangesCardTest</c> are the twins.
/// </summary>
[Collection("StringsGlobal")]
public class SettingsUnsentChangesTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["settings/unsent_changes/kind_like"] = "Like on a post",
            ["settings/unsent_changes/kind_message"] = "Message: {excerpt}",
            ["settings/unsent_changes/state_waiting"] = "Waiting to send",
            ["settings/unsent_changes/state_held"] = "Waiting behind a change that was not sent",
            ["settings/unsent_changes/state_unsent"] = "Not sent",
            ["settings/unsent_changes/status_with_reason"] = "{state} — {reason}",
            ["settings/unsent_changes/reason_fallback"] = "Your nest refused this change ({code})",
        };

        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public SettingsUnsentChangesTests() => Strings.Initialize(new FakeLocalizer());

    private sealed class FakeDoors : IUnsentChangesDoors
    {
        public List<UnsentChange> Rows { get; set; } = new();
        public List<string> Calls { get; } = new();
        public bool FailNextList { get; set; }

        public Task<IReadOnlyList<UnsentChange>> ListAsync()
        {
            Calls.Add("List");
            if (FailNextList)
            {
                FailNextList = false;
                throw new InvalidOperationException("the store is not open");
            }
            return Task.FromResult<IReadOnlyList<UnsentChange>>(Rows.ToList());
        }

        public Task<bool> RetryAsync(string intentId)
        {
            Calls.Add($"Retry {intentId}");
            return Task.FromResult(true);
        }

        public Task<bool> DiscardAsync(string intentId)
        {
            Calls.Add($"Discard {intentId}");
            Rows.RemoveAll(r => r.@intentId == intentId);
            return Task.FromResult(true);
        }
    }

    private static UnsentChange Change(string id, UnsentState state, string labelKey = "settings.unsent_changes.kind_like") =>
        new(
            intentId: id,
            kind: "fauna.posts.interact",
            scope: "posts",
            state: state,
            createdAt: 1_700_000_000,
            attempts: 0,
            description: new IntentDescription(
                new LocalizedText(labelKey, new Dictionary<string, string>()), null),
            hold: null);

    private static SettingsViewModel Vm(FakeDoors doors) =>
        new(new MockNestHttpClient(), new MockNestRpcClient(), unsentDoors: doors);

    [Fact]
    public async Task TheSectionIsNullBeforeTheFirstRead_AndEmptyOnceReadWithNothingUnsent()
    {
        var doors = new FakeDoors();
        var vm = Vm(doors);
        Assert.Null(vm.UnsentChanges);

        await vm.LoadUnsentChangesAsync();

        Assert.NotNull(vm.UnsentChanges);
        Assert.Empty(vm.UnsentChanges!);
    }

    [Fact]
    public async Task EachRowCarriesTheSharedWords_AndOnlyANotSentRowOffersRetryAndDiscard()
    {
        var doors = new FakeDoors
        {
            Rows =
            {
                Change("aa", new UnsentState.Waiting()),
                Change("bb", new UnsentState.Unsent("NotFound")),
                Change("cc", new UnsentState.Held()),
            },
        };
        var vm = Vm(doors);

        await vm.LoadUnsentChangesAsync();

        var rows = vm.UnsentChanges!;
        Assert.Equal(new[] { "aa", "bb", "cc" }, rows.Select(r => r.IntentId));
        Assert.All(rows, r => Assert.Equal("Like on a post", r.Description));

        Assert.Equal("Waiting to send", rows[0].Status);
        Assert.False(rows[0].ShowsRetry);
        Assert.False(rows[0].ShowsDiscard);

        // A not-sent row says why: the shared reason, here the fallback for a code the
        // reason map does not name, through status_with_reason.
        Assert.Equal("Not sent — Your nest refused this change (NotFound)", rows[1].Status);
        Assert.True(rows[1].ShowsRetry);
        Assert.True(rows[1].ShowsDiscard);
        Assert.False(rows[1].ShowsDiscardConfirm);

        Assert.Equal("Waiting behind a change that was not sent", rows[2].Status);
        Assert.False(rows[2].ShowsRetry);
    }

    [Fact]
    public async Task RetryReArmsTheChangeThroughTheDoor_ThenReReads()
    {
        var doors = new FakeDoors { Rows = { Change("bb", new UnsentState.Unsent("NotFound")) } };
        var vm = Vm(doors);
        await vm.LoadUnsentChangesAsync();
        doors.Calls.Clear();

        await vm.RetryUnsentChangeAsync("bb");

        Assert.Equal(new[] { "Retry bb", "List" }, doors.Calls);
    }

    [Fact]
    public async Task DiscardArmsTheConfirmFirst_AndOnlyTheConfirmDeletes()
    {
        var doors = new FakeDoors { Rows = { Change("bb", new UnsentState.Unsent("NotFound")) } };
        var vm = Vm(doors);
        await vm.LoadUnsentChangesAsync();

        vm.ArmUnsentDiscard("bb");

        var armed = Assert.Single(vm.UnsentChanges!);
        Assert.True(armed.ShowsDiscardConfirm);
        Assert.False(armed.ShowsDiscard);
        Assert.DoesNotContain(doors.Calls, c => c.StartsWith("Discard"));

        await vm.ConfirmUnsentDiscardAsync("bb");

        Assert.Contains("Discard bb", doors.Calls);
        Assert.NotNull(vm.UnsentChanges);
        Assert.Empty(vm.UnsentChanges!);
    }

    [Fact]
    public async Task AnArmedDiscardWhoseRowIsNoLongerNotSentIsDisarmed()
    {
        var doors = new FakeDoors { Rows = { Change("bb", new UnsentState.Unsent("NotFound")) } };
        var vm = Vm(doors);
        await vm.LoadUnsentChangesAsync();
        vm.ArmUnsentDiscard("bb");

        // Retried elsewhere: the row waits again.
        doors.Rows[0] = Change("bb", new UnsentState.Waiting());
        await vm.LoadUnsentChangesAsync();
        doors.Rows[0] = Change("bb", new UnsentState.Unsent("NotFound"));
        await vm.LoadUnsentChangesAsync();

        var row = Assert.Single(vm.UnsentChanges!);
        Assert.False(row.ShowsDiscardConfirm);
        Assert.True(row.ShowsDiscard);
    }

    [Fact]
    public async Task AFailedReadKeepsWhatTheLastReadShowed_AndAnUnchangedReadRepaintsNothing()
    {
        var doors = new FakeDoors { Rows = { Change("aa", new UnsentState.Waiting()) } };
        var vm = Vm(doors);
        await vm.LoadUnsentChangesAsync();
        var shown = vm.UnsentChanges;
        var changes = 0;
        vm.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName == nameof(SettingsViewModel.UnsentChanges)) changes++;
        };

        doors.FailNextList = true;
        await vm.LoadUnsentChangesAsync();
        await vm.LoadUnsentChangesAsync();

        Assert.Same(shown, vm.UnsentChanges);
        Assert.Equal(0, changes);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task TheRefreshReadsAtOnce_AndStopsWhenDisposed()
    {
        var reads = 0;
        var refresh = UnsentRefresh.Start("test", () =>
        {
            reads++;
            return Task.CompletedTask;
        }, TimeSpan.FromHours(1));

        Assert.Equal(1, reads);
        refresh.Dispose();
        await refresh.Running;
        Assert.Equal(1, reads);
    }
}
