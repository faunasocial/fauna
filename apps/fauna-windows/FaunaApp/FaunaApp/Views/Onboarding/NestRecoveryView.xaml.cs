using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Onboarding;

/// <summary>
/// Box-recovery step-4 hub (Task E): pick a custodied box, then re-provision it
/// on a cloud host or install it on your own server. Mirrors
/// <c>apps/fauna-linux/src/views/onboarding/nest_recovery.rs</c> + the web
/// reference, driving the shared <see cref="OnboardingViewModel"/>'s recovery
/// surface (which proxies the landed onboarding-machine recovery branch). Per
/// <c>docs/goal/architecture/nest/box-recovery.md</c> § Recovery UI (step 4).
///
/// The box list is empty in production (the reachable-nest fetch is gated) → the empty message shows; the e2e seeds it via
/// <c>call_machine_method("set_recovery_boxes")</c>.
/// </summary>
public sealed partial class NestRecoveryView : UserControl
{
    internal OnboardingViewModel ViewModel { get; private set; } = null!;

    public NestRecoveryView()
    {
        InitializeComponent();
    }

    internal NestRecoveryView(OnboardingViewModel vm) : this()
    {
        ViewModel = vm;
        DataContext = vm;
        // Same x:Bind refresh pattern as VpsConfigView: the VM raises
        // PropertyChanged (empty name) on every observer tick; Bindings.Update()
        // re-runs the OneWay sources so the box list, empty/list visibility, and
        // the method-button gating reflect a fresh seed or a new selection.
        ViewModel.PropertyChanged += (_, _) => Bindings.Update();
    }

    /// <summary>Forward a box-row click to the machine (enables the method
    /// buttons). Reads the bound item VM from the Button's DataContext, mirroring
    /// <see cref="VpsConfigView.OnVpsServerTypeChecked"/>.</summary>
    private void OnRecoveryBoxItemClick(object sender, RoutedEventArgs e)
    {
        if (sender is not Button btn) return;
        if (btn.DataContext is not RecoveryBoxItemViewModel item) return;
        ViewModel.SelectRecoveryBox(item.Id);
    }

    /// <summary>x:Bind helper: plain bool → Visibility. Instance (not static) —
    /// x:Bind function bindings emit an instance call. The argument path is
    /// null-safe under x:Bind, so it evaluates cleanly during the parameterless
    /// ctor before <see cref="ViewModel"/> is assigned.</summary>
    public Visibility BoolToVisibility(bool value)
        => value ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>x:Bind helper: inverse of <see cref="BoolToVisibility"/> — shows
    /// the empty message exactly when there are no custodied boxes.</summary>
    public Visibility BoolToVisibilityInverse(bool value)
        => value ? Visibility.Collapsed : Visibility.Visible;
}
