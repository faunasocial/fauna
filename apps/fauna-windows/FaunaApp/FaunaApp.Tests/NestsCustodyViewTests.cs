using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_capabilities;
using uniffi.fauna_core;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The rules the Settings → Nests page's custodian-nest rows and escrow-holder
/// badge paint by (<see cref="NestsCustodyView"/>; ui/nests.md § Trust facet —
/// custody rows; participants.md § The participant model → Roles): only a
/// NEST-anchored custody renders here (a device-anchored one stays on the Devices
/// page), a pending ceremony disables revoke, the badge rests on exactly the nest
/// whose identity is among the recorded holders, an unreadable pass keeps the
/// previous answer, and the revoke reaches <c>custody_revoke</c> keyed by grant id
/// and custodian key — never a row index.
/// </summary>
public class NestsCustodyViewTests
{
    private static readonly byte[] Host = Enumerable.Repeat((byte)0xAB, 32).ToArray();
    private static readonly byte[] GrantA = Enumerable.Repeat((byte)1, 32).ToArray();
    private static readonly byte[] GrantB = Enumerable.Repeat((byte)2, 32).ToArray();
    private static readonly byte[] Key = Enumerable.Repeat((byte)3, 32).ToArray();

    private static LocalizedText Text(string key) => new(key, new Dictionary<string, string>());

    private static CustodyReceiptRowView NoReceipt() => new(
        @statusLabel: Text("devices.custody_receipt_none"),
        @attestedAtSecs: null,
        @heldBytesLabel: Text("devices.custody_held_bytes"),
        @held: Text("0 B"),
        @cap: Text("1 GB"),
        @degraded: false,
        @heldBytes: null,
        @attestedCap: null);

    private static CustodyHolderRowView Row(
        byte[] grant, string? nestUrl, bool pending = false, CustodyReceiptStateView state = CustodyReceiptStateView.NoReceiptYet) => new(
        @grantId: grant,
        @host: Host,
        @custodianKey: pending ? null : Key,
        @custodianNestUrl: nestUrl,
        @scopes: null,
        @lastsUntil: null,
        @liveness: null,
        @receiptState: state,
        @receipt: NoReceipt(),
        @pending: pending);

    private static CustodyFacetView Facet(params CustodyHolderRowView[] rows) => new(
        @rows: rows,
        @held: Array.Empty<CustodyHeldRowView>(),
        @offers: Array.Empty<CustodyOfferRowView>());

    [Fact]
    public async Task OnlyANestAnchoredCustody_RendersAsANestRow_WithTheSharedCopy()
    {
        var rpc = new MockNestRpcClient
        {
            NextCustodyFacet = Facet(Row(GrantA, "wss://custodian.example.test"), Row(GrantB, null)),
        };
        var view = new NestsCustodyView();

        await view.LoadAsync(rpc);

        var row = Assert.Single(view.Rows);
        Assert.Equal(GrantA, row.GrantId);
        Assert.Equal(Key, row.Holder);
        var hostShort = FaunaFfiMethods.ShortId(FaunaFfiMethods.HexFull(Host));
        Assert.Equal(Strings.Format("nests/custody_nest_label", hostShort), row.Label);
        Assert.Equal(Strings.Get("devices/custody_holder_scope"), row.Scope);
        Assert.Equal(Strings.Resolve(Text("devices.custody_receipt_none")), row.ReceiptStatusText);
        Assert.False(row.Stale);
        Assert.True(row.RevokeEnabled);
    }

    [Fact]
    public async Task APendingCeremony_DisablesRevoke_AndNamesNoHolder()
    {
        var rpc = new MockNestRpcClient
        {
            NextCustodyFacet = Facet(Row(GrantA, "wss://custodian.example.test", pending: true)),
        };
        var view = new NestsCustodyView();

        await view.LoadAsync(rpc);

        var row = Assert.Single(view.Rows);
        Assert.True(row.Pending);
        Assert.False(row.RevokeEnabled);
        Assert.Null(row.Holder);
    }

    [Fact]
    public async Task AStaleReceipt_IsMarkedStale()
    {
        var rpc = new MockNestRpcClient
        {
            NextCustodyFacet = Facet(Row(GrantA, "wss://custodian.example.test", state: CustodyReceiptStateView.Stale)),
        };
        var view = new NestsCustodyView();

        await view.LoadAsync(rpc);

        Assert.True(Assert.Single(view.Rows).Stale);
    }

    [Fact]
    public async Task TheEscrowBadge_RestsOnExactlyTheHoldingNest()
    {
        var holder = Convert.ToHexString(Enumerable.Repeat((byte)0xAA, 32).ToArray()).ToLowerInvariant();
        var other = Convert.ToHexString(Enumerable.Repeat((byte)0xBB, 32).ToArray()).ToLowerInvariant();
        var rpc = new MockNestRpcClient { NextEscrowHolders = new[] { holder } };
        var view = new NestsCustodyView();

        await view.LoadAsync(rpc);

        Assert.True(view.HoldsEscrow(holder));
        Assert.False(view.HoldsEscrow(other));
        Assert.False(view.HoldsEscrow("not-hex"));
    }

    [Fact]
    public async Task AnUnreadablePass_KeepsThePreviousRowsAndHolders()
    {
        var holder = Convert.ToHexString(Enumerable.Repeat((byte)0xAA, 32).ToArray()).ToLowerInvariant();
        var rpc = new MockNestRpcClient
        {
            NextCustodyFacet = Facet(Row(GrantA, "wss://custodian.example.test")),
            NextEscrowHolders = new[] { holder },
        };
        var view = new NestsCustodyView();
        await view.LoadAsync(rpc);

        rpc.NextCustodyFacet = null;
        rpc.NextEscrowHolders = null;
        await view.LoadAsync(rpc);

        Assert.Single(view.Rows);
        Assert.True(view.HoldsEscrow(holder));
    }

    [Fact]
    public async Task Revoke_ReachesTheActByGrantIdAndKey_AndRepaintsFromTheRefold()
    {
        var rpc = new MockNestRpcClient
        {
            NextCustodyFacet = Facet(Row(GrantA, "wss://custodian.example.test")),
        };
        var view = new NestsCustodyView();
        await view.LoadAsync(rpc);
        rpc.NextCustodyActOutcome = new FfiCustodyActOutcome(Facet(), null);

        await view.RevokeAsync(rpc, GrantA, Key);

        Assert.Equal((GrantA, (byte[]?)Key), rpc.LastCustodyRevoke);
        Assert.Null(view.Error);
        Assert.Empty(view.Rows);
    }

    [Fact]
    public async Task ARefusedRevoke_AnswersOnError_AndKeepsThePaintedRows()
    {
        var rpc = new MockNestRpcClient
        {
            NextCustodyFacet = Facet(Row(GrantA, "wss://custodian.example.test")),
        };
        var view = new NestsCustodyView();
        await view.LoadAsync(rpc);
        rpc.NextCustodyActOutcome = new FfiCustodyActOutcome(null, "the registry door refused");

        await view.RevokeAsync(rpc, GrantA, Key);

        Assert.Equal(Strings.Format("devices/error_revoke_custody", "the registry door refused"), view.Error);
        Assert.Single(view.Rows);
    }

    [Fact]
    public async Task AThrownRevoke_AnswersOnError()
    {
        var rpc = new MockNestRpcClient
        {
            NextCustodyFacet = Facet(Row(GrantA, "wss://custodian.example.test")),
        };
        var view = new NestsCustodyView();
        await view.LoadAsync(rpc);
        rpc.NextError = "nest unreachable";

        await view.RevokeAsync(rpc, GrantA, Key);

        Assert.False(string.IsNullOrEmpty(view.Error));
        Assert.Single(view.Rows);
    }
}
