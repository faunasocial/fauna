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
/// <para>[Collection("StringsGlobal")] because the restore cases install a localizer
/// carrying the restore copy into the process-global <see cref="Strings"/> —
/// without it every template is its bare key, and a "count substituted" or
/// "reason resolved nested" assertion would prove nothing.</para>
/// </summary>
[Collection("StringsGlobal")]
public class NestsCustodyViewTests
{
    /// <summary>The restore copy, verbatim from en.yaml.</summary>
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["nests/custody_restore_running"] = "Restoring from the custodian nest…",
            ["nests/custody_restore_refused"] = "That address can't be used: {reason}",
            ["nests/custody_restore_revoked"] = "you revoked this nest's custody",
            ["nests/custody_restore_not_admitted"] =
                "That nest didn't let this device in. The address may be wrong, the nest may be down, or it may hold nothing for this account — this device can't tell which.",
            ["nests/custody_restore_restored"] = "Restored {count} of your account's records.",
            ["nests/custody_restore_sealed"] =
                "{count} came back still encrypted and stay unreadable until this device has their key.",
            ["nests/custody_restore_nothing_new"] =
                "Nothing new — this device already has everything that nest holds for you.",
            ["nests/custody_restore_mismatch"] =
                "Restored — but the nest that answered is not the one your records name for this address. Your records stand; check the address with whoever runs that nest.",
        };

        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public NestsCustodyViewTests() => Strings.Initialize(new FakeLocalizer());

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

    // ── Restore from a custodian nest (ui/nests.md § Trust facet — restore from
    // a custodian nest): one command for both entries, the shared copy resolved
    // nested and joined by a space, the shared form decision, and a Failed
    // gesture on the error — never the notice.

    /// <summary>The notice every UniFFI app paints: the shared parts, each
    /// resolved nested, joined by a space.</summary>
    private static string SharedNotice(FfiCustodianRestoreOutcome outcome)
        => string.Join(" ", FaunaFfiMethods.CustodianRestoreNotice(outcome).Select(Strings.ResolveNested));

    [Fact]
    public async Task ACustodyRow_CarriesItsAddress_ForTheRowRestoreGesture()
    {
        var rpc = new MockNestRpcClient
        {
            NextCustodyFacet = Facet(Row(GrantA, "https://custodian.example.test:8443")),
        };
        var view = new NestsCustodyView();

        await view.LoadAsync(rpc);

        Assert.Equal("https://custodian.example.test:8443", Assert.Single(view.Rows).Url);
    }

    [Fact]
    public async Task NoRestoreRan_NoNotice_AndTheFormIsClosed()
    {
        var view = new NestsCustodyView();
        await view.LoadAsync(new MockNestRpcClient());

        Assert.Null(view.RestoreNotice);
        Assert.False(view.RestoreFormOpen);
        Assert.False(view.RestoreSubmitEnabled);
    }

    [Fact]
    public async Task AReachedCustodian_ClosesTheForm_WithTheSharedNotice()
    {
        var restored = new FfiCustodianRestoreOutcome.Restored(@applied: 12, @sealedRows: 2);
        var rpc = new MockNestRpcClient { NextCustodianRestoreOutcome = restored };
        var view = new NestsCustodyView();
        view.OpenRestoreForm();
        view.SetRestoreAddress("  https://custodian.example.test  ");
        Assert.True(view.RestoreSubmitEnabled);

        await view.RestoreAsync(rpc, view.RestoreAddress);

        Assert.Equal(new[] { "https://custodian.example.test" }, rpc.CustodianRestoreUrls);
        var notice = SharedNotice(restored);
        Assert.False(string.IsNullOrEmpty(notice));
        Assert.Contains("12", notice);
        Assert.Equal(notice, view.RestoreNotice);
        Assert.False(view.RestoreFormOpen);
        Assert.Equal(string.Empty, view.RestoreAddress);
        Assert.Null(view.Error);
        // The custody fold re-reads after the restore, so a restored row paints.
        Assert.Contains("CustodyFacetLoad", rpc.Calls);
    }

    [Theory]
    [InlineData("nothing-new")]
    [InlineData("mismatch")]
    public async Task EveryOtherReachedOutcome_ClosesTheFormToo(string which)
    {
        FfiCustodianRestoreOutcome outcome = which == "nothing-new"
            ? new FfiCustodianRestoreOutcome.NothingNew()
            : new FfiCustodianRestoreOutcome.IdentityMismatch(
                Enumerable.Repeat((byte)1, 32).ToArray(), Enumerable.Repeat((byte)2, 32).ToArray());
        var rpc = new MockNestRpcClient { NextCustodianRestoreOutcome = outcome };
        var view = new NestsCustodyView();
        view.OpenRestoreForm();
        view.SetRestoreAddress("https://custodian.example.test");

        await view.RestoreAsync(rpc, view.RestoreAddress);

        Assert.Equal(SharedNotice(outcome), view.RestoreNotice);
        Assert.False(view.RestoreFormOpen);
    }

    [Fact]
    public async Task ARefusedAddress_IsANotice_AndKeepsTheFormOpenWithTheAddress()
    {
        var refused = new FfiCustodianRestoreOutcome.AddressRefused("a plaintext address off loopback");
        var rpc = new MockNestRpcClient { NextCustodianRestoreOutcome = refused };
        var view = new NestsCustodyView();
        view.OpenRestoreForm();
        view.SetRestoreAddress("http://custodian.example");

        await view.RestoreAsync(rpc, view.RestoreAddress);

        Assert.Equal(SharedNotice(refused), view.RestoreNotice);
        Assert.Contains("a plaintext address off loopback", view.RestoreNotice);
        Assert.True(view.RestoreFormOpen);
        Assert.Equal("http://custodian.example", view.RestoreAddress);
        Assert.Null(view.Error);
    }

    [Fact]
    public async Task ARevokedCustodian_ResolvesItsReasonNested_NeverTheRawKey()
    {
        var rpc = new MockNestRpcClient { NextCustodianRestoreOutcome = new FfiCustodianRestoreOutcome.Revoked() };
        var view = new NestsCustodyView();

        await view.RestoreAsync(rpc, "https://custodian.example.test");

        Assert.Contains(Strings.Get("nests/custody_restore_revoked"), view.RestoreNotice);
        Assert.DoesNotContain("nests.custody_restore_revoked", view.RestoreNotice);
    }

    [Fact]
    public async Task ANestThatDoesNotAdmit_KeepsTheFormOpen()
    {
        var rpc = new MockNestRpcClient { NextCustodianRestoreOutcome = new FfiCustodianRestoreOutcome.NotAdmitted() };
        var view = new NestsCustodyView();
        view.OpenRestoreForm();
        view.SetRestoreAddress("https://127.0.0.1:1");

        await view.RestoreAsync(rpc, view.RestoreAddress);

        Assert.Equal(SharedNotice(new FfiCustodianRestoreOutcome.NotAdmitted()), view.RestoreNotice);
        Assert.Contains(Strings.Get("nests/custody_restore_not_admitted"), view.RestoreNotice);
        Assert.True(view.RestoreFormOpen);
        Assert.Equal("https://127.0.0.1:1", view.RestoreAddress);
    }

    [Fact]
    public async Task AFailedGesture_IsNoNotice_ItsReasonRidesTheError_AndTheAddressStays()
    {
        var rpc = new MockNestRpcClient
        {
            NextCustodianRestoreOutcome = new FfiCustodianRestoreOutcome.Failed("the account runtime is not running"),
        };
        var view = new NestsCustodyView();
        view.OpenRestoreForm();
        view.SetRestoreAddress("https://custodian.example.test");

        await view.RestoreAsync(rpc, view.RestoreAddress);

        Assert.Null(view.RestoreNotice);
        Assert.Equal("the account runtime is not running", view.Error);
        Assert.True(view.RestoreFormOpen);
        Assert.Equal("https://custodian.example.test", view.RestoreAddress);
    }

    [Fact]
    public async Task ABlankAddress_DispatchesNothing()
    {
        var rpc = new MockNestRpcClient();
        var view = new NestsCustodyView();

        await view.RestoreAsync(rpc, "   ");

        Assert.Empty(rpc.CustodianRestoreUrls);
        Assert.Null(view.RestoreNotice);
    }

    [Fact]
    public async Task ARestoreInFlight_ReadsRunning_AndASecondDispatchIsIgnored()
    {
        var gate = new TaskCompletionSource();
        var rpc = new MockNestRpcClient
        {
            CustodianRestoreGate = gate.Task,
            NextCustodianRestoreOutcome = new FfiCustodianRestoreOutcome.NothingNew(),
        };
        var view = new NestsCustodyView();
        view.OpenRestoreForm();
        view.SetRestoreAddress("https://custodian.example.test");

        var first = view.RestoreAsync(rpc, view.RestoreAddress);

        Assert.True(view.RestoreRunning);
        Assert.Equal(Strings.Get("nests/custody_restore_running"), view.RestoreNotice);
        Assert.False(view.RestoreSubmitEnabled);
        await view.RestoreAsync(rpc, "https://another.example.test");
        Assert.Single(rpc.CustodianRestoreUrls);

        gate.SetResult();
        await first;
        Assert.False(view.RestoreRunning);
        Assert.Equal(SharedNotice(new FfiCustodianRestoreOutcome.NothingNew()), view.RestoreNotice);
    }

    [Fact]
    public void Cancel_ClosesTheForm_AndDropsTheAddress()
    {
        var view = new NestsCustodyView();
        view.OpenRestoreForm();
        view.SetRestoreAddress("https://custodian.example.test");

        view.CancelRestoreForm();

        Assert.False(view.RestoreFormOpen);
        Assert.Equal(string.Empty, view.RestoreAddress);
    }
}
