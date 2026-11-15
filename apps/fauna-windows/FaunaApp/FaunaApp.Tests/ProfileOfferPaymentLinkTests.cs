#if PAYMENTS
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>The payment-link gesture (<c>subscription-offer-payment-link</c>;
/// monetization.md § Pillar 3 → Buyer↔actor binding (Q4), the Q13 rules (1) and (2)):
/// the link SUBSCRIBES FIRST — across nests at the page's actor home, the home the
/// follow carries — and the page opens only the checkout URL the shared
/// <c>subscriptions_subscribe_and_checkout</c> composed, never the author's raw link.
/// Mirrors tui <c>Action::OpenPaymentLink</c>.</summary>
public class ProfileOfferPaymentLinkTests
{
    // Built, not spelled: a 64-hex literal reads as a key to the publish secret scan.
    private static readonly string BobHex = System.Convert.ToHexString(new byte[32]).ToLowerInvariant();
    private const string Home = "https://peer.example";
    private const string RawLink = "https://pay.example/checkout";
    private const string Composed = "https://pay.example/checkout?client_reference_id=x";

    private static MockNestRpcClient RpcWithGoldOffer() => new()
    {
        NextOffers = new[] { MockNestRpcClient.MakeTier("gold", paymentUrl: RawLink) },
    };

    [Fact]
    public async Task PaymentLink_SubscribesWithTheHome_ThenHandsBackTheComposedUrl()
    {
        var rpc = RpcWithGoldOffer();
        rpc.NextCheckoutOpen = new FfiCheckoutOpen(new FfiSubscribeReply.Approved("gold", null), Composed);
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false, home: Home);
        await vm.LoadOffersAsync();

        var url = await vm.PayToOfferAsync("gold", RawLink);

        Assert.Equal(Composed, url);
        Assert.Equal("gold", rpc.LastSubscribeAndCheckout?.Tier);
        Assert.Equal(RawLink, rpc.LastSubscribeAndCheckout?.PaymentUrl);
        Assert.Equal(Home, rpc.LastSubscribeAndCheckout?.Home);
        // The subscribe-and-checkout IS the subscribe: no second, unbound subscribe.
        Assert.DoesNotContain("SubscriptionSubscribe", rpc.Calls);
        // Approved re-reads the offers, as the row's Subscribe does.
        Assert.Equal(2, rpc.Calls.Count(c => c == "SubscriptionOffersList"));
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task PaymentLink_Queued_FlipsTheRowToPending()
    {
        var rpc = RpcWithGoldOffer();
        rpc.NextCheckoutOpen = new FfiCheckoutOpen(new FfiSubscribeReply.Queued(7), Composed);
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false, home: Home);
        await vm.LoadOffersAsync();

        var url = await vm.PayToOfferAsync("gold", RawLink);

        Assert.Equal(Composed, url);
        Assert.Equal(uniffi.fauna_core.OfferStatus.Pending, vm.Offers.Single().Status);
        Assert.Equal(1, rpc.Calls.Count(c => c == "SubscriptionOffersList"));
    }

    [Fact]
    public async Task PaymentLink_RefusedByTheSharedGuard_OpensNothing()
    {
        var rpc = RpcWithGoldOffer();
        rpc.CheckoutRefused = true;
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false, home: Home);
        await vm.LoadOffersAsync();

        var url = await vm.PayToOfferAsync("gold", RawLink);

        Assert.Null(url);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task PaymentLink_RefusedSubscribe_SurfacesTheErrorAndOpensNothing()
    {
        var rpc = RpcWithGoldOffer();
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false, home: Home);
        await vm.LoadOffersAsync();
        rpc.NextError = "boom";

        var url = await vm.PayToOfferAsync("gold", RawLink);

        Assert.Null(url);
        Assert.NotNull(vm.ErrorMessage);
    }

    [Fact]
    public async Task RowSubscribe_GoesAcrossNestsWithThePageHome()
    {
        var rpc = RpcWithGoldOffer();
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false, home: Home);
        await vm.LoadOffersAsync();

        await vm.SubscribeToOfferAsync("gold");

        Assert.Equal("gold", rpc.LastSubscribe?.Tier);
        Assert.Equal(Home, rpc.LastSubscribeHome);
    }
}
#endif   // PAYMENTS
