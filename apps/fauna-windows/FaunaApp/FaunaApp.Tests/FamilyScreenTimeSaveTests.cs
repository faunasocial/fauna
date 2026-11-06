using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The guardian editor's screen-time write validation (family-safety.md § Where logic
/// lives → <i>Write validation</i>): the save path pre-validates the parsed window and
/// budget through the shared <c>ScreenTimePolicy::validate</c>
/// (<c>FaunaFfiMethods.ValidateScreenTime</c>), so a half-set or empty window is refused
/// on <c>error-message</c> with the rule's own reason and never reaches
/// <c>fauna.family.policy.update</c>. The per-field parsers judge one input each and
/// cannot see the pair. The e2e twin is
/// <c>test_family_screen_time_editor_refuses_half_or_empty_window</c>.
/// </summary>
public class FamilyScreenTimeSaveTests
{
    private static FfiReachPolicy Policy() =>
        new(contactApproval: false, unknownSenderMail: "allow", federationContact: true,
            feedSources: "allow", contentPolicy: null, screenTime: null,
            contentNotify: null, unknownPeerDm: null);

    private static async Task<(FamilyViewModel Vm, MockNestRpcClient Rpc)> LoadedEditor()
    {
        var rpc = new MockNestRpcClient
        {
            NextFamilyStatus = FfiFamilyStatusFixture.Make(wards:
                [FfiFamilyWardInfoFixture.Make(actorId: Enumerable.Repeat((byte)7, 32).ToArray(), policy: Policy())]),
        };
        var vm = new FamilyViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        return (vm, rpc);
    }

    [Theory]
    [InlineData("08:00", "", "pairs")]
    [InlineData("", "07:00", "pairs")]
    [InlineData("09:30", "09:30", "start == end")]
    public async Task AWindowTheNestWouldRefuse_IsRefusedHere_AndNothingIsSent(
        string start, string end, string reason)
    {
        var (vm, rpc) = await LoadedEditor();

        vm.PolicyScreenWindowStart = start;
        vm.PolicyScreenWindowEnd = end;
        await vm.SavePolicyAsync();

        Assert.NotNull(vm.ActionError);
        Assert.Contains(reason, vm.ActionError);
        Assert.DoesNotContain("FamilyPolicyUpdate", rpc.Calls);
    }

    [Fact]
    public async Task AWrappingWindowAndABudget_PassTheCheck_AndAreSent()
    {
        var (vm, rpc) = await LoadedEditor();

        vm.PolicyScreenWindowStart = "21:00";
        vm.PolicyScreenWindowEnd = "07:00";
        vm.PolicyScreenDailyMinutes = "90";
        await vm.SavePolicyAsync();

        Assert.Null(vm.ActionError);
        var screenTime = Assert.IsType<FfiScreenTimePolicy>(rpc.LastFamilyPolicyUpdate!.Value.Policy.@screenTime);
        Assert.Equal((ushort)(21 * 60), screenTime.@windowStart);
        Assert.Equal((ushort)(7 * 60), screenTime.@windowEnd);
        Assert.Equal((ushort)90, screenTime.@dailyMinutes);
    }
}
