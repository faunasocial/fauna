using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Onboarding;

/// <summary>
/// Stage 2 of handle-first onboarding: handle entry + Check + outcome
/// rendering (Wave 3 / handle-check-and-progress). The machine drives a
/// phased probe (DNS → nest health → challenge → optional price) and
/// publishes a HandleCheckSnapshot; this view renders against
/// <see cref="OnboardingViewModel.HandleCheckMessage"/> and friends.
///
/// The seven outcomes (FormatInvalid / TldInvalid / DomainAvailable /
/// RegisteredNoNest / AlreadyOnNest / NestRunningUserUnregistered /
/// ProbeError) are surfaced through the same `handle-message-area`
/// TextBlock; Continue routes by outcome inside the machine, and the
/// VM's HandleEntryContinueAsync handles wizard_outcome on Done.
/// </summary>
public sealed partial class HandleEntryView : UserControl
{
    internal OnboardingViewModel ViewModel { get; private set; } = null!;

    public HandleEntryView()
    {
        InitializeComponent();
    }

    internal HandleEntryView(OnboardingViewModel vm) : this()
    {
        ViewModel = vm;
        DataContext = vm;
        // The VM raises PropertyChanged with empty PropertyName on every
        // observer tick from the OnboardingMachine; Bindings.Update()
        // re-runs all x:Bind expressions so HandleCheckMessage,
        // HandleCheckEnabled, HandleControlCheckboxVisible, and
        // HandleCheckContinueEnabled refresh as the snapshot evolves.
        vm.PropertyChanged += (_, _) => Bindings.Update();
    }

    /// <summary>
    /// x:Bind helper bridging the cross-platform VM bool to WinUI's
    /// <see cref="Visibility"/>. FaunaApp.Core can't reference WinUI types
    /// directly so the VM exposes booleans and each platform converts.
    /// </summary>
    public static Visibility BoolToVisibility(bool value)
        => value ? Visibility.Visible : Visibility.Collapsed;
}
