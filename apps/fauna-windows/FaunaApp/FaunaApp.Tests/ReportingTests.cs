using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_core;
using uniffi.fauna_ffi;
using Xunit;
using ContentLabelEntry = uniffi.fauna_core.ContentLabelEntry;

namespace FaunaApp.Tests;

/// <summary>
/// User-initiated reporting on windows (moderation.md § User-initiated reporting):
/// the shared report sheet's follow-up order, the reporter-side hide as a third
/// input to the item verdict, the reporter's ledger and the admin queue.
///
/// <para>Everything that DECIDES is shared Rust, called for real (the native dll
/// loads in the test host — reference_windows_dotnet_test_loads_native_ffi): the
/// reason list, the submit gate, the include-text rule, the words, the item
/// verdict. These pin the C# GLUE — the order of the follow-ups, where a failure
/// lands, the join of a row's line — over the <see cref="MockNestRpcClient"/> seam.
/// The cache is process-static state, hence the shared serializing collection.</para>
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class ReportingTests : IDisposable
{
    public ReportingTests() => ContentPolicyCache.Reset();
    public void Dispose() => ContentPolicyCache.Reset();

    private const string Cid = "aa11";
    private const string Author = "bb22";

    private static FfiReportTarget PostTarget(bool gated = false) =>
        FaunaFfiMethods.ReportPostTarget(Cid, Author, "the post text", gated);

    private static LocalizedText Text(string key) =>
        new(key, new Dictionary<string, string>());

    // ── The sheet ─────────────────────────────────────────────────────────

    [Fact]
    public void Sheet_IsNotSendableWithoutAReason()
    {
        var vm = new ReportSheetViewModel(new MockNestRpcClient(), PostTarget());

        Assert.False(vm.View.canSubmit);
        // Picking a shared reason token opens the gate.
        vm.Reason = vm.View.reasons[0].reason;
        Assert.True(vm.View.canSubmit);
    }

    [Fact]
    public void Sheet_OffersTheExcerptOnlyForSealedContent()
    {
        // A public post needs no excerpt; a gated one is sealed (the nest holds no
        // readable bytes) so the reporter may attach what they already hold. An
        // account has no text at all. The rule is report_*_target's, carried once.
        Assert.False(new ReportSheetViewModel(new MockNestRpcClient(), PostTarget(false)).View.showIncludeText);
        Assert.True(new ReportSheetViewModel(new MockNestRpcClient(), PostTarget(true)).View.showIncludeText);
        Assert.False(new ReportSheetViewModel(
            new MockNestRpcClient(), FaunaFfiMethods.ReportActorTarget(Author)).View.showIncludeText);
    }

    [Fact]
    public async Task Submit_SendsThenHidesAndAcknowledges()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ReportSheetViewModel(rpc, PostTarget()) { Note = "rude" };
        vm.Reason = vm.View.reasons[0].reason;

        var outcome = await vm.SubmitAsync();

        Assert.True(outcome.Sent);
        Assert.False(string.IsNullOrEmpty(outcome.Acknowledgement));
        Assert.Null(outcome.Error);
        Assert.Null(outcome.FollowUpError);
        Assert.Equal(vm.Reason, rpc.LastReportSubmit!.Value.Form.reason);
        Assert.Equal("rude", rpc.LastReportSubmit!.Value.Form.note);
        // Send, then hide — no block was ticked.
        Assert.Equal(new[] { "AbuseReportSubmit", "HideReported" }, rpc.Calls.ToArray());
        Assert.Equal(new[] { Cid }, rpc.HiddenContent);
        // The stored list reached the render state: the post now hides for its reporter.
        Assert.True(ContentPolicyCache.ItemVerdictFor(Array.Empty<ContentLabelEntry>(), Cid, Author).Reported);
    }

    [Fact]
    public async Task Submit_BlocksTheAuthorBetweenSendAndHideWhenTicked()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ReportSheetViewModel(rpc, PostTarget()) { BlockAuthor = true };
        vm.Reason = vm.View.reasons[0].reason;

        var outcome = await vm.SubmitAsync();

        Assert.True(outcome.Sent);
        Assert.Equal(Author, outcome.BlockedAuthor);
        Assert.Equal(Author, rpc.LastKnockPeer);
        Assert.Equal(
            new[] { "AbuseReportSubmit", "KnocksBlock", "HideReported" }, rpc.Calls.ToArray());
    }

    [Fact]
    public async Task Submit_AFailedSendKeepsTheSheetAndHidesNothing()
    {
        var rpc = new MockNestRpcClient { ReportSubmitError = new InvalidOperationException("nest said no") };
        var vm = new ReportSheetViewModel(rpc, PostTarget());
        vm.Reason = vm.View.reasons[0].reason;

        var outcome = await vm.SubmitAsync();

        Assert.False(outcome.Sent);
        Assert.False(string.IsNullOrEmpty(outcome.Error));
        // Nothing past the failed send ran: no block, no hide, nothing stored.
        Assert.Equal(new[] { "AbuseReportSubmit" }, rpc.Calls.ToArray());
        Assert.Empty(rpc.HiddenContent);
        Assert.False(ContentPolicyCache.ItemVerdictFor(Array.Empty<ContentLabelEntry>(), Cid, Author).Reported);
        // …and the sheet is not stuck: a retry may send.
        Assert.False(vm.Sending);
    }

    [Fact]
    public async Task Submit_AFailedBlockLandsBesideTheAcknowledgement()
    {
        var rpc = new MockNestRpcClient { KnocksBlockError = new InvalidOperationException("no edge") };
        var vm = new ReportSheetViewModel(rpc, PostTarget()) { BlockAuthor = true };
        vm.Reason = vm.View.reasons[0].reason;

        var outcome = await vm.SubmitAsync();

        // The report landed and the hide still ran; the block failure is its own line.
        Assert.True(outcome.Sent);
        Assert.NotNull(outcome.Acknowledgement);
        Assert.StartsWith("block:", outcome.FollowUpError);
        Assert.Null(outcome.BlockedAuthor);
        Assert.Equal(new[] { Cid }, rpc.HiddenContent);
    }

    [Fact]
    public async Task Submit_AFailedHideLandsBesideTheAcknowledgement()
    {
        var rpc = new MockNestRpcClient { HideReportedError = new InvalidOperationException("no store") };
        var vm = new ReportSheetViewModel(rpc, PostTarget());
        vm.Reason = vm.View.reasons[0].reason;

        var outcome = await vm.SubmitAsync();

        Assert.True(outcome.Sent);
        Assert.StartsWith("hide:", outcome.FollowUpError);
    }

    [Fact]
    public async Task Submit_RefusesWhenTheSharedGateIsClosed()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ReportSheetViewModel(rpc, PostTarget());   // no reason picked

        var outcome = await vm.SubmitAsync();

        Assert.False(outcome.Sent);
        Assert.Null(outcome.Error);
        Assert.Empty(rpc.Calls);
    }

    // ── The hide: the item verdict's third input ──────────────────────────

    [Fact]
    public void Hide_AReportedPostBlocksWithNoPolicyInputsAtAll()
    {
        // The regression this guards: with no guardian floor and no own threshold the
        // label verdict short-circuits to "show" WITHOUT asking shared Rust, so a
        // reported item must be decided by the ITEM verdict or it paints in full.
        ContentPolicyCache.SetHiddenContent(new[] { Cid });

        var reported = ContentPolicyCache.ItemVerdictFor(Array.Empty<ContentLabelEntry>(), Cid, Author);
        var other = ContentPolicyCache.ItemVerdictFor(Array.Empty<ContentLabelEntry>(), "cc33", "dd44");

        Assert.Equal("block", reported.Verdict);
        Assert.True(reported.Reported);
        Assert.Equal("show", other.Verdict);
        Assert.False(other.Reported);
    }

    [Fact]
    public void Hide_AReportedAccountHidesEverythingItAuthored()
    {
        ContentPolicyCache.SetHiddenContent(new[] { Author });

        var theirs = ContentPolicyCache.ItemVerdictFor(Array.Empty<ContentLabelEntry>(), "any-post", Author);

        Assert.True(theirs.Reported);
        Assert.Equal("block", theirs.Verdict);
    }

    [Fact]
    public void Hide_WithNoReportsTheItemVerdictIsTheLabelVerdict()
    {
        var labels = new[] { new ContentLabelEntry("spam", 900) };

        Assert.Equal(
            ContentPolicyCache.VerdictFor(labels),
            ContentPolicyCache.ItemVerdictFor(labels, Cid, Author).Verdict);
        Assert.False(ContentPolicyCache.ItemVerdictFor(labels, Cid, Author).Reported);
    }

    [Fact]
    public void Hide_ResetForgetsTheList()
    {
        ContentPolicyCache.SetHiddenContent(new[] { Cid });
        ContentPolicyCache.Reset();

        Assert.False(ContentPolicyCache.ItemVerdictFor(Array.Empty<ContentLabelEntry>(), Cid, Author).Reported);
    }

    [Fact]
    public void Hide_ARegionBlockKeepsItsPlaceholderBeneathTheReport()
    {
        // Without a region plane open (a unit test) the decision is the item verdict's;
        // the reported flag rides RegionRenderDecision to the surface.
        ContentPolicyCache.SetHiddenContent(new[] { Cid });

        var decision = ContentPolicyCache.RenderFor(
            Array.Empty<ContentLabelEntry>(),
            new RegionSubject(Cid, Author, "text", Array.Empty<string>(), false, Cid, Author));

        Assert.True(decision.Reported);
        Assert.Equal("block", decision.Verdict);
        Assert.Null(decision.Placeholder);
    }

    // ── The reporter's ledger ─────────────────────────────────────────────

    private static FfiReportLedgerRow LedgerRow(
        string id, bool canWithdraw, string status, LocalizedText? outcome = null) =>
        new(id, new FfiReportSubject.Actor(new string('a', 32)), 1_000_000L,
            Text("moderation.report.reason.impersonation"), Text(status), outcome,
            Text("moderation.report.ledger_routed_to"), canWithdraw);

    [Fact]
    public async Task Ledger_LoadReadsTheRowsAndMarksTheReadLanded()
    {
        var rpc = new MockNestRpcClient
        {
            NextReportLedger = new[]
            {
                LedgerRow("r1", true, "moderation.report.status_open"),
                LedgerRow("r2", false, "moderation.report.status_withdrawn"),
            },
        };
        var vm = new ModerationViewModel(rpc);
        Assert.False(vm.ReportsLoaded);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.True(vm.ReportsLoaded);
        Assert.Equal(2, vm.Reports.Count);
        Assert.True(vm.Reports[0].CanWithdraw);
        Assert.False(vm.Reports[1].CanWithdraw);
        // reason · status · short subject id — destinations; the e2e finds the row by
        // the first 8 hex of the subject.
        Assert.Contains("aaaaaaaa", vm.Reports[0].Line);
        Assert.Contains(" · ", vm.Reports[0].Line);
        Assert.Contains(" — ", vm.Reports[0].Line);
    }

    [Fact]
    public async Task Ledger_AFailedReadNeverMarksTheReadLanded()
    {
        // moderation.md / ui README: loading is not empty — a read that did not land
        // must never let the page say "you have not reported anything".
        var rpc = new MockNestRpcClient { NextError = "nest down" };
        var vm = new ModerationViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.False(vm.ReportsLoaded);
        Assert.Empty(vm.Reports);
    }

    [Fact]
    public async Task Ledger_WithdrawActsOnTheRowThenReReadsAndSaysSo()
    {
        var rpc = new MockNestRpcClient
        {
            NextReportLedger = new[] { LedgerRow("r1", true, "moderation.report.status_open") },
        };
        var vm = new ModerationViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        rpc.NextReportLedger = new[] { LedgerRow("r1", false, "moderation.report.status_withdrawn") };

        await vm.WithdrawReportCommand.ExecuteAsync(vm.Reports[0]);

        Assert.Equal("r1", rpc.LastReportWithdraw);
        Assert.False(vm.Reports[0].CanWithdraw);
        Assert.False(string.IsNullOrEmpty(vm.ReportWithdrawStatus));
    }

    // ── The admin queue ───────────────────────────────────────────────────

    private static FfiReportQueueRow QueueRow(
        string id, FfiReportSubject subject, bool canOpenTakedown, string? note = null, string? excerpt = null) =>
        new(id, subject, null, Text("moderation.report.reason.harassment"), note, excerpt,
            Text("admin.nest_page.reports_origin_local"), 1_700_000_000_000_000L, canOpenTakedown);

    [Fact]
    public async Task Queue_LoadsInTheLoadWithItsOwnLoadedBit()
    {
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new FfiAdminServiceFlags(true, true),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(),
            NextReportQueue = new[] { QueueRow("q1", new FfiReportSubject.Post("pp"), true, note: "rude") },
        };
        var vm = new AdminNestViewModel(rpc);
        Assert.False(vm.ReportsLoaded);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.True(vm.ReportsLoaded);
        var row = Assert.Single(vm.Reports);
        // reason · kind id · origin · when — note
        Assert.Contains(" · post pp · ", row.Line);
        Assert.EndsWith(" — rude", row.Line);
        Assert.True(row.CanOpenTakedown);
    }

    [Fact]
    public void Queue_RowLineCarriesNoteAndExcerptInOrder()
    {
        var row = AdminNestViewModel.MapReportRow(
            QueueRow("q1", new FfiReportSubject.Post("pp"), true, note: "n", excerpt: "x"));

        Assert.EndsWith(" — n — “x”", row.Line);
    }

    [Fact]
    public async Task Queue_AnAccountReportOffersNoTakedown()
    {
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new FfiAdminServiceFlags(true, true),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(),
            NextReportQueue = new[] { QueueRow("q1", new FfiReportSubject.Actor("aa"), false) },
        };
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Contains(" · actor aa · ", Assert.Single(vm.Reports).Line);
        Assert.False(vm.Reports[0].CanOpenTakedown);
        Assert.False(vm.OpenReportTakedown("q1"));
    }

    [Fact]
    public async Task Queue_OpenTakedownPrefillsTheConsoleButKeepsItsGuard()
    {
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new FfiAdminServiceFlags(true, true),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(),
            NextReportQueue = new[] { QueueRow("q1", new FfiReportSubject.Post("pp"), true) },
        };
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.True(vm.OpenReportTakedown("q1"));

        Assert.Equal("pp", vm.TakedownContentId);
        Assert.False(vm.TakedownConversation);
        Assert.Equal(string.Empty, vm.TakedownReference);
        // No citation was prefilled: the shared form view still refuses to arm.
        Assert.False(vm.TakedownCanSubmit);
    }

    [Fact]
    public async Task Queue_ResolveRecordsTheOutcomeThenReReads()
    {
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new FfiAdminServiceFlags(true, true),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(),
            NextReportQueue = new[] { QueueRow("q1", new FfiReportSubject.Post("pp"), true) },
        };
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        rpc.NextReportQueue = Array.Empty<FfiReportQueueRow>();

        await vm.ResolveReportAsync("q1", acted: true);

        Assert.Equal(("q1", true), rpc.LastReportResolve!.Value);
        Assert.Empty(vm.Reports);
        Assert.False(string.IsNullOrEmpty(vm.ReportsStatus));
        // A resolved row leaves the queue, and "no open reports" now paints (loaded).
        Assert.True(vm.ReportsLoaded);
    }

    [Fact]
    public async Task Queue_AFaultedReadLeavesTheOtherSectionsAndSaysSo()
    {
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new FfiAdminServiceFlags(true, true),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(),
        };
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        Assert.True(vm.PairingEnabled);
        rpc.NextError = "queue down";

        await vm.LoadReportsAsync();

        // Its own line, never the page error; the loaded bit stays as it was.
        Assert.Null(vm.Error);
        Assert.NotNull(vm.ReportsStatus);
    }
}
