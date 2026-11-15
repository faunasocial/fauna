using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_feed;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// The feed's <c>gated-post-payment-link</c> rides the manager's shared
/// <c>pay_unlock_offer</c> (subscribe first, then the composed checkout URL —
/// monetization.md § Pillar 3 → Buyer↔actor binding (Q4)), never the author's raw
/// payment_url. Offline, as <see cref="FeedViewModelAuthorWindowTests"/> is:
/// <c>NestClient::new</c> opens no socket, and a post the manager has not loaded has
/// no resolved offer, so the shared rule answers "nothing to open" before any
/// subscribe — the null the card host maps to "open nothing".
/// </summary>
public class FeedPayUnlockOfferTests
{
    private sealed class NullObserver : FeedSnapshotObserver
    {
        public void OnChanged() { }
    }

    [Fact]
    public async Task PayUnlockOffer_UnloadedPost_HandsBackNothingToOpen()
    {
        using var nest = new FfiNestClient("wss://127.0.0.1:0/ws", new byte[32]);
        using var manager = nest.FeedManager(new byte[32]);
        var vm = new FeedViewModel(manager, new NullObserver(), new MockNestRpcClient());

        var url = await vm.PayUnlockOfferAsync("no-such-post");

        Assert.Null(url);
    }
}
