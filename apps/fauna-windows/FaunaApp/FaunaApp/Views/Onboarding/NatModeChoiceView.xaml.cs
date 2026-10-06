using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;
using NodeMode = uniffi.fauna_core.NodeMode;

namespace FaunaApp.Views.Onboarding;

/// <summary>
/// Stage 3b-bis of handle-first onboarding: the NAT-mode choice for a
/// freshly claimed nest — the single, terminal admin-path setup step
/// (no-modes, ratified 2026-07-12). Mirrors
/// <c>apps/fauna-linux/src/views/onboarding/nat_mode_choice.rs</c> and
/// apple's <c>MacNatModeChoiceView</c>/<c>NatModeChoiceView</c>.
///
/// The two RadioButtons are synced from the snapshot in
/// <see cref="SyncRadios"/> rather than two-way x:Bind, guarded by
/// <see cref="_syncingRadios"/> so the programmatic sync doesn't re-fire
/// <see cref="OnboardingViewModel.SelectNatMode"/> (same shape as
/// <see cref="VpsConfigView"/>'s mail-mode checkbox guard).
/// </summary>
public sealed partial class NatModeChoiceView : UserControl
{
    internal OnboardingViewModel ViewModel { get; private set; } = null!;

    private bool _syncingRadios;

    public NatModeChoiceView()
    {
        InitializeComponent();
    }

    internal NatModeChoiceView(OnboardingViewModel vm) : this()
    {
        ViewModel = vm;
        DataContext = vm;
        vm.PropertyChanged += (_, _) => { Bindings.Update(); SyncRadios(); };
        SyncRadios();
    }

    /// <summary>
    /// Re-sync radio selection from the snapshot. The pre-selection is the
    /// nest's seeded <c>node_mode</c> (refined private-ward for a
    /// private-network target), so the first render reflects it without a
    /// click — matching the "confirm-only in the common case" design.
    /// </summary>
    private void SyncRadios()
    {
        _syncingRadios = true;
        try
        {
            if (PublicRadio.IsChecked != ViewModel.NatModePublicSelected)
                PublicRadio.IsChecked = ViewModel.NatModePublicSelected;
            if (PrivateRadio.IsChecked != ViewModel.NatModePrivateSelected)
                PrivateRadio.IsChecked = ViewModel.NatModePrivateSelected;
        }
        finally { _syncingRadios = false; }
    }

    private void PublicRadio_Checked(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (_syncingRadios) return;
        ViewModel.SelectNatMode(NodeMode.Public);
    }

    private void PrivateRadio_Checked(object sender, Microsoft.UI.Xaml.RoutedEventArgs e)
    {
        if (_syncingRadios) return;
        ViewModel.SelectNatMode(NodeMode.Private);
    }
}
