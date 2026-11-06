using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using uniffi.fauna_onboarding_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The signed-out lock door (<c>docs/goal/ui/sessions.md</c> § Layout &amp; flow →
/// <i>The signed-out door (leg 3)</i>; <c>devices.md</c> § The signed-out door):
/// <c>identity_choice</c> → <c>emergency_lockout_entry</c>. The ceremony — the
/// identity-secret parse, the <c>LOCK</c> check, the nest resolution, the anonymous
/// lock — is the onboarding machine's; these pin that the windows VM routes to the
/// step, gates the button on the literal word, and says every outcome through the
/// machine's one table (<c>emergency_lockout_outcome_message</c>), dropping what was
/// typed once a lock lands. The refusals below are answered by the machine before
/// anything is sent, so they need no nest.
/// </summary>
public class OnboardingViewModelEmergencyLockoutTests
{
    private sealed class FakeOnboardingObserver : OnboardingObserver
    {
        public void OnChanged() { }
    }

    private static OnboardingViewModel AtTheDoor()
    {
        var vm = new OnboardingViewModel(new FakeOnboardingObserver(), new FakeAccountRegistry());
        vm.BeginEmergencyLockoutEntryCommand.Execute(null);
        return vm;
    }

    [Fact]
    public void The_entry_button_opens_the_door_unarmed()
    {
        var vm = AtTheDoor();

        Assert.Equal(OnboardingStep.EmergencyLockoutEntry, vm.CurrentStep);
        Assert.False(vm.LockoutArmed);
        vm.LockoutWord = "lock";
        Assert.False(vm.LockoutArmed);
        vm.LockoutWord = "LOCK";
        Assert.True(vm.LockoutArmed);
    }

    [Fact]
    public async Task A_wrong_word_is_refused_by_the_machine_and_stays_on_the_door()
    {
        var vm = AtTheDoor();
        vm.LockoutSecret = new string('a', 64);
        vm.LockoutAccount = "alice@example.com";
        vm.LockoutWord = "lock";

        await vm.SubmitEmergencyLockoutCommand.ExecuteAsync(null);

        Assert.Equal(OnboardingStep.EmergencyLockoutEntry, vm.CurrentStep);
        var shared = FaunaOnboardingMachineMethods.EmergencyLockoutOutcomeMessage(
            new EmergencyLockoutOutcome.WrongConfirmWord(), "");
        Assert.Equal(Strings.Resolve(shared), vm.ErrorMessage);
    }

    [Fact]
    public async Task A_recovery_kit_is_not_a_secret_key_and_the_door_says_so()
    {
        var vm = AtTheDoor();
        vm.LockoutSecret = "fauna://recovery?secret=" + new string('7', 64) + "&actor=" + new string('a', 64);
        vm.LockoutAccount = "alice@example.com";
        vm.LockoutWord = "LOCK";

        await vm.SubmitEmergencyLockoutCommand.ExecuteAsync(null);

        Assert.Equal(OnboardingStep.EmergencyLockoutEntry, vm.CurrentStep);
        var shared = FaunaOnboardingMachineMethods.EmergencyLockoutOutcomeMessage(
            new EmergencyLockoutOutcome.InvalidSecret(), "");
        Assert.Equal("onboarding.emergency_lockout.invalid_secret", shared.@key);
        Assert.Equal(Strings.Resolve(shared), vm.ErrorMessage);
        // A refusal keeps what was typed, so the user can correct it.
        Assert.Equal("alice@example.com", vm.LockoutAccount);
    }

    [Fact]
    public void A_landed_lock_says_when_it_ends_and_drops_everything_typed()
    {
        var vm = AtTheDoor();
        vm.LockoutSecret = new string('a', 64);
        vm.LockoutAccount = "alice@example.com";
        vm.LockoutWord = "LOCK";
        var outcome = new EmergencyLockoutOutcome.Locked(1_800_000_000L);

        vm.SettleEmergencyLockout(outcome);

        var shared = FaunaOnboardingMachineMethods.EmergencyLockoutOutcomeMessage(
            outcome, FaunaFfiMethods.FormatUnixLocal(1_800_000_000L));
        Assert.Equal("onboarding.emergency_lockout.locked", shared.@key);
        Assert.Equal(Strings.Resolve(shared), vm.ErrorMessage);
        Assert.Equal("", vm.LockoutSecret);
        Assert.Equal("", vm.LockoutAccount);
        Assert.Equal("", vm.LockoutWord);
        Assert.False(vm.LockoutArmed);
    }

    [Fact]
    public void Reopening_the_door_starts_empty()
    {
        var vm = AtTheDoor();
        vm.LockoutSecret = new string('a', 64);
        vm.LockoutWord = "LOCK";
        vm.BackCommand.Execute(null);
        Assert.Equal(OnboardingStep.IdentityChoice, vm.CurrentStep);

        vm.BeginEmergencyLockoutEntryCommand.Execute(null);

        Assert.Equal("", vm.LockoutSecret);
        Assert.Equal("", vm.LockoutWord);
        Assert.False(vm.LockoutArmed);
    }
}
