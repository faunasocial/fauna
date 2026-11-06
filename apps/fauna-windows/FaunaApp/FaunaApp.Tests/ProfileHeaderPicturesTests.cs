using System.Threading.Tasks;
using FaunaApp.Core.Models;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>The header's stored pictures (profile.md § The remote read → <i>The header's
/// pictures</i>): a picture is named only when the profile names it, and the host pick is
/// this nest for a profile it served, the <c>relayed_from</c> home — with the identity this
/// nest stamped — for a relayed one.</summary>
public class ProfileHeaderPicturesTests
{
    // Built, not spelled: a 64-hex literal reads as a key to the publish secret scan.
    private static readonly string BobHex = System.Convert.ToHexString(new byte[32]).ToLowerInvariant();
    private static readonly string HashA = new string('a', 8);
    private static readonly string HashB = new string('b', 8);
    private const string Home = "https://peer.example";
    private const string HomeId = "home-identity";

    [Fact]
    public async Task TheHeaderNamesOnlyThePicturesTheProfileNames()
    {
        var rpc = new MockNestRpcClient
        {
            NextProfile = MockNestRpcClient.MakeProfile(
                displayName: "Me", pictures: new ProfilePictures(HashA, null)),
        };
        var vm = new ProfileViewModel(rpc, BobHex);

        await vm.RefreshHeaderAsync();

        Assert.Equal(HashA, vm.AvatarHash);
        Assert.Null(vm.BannerHash);
        Assert.Null(vm.PictureHome);   // this nest served it
    }

    [Fact]
    public async Task AFailedReadNamesNothing()
    {
        var rpc = new MockNestRpcClient
        {
            NextProfile = MockNestRpcClient.MakeProfile(pictures: new ProfilePictures(HashA, HashB)),
        };
        var vm = new ProfileViewModel(rpc, BobHex);
        await vm.RefreshHeaderAsync();

        rpc.NextProfile = null;   // the next read finds nothing
        await vm.RefreshHeaderAsync();

        Assert.Null(vm.AvatarHash);
        Assert.Null(vm.BannerHash);
        Assert.Equal("", vm.ErrorMessage ?? "");   // never error-message
    }

    [Fact]
    public async Task ARelayedProfilesPicturesComeFromTheHomeWithItsStampedIdentity()
    {
        var rpc = new MockNestRpcClient
        {
            NextHeaderRead = MockNestRpcClient.MakeProfile(
                displayName: "Bob", pictures: new ProfilePictures(HashA, HashB, Home, HomeId)),
        };
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false, home: Home);

        await vm.RefreshHeaderAsync();

        var home = Assert.IsType<FfiPostBlobHome>(vm.PictureHome);
        Assert.Equal(Home, home.url);
        Assert.Equal(HomeId, home.nestActorId);
        Assert.Equal(HashA, vm.AvatarHash);
        Assert.Equal(HashB, vm.BannerHash);
    }

    [Fact]
    public async Task AProfileThisNestServedIsFetchedFromThisNest()
    {
        var rpc = new MockNestRpcClient
        {
            NextHeaderRead = MockNestRpcClient.MakeProfile(
                displayName: "Bob", pictures: new ProfilePictures(HashA, null, null, null)),
        };
        var vm = new ProfileViewModel(rpc, BobHex, isSelf: false, home: Home);

        await vm.RefreshHeaderAsync();

        Assert.Null(vm.PictureHome);
        Assert.Equal(HashA, vm.AvatarHash);
    }
}
