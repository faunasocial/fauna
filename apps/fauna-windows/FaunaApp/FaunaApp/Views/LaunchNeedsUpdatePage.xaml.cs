using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// Launch-screen NON-retry "update required" surface, shown when the saved
/// nest authoritatively reports it is outdated (<c>fauna.nest.outdated</c> →
/// the launch machine's <c>Offline { transient: false }</c>, carrying the
/// localized message in <c>LaunchSnapshot.last_error</c>). The
/// <c>Offline { transient }</c> arm of
/// <c>App.xaml.cs:DispatchLaunchSnapshotAsync</c> builds a
/// <see cref="LaunchNeedsUpdateContext"/> and navigates here.
///
/// Distinct from <see cref="LaunchRetryPage"/>: retrying the same outdated
/// nest is futile (the nest told us authoritatively it can't serve this
/// client version), so there is NO Retry CTA — only "Use a different nest".
/// The localized message renders in the canonical <c>error-message</c>
/// element (matching web/linux/android), not a static string.
///
/// Goal docs: <c>docs/goal/architecture/version-compatibility.md</c> Dim 4 /
/// <c>docs/goal/behavior/onboarding.md</c> § App-launch routing
/// (version-mismatch row). Mirrors linux
/// <c>views/launch.rs::LaunchPhase::NeedsUpdate</c> + android
/// <c>LaunchNeedsUpdateScreen.kt</c>. Cross-app element ids:
/// <c>error-message</c>, <c>launch-fallthrough-button</c>.
/// </summary>
public sealed partial class LaunchNeedsUpdatePage : Page
{
    private LaunchNeedsUpdateContext? _context;

    public LaunchNeedsUpdatePage()
    {
        InitializeComponent();
        FallthroughButton.Content = S.Get("onboarding/launch/use_different_nest");
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is LaunchNeedsUpdateContext ctx)
        {
            _context = ctx;
            // The localized outdated-nest message arrives on the nav parameter
            // (not a static string like LaunchRetryPage's transient copy).
            MessageText.Text = ctx.Message;
        }
    }

    private void OnFallthroughClick(object sender, RoutedEventArgs e)
    {
        _context?.Fallthrough();
    }
}

/// <summary>
/// Navigation parameter for <see cref="LaunchNeedsUpdatePage"/>. Carries the
/// localized outdated-nest message (from <c>LaunchSnapshot.last_error</c>) and
/// the <see cref="Fallthrough"/> callback that drops to the wizard with the
/// identity pre-seeded (so the user can point at a different nest).
/// </summary>
public sealed record LaunchNeedsUpdateContext(
    string Message,
    Action Fallthrough);
