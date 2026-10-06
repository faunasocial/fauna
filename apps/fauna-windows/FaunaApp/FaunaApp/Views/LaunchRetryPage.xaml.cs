using System;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// Launch-screen surface shown when the <c>Offline { transient }</c> arm
/// of <c>App.xaml.cs:DispatchLaunchSnapshotAsync</c> can't reach the saved
/// nest (builds a <see cref="LaunchRetryContext"/> and navigates here).
/// Per docs/goal/behavior/onboarding.md §"App-launch routing": "Show a
/// transient retry indicator on the launch screen rather than dropping
/// to the wizard. The user can wait, hit retry, or — if they want to
/// use a different nest — fall back to the wizard at handle_entry."
///
/// Two CTAs:
///   - Retry → re-runs the silent challenge against the same secret +
///     nest_url. The retry callback is supplied by the launch flow (see
///     LaunchRetryContext below) so this page doesn't need to know
///     about App internals.
///   - Use a different nest → drops into the OnboardingPage with the
///     identity pre-seeded so the user lands at handle_entry.
///
/// Cross-app element ids match the web reference at
/// apps/fauna-web/src/routes/onboarding/+page.svelte:781-799:
/// `launch-transient-error`, `launch-retry-button`,
/// `launch-fallthrough-button`.
/// </summary>
public sealed partial class LaunchRetryPage : Page
{
    private LaunchRetryContext? _context;
    // The launch-time reachable-nest box list (box-recovery.md § Recovery UI
    // step 4), stashed by ShowRecoverButton so OnRecoverClick can hand it to
    // LaunchRetryContext.Recover without a second read.
    private string[] _recoverableBoxes = Array.Empty<string>();

    public LaunchRetryPage()
    {
        InitializeComponent();
        MessageText.Text = S.Get("onboarding/launch/transient_error");
        RetryButton.Content = S.Get("common/retry");
        FallthroughButton.Content = S.Get("onboarding/launch/use_different_nest");
        RecoverButton.Content = S.Get("onboarding/launch/recover_lost_box");
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is LaunchRetryContext ctx)
            _context = ctx;
    }

    /// <summary>
    /// Reveal <c>launch-recover-button</c> once the launch-time reachable-nest
    /// box-list read resolves with >= 1 custodied box; an empty read (the
    /// common case — a truly dead saved nest) leaves it collapsed. Called
    /// best-effort from App.xaml.cs after this page paints, so the button
    /// only appears once the read completes (never blocks the retry surface).
    /// </summary>
    internal void ShowRecoverButton(string[] boxes)
    {
        _recoverableBoxes = boxes;
        RecoverButton.Visibility = boxes.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private async void OnRetryClick(object sender, RoutedEventArgs e)
    {
        if (_context is null) return;
        // Disable both buttons while the retry is in flight; the page
        // navigates away on success or comes back here on a repeat
        // failure (then the OnNavigatedTo above re-arms us).
        RetryButton.IsEnabled = false;
        FallthroughButton.IsEnabled = false;
        try { await _context.Retry(); }
        finally
        {
            // If we're still on this page (repeat failure), re-enable.
            RetryButton.IsEnabled = true;
            FallthroughButton.IsEnabled = true;
        }
    }

    private void OnFallthroughClick(object sender, RoutedEventArgs e)
    {
        _context?.Fallthrough();
    }

    /// <summary>box-recovery.md § Recovery UI (step 4): hand the launch-time
    /// box list to <see cref="LaunchRetryContext.Recover"/>, which seeds the
    /// wizard for recovery and drops straight into nest_recovery.</summary>
    private void OnRecoverClick(object sender, RoutedEventArgs e)
    {
        _context?.Recover(_recoverableBoxes);
    }
}

/// <summary>
/// Navigation parameter for <see cref="LaunchRetryPage"/>. The launch
/// flow constructs all three callbacks: <see cref="Retry"/> re-runs the
/// silent challenge, <see cref="Fallthrough"/> drops to the wizard
/// with the identity pre-seeded, <see cref="Recover"/> drops to the
/// wizard seeded for box recovery (box-recovery.md § Recovery UI step 4,
/// surviving-device entry) — only reachable once
/// <see cref="LaunchRetryPage.ShowRecoverButton"/> has revealed the button.
/// </summary>
public sealed record LaunchRetryContext(
    Func<Task> Retry,
    Action Fallthrough,
    Action<string[]> Recover);
