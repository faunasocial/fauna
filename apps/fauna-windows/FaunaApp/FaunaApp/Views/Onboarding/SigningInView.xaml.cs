using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Onboarding;

/// <summary>
/// Transient placeholder shown for the gap between the wizard reaching
/// <c>Done</c>/<c>LoggedIn</c> and <c>App.StartMainAppAsync</c> finishing its
/// (WS-RPC connect + MLS session build) handoff and navigating to
/// <see cref="MainPage"/> — typically 1-3s. Mirrors linux's identical
/// spinner + "Signing you in…" sub-state (views/launch.rs); no test ID there
/// either.
/// </summary>
public sealed partial class SigningInView : UserControl
{
    public SigningInView()
    {
        InitializeComponent();
    }

    internal SigningInView(OnboardingViewModel vm) : this()
    {
        DataContext = vm;
    }
}
