using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// The admin hub's age-band surfaces (family-safety.md § App surface →
/// *Age-band surfaces*) over the <see cref="MockNestRpcClient"/> seam: the two
/// admission band selects (a band rides only with a guardian, clearing the
/// guardian resets it), the request row's claim seed, and the Registration
/// require-knob that the section's one save sends only when it changed. The
/// vocabulary and every rule with a shared home come from UniFFI
/// (<c>age_band_not_set_value</c>, <c>claimed_age_band_option</c>) — these
/// tests pin that the VM defers to them, never a C# spelling.
/// </summary>
public class AdminUsersAgeBandTests
{
    private static readonly string AdminActor = "aa" + new string('0', 62);
    private static readonly string JoinerActor = "bb" + new string('0', 62);
    private static readonly byte[] GuardianId = Convert.FromHexString(AdminActor);

    private static MockNestRpcClient SeededRpc() => new()
    {
        NextAdminUsers = new[] { MockNestRpcClient.MakeAdminUser(AdminActor, tier: "free", label: "admin") },
        NextAdminUsersTotal = 1,
        NextAdminTiers = new[] { MockNestRpcClient.MakeAdminTier("free") },
        NextAdminInviteRequests = new[]
        {
            MockNestRpcClient.MakeAdminInviteRequest(7, handle: "joiner", actorIdHex: JoinerActor),
        },
        NextSetupStatus = MockNestRpcClient.MakeSetupStatus() with { registrationMode = "open" },
    };

    // ── The mint form's band select ──

    [Fact]
    public async Task MintForm_StartsNotSet_AndIsDisabledWithoutAGuardian()
    {
        var vm = new AdminUsersViewModel(SeededRpc());
        await vm.LoadCommand.ExecuteAsync(null);

        vm.BeginCreateCodeCommand.Execute(null);

        Assert.Equal(FaunaFfiMethods.AgeBandNotSetValue(), vm.NewCodeAgeBand);
        Assert.False(vm.NewCodeAgeBandEnabled);
        vm.NewCodeGuardianActor = GuardianId;
        Assert.True(vm.NewCodeAgeBandEnabled);
    }

    [Fact]
    public async Task MintForm_ClearingTheGuardian_ResetsTheBand()
    {
        var vm = new AdminUsersViewModel(SeededRpc());
        await vm.LoadCommand.ExecuteAsync(null);
        vm.BeginCreateCodeCommand.Execute(null);
        vm.NewCodeGuardianActor = GuardianId;
        vm.NewCodeAgeBand = "u13";

        vm.NewCodeGuardianActor = null;

        Assert.Equal(FaunaFfiMethods.AgeBandNotSetValue(), vm.NewCodeAgeBand);
    }

    [Fact]
    public async Task CreateCode_CarriesThePickedBandBesideTheGuardian()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        vm.BeginCreateCodeCommand.Execute(null);
        vm.NewCodeGuardianActor = GuardianId;
        vm.NewCodeAgeBand = "13-15";

        await vm.CreateCodeCommand.ExecuteAsync(null);

        Assert.Equal(GuardianId, rpc.LastAdminInviteCreateGuardianActor);
        Assert.Equal("13-15", rpc.LastAdminInviteCreateAgeBand);
    }

    [Fact]
    public async Task CreateCode_WithoutAGuardian_SendsNoBand()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        vm.BeginCreateCodeCommand.Execute(null);

        await vm.CreateCodeCommand.ExecuteAsync(null);

        Assert.Null(rpc.LastAdminInviteCreateGuardianActor);
        Assert.Null(rpc.LastAdminInviteCreateAgeBand);
    }

    // ── The pending-request row ──

    [Fact]
    public async Task RequestRow_SeedsFromTheApplicantsClaim_ElseNotSet()
    {
        var rpc = SeededRpc();
        rpc.NextAdminInviteRequests = new[]
        {
            MockNestRpcClient.MakeAdminInviteRequest(7, handle: "joiner", actorIdHex: JoinerActor),
            MockNestRpcClient.MakeAdminInviteRequest(8, handle: "claimer", actorIdHex: JoinerActor)
                with { ageBand = "16-17", ageBandProvenance = "attested-android" },
        };
        var vm = new AdminUsersViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(FaunaFfiMethods.ClaimedAgeBandOption(null), vm.InviteRequests[0].AgeBandSeed);
        Assert.Equal(FaunaFfiMethods.AgeBandNotSetValue(), vm.InviteRequests[0].AgeBandSeed);
        Assert.Equal("16-17", vm.InviteRequests[1].AgeBandSeed);
        // The claim text is total: a claim-less row still says something.
        Assert.False(string.IsNullOrEmpty(vm.InviteRequests[0].AgeClaimText));
    }

    [Fact]
    public async Task ApproveRequest_CarriesTheBandOnlyWithAGuardian()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.ApproveRequestAsync(vm.InviteRequests[0], "free", GuardianId, "13-15");
        Assert.Equal("13-15", rpc.LastAdminApproveAgeBand);

        await vm.LoadCommand.ExecuteAsync(null);
        await vm.ApproveRequestAsync(vm.InviteRequests[0], "free", null, "13-15");
        Assert.Null(rpc.LastAdminApproveAgeBand);
    }

    // ── The Registration require-knob ──

    [Fact]
    public async Task Load_SeedsTheKnobDraftFromSetupStatus()
    {
        var rpc = SeededRpc();
        rpc.NextSetupStatus = rpc.NextSetupStatus! with { ageVerificationRequired = true };
        var vm = new AdminUsersViewModel(rpc);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.True(vm.AgeVerificationRequired);
        Assert.True(vm.AgeVerificationDraft);
    }

    [Fact]
    public async Task SaveRegistration_SendsTheKnobBesideTheMode_WhenItChanged()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        vm.AgeVerificationDraft = true;

        await vm.SaveRegistrationCommand.ExecuteAsync(null);

        Assert.NotNull(rpc.LastRegistrationModeSet);
        Assert.Equal(true, rpc.LastAgeVerificationRequiredSet);
        Assert.Null(vm.ActionError);
    }

    [Fact]
    public async Task SaveRegistration_LeavesTheKnobAlone_WhenUnchanged()
    {
        var rpc = SeededRpc();
        var vm = new AdminUsersViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.SaveRegistrationCommand.ExecuteAsync(null);

        Assert.NotNull(rpc.LastRegistrationModeSet);
        Assert.DoesNotContain("AdminSetAgeVerificationRequired", rpc.Calls);
    }

    // ── The invite-code row echo ──

    [Fact]
    public void InviteCodeRow_ItemText_IsTierAndUses_WithoutABand()
    {
        var row = AdminInviteCodeRow.From(MockNestRpcClient.MakeAdminInviteCode("C", "free", 1));

        Assert.Equal("free · 1", row.ItemText);
    }

    [Fact]
    public void InviteCodeRow_ItemText_AppendsTheMintedBand()
    {
        var row = AdminInviteCodeRow.From(
            MockNestRpcClient.MakeAdminInviteCode("C", "free", 1) with { ageBand = "u13" });

        Assert.StartsWith("free · 1 · ", row.ItemText);
    }
}
