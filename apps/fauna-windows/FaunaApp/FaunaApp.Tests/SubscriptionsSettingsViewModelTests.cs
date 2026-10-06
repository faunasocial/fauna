using Xunit;
using FaunaApp.Core.Models;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Tests;

// ── SubscriptionsSettingsViewModel Tests — subscriptions Slice B (consumer page).
// monetization.md § Pillar 1 consumer path. The VM mirrors ProfileViewModel in
// structure (observer-free, manual re-read after mutation), but is the CONSUMER
// side: it lists the actor's own subscriptions (mine.list) and lets the consumer
// unsubscribe. ──

public class SubscriptionsSettingsViewModelTests
{
    private static readonly byte[] Alice = Enumerable.Repeat((byte)0xa1, 32).ToArray();
    private static readonly byte[] Bob = new byte[32]; // all-zero, no handle → hex fallback

    private static string Hex(byte[] b) => Convert.ToHexString(b).ToLowerInvariant();

    [Fact]
    public async Task HydrateAsync_MapsRows_HandleAndHexFallback()
    {
        // Alice has a handle; Bob is all-zero bytes → hex fallback.
        var rpc = new MockNestRpcClient
        {
            NextMineSubscriptions = new[]
            {
                MockNestRpcClient.MakeMineSubscription(Alice, "gold", "active", handle: "alice"),
                MockNestRpcClient.MakeMineSubscription(Bob, "silver", "pending", handle: null),
            },
        };
        var vm = new SubscriptionsSettingsViewModel(rpc);

        await vm.HydrateAsync();

        Assert.Equal(2, vm.Subscriptions.Count);
        Assert.Equal("alice", vm.Subscriptions[0].AuthorDisplay);
        Assert.Equal(Hex(Bob), vm.Subscriptions[1].AuthorDisplay);
    }

    [Fact]
    public async Task HydrateAsync_StatusPassthrough()
    {
        // Status must be the raw wire string — not capitalised, not i18n-translated.
        var rpc = new MockNestRpcClient
        {
            NextMineSubscriptions = new[]
            {
                MockNestRpcClient.MakeMineSubscription(Alice, "gold", "active", handle: "alice"),
                MockNestRpcClient.MakeMineSubscription(Bob, "silver", "pending", handle: null),
            },
        };
        var vm = new SubscriptionsSettingsViewModel(rpc);

        await vm.HydrateAsync();

        Assert.Equal("active", vm.Subscriptions[0].Status);
        Assert.Equal("pending", vm.Subscriptions[1].Status);
    }

    [Fact]
    public async Task UnsubscribeAsync_DispatchesAuthorId_AndReReads()
    {
        // After Unsubscribe, SubscriptionMineList must be called again (re-read).
        // We set NextMineSubscriptions empty after the unsubscribe call to confirm the
        // collection clears on the re-read.
        var rpc = new MockNestRpcClient
        {
            NextMineSubscriptions = new[]
            {
                MockNestRpcClient.MakeMineSubscription(Alice, "gold", "active", handle: "alice"),
            },
            NextUnsubscribeReply = new uniffi.fauna_ffi.FfiUnsubscribeReply.Removed(),
        };
        var vm = new SubscriptionsSettingsViewModel(rpc);
        await vm.HydrateAsync();

        // After unsubscribe clear the next list to empty so we can confirm the re-read
        rpc.NextMineSubscriptions = Array.Empty<uniffi.fauna_ffi.FfiMineSubscription>();
        await vm.UnsubscribeAsync(Alice);

        Assert.Equal(Alice, rpc.LastUnsubscribeAuthorId);
        // The re-read must have happened: collection cleared from initial 1 row to 0.
        Assert.Empty(vm.Subscriptions);
        // Calls: SubscriptionMineList (hydrate) + SubscriptionUnsubscribe + SubscriptionMineList (re-read)
        var calls = rpc.Calls.ToList();
        int firstMine = calls.IndexOf("SubscriptionMineList");
        int unsub = calls.IndexOf("SubscriptionUnsubscribe");
        int secondMine = calls.LastIndexOf("SubscriptionMineList");
        Assert.True(firstMine >= 0, "expected SubscriptionMineList call");
        Assert.True(unsub > firstMine, "expected SubscriptionUnsubscribe after first mine list");
        Assert.True(secondMine > unsub, "expected second SubscriptionMineList after unsubscribe (re-read)");
    }

    [Fact]
    public async Task HydrateAsync_ErrorSurfacesToErrorMessage()
    {
        var rpc = new MockNestRpcClient { NextError = "boom" };
        var vm = new SubscriptionsSettingsViewModel(rpc);

        await vm.HydrateAsync();

        Assert.NotNull(vm.ErrorMessage);
        Assert.Empty(vm.Subscriptions);
    }

#if PAYMENTS
// Gated with the plane under test — dynamic-features.md § The feature-matrix
// test story: both flavors build and run their test subset, and a store-safe
// build has no payments members to drive.
    // ── Claim redemption (monetization.md § Pillar 3 Q4 — the universal fallback
    // binding). RedeemClaimAsync binds a post-payment claim code to this actor,
    // then re-reads mine.list — the queued grant renders exactly like a queued
    // subscribe (a "pending" row), mirrors linux/android's redeem_claim. ──

    [Fact]
    public async Task RedeemClaimAsync_DispatchesTrimmedCodeAndReReads()
    {
        var rpc = new MockNestRpcClient();
        var vm = new SubscriptionsSettingsViewModel(rpc);
        await vm.HydrateAsync();

        rpc.NextMineSubscriptions = new[]
        {
            MockNestRpcClient.MakeMineSubscription(Alice, "gold", "pending", handle: "alice"),
        };
        await vm.RedeemClaimAsync("  ABC123  ");

        Assert.Equal("ABC123", rpc.LastClaimsRedeem);
        Assert.Contains("PaymentsClaimsRedeem", rpc.Calls);
        // Re-read must have happened: the queued grant now renders as a pending row.
        Assert.Single(vm.Subscriptions);
        Assert.Equal("pending", vm.Subscriptions[0].Status);
    }

    [Fact]
    public async Task RedeemClaimAsync_BlankCodeIsNoop()
    {
        var rpc = new MockNestRpcClient();
        var vm = new SubscriptionsSettingsViewModel(rpc);

        await vm.RedeemClaimAsync("   ");

        Assert.DoesNotContain("PaymentsClaimsRedeem", rpc.Calls);
    }

    [Fact]
    public async Task RedeemClaimAsync_TypedErrorSurfacesToErrorMessage()
    {
        var rpc = new MockNestRpcClient { NextError = "fauna.payments.claim_not_found" };
        var vm = new SubscriptionsSettingsViewModel(rpc);

        await vm.RedeemClaimAsync("NOSUCH1234");

        Assert.NotNull(vm.ErrorMessage);
    }
#endif   // PAYMENTS
}
