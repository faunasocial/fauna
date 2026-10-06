using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_onboarding_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The windows leg of the two identity-recovery screens (<c>onboarding.md</c>
/// § 1 Identity): the view model declares <c>set_renders_recovery_kit</c>, so a
/// created identity routes through <c>recovery_kit</c> (whose QR/copy payload is
/// the machine's one <c>fauna://recovery</c> URI, never the bare hex), and the
/// phrase restore's outcomes fold back into the wizard through the shared
/// outcome → message table. The ceremony itself (nest round-trips, the deferred
/// registration) is the e2e journeys' to pin.
/// </summary>
public class OnboardingViewModelRecoveryKitTests
{
    private sealed class FakeOnboardingObserver : OnboardingObserver
    {
        public void OnChanged() { }
    }

    private static OnboardingViewModel NewVm() =>
        new(new FakeOnboardingObserver(), new FakeAccountRegistry());

    private static OnboardingViewModel AtKitScreen()
    {
        var vm = NewVm();
        vm.BeginCreateIdentityCommand.Execute(null);
        vm.ConfirmGeneratedIdentityCommand.Execute(null);
        return vm;
    }

    [Fact]
    public void CreatePath_RoutesThroughTheKitScreen_WithAFreshRootAndTheUriPayload()
    {
        var vm = AtKitScreen();

        Assert.Equal(OnboardingStep.RecoveryKit, vm.CurrentStep);
        var kit = vm.RecoveryKitSecretHex;
        Assert.NotNull(kit);
        Assert.Equal(64, kit!.Length);
        Assert.Equal(kit, vm.RecoveryKitSecretText);
        // A fresh random root, never the identity seed shown back.
        Assert.NotEqual(vm.GeneratedSecret, kit);
        // QR + copy carry the URI form, which wraps the same root.
        Assert.StartsWith("fauna://recovery", vm.RecoveryKitUri);
        Assert.Contains(kit, vm.RecoveryKitUri);
    }

    [Fact]
    public void Confirm_AndSkip_BothAdvanceToHandleEntry()
    {
        var confirmed = AtKitScreen();
        confirmed.ConfirmRecoveryKitCommand.Execute(null);
        Assert.Equal(OnboardingStep.HandleEntry, confirmed.CurrentStep);

        var skipped = AtKitScreen();
        skipped.SkipRecoveryKitCommand.Execute(null);
        Assert.Equal(OnboardingStep.HandleEntry, skipped.CurrentStep);
        // The URI lives exactly as long as the pending root.
        Assert.Null(skipped.RecoveryKitUri);
    }

    [Fact]
    public void RestoreButton_OpensTheRecoveryEntryScreen_AndBackReturnsToChoice()
    {
        var vm = NewVm();
        vm.BeginRecoveryEntryCommand.Execute(null);
        Assert.Equal(OnboardingStep.RecoveryEntry, vm.CurrentStep);

        vm.BackCommand.Execute(null);
        Assert.Equal(OnboardingStep.IdentityChoice, vm.CurrentStep);
    }

    [Fact]
    public void SupersededOutcome_RoutesToImport_WithTheSharedReason()
    {
        var vm = NewVm();
        vm.BeginRecoveryEntryCommand.Execute(null);

        vm.SettleRecoveryEntry(new RecoveryEntryOutcome.Superseded());

        Assert.Equal(OnboardingStep.IdentityImport, vm.CurrentStep);
        Assert.Equal(Strings.Get("onboarding/recovery_entry/superseded"), vm.ErrorMessage);
    }

    [Fact]
    public void RefusalOutcome_SpeaksThroughTheSharedTable_AndStaysOnThePage()
    {
        var vm = NewVm();
        vm.BeginRecoveryEntryCommand.Execute(null);

        var outcome = new RecoveryEntryOutcome.AccountUnknown("alice@example.com");
        vm.SettleRecoveryEntry(outcome);

        Assert.Equal(OnboardingStep.RecoveryEntry, vm.CurrentStep);
        // Exactly the shared table's answer — the key and its argument — never a
        // windows-side re-derivation.
        var shared = FaunaOnboardingMachineMethods.RecoveryEntryOutcomeMessage(outcome);
        Assert.NotNull(shared);
        Assert.Equal("onboarding.recovery_entry.account_unknown", shared!.@key);
        Assert.Equal(Strings.Resolve(shared), vm.ErrorMessage);
    }
}
