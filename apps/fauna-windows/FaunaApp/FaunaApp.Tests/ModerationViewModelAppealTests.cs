using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_moderation;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// The moderation queue's appeal affordance (moderation.md § Legal takedown — the
/// transparency triple's second leg). Every gate and sentence is shared Rust's
/// (<c>row_is_appealable</c>, <c>appeal_form_view</c>, <c>appeal_verdict</c>); these pin
/// the three properties every app keeps: the outcome is the form's own status (never the
/// page error), success closes the form while failure keeps it open with the reason
/// intact, and the draft captures its row's <c>content_id</c> at OPEN time.
/// </summary>
public class ModerationViewModelAppealTests
{
    private static FfiObligationAction ServerRow(string contentId, long ts) =>
        new(0L, "post", contentId, "illegal", 1000, 5, ts);

    private static LocalDetection LocalRow(string contentId, long ts) =>
        new(contentId, "message", "spam", 720, ts);

    private sealed class FakeLocal : IModerationLocalDetections
    {
        private readonly List<LocalDetection> _items;
        public FakeLocal(params LocalDetection[] items) => _items = new List<LocalDetection>(items);
        public IReadOnlyList<LocalDetection> Snapshot() => _items;
        public string? Body(string contentId) => null;
        public void Remove(string contentId) => _items.RemoveAll(d => d.contentId == contentId);
    }

    private static async Task<(ModerationViewModel Vm, MockNestRpcClient Rpc)> LoadedAsync(
        params string[] serverIds)
    {
        var actions = new List<FfiObligationAction>();
        long ts = 1000;
        foreach (var id in serverIds) actions.Add(ServerRow(id, ts--));
        var rpc = new MockNestRpcClient { NextModerationActions = actions };
        var vm = new ModerationViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        return (vm, rpc);
    }

    [Fact]
    public async Task OnlyAServerRowOffersAnAppeal()
    {
        var rpc = new MockNestRpcClient
        {
            NextModerationActions = new List<FfiObligationAction> { ServerRow("aa", 200) },
        };
        var vm = new ModerationViewModel(rpc, new FakeLocal(LocalRow("bb", 100)));
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.True(vm.Actions[0].IsAppealable);   // nest-issued: there is a decision to appeal
        Assert.False(vm.Actions[1].IsAppealable);  // client-side local detection: none
    }

    [Fact]
    public async Task OpenAppeal_ReasonlessIsNotSubmittable_ReasonedIs()
    {
        var (vm, _) = await LoadedAsync("aa");

        vm.OpenAppeal(vm.Actions[0]);

        Assert.True(vm.AppealOpen);
        Assert.False(vm.AppealCanSubmit);
        Assert.False(string.IsNullOrEmpty(vm.AppealBlockedReason));

        vm.AppealReason = "I hold the licence.";

        Assert.True(vm.AppealCanSubmit);
        Assert.Null(vm.AppealBlockedReason);
    }

    [Fact]
    public async Task Submit_Success_DispatchesAndClosesTheForm_WithTheRecordedLine_NotAPageError()
    {
        var (vm, rpc) = await LoadedAsync("aa");
        vm.OpenAppeal(vm.Actions[0]);
        vm.AppealReason = "I hold the licence.";

        await vm.SubmitAppealAsync();

        Assert.Equal(("aa", "I hold the licence."), rpc.LastAppeal);
        Assert.False(vm.AppealOpen);
        // The shared verdict's words (the unit host carries no .resw, so compare to
        // the fold's own resolution rather than the English sentence).
        Assert.Equal(Strings.Resolve(FaunaFfiMethods.AppealVerdict(null)), vm.AppealStatus);
        Assert.Null(vm.ErrorMessage);              // form feedback, never the page error
        Assert.Single(vm.Actions);                 // additive history: the row stays
    }

    [Fact]
    public async Task Submit_Failure_KeepsTheFormOpen_ReasonIntact_StatusNotPageError()
    {
        var (vm, rpc) = await LoadedAsync("aa");
        rpc.AppealError = new InvalidOperationException("fauna.moderation.not_found");
        vm.OpenAppeal(vm.Actions[0]);
        vm.AppealReason = "Please look again.";

        await vm.SubmitAppealAsync();

        Assert.True(vm.AppealOpen);
        Assert.Equal("Please look again.", vm.AppealReason);
        Assert.False(string.IsNullOrEmpty(vm.AppealStatus));
        Assert.NotEqual(Strings.Resolve(FaunaFfiMethods.AppealVerdict(null)), vm.AppealStatus);
        Assert.Null(vm.ErrorMessage);
        Assert.Single(vm.Actions);                 // the queue the user is reading is not blanked
    }

    [Fact]
    public async Task TheDraftCapturesItsRowsContentIdAtOpenTime()
    {
        var (vm, rpc) = await LoadedAsync("aa", "bb");
        vm.OpenAppeal(vm.Actions[1]);              // the row for "bb"
        vm.AppealReason = "wrong call";

        // The queue refetches under the open form and the rows reorder: a dispatch
        // keyed on the row INDEX would now appeal "aa".
        rpc.NextModerationActions = new List<FfiObligationAction>
        {
            ServerRow("cc", 3000), ServerRow("bb", 2000), ServerRow("aa", 1000),
        };
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.SubmitAppealAsync();

        Assert.Equal(("bb", "wrong call"), rpc.LastAppeal);
    }

    [Fact]
    public async Task Cancel_DropsTheDraft_AndTheStaleStatus()
    {
        var (vm, rpc) = await LoadedAsync("aa");
        vm.OpenAppeal(vm.Actions[0]);
        vm.AppealReason = "x";

        vm.CancelAppeal();

        Assert.False(vm.AppealOpen);
        Assert.Null(vm.AppealStatus);
        Assert.Null(rpc.LastAppeal);
    }

    [Fact]
    public async Task SubmitWithoutAReason_DispatchesNothing()
    {
        var (vm, rpc) = await LoadedAsync("aa");
        vm.OpenAppeal(vm.Actions[0]);

        await vm.SubmitAppealAsync();

        Assert.Null(rpc.LastAppeal);
        Assert.True(vm.AppealOpen);
    }
}
