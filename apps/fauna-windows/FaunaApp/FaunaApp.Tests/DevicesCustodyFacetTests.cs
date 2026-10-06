using System;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_capabilities;
using uniffi.fauna_conversations;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The rules the Settings → Devices page's custody facet paints by
/// (<see cref="DevicesCustodyFacet"/>; ui/devices.md § Custody facet, pieces
/// 1–3 + the mint): every gesture reaches its UniFFI act keyed by its grant id,
/// every refusal answers on <c>Error</c> (e2e convention 11), a null re-fold
/// keeps the painted rows, the budget is parsed by the SHARED
/// <c>parse_byte_size</c> (the real export — this assembly loads the native
/// library), and the mint flow never opens over an empty host list.
/// </summary>
public class DevicesCustodyFacetTests
{
    private static readonly byte[] Grant = Enumerable.Repeat((byte)7, 32).ToArray();

    private static CustodyOfferRowView Offer(byte[] grant, bool nestCanHold = false) => new(
        @grantId: grant,
        @owner: Enumerable.Repeat((byte)1, 32).ToArray(),
        @scopes: new CustodyScopesView(@wholeAccount: true, @scopes: Array.Empty<string>()),
        @offeredAtSecs: 0,
        @nestCanHold: nestCanHold);

    private static CustodyFacetView Facet(params CustodyOfferRowView[] offers) => new(
        @rows: Array.Empty<CustodyHolderRowView>(),
        @held: Array.Empty<CustodyHeldRowView>(),
        @offers: offers);

    /// <summary>A real, offline <see cref="ConversationsSession"/> (the
    /// <c>CustodyDriveFfiTests</c> recipe) — the accept and mint seams require
    /// one; the mock never touches it.</summary>
    private static (FfiNestClient Nest, ConversationsSession Session) Session()
    {
        var secret = Enumerable.Repeat((byte)9, 32).ToArray();
        var dir = Path.Combine(Path.GetTempPath(), $"fauna-custody-facet-{Guid.NewGuid():N}");
        Directory.CreateDirectory(dir);
        var nest = new FfiNestClient("wss://127.0.0.1:0/ws", secret);
        var session = nest.ConversationsSession(
            "someone@example.test", secret, Path.Combine(dir, "mls.sqlite"), null, Array.Empty<byte[]>());
        return (nest, session);
    }

    [Fact]
    public async Task SetBudget_ParsesThroughTheSharedParser_AndCallsWithTheCap()
    {
        var rpc = new MockNestRpcClient();
        var facet = new DevicesCustodyFacet();

        await facet.SetBudgetAsync(rpc, Grant, "3072 MB");

        var act = Assert.Single(rpc.CustodyActs);
        Assert.Equal("CustodySetBudget", act.Act);
        Assert.Equal(Grant, act.GrantId);
        Assert.Equal(3072UL * 1024 * 1024, act.Extra);
        Assert.Null(facet.Error);
    }

    [Theory]
    [InlineData("lots")]
    [InlineData("0 GB")]
    public async Task SetBudget_Unparseable_MakesNoCall_AndSaysSo(string typed)
    {
        var rpc = new MockNestRpcClient();
        var facet = new DevicesCustodyFacet();

        await facet.SetBudgetAsync(rpc, Grant, typed);

        Assert.Empty(rpc.CustodyActs);
        Assert.Equal(Strings.Get("backups/backup_destination_capacity_invalid"), facet.Error);
    }

    [Fact]
    public async Task ActError_ReachesError_AndANullRefold_KeepsThePaintedRows()
    {
        var rpc = new MockNestRpcClient { NextCustodyFacet = Facet(Offer(Grant)) };
        var facet = new DevicesCustodyFacet();
        await facet.LoadAsync(rpc, session: null);
        var painted = facet.Facet;
        Assert.NotNull(painted);

        rpc.NextCustodyActOutcome = new FfiCustodyActOutcome(null, "the registry door refused");
        await facet.StopAsync(rpc, Grant);

        Assert.Equal("the registry door refused", facet.Error);
        Assert.Same(painted, facet.Facet);
    }

    [Fact]
    public async Task ASucceedingGesture_ClearsTheLastError_AndRepaintsFromItsFacet()
    {
        var rpc = new MockNestRpcClient { NextCustodyActOutcome = new FfiCustodyActOutcome(null, "no") };
        var facet = new DevicesCustodyFacet();
        await facet.RemoveAsync(rpc, Grant);
        Assert.NotNull(facet.Error);

        var refolded = Facet();
        rpc.NextCustodyActOutcome = new FfiCustodyActOutcome(refolded, null);
        await facet.DeclineAsync(rpc, Grant);

        Assert.Null(facet.Error);
        Assert.Same(refolded, facet.Facet);
        Assert.Equal(new[] { "CustodyRemove", "CustodyDecline" }, rpc.CustodyActs.Select(a => a.Act));
    }

    [Fact]
    public async Task AThrownAct_ReachesError()
    {
        var rpc = new MockNestRpcClient { NextError = "nest unreachable" };
        var facet = new DevicesCustodyFacet();

        await facet.RevokeAsync(rpc, Grant, holder: null);

        // The mock throws InvalidOperationException(NextError); the page shows
        // the same localized sentence any other thrown gesture gets.
        Assert.Equal(Strings.Error(new InvalidOperationException("nest unreachable")), facet.Error);
    }

    [Fact]
    public async Task Accept_PassesTheTargetChoice_AndNeedsASession()
    {
        var rpc = new MockNestRpcClient();
        var facet = new DevicesCustodyFacet();

        await facet.AcceptAsync(rpc, session: null, Grant, onNest: false);
        Assert.Empty(rpc.CustodyActs);
        Assert.Equal(Strings.Get("devices/custody_mint_no_contacts"), facet.Error);

        var (nest, session) = Session();
        using (nest)
        using (session)
        {
            await facet.AcceptAsync(rpc, session, Grant, onNest: true);
        }
        var act = Assert.Single(rpc.CustodyActs);
        Assert.Equal(("CustodyAccept", true), (act.Act, (bool)act.Extra!));
        Assert.Null(facet.Error);
    }

    [Fact]
    public async Task TheTargetSelect_FollowsTheSharedAnswer_PerOffer()
    {
        var shown = Enumerable.Repeat((byte)2, 32).ToArray();
        var rpc = new MockNestRpcClient
        {
            NextCustodyFacet = Facet(Offer(Grant, nestCanHold: false), Offer(shown, nestCanHold: true)),
            CustodyShowsTargetSelect = o => o.@nestCanHold,
        };
        var facet = new DevicesCustodyFacet();

        await facet.LoadAsync(rpc, session: null);

        Assert.False(facet.ShowsTargetSelect(Grant));
        Assert.True(facet.ShowsTargetSelect(shown));
    }

    [Fact]
    public void OpenMint_WithNoOneToAsk_StaysClosed_AndSaysSo()
    {
        var rpc = new MockNestRpcClient();
        var facet = new DevicesCustodyFacet();

        facet.OpenMint(rpc, session: null);
        Assert.Null(facet.MintCandidates);
        Assert.Equal(Strings.Get("devices/custody_mint_no_contacts"), facet.Error);

        var (nest, session) = Session();
        using (nest)
        using (session)
        {
            facet.OpenMint(rpc, session);
        }
        Assert.Null(facet.MintCandidates);
        Assert.Equal(Strings.Get("devices/custody_mint_no_contacts"), facet.Error);
    }

    [Fact]
    public async Task Mint_SendsTheChosenCandidateUnchanged_AndClosesOnSuccessOnly()
    {
        var candidate = new CustodyMintCandidateView(
            @host: Enumerable.Repeat((byte)3, 32).ToArray(), @channelHex: "ab12", @label: "bob");
        var rpc = new MockNestRpcClient { NextCustodyMintCandidates = new[] { candidate } };
        var facet = new DevicesCustodyFacet();
        var (nest, session) = Session();
        using (nest)
        using (session)
        {
            facet.OpenMint(rpc, session);
            Assert.Equal(new[] { candidate }, facet.MintCandidates);
            Assert.Null(facet.Error);

            rpc.NextCustodyActOutcome = new FfiCustodyActOutcome(null, "not a mint candidate");
            await facet.MintAsync(rpc, session, candidate);
            Assert.NotNull(facet.MintCandidates);
            Assert.Equal("not a mint candidate", facet.Error);

            rpc.NextCustodyActOutcome = new FfiCustodyActOutcome(null, null);
            await facet.MintAsync(rpc, session, candidate);
        }
        Assert.Null(facet.MintCandidates);
        Assert.Null(facet.Error);
        Assert.All(rpc.CustodyActs, a => Assert.Equal(("CustodyMint", "ab12"), (a.Act, (string)a.Extra!)));
        Assert.All(rpc.CustodyActs, a => Assert.Equal(candidate.@host, a.GrantId));
    }

    [Fact]
    public async Task KeylessPosture_JoinsByPrincipal_AndNeverRestsOnAnUnknown()
    {
        var rpc = new MockNestRpcClient { KeylessPrincipal = p => p == "kiosk" };
        var facet = new DevicesCustodyFacet();

        await facet.LoadKeylessPostureAsync(rpc, new string?[] { "laptop", null, "kiosk" });

        Assert.True(facet.IsKeyless("kiosk"));
        Assert.False(facet.IsKeyless("laptop"));
        Assert.False(facet.IsKeyless(null));
        Assert.False(facet.IsKeyless("never-read"));

        // A failed re-read keeps the last answer rather than flickering off.
        rpc.NextError = "store busy";
        await facet.LoadKeylessPostureAsync(rpc, new string?[] { "kiosk" });
        Assert.True(facet.IsKeyless("kiosk"));
    }
}
