using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_core;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The admin block of a reported item on windows (takedown-and-block.md § Admin
/// block of a reported item): the Block verb on a report row, the one armed
/// confirm Block and Unblock share, the blocked list, and the notice the feed and
/// conversation renders paint.
///
/// <para>Everything that DECIDES is shared Rust, called for real (the native dll
/// loads in the test host — reference_windows_dotnet_test_loads_native_ffi): the
/// form view and its gate, the confirm summary, the verdict, the notice. These pin
/// the C# GLUE — which row is armed, that the confirm disarms before it
/// dispatches, that both lists re-read, where a refusal lands.</para>
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class AdminBlockTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["admin/nest_page/block_arm_block"] = "Block",
            ["admin/nest_page/block_arm_unblock"] = "Unblock",
            ["admin/nest_page/block_confirm_button_block"] = "Confirm block",
            ["admin/nest_page/block_confirm_button_unblock"] = "Confirm unblock",
            ["admin/nest_page/block_confirm_block"] = "Block {item} {content_id}? Reason: {reason}.",
            ["admin/nest_page/block_confirm_unblock"] = "Unblock {item} {content_id} (blocked for: {reason})?",
            ["admin/nest_page/block_item_post"] = "post",
            ["admin/nest_page/block_item_message"] = "message",
            ["admin/nest_page/block_done"] = "Blocked.",
            ["admin/nest_page/block_restored"] = "Unblocked.",
            ["admin/nest_page/block_failed"] = "The nest refused the request: {error}",
            ["admin/nest_page/block_working"] = "Submitting…",
            ["moderation/report/reason/harassment"] = "Harassment",
            ["moderation/admin_block/notice"] = "Blocked by this nest's admin after a report: {reason}",
        };
        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public AdminBlockTests() => Strings.Initialize(new FakeLocalizer());

    private static LocalizedText Text(string key) => new(key, new Dictionary<string, string>());

    private static FfiReportQueueRow QueueRow(string id, FfiReportSubject subject, bool item) =>
        new(id, subject, null, Text("moderation.report.reason.harassment"), null, null,
            Text("admin.nest_page.reports_origin_local"), 1_700_000_000_000_000L, item, item,
            reasonToken: "harassment");

    private static FfiBlockedRow BlockedRow(string id, string cid) =>
        new(id, new FfiReportSubject.Post(cid), "harassment", Text("moderation.report.reason.harassment"),
            1_700_000_000_000_000L,
            FaunaFfiMethods.ReportBlockFormView(new FfiReportSubject.Post(cid), true));

    private static async Task<(MockNestRpcClient rpc, AdminNestViewModel vm)> LoadedAsync(
        IReadOnlyList<FfiReportQueueRow>? queue = null, IReadOnlyList<FfiBlockedRow>? blocked = null)
    {
        var rpc = new MockNestRpcClient
        {
            NextServiceFlags = new FfiAdminServiceFlags(true, true),
            NextSetupStatus = MockNestRpcClient.MakeSetupStatus(),
            NextReportQueue = queue ?? Array.Empty<FfiReportQueueRow>(),
            NextBlockedList = blocked ?? Array.Empty<FfiBlockedRow>(),
        };
        var vm = new AdminNestViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        return (rpc, vm);
    }

    [Fact]
    public async Task Load_ReadsTheBlockedListWithItsOwnLoadedBit()
    {
        var (_, vm) = await LoadedAsync(blocked: new[] { BlockedRow("b1", "pp") });

        Assert.True(vm.BlockedLoaded);
        var row = Assert.Single(vm.Blocked);
        // reason · kind id · when
        Assert.StartsWith("Harassment · post pp · ", row.Line);
        Assert.True(row.CanUnblock);
        Assert.Equal("Unblock", row.UnblockLabel);
    }

    [Fact]
    public async Task Queue_AnItemRowOffersBlockAndAnAccountRowDoesNot()
    {
        var (_, vm) = await LoadedAsync(queue: new[]
        {
            QueueRow("q1", new FfiReportSubject.Post("pp"), item: true),
            QueueRow("q2", new FfiReportSubject.Actor("aa"), item: false),
        });

        Assert.True(vm.Reports[0].CanBlock);
        Assert.Equal("Block", vm.Reports[0].BlockLabel);
        Assert.False(vm.Reports[1].CanBlock);
        // A driver-forced press on the account row arms nothing.
        Assert.False(vm.ArmBlock("q2"));
        Assert.Null(vm.BlockArmed);
    }

    [Fact]
    public async Task ArmBlock_NamesTheVerbTheItemAndTheReasonBeforeAnythingDispatches()
    {
        var (rpc, vm) = await LoadedAsync(queue: new[] { QueueRow("q1", new FfiReportSubject.Post("pp"), true) });

        Assert.True(vm.ArmBlock("q1"));

        var armed = Assert.IsType<BlockArmedView>(vm.BlockArmed);
        Assert.Equal("Block post pp? Reason: Harassment.", armed.Summary);
        Assert.Equal("Confirm block", armed.ConfirmLabel);
        Assert.False(armed.Restore);
        Assert.DoesNotContain("AdminBlock", rpc.Calls);
    }

    [Fact]
    public async Task ConfirmBlock_DisarmsDispatchesWordsAndReReadsBothLists()
    {
        var (rpc, vm) = await LoadedAsync(queue: new[] { QueueRow("q1", new FfiReportSubject.Post("pp"), true) });
        vm.ArmBlock("q1");
        rpc.NextReportQueue = Array.Empty<FfiReportQueueRow>();
        rpc.NextBlockedList = new[] { BlockedRow("q1", "pp") };

        await vm.ConfirmBlockAsync();

        Assert.Null(vm.BlockArmed);
        Assert.Equal(("q1", false), rpc.LastAdminBlock!.Value);
        Assert.Equal("Blocked.", vm.BlockStatus);
        Assert.Empty(vm.Reports);
        Assert.Single(vm.Blocked);
        // A second press after the disarm repeats nothing.
        var blocks = rpc.Calls.Count(c => c == "AdminBlock");
        await vm.ConfirmBlockAsync();
        Assert.Equal(blocks, rpc.Calls.Count(c => c == "AdminBlock"));
    }

    [Fact]
    public async Task Unblock_ArmsTheSameConfirmWithRestore()
    {
        var (rpc, vm) = await LoadedAsync(blocked: new[] { BlockedRow("b1", "pp") });

        Assert.True(vm.ArmUnblock("b1"));
        var armed = Assert.IsType<BlockArmedView>(vm.BlockArmed);
        Assert.Equal("Unblock post pp (blocked for: Harassment)?", armed.Summary);
        Assert.Equal("Confirm unblock", armed.ConfirmLabel);
        rpc.NextBlockedList = Array.Empty<FfiBlockedRow>();

        await vm.ConfirmBlockAsync();

        Assert.Equal(("b1", true), rpc.LastAdminBlock!.Value);
        Assert.Equal("Unblocked.", vm.BlockStatus);
        Assert.Empty(vm.Blocked);
    }

    [Fact]
    public async Task Cancel_DisarmsTouchingNothing()
    {
        var (rpc, vm) = await LoadedAsync(queue: new[] { QueueRow("q1", new FfiReportSubject.Post("pp"), true) });
        vm.ArmBlock("q1");

        vm.CancelBlock();

        Assert.Null(vm.BlockArmed);
        Assert.DoesNotContain("AdminBlock", rpc.Calls);
    }

    [Fact]
    public async Task ARefusedBlockIsWordedOnItsOwnLineNeverThePageError()
    {
        var (rpc, vm) = await LoadedAsync(queue: new[] { QueueRow("q1", new FfiReportSubject.Post("pp"), true) });
        vm.ArmBlock("q1");
        rpc.AdminBlockError = new InvalidOperationException("not open");

        await vm.ConfirmBlockAsync();

        Assert.StartsWith("The nest refused the request: ", vm.BlockStatus);
        Assert.Null(vm.Error);
    }

    private const string Notice = "Blocked by this nest's admin after a report: Harassment";

    [Fact]
    public void Notice_ResolvesTheSharedFaceWithTheReasonWord()
    {
        Assert.Equal(Notice, FeedPostItem.AdminBlockNoticeText("harassment"));
    }

    // ── The quoted-post embed — the read every viewer makes of a blocked post ──

    private static FeedPostItem QuotingCard(string body, string? reason) =>
        new(PostSummaryFixture.Make(postId: "q", author: "a", document: new RenderDocument(new RenderBlock[]
        {
            RenderBlockFixture.QuotedPost("target", body.Length == 0 ? "" : "author", body,
                adminBlockReason: reason),
        })));

    [Fact]
    public void QuotedPost_AWithheldBlockPaintsTheNoticeInPlaceWithoutHeader()
    {
        var item = QuotingCard("", "harassment");

        Assert.True(item.QuotedPostIsAdminBlocked);
        Assert.Equal(Notice, item.QuotedPostDisplayBody);
        Assert.False(item.QuotedPostShowHeaderRow);
        // The empty author would prune the Border's UIA peer; the notice names it.
        Assert.Equal(Notice, item.QuotedPostAccessibleName);
    }

    [Fact]
    public void QuotedPost_TheAuthorsOwnReadKeepsTheBodyWithTheNoticeBesideIt()
    {
        var item = QuotingCard("block target", "harassment");

        Assert.False(item.QuotedPostIsAdminBlocked);
        Assert.Equal("block target\n" + Notice, item.QuotedPostDisplayBody);
        Assert.True(item.QuotedPostShowHeaderRow);
    }

    [Fact]
    public void QuotedPost_AnUnblockedQuotePaintsItsBodyAlone()
    {
        var item = QuotingCard("block target", null);

        Assert.False(item.QuotedPostIsAdminBlocked);
        Assert.Equal("block target", item.QuotedPostDisplayBody);
    }

    // ── The post itself, reached through the resolve_post door ──

    [Fact]
    public void Post_AWithheldBlockStandsInPlaceAndTheAuthorsKeepsItsBody()
    {
        var withheld = new FeedPostItem(PostSummaryFixture.Make(postId: "p", author: "", body: "",
            adminBlockReason: "harassment"));
        Assert.Equal(Notice, withheld.AdminBlockNotice);
        Assert.True(withheld.IsAdminBlockedInPlace);

        var own = new FeedPostItem(PostSummaryFixture.Make(postId: "p", author: "a", body: "mine",
            adminBlockReason: "harassment"));
        Assert.Equal(Notice, own.AdminBlockNotice);
        Assert.False(own.IsAdminBlockedInPlace);
    }

    // ── The DM bubble's arm ──

    [Fact]
    public void Gate_AWithheldRecordIsTheAdminBlockArmAheadOfThePolicy()
    {
        Assert.Equal(SocialRenderArm.AdminBlocked,
            SocialRenderGate.Decide("block", false, true, adminBlockInPlace: true));
        // The legal takedown still outranks it.
        Assert.Equal(SocialRenderArm.LegalTakedown,
            SocialRenderGate.Decide("show", false, false, legalTakedownRef: "ref", adminBlockInPlace: true));
        // The author's kept body is not in place: it falls through to Live.
        Assert.Equal(SocialRenderArm.Live, SocialRenderGate.Decide("show", false, false));
    }
}
