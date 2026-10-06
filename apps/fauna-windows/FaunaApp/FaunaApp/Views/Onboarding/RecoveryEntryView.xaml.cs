using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Onboarding;

/// <summary>
/// The phrase-only identity restore (<c>onboarding.md</c> § 1 Identity) —
/// windows' leg of the page tui led. Mirrors <c>apps/fauna-linux/src/views/onboarding/recovery_entry.rs</c>.
///
/// <para>Pure render: the submit (<see cref="OnboardingViewModel.SubmitRecoveryEntryCommand"/>)
/// runs the shared pre-identity escrow restore and folds its outcome back into the
/// wizard — a restored seed lands on <c>handle_entry</c> committed exactly like an
/// import, <c>Superseded</c> routes to the import screen, and every refusal speaks
/// through the shared outcome → message table. <c>qr-camera-view</c> is scoped to
/// apps with a camera; a desktop paste is the windows path.</para>
/// </summary>
public sealed partial class RecoveryEntryView : UserControl
{
    public RecoveryEntryView()
    {
        InitializeComponent();
    }

    internal RecoveryEntryView(OnboardingViewModel vm) : this()
    {
        DataContext = vm;
    }
}
