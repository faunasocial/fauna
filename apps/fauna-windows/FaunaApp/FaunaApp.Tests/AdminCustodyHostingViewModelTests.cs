using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The admin <c>admin-custody-hosting</c> page VM (docs/goal/architecture/
/// account-data-plane.md § Two-sided bounds). Mirrors the invariants tui's own
/// <c>custody_hosting.rs</c> test suite pins:
/// <list type="number">
/// <item><b>Rows are never re-sorted</b> — the FFI boundary's
/// <c>custody_hosting_list()</c> already applies the shared
/// <c>admin_hosting_rows</c> fold, so the VM renders exactly the order it
/// receives.</item>
/// <item><b>A capless row reads as Default, never as zero bytes</b> — a
/// <c>retainedBytesCap</c> of 0 means the row carries no cap, not "no bytes
/// allowed".</item>
/// <item><b>Remove always re-fetches</b>, regardless of the verdict — an
/// honest no-op (<c>removed == false</c>) is not an error.</item>
/// </list>
///
/// <para>[Collection("StringsGlobal")] because row composition resolves
/// shared <c>LocalizedText</c> through <see cref="Strings"/>, whose localizer
/// is process-global (same reason <c>MemberReviewViewModelTests</c> needs
/// it). This class installs its own <see cref="FakeLocalizer"/> in its
/// constructor mapping the real <c>admin/custody_hosting/*</c> templates.
/// </para>
/// </summary>
[Collection("StringsGlobal")]
public class AdminCustodyHostingViewModelTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["admin/custody_hosting/budget_default"] = "Default",
            ["admin/custody_hosting/stopped"] = "Paused",
            ["admin/custody_hosting/active"] = "Active",
            ["admin/custody_hosting/receipt_fresh"] = "Confirmed recently",
            ["admin/custody_hosting/receipt_stale"] = "Not confirmed lately",
            ["admin/custody_hosting/receipt_none"] = "Never confirmed",
            ["admin/custody_hosting/removed"] = "Removed.",
            ["admin/custody_hosting/removed_with_store"] = "Removed, and the stored copy was freed.",
            ["admin/custody_hosting/remove_missing"] = "That row was already gone.",
            ["admin/custody_hosting/count"] = "{count} held for others",
        };
        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public AdminCustodyHostingViewModelTests() => Strings.Initialize(new FakeLocalizer());

    private static FfiAdminHostingRow Row(
        string host = "aa", string owner = "bb", string url = "https://owner.example",
        byte[]? grant = null, long cap = 8192, long held = 4096, bool stopped = false,
        FfiReceiptState receipt = FfiReceiptState.Fresh) =>
        new(
            hostActorId: host.PadRight(64, '0'),
            ownerActorId: owner.PadRight(64, '0'),
            ownerNestUrl: url,
            grantId: grant ?? new byte[] { 1, 2, 3 },
            retainedBytesCap: cap,
            heldBytes: held,
            stopped: stopped,
            receiptState: receipt);

    [Fact]
    public async Task LoadAsync_EmptyRegistry_NoRowsButHasLoaded()
    {
        var rpc = new MockNestRpcClient { NextAdminCustodyHostingRows = System.Array.Empty<FfiAdminHostingRow>() };
        var vm = new AdminCustodyHostingViewModel(rpc);

        await vm.LoadAsync();

        Assert.Empty(vm.Rows);
        Assert.True(vm.HasLoaded);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task LoadAsync_NeverResorts_RendersReceivedOrder()
    {
        // The FFI boundary's custody_hosting_list() already applies the
        // shared admin_hosting_rows fold -- the VM must not re-derive order.
        // Deliberately NOT heaviest-first, to prove nothing here re-sorts.
        var rpc = new MockNestRpcClient
        {
            NextAdminCustodyHostingRows = new[]
            {
                Row(host: "cc", held: 100),
                Row(host: "aa", held: 900),
                Row(host: "bb", held: 500),
            },
        };
        var vm = new AdminCustodyHostingViewModel(rpc);

        await vm.LoadAsync();

        Assert.Equal(
            new[] { "cc".PadRight(64, '0'), "aa".PadRight(64, '0'), "bb".PadRight(64, '0') },
            vm.Rows.Select(r => r.HostActorId).ToArray());
    }

    [Fact]
    public async Task LoadAsync_CaplessRow_RendersDefaultNeverZeroBytes()
    {
        var rpc = new MockNestRpcClient
        {
            NextAdminCustodyHostingRows = new[] { Row(cap: 0) },
        };
        var vm = new AdminCustodyHostingViewModel(rpc);

        await vm.LoadAsync();

        var row = Assert.Single(vm.Rows);
        Assert.Equal("Default", row.BudgetText);
    }

    [Fact]
    public async Task LoadAsync_ACappedRow_RendersItsByteSize()
    {
        var rpc = new MockNestRpcClient
        {
            NextAdminCustodyHostingRows = new[] { Row(cap: 8192) },
        };
        var vm = new AdminCustodyHostingViewModel(rpc);

        await vm.LoadAsync();

        var row = Assert.Single(vm.Rows);
        Assert.NotEqual("Default", row.BudgetText);
    }

    [Theory]
    [InlineData(false, "Active")]
    [InlineData(true, "Paused")]
    public async Task LoadAsync_StoppedFlag_RendersTheRightWord(bool stopped, string expected)
    {
        // A stopped row still holds its bytes -- remove exists precisely
        // because stop alone does not free them.
        var rpc = new MockNestRpcClient
        {
            NextAdminCustodyHostingRows = new[] { Row(stopped: stopped) },
        };
        var vm = new AdminCustodyHostingViewModel(rpc);

        await vm.LoadAsync();

        Assert.Equal(expected, Assert.Single(vm.Rows).StoppedText);
    }

    // FfiReceiptState is internal, and xUnit theory methods must be public --
    // an internal-typed [InlineData] parameter fails CS0051. Three plain
    // facts instead of a theory.
    [Fact]
    public async Task LoadAsync_ReceiptFresh_RendersItsWord() =>
        await AssertReceiptText(FfiReceiptState.Fresh, "Confirmed recently");

    [Fact]
    public async Task LoadAsync_ReceiptStale_RendersItsWord() =>
        await AssertReceiptText(FfiReceiptState.Stale, "Not confirmed lately");

    [Fact]
    public async Task LoadAsync_ReceiptNoReceiptYet_RendersItsWord() =>
        await AssertReceiptText(FfiReceiptState.NoReceiptYet, "Never confirmed");

    private static async Task AssertReceiptText(FfiReceiptState state, string expected)
    {
        var rpc = new MockNestRpcClient
        {
            NextAdminCustodyHostingRows = new[] { Row(receipt: state) },
        };
        var vm = new AdminCustodyHostingViewModel(rpc);

        await vm.LoadAsync();

        Assert.Equal(expected, Assert.Single(vm.Rows).ReceiptText);
    }

    [Fact]
    public async Task RemoveAsync_AddressesByTheHostGrantPair_NeverAPaintedIndex()
    {
        var rpc = new MockNestRpcClient
        {
            NextAdminCustodyHostingRows = new[] { Row(host: "aa", grant: new byte[] { 9, 9 }) },
        };
        var vm = new AdminCustodyHostingViewModel(rpc);
        await vm.LoadAsync();
        var row = Assert.Single(vm.Rows);

        await vm.RemoveAsync(row.HostActorId, row.GrantId);

        Assert.Equal(row.HostActorId, rpc.LastAdminCustodyHostingRemove?.HostActorId);
        Assert.Equal(row.GrantId, rpc.LastAdminCustodyHostingRemove?.GrantId);
    }

    [Fact]
    public async Task RemoveAsync_Removed_SetsStatusAndReReads()
    {
        var rpc = new MockNestRpcClient
        {
            NextAdminCustodyHostingRows = new[] { Row() },
            NextAdminCustodyHostingRemoveReply = new FfiAdminHostingRemoveReply(removed: true, storeDropped: false),
        };
        var vm = new AdminCustodyHostingViewModel(rpc);
        await vm.LoadAsync();

        // The nest now reports the registry empty -- the VM must re-read
        // rather than assume, same as every other adjudication on this page.
        rpc.NextAdminCustodyHostingRows = System.Array.Empty<FfiAdminHostingRow>();
        await vm.RemoveAsync("host", new byte[] { 1 });

        Assert.Equal("Removed.", vm.Status);
        Assert.Empty(vm.Rows);
    }

    [Fact]
    public async Task RemoveAsync_RemovedWithStore_SaysSo()
    {
        var rpc = new MockNestRpcClient
        {
            NextAdminCustodyHostingRows = new[] { Row() },
            NextAdminCustodyHostingRemoveReply = new FfiAdminHostingRemoveReply(removed: true, storeDropped: true),
        };
        var vm = new AdminCustodyHostingViewModel(rpc);
        await vm.LoadAsync();

        await vm.RemoveAsync("host", new byte[] { 1 });

        Assert.Equal("Removed, and the stored copy was freed.", vm.Status);
    }

    [Fact]
    public async Task RemoveAsync_AlreadyGone_IsAnHonestNoOpNotAnError()
    {
        var rpc = new MockNestRpcClient
        {
            NextAdminCustodyHostingRows = new[] { Row() },
            NextAdminCustodyHostingRemoveReply = new FfiAdminHostingRemoveReply(removed: false, storeDropped: false),
        };
        var vm = new AdminCustodyHostingViewModel(rpc);
        await vm.LoadAsync();

        await vm.RemoveAsync("host", new byte[] { 1 });

        Assert.Equal("That row was already gone.", vm.Status);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task LoadAsync_ReadFailure_SetsErrorMessageNotAnException()
    {
        var rpc = new MockNestRpcClient { NextError = "hosting list failed" };
        var vm = new AdminCustodyHostingViewModel(rpc);

        await vm.LoadAsync();

        Assert.False(string.IsNullOrEmpty(vm.ErrorMessage));
        Assert.Empty(vm.Rows);
    }
}
