using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Onboarding;

/// <summary>
/// Stage 3b-ter: the one-tap "trust this box" offer (`onboarding.md` §
/// 3b-ter) — the windows leg (tui led 2026-08-14; linux/android/web/macOS/iOS
/// all landed since). Mirrors
/// <c>apps/fauna-linux/src/views/onboarding/trust_prompt.rs</c> and apple's
/// <c>TrustPromptView</c>.
///
/// Pure render: both commands (<see cref="OnboardingViewModel.GrantDefaultTrustCommand"/>
/// / <see cref="OnboardingViewModel.SkipTrustPromptCommand"/>) latch the answer
/// on the machine and return — see the "Trust prompt" section in
/// <c>OnboardingViewModel</c> for why they must not, and cannot safely, drive
/// the wizard's conclusion from inside the click.
/// </summary>
public sealed partial class TrustPromptView : UserControl
{
    internal OnboardingViewModel ViewModel { get; private set; } = null!;

    public TrustPromptView()
    {
        InitializeComponent();
    }

    internal TrustPromptView(OnboardingViewModel vm) : this()
    {
        ViewModel = vm;
        DataContext = vm;
    }
}
