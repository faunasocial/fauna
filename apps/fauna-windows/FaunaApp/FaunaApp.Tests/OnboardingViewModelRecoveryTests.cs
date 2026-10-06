using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_onboarding_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Windows leg of box-recovery step-4 (Task E). The OnboardingViewModel is a thin
/// proxy over the shared UniFFI OnboardingMachine (branch tests live in
/// libs/fauna-onboarding-machine), so these pin only the <b>windows glue</b> the
/// two recovery pages bind: the box-list projection into dash-indexed
/// <c>recover-box-item-{i}</c> item VMs, the method-button gating, the
/// begin-recovery transition, and the placeholder self-hosted command.
///
/// Constructs a real <see cref="OnboardingMachine"/> (the native fauna_ffi dll
/// loads in the test host — memory <c>reference_windows_dotnet_test_loads_native_ffi</c>);
/// boxes are seeded via the same <c>call_machine_method("set_recovery_boxes")</c>
/// test-helper the e2e uses. The unit-test host has no WinRT localizer, so
/// <see cref="Strings.Get"/> returns the raw key; assertions compare against it.
/// Per docs/goal/architecture/nest/box-recovery.md § Recovery UI (step 4).
/// </summary>
public class OnboardingViewModelRecoveryTests
{
    // 64-hex nest ids so ShortId's head8…tail8 truncation is exercised.
    private const string BoxA =
        "aaaaaaaabbbbbbbbccccccccddddddddeeeeeeeeffffffff0000000011111111";
    private const string BoxB =
        "2222222233333333444444445555555566666666777777778888888899999999";

    private sealed class FakeOnboardingObserver : OnboardingObserver
    {
        public void OnChanged() { }
    }

    private static OnboardingViewModel NewVm() =>
        new(new FakeOnboardingObserver(), new FakeAccountRegistry());

    private static void SeedBoxes(OnboardingViewModel vm, params string[] ids)
        => vm.CallMachineMethod("set_recovery_boxes",
            "[" + string.Join(",", System.Array.ConvertAll(ids, id => $"\"{id}\"")) + "]");

    [Fact]
    public void BeginRecoverLostBox_TransitionsToIdentityImportWithRecoveryIntent()
    {
        var vm = NewVm();
        vm.BeginRecoverLostBoxCommand.Execute(null);

        Assert.Equal(OnboardingStep.IdentityImport, vm.CurrentStep);
        Assert.True(vm.RecoveryIntent);
    }

    [Fact]
    public void RecoveryBoxItems_MapMachineBoxesToDashIndexedIds()
    {
        var vm = NewVm();
        SeedBoxes(vm, BoxA, BoxB);

        Assert.True(vm.HasRecoveryBoxes);
        Assert.Equal(2, vm.RecoveryBoxItems.Count);
        Assert.Equal("recover-box-item-0", vm.RecoveryBoxItems[0].BoxItemId);
        Assert.Equal("recover-box-item-1", vm.RecoveryBoxItems[1].BoxItemId);
        Assert.Equal(BoxA, vm.RecoveryBoxItems[0].Id);
        // head-8 … tail-8 short display (mirrors linux short_nest_id).
        Assert.Equal("aaaaaaaa…11111111", vm.RecoveryBoxItems[0].ShortId);
    }

    [Fact]
    public void RecoveryBoxes_Empty_HasRecoveryBoxesFalse()
    {
        var vm = NewVm();
        SeedBoxes(vm); // []

        Assert.False(vm.HasRecoveryBoxes);
        Assert.Empty(vm.RecoveryBoxItems);
    }

    [Fact]
    public void RecoveryMethodButtons_GatedOnSelection()
    {
        var vm = NewVm();
        SeedBoxes(vm, BoxA, BoxB);

        Assert.False(vm.RecoveryMethodButtonsEnabled);

        vm.SelectRecoveryBox(BoxA);

        Assert.True(vm.RecoveryMethodButtonsEnabled);
        Assert.True(vm.RecoveryBoxItems[0].IsSelected);
        Assert.False(vm.RecoveryBoxItems[1].IsSelected);
    }

    [Fact]
    public void RecoverViaSelfhosted_AfterSelection_TransitionsToInstructions()
    {
        var vm = NewVm();
        SeedBoxes(vm, BoxA);
        vm.SelectRecoveryBox(BoxA);

        vm.RecoverViaSelfhostedCommand.Execute(null);

        Assert.Equal(OnboardingStep.RecoverSelfhostedInstructions, vm.CurrentStep);
    }

    [Fact]
    public void RecoverViaCloud_WithoutSelection_DoesNotThrow()
    {
        var vm = NewVm();
        SeedBoxes(vm, BoxA); // seeded but not selected → guard refuses

        // The command wraps the machine's OnboardingException guard; the
        // unselected click is a benign no-op, not a crash.
        vm.RecoverViaCloudCommand.Execute(null);

        Assert.NotEqual(OnboardingStep.VpsConfig, vm.CurrentStep);
    }

    [Fact]
    public void RecoverSelfhostedCommand_IsPendingPlaceholder()
    {
        var vm = NewVm();
        Assert.Equal(
            Strings.Get("onboarding/recovery/selfhosted_command_pending"),
            vm.RecoverSelfhostedCommand);
    }
}
