using FaunaApp.Core.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using uniffi.fauna_provisioning;
using System.ComponentModel;

namespace FaunaApp.Views.Onboarding;

/// <summary>
/// Page 6 of handle-first onboarding: snapshot-driven nest provisioning
/// progress. Pure observer-render — every transition flows through
/// <see cref="OnboardingViewModel.ProvisioningSnapshot"/> and the derived
/// display properties; the view binds, never decides. Mirrors
/// apps/fauna-linux/src/views/onboarding/nest_provisioning.rs.
///
/// The 1Hz elapsed-counter tick lives here (not in the VM) so it fires on
/// the UI thread via DispatcherTimer — firing PropertyChanged from a
/// thread-pool thread caused queued-up dispatch backlog that blocked the
/// test bridge's call_machine_method. See commit history.
///
/// Design tracked internally.
/// Target spec: docs/goal/behavior/onboarding.md §6.
/// </summary>
public sealed partial class NestProvisioningView : UserControl
{
    // Typed VM property used by x:Bind. Defaulted to null! since the
    // parameterless ctor (used by the XAML loader's design-time path)
    // never gets a VM.
    internal OnboardingViewModel ViewModel { get; private set; } = null!;

    private DispatcherTimer? _elapsedTimer;
    private OverallStatus? _lastOverall;

    public NestProvisioningView()
    {
        InitializeComponent();
    }

    internal NestProvisioningView(OnboardingViewModel vm) : this()
    {
        ViewModel = vm;
        DataContext = vm;

        // x:Bind generates one-time evaluations for OneWay-on-non-INPC
        // sources; the VM raises PropertyChanged with empty PropertyName on
        // every observer tick and Bindings.Update() re-runs them all.
        // Without this, IsEnabled / Visibility / text properties would freeze
        // at their initial values (same pattern as DnsConfigView / VpsConfigView).
        // We also reconcile the elapsed-counter timer here — see
        // ReconcileElapsedTimer for why the tick lives in the View.
        ViewModel.PropertyChanged += OnViewModelPropertyChanged;

        // Reconcile once at construction in case we already entered a
        // Running snapshot before the page rendered.
        ReconcileElapsedTimer();
    }

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        // The VM raises PropertyChanged with empty string on every observer
        // tick (see OnboardingViewModel constructor). Refresh x:Bind targets
        // and re-evaluate timer state on every notification — both are
        // cheap idempotent checks.
        Bindings.Update();
        ReconcileElapsedTimer();
    }

    private void ReconcileElapsedTimer()
    {
        var overall = ViewModel.ProvisioningSnapshot.@overall;
        if (overall == _lastOverall) return;
        _lastOverall = overall;

        if (overall is OverallStatus.Running)
        {
            _elapsedTimer ??= new DispatcherTimer { Interval = System.TimeSpan.FromSeconds(1) };
            _elapsedTimer.Tick -= OnElapsedTick;
            _elapsedTimer.Tick += OnElapsedTick;
            _elapsedTimer.Start();
        }
        else
        {
            _elapsedTimer?.Stop();
        }
    }

    private void OnElapsedTick(object? sender, object e)
    {
        ViewModel.TickElapsed();
    }

    /// <summary>XAML bool→Visibility helper. FaunaApp.Core (net10.0) can't
    /// reference Microsoft.UI.Xaml.Visibility, so per-view static helpers are
    /// the established pattern — see DnsConfigView.BoolToVisibility and
    /// VpsConfigView.BoolToVisibility.</summary>
    public static Visibility BoolToVisibility(bool value)
        => value ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>Hide the Continue-blocked reason when there's nothing to explain
    /// (rule-5 render lift — the reason text is "" iff Continue is enabled).</summary>
    public static Visibility TextToVisibility(string value)
        => value.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
}
