#if PAYMENTS
// Gated with the plane under test — dynamic-features.md § The feature-matrix test
// story: a store-safe build has no payments members to drive.
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.ViewModels;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>The OTHER profile Tiers tab's claim-redeem door (<c>profile.md</c> § Layout
/// &amp; flow → <i>Another's profile</i>; <c>monetization.md</c> § Pillar 3 → <i>Which
/// nest an in-app redeem binds on</i>): the code redeems where THIS creator lives — the
/// page's actor-home slot, exactly as the follow carries it — under the viewed actor's
/// id. Mirrors tui's <c>profile/mod.rs</c> redeem tests.</summary>
public class ProfileClaimRedeemTests
{
    private static readonly byte[] Bob = Enumerable.Repeat((byte)0xb2, 32).ToArray();
    private static readonly string BobHex = System.Convert.ToHexString(Bob).ToLowerInvariant();
    private const string Home = "https://author-home.test";

    [Fact]
    public async Task Redeem_GoesToTheViewedCreatorsHome_WithTheTrimmedCode()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false, home: Home);
        vm.RedeemCode = "  CODE42 ";

        Assert.True(await vm.RedeemClaimAsync());

        Assert.Contains("PaymentsClaimsRedeemAcrossNests", rpc.Calls);
        Assert.Equal("CODE42", rpc.LastClaimsRedeem?.Code);
        Assert.Equal(Bob, rpc.LastClaimsRedeem?.AuthorId);
        Assert.Equal(Home, rpc.LastClaimsRedeem?.AuthorHomeUrl);

        // The same home the follow button passes.
        await vm.ToggleFollowAsync();
        Assert.Equal(rpc.LastSubscribeHome, rpc.LastClaimsRedeem?.AuthorHomeUrl);
    }

    [Fact]
    public async Task Redeem_BlankCodeIsANoOp()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false, home: Home);
        vm.RedeemCode = "   ";

        Assert.False(await vm.RedeemClaimAsync());

        Assert.DoesNotContain("PaymentsClaimsRedeemAcrossNests", rpc.Calls);
    }

    [Fact]
    public async Task Redeem_OnTheViewersOwnProfileIsANoOp()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ProfileViewModel(rpc, BobHex);
        vm.RedeemCode = "CODE42";

        Assert.False(await vm.RedeemClaimAsync());

        Assert.DoesNotContain("PaymentsClaimsRedeemAcrossNests", rpc.Calls);
    }

    [Fact]
    public async Task Redeem_TakenCode_ClearsTheInput_RereadsOffers_AndMarksTheTierPending()
    {
        var rpc = new MockNestRpcClient
        {
            NextOffers = new[] { MockNestRpcClient.MakeTier("gold"), MockNestRpcClient.MakeTier("silver") },
            NextClaimRedeemReply = MockNestRpcClient.MakeClaimRedeemReply(author: Bob, tier: "gold", queued: true),
        };
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false, home: Home);
        await vm.HydrateAsync();
        vm.RedeemCode = "CODE42";

        Assert.True(await vm.RedeemClaimAsync());

        Assert.Equal("", vm.RedeemCode);
        Assert.Null(vm.ErrorMessage);
        Assert.Equal(2, rpc.Calls.Count(c => c == "SubscriptionOffersList"));
        Assert.Equal(uniffi.fauna_core.OfferStatus.Pending, vm.Offers.Single(o => o.Name == "gold").Status);
        Assert.Equal(uniffi.fauna_core.OfferStatus.None, vm.Offers.Single(o => o.Name == "silver").Status);
    }

    [Fact]
    public async Task Redeem_Refusal_KeepsTheCodeAndSurfacesTheError()
    {
        var rpc = new MockNestRpcClient { NextError = "fauna.payments.claim_not_found" };
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false, home: Home);
        vm.RedeemCode = "NOSUCH1234";

        Assert.False(await vm.RedeemClaimAsync());

        Assert.Equal("NOSUCH1234", vm.RedeemCode);
        Assert.NotNull(vm.ErrorMessage);
    }

    [Fact]
    public void Redeem_AFreshOpenStartsWithAnEmptyBuffer()
    {
        var vm = new ProfileViewModel(new MockNestRpcClient(), BobHex, isSelf: false, home: Home);

        Assert.Equal("", vm.RedeemCode);
    }
}
#endif   // PAYMENTS
