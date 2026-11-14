using System.Threading.Tasks;
using FaunaApp.Core.ViewModels;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>The federated follow's profile legs (federated-follow.md; profile.md §
/// Where logic lives → The actor's home): the carried home reaches the follow and the
/// OTHER header read, and the audience toggle saves only a press that differs from the
/// base.</summary>
public class ProfileFederatedFollowTests
{
    // Built, not spelled: a 64-hex literal reads as a key to the publish secret scan.
    private static readonly string BobHex = System.Convert.ToHexString(new byte[32]).ToLowerInvariant();
    private const string Home = "https://peer.example";

    [Fact]
    public async Task Follow_CarriesTheHomeTheOpenWasGiven()
    {
        var rpc = new MockNestRpcClient();
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false, home: Home);

        await vm.ToggleFollowAsync();

        Assert.Equal("followers", rpc.LastSubscribe?.Tier);
        Assert.Equal(Home, rpc.LastSubscribeHome);
    }

    [Fact]
    public async Task OtherHeaderRead_IsSeededWithTheCarriedHome()
    {
        var rpc = new MockNestRpcClient
        {
            NextHeaderRead = MockNestRpcClient.MakeProfile(displayName: "Bob"),
        };
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false, home: Home);

        await vm.RefreshHeaderAsync();

        Assert.Contains("ProfileHeaderRead", rpc.Calls);
        Assert.Equal(Home, rpc.LastHeaderReadHome);
    }

    [Fact]
    public async Task AudienceToggle_UntouchedSavesNone()
    {
        var rpc = new MockNestRpcClient { NextProfile = MockNestRpcClient.MakeProfile(displayName: "Me") };
        var vm = new ProfileViewModel(rpc, BobHex);
        await vm.OpenEditFormAsync();

        await vm.SaveEditAsync();

        Assert.Null(rpc.LastProfileSetAudience?.AudienceEveryone);
    }

    [Fact]
    public async Task AudienceToggle_APressThatDiffersFromTheBaseIsSaved_AndOneThatEqualsItIsNot()
    {
        var rpc = new MockNestRpcClient { NextProfile = MockNestRpcClient.MakeProfile(displayName: "Me") };
        var vm = new ProfileViewModel(rpc, BobHex);
        await vm.OpenEditFormAsync();

        vm.SetAudience(true);
        await vm.SaveEditAsync();
        Assert.True(rpc.LastProfileSetAudience?.AudienceEveryone);

        await vm.OpenEditFormAsync();
        vm.SetAudience(true);
        vm.SetAudience(false);   // back to the base (off): no press
        await vm.SaveEditAsync();
        Assert.Null(rpc.LastProfileSetAudience?.AudienceEveryone);
    }
}
