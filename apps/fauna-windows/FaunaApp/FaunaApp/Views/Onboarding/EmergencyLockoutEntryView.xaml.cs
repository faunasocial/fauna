using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Onboarding;

/// <summary>
/// The signed-out lock door — onboarding step <c>emergency_lockout_entry</c>
/// (<c>docs/goal/ui/sessions.md</c> § Layout &amp; flow → <i>The signed-out door
/// (leg 3)</i>; <c>devices.md</c> § The signed-out door): lock an account for 24
/// hours from a device the owner will not sign in on. Twin of tui's
/// <c>wizard/emergency_lockout_entry.rs</c>, android's
/// <c>EmergencyLockoutEntryScreen</c> and FaunaKit's <c>EmergencyLockoutEntryView</c>.
///
/// <para>Pure render: the submit
/// (<see cref="OnboardingViewModel.SubmitEmergencyLockoutCommand"/>) runs the
/// machine's <c>SubmitEmergencyLockout</c> — the anonymous, seed-signed
/// <c>fauna.account.lockout</c> — and folds its outcome back; a landed lock returns
/// to <c>identity_choice</c> with the confirmation, every refusal stays here.
/// Nothing typed is persisted.</para>
/// </summary>
public sealed partial class EmergencyLockoutEntryView : UserControl
{
    public EmergencyLockoutEntryView()
    {
        InitializeComponent();
    }

    internal EmergencyLockoutEntryView(OnboardingViewModel vm) : this()
    {
        DataContext = vm;
    }

    /// <summary>The machine re-checks the word (the action arm), so a press that
    /// reached here past a disabled render still cannot lock on a wrong one; a
    /// second press while one is in flight is the command's own to refuse.</summary>
    private async void OnLockClick(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (DataContext is OnboardingViewModel vm && vm.SubmitEmergencyLockoutCommand.CanExecute(null))
            await vm.SubmitEmergencyLockoutCommand.ExecuteAsync(null);
    }
}
