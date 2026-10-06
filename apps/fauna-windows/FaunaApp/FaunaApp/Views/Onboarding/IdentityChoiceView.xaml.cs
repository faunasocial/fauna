using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Onboarding;

public sealed partial class IdentityChoiceView : UserControl
{
    private readonly OnboardingViewModel? _vm;

    public IdentityChoiceView()
    {
        InitializeComponent();
    }

    internal IdentityChoiceView(OnboardingViewModel vm) : this()
    {
        DataContext = vm;
        _vm = vm;
        // IsAppendMode is fixed for the lifetime of this wizard instance (set once
        // at OnboardingViewModel construction from App.IsAppendingAccount), so a
        // one-time read here is sufficient — no PropertyChanged subscription needed.
        CancelButton.Visibility = vm.IsAppendMode ? Visibility.Visible : Visibility.Collapsed;
        // The residue view repaints from the view-model on every change: the
        // sign-out hands the residue over AFTER this view is constructed
        // (OnboardingPage.OnNavigatedTo), and Remove Again replaces or clears it.
        // No unsubscribe — the view lives exactly as long as its page and VM.
        vm.PropertyChanged += (_, _) => PaintSignOutResidue();
        PaintSignOutResidue();
    }

    // sign-out-residue: present exactly while the residue owes work
    // (account-scoping.md § Erasure follows scope → the residue surface).
    // Visibility is set here rather than bound because a classic {Binding} has
    // no bool→Visibility conversion.
    private void PaintSignOutResidue()
    {
        var line = _vm?.SignOutResidueMessage;
        if (line is null)
        {
            SignOutResidueView.Visibility = Visibility.Collapsed;
            return;
        }
        if (SignOutResidueMessage.Text != line) SignOutResidueMessage.Text = line;
        SignOutResidueView.Visibility = Visibility.Visible;
    }

    // Remove Again. The view-model serializes presses and runs the re-sweep off
    // the UI thread; the PropertyChanged subscription above paints the outcome.
    private async void OnRemoveAgainClick(object sender, RoutedEventArgs e)
    {
        if (_vm is { } vm) await vm.RetrySignOutResidueAsync();
    }

    // Abandon the in-progress append and restore the running session's UI. The
    // append's confirm step wrote nothing, and its terminals (LoggedIn, the
    // pending-invite submit, the AwaitingManualDns exit) each leave append mode as
    // they register, so none of them can precede this click — see
    // App.AbandonAddAccountHandler. (A provisioning run's pending-provision mint is
    // the one mid-run writer; its append-mode shape is an open cross-app question.)
    private void OnCancelClick(object sender, RoutedEventArgs e)
    {
        App.AbandonAddAccountHandler?.Invoke();
    }
}
