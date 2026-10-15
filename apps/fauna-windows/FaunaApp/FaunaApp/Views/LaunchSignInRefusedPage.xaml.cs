using System;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// Launch-screen "the saved nest no longer signs you in" surface, shown when
/// the launch machine's <c>Offline { transient: false }</c> snapshot carries
/// the <c>sign_in_refused</c> side channel (a claimed nest answered the silent
/// challenge <c>not_registered</c> for a previously-signed-in identity — its
/// admin suspended or removed the account, deliberately unsaid). The arm in
/// <c>App.xaml.cs:DispatchLaunchSnapshotAsync</c> is checked AHEAD of the
/// generic <c>Offline</c> split and navigates here.
///
/// Two CTAs, unlike <see cref="LaunchNeedsUpdatePage"/>: Retry
/// (<c>retry_silent_challenge</c>, the way back in once the admin restores the
/// account) and "Use a different nest". The sentence renders in the dedicated
/// <c>launch-sign-in-refused-notice</c>.
///
/// Goal docs: <c>docs/goal/behavior/onboarding.md</c> § App-launch routing
/// (the previously-signed-in row); ui.yaml page <c>launch_sign_in_refused</c>.
/// Mirrors linux <c>views/launch.rs</c>.
/// </summary>
public sealed partial class LaunchSignInRefusedPage : Page
{
    private LaunchSignInRefusedContext? _context;

    public LaunchSignInRefusedPage()
    {
        InitializeComponent();
        MessageText.Text = S.Get("onboarding/launch/sign_in_refused");
        RetryButton.Content = S.Get("common/retry");
        FallthroughButton.Content = S.Get("onboarding/launch/use_different_nest");
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is LaunchSignInRefusedContext ctx)
        {
            _context = ctx;
            // The machine's own localized sentence wins; the static copy above
            // only covers a snapshot that carried none.
            if (!string.IsNullOrEmpty(ctx.Message))
                MessageText.Text = ctx.Message;
        }
    }

    private async void OnRetryClick(object sender, RoutedEventArgs e)
    {
        if (_context is null) return;
        RetryButton.IsEnabled = false;
        FallthroughButton.IsEnabled = false;
        try { await _context.Retry(); }
        finally
        {
            // A repeat refusal re-navigates to a fresh page; if we are still
            // the visible page (e.g. the retry threw), re-arm the buttons.
            RetryButton.IsEnabled = true;
            FallthroughButton.IsEnabled = true;
        }
    }

    private void OnFallthroughClick(object sender, RoutedEventArgs e)
    {
        _context?.Fallthrough();
    }
}

/// <summary>
/// Navigation parameter for <see cref="LaunchSignInRefusedPage"/>: the
/// machine's localized sentence (<c>LaunchSnapshot.last_error</c>), the
/// <see cref="Retry"/> callback that re-runs the silent challenge and
/// re-dispatches the fresh snapshot, and the <see cref="Fallthrough"/>
/// callback that drops to the wizard with the identity pre-seeded.
/// </summary>
public sealed record LaunchSignInRefusedContext(
    string Message,
    Func<Task> Retry,
    Action Fallthrough);
