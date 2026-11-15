using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.ViewModels;
using Xunit;

namespace FaunaApp.Tests;

/// <summary><c>profile-follow-button</c> as the Follow ⇄ Following toggle
/// (<c>subscriptions.md</c> § The free followers tier — follow and unfollow, rule 4;
/// <c>profile.md</c> § Where logic lives → Follow / unfollow / follow-status). The
/// derivation is the shared <c>is_following</c> over the open's <c>status.get</c>
/// read; the optimistic flip and the armed paid-tier confirm are the view model's —
/// the shape tui's <c>ProfileState</c> and FaunaKit's <c>ProfileOffersVM</c> carry.</summary>
public class ProfileFollowToggleTests
{
    private static readonly byte[] Bob = Enumerable.Repeat((byte)0xb2, 32).ToArray();
    private static readonly string BobHex = System.Convert.ToHexString(Bob).ToLowerInvariant();
    private const string Home = "https://peer.example";

    private static async Task<(MockNestRpcClient Rpc, ProfileViewModel Vm)> OpenAsync(
        string? tier = null, bool followPending = false, string? home = null)
    {
        var rpc = new MockNestRpcClient
        {
            NextStatus = MockNestRpcClient.MakeStatus(tier: tier, followPending: followPending),
            NextSubscribeReply = new uniffi.fauna_ffi.FfiSubscribeReply.Queued(1),
            NextUnsubscribeReply = new uniffi.fauna_ffi.FfiUnsubscribeReply.Queued(2),
        };
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false, home: home);
        await vm.HydrateAsync();
        return (rpc, vm);
    }

    [Theory]
    [InlineData(null, false, false)]
    [InlineData("followers", false, true)]
    // A paid subscriber holds followers by the cascade, and status.get names the
    // paid tier: the shared rule reads Following (web's old tier == followers test
    // read Follow here).
    [InlineData("gold", false, true)]
    // The pending window: the follow is only asked, the toggle still holds.
    [InlineData(null, true, true)]
    public async Task TheOpen_DerivesFollowingFromTheStatusRead(string? tier, bool pending, bool following)
    {
        var (rpc, vm) = await OpenAsync(tier, pending);

        Assert.Contains("SubscriptionStatusGet", rpc.Calls);
        Assert.Equal(following, vm.IsFollowing);
        Assert.False(vm.UnfollowArmed);
    }

    [Fact]
    public async Task FollowPressed_SubscribesToTheFollowersTierAndFlipsToFollowing()
    {
        var (rpc, vm) = await OpenAsync();

        await vm.ToggleFollowAsync();

        Assert.Equal("followers", rpc.LastSubscribe?.Tier);
        Assert.Equal(Bob, rpc.LastSubscribe?.AuthorId);
        Assert.DoesNotContain("SubscriptionUnsubscribe", rpc.Calls);
        // The status still reads not-following (the follow is queued): the flip
        // outranks it.
        Assert.True(vm.IsFollowing);
    }

    [Fact]
    public async Task AFreeFollowersFollowingPress_UnfollowsAtOnceAtTheFollowersFloor()
    {
        var (rpc, vm) = await OpenAsync(tier: "followers");

        await vm.ToggleFollowAsync();

        Assert.Equal(Bob, rpc.LastUnsubscribeAuthorId);
        Assert.Equal(("followers", (string?)null), rpc.LastUnsubscribeRoute);
        Assert.DoesNotContain("SubscriptionSubscribe", rpc.Calls);
        // The leave is queued — the status still names the tier — and the flip reads
        // Follow at once.
        Assert.False(vm.IsFollowing);
        Assert.False(vm.UnfollowArmed);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public async Task APendingFollowsPress_WithdrawsTheRequest()
    {
        var (rpc, vm) = await OpenAsync(tier: null, followPending: true);

        await vm.ToggleFollowAsync();

        Assert.Equal(("followers", (string?)null), rpc.LastUnsubscribeRoute);
        Assert.False(vm.IsFollowing);
    }

    [Fact]
    public async Task APaidHoldersFirstPressOnlyArms_TheSecondLeaves()
    {
        var (rpc, vm) = await OpenAsync(tier: "gold");

        await vm.ToggleFollowAsync();

        Assert.DoesNotContain("SubscriptionUnsubscribe", rpc.Calls);
        Assert.True(vm.UnfollowArmed);
        Assert.True(vm.IsFollowing);

        await vm.ToggleFollowAsync();

        Assert.Equal(("followers", (string?)null), rpc.LastUnsubscribeRoute);
        Assert.False(vm.UnfollowArmed);
        Assert.False(vm.IsFollowing);
    }

    [Fact]
    public async Task BothDirections_CarryThePagesActorHome()
    {
        var (rpc, vm) = await OpenAsync(home: Home);

        await vm.ToggleFollowAsync();
        Assert.Equal(Home, rpc.LastSubscribeHome);

        await vm.ToggleFollowAsync();
        Assert.Equal(("followers", (string?)Home), rpc.LastUnsubscribeRoute);
    }

    [Fact]
    public async Task AnotherGesture_DisarmsTheConfirm()
    {
        var (_, vm) = await OpenAsync(tier: "gold");
        await vm.ToggleFollowAsync();
        Assert.True(vm.UnfollowArmed);

        await vm.SubscribeToOfferAsync("gold");

        Assert.False(vm.UnfollowArmed);
        Assert.True(vm.IsFollowing);
    }

    [Fact]
    public async Task ARefusedUnfollow_KeepsFollowingAndSurfacesTheError()
    {
        var (rpc, vm) = await OpenAsync(tier: "followers");
        rpc.NextError = "not_subscribed";

        await vm.ToggleFollowAsync();

        Assert.True(vm.IsFollowing);
        Assert.NotNull(vm.ErrorMessage);
    }

    [Fact]
    public async Task APressAndTheArm_RaiseTheChangesThePageRepaintsOn()
    {
        var (_, vm) = await OpenAsync(tier: "gold");
        var raised = new List<string?>();
        vm.PropertyChanged += (_, e) => raised.Add(e.PropertyName);

        await vm.ToggleFollowAsync();
        Assert.Contains(nameof(ProfileViewModel.UnfollowArmed), raised);

        raised.Clear();
        await vm.ToggleFollowAsync();
        Assert.Contains(nameof(ProfileViewModel.IsFollowing), raised);
        Assert.Contains(nameof(ProfileViewModel.UnfollowArmed), raised);
    }
}
