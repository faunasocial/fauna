using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Onboarding;

public sealed partial class ClaimCodeView : UserControl
{
    internal OnboardingViewModel ViewModel { get; private set; } = null!;

    public ClaimCodeView()
    {
        InitializeComponent();
    }

    internal ClaimCodeView(OnboardingViewModel vm) : this()
    {
        ViewModel = vm;
        DataContext = vm;
        vm.PropertyChanged += (_, _) => { ApplyPrefill(); Bindings.Update(); };
        ApplyPrefill();
    }

    /// <summary>
    /// Factory-reset re-onboard: the human never sees the claim code the reset
    /// returned, so seed the (empty) input from the machine's prefill or the
    /// admin is stranded. Only fill an empty input so we never fight a user edit
    /// on a later tick. Mirrors linux's claim_code.rs prefill. Per
    /// <c>docs/goal/behavior/mail-bridge-lifecycle.md</c> § Factory reset.
    /// </summary>
    private void ApplyPrefill()
    {
        if (!string.IsNullOrEmpty(ViewModel.ClaimCode)) return;
        var code = ViewModel.ClaimCodePrefill;
        if (!string.IsNullOrEmpty(code)) ViewModel.ClaimCode = code;
    }
}
