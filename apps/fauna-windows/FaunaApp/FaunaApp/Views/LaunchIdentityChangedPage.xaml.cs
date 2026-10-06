using System;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// Launch-screen `launch_identity_changed` surface: the nest's pinned
/// deployment identity changed, or a pinned nest can no longer prove any
/// identity (security.md § Transport trust — the SSH
/// <c>known_hosts</c> model). Shown by the <c>LaunchPhase.IdentityChanged</c>
/// arm of <c>App.xaml.cs:DispatchLaunchSnapshotAsync</c> (builds a
/// <see cref="LaunchIdentityChangedContext"/> and navigates here).
///
/// Auto-entry is BLOCKED and there is deliberately NO Retry CTA — a retry
/// cannot change the verdict and must never silently re-pin. The only two
/// ways out are "trust this nest" (forget the pin, re-TOFU, re-challenge)
/// and "use a different nest" (wizard fallthrough, which must NOT forget
/// the pin — only the explicit trust button does that).
///
/// Reference implementations: android
/// <c>ui/screen/LaunchIdentityChangedScreen.kt</c> + <c>AppLaunchVM.navTargetFor</c>;
/// apple <c>FaunaKit/Sources/FaunaKit/Views/LaunchIdentityChangedView.swift</c>;
/// linux <c>views/launch.rs</c>; web <c>routes/onboarding/+page.svelte</c>.
/// Cross-app element ids: <c>nest-identity-changed-warning</c>,
/// <c>nest-identity-changed-trust-button</c>, <c>launch-fallthrough-button</c>.
/// </summary>
public sealed partial class LaunchIdentityChangedPage : Page
{
    private LaunchIdentityChangedContext? _context;

    public LaunchIdentityChangedPage()
    {
        InitializeComponent();
        WarningText.Text = S.Get("onboarding/launch/identity_changed_warning");
        TrustButton.Content = S.Get("onboarding/launch/identity_changed_trust");
        FallthroughButton.Content = S.Get("onboarding/launch/use_different_nest");
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is LaunchIdentityChangedContext ctx)
            _context = ctx;
    }

    private async void OnTrustClick(object sender, RoutedEventArgs e)
    {
        if (_context is null) return;
        // Disable both while the trust + re-TOFU + re-challenge round-trip is
        // in flight; the page navigates away on success or comes back here
        // (re-arming via OnNavigatedTo) if the nest still can't prove itself.
        TrustButton.IsEnabled = false;
        FallthroughButton.IsEnabled = false;
        try { await _context.Trust(); }
        finally
        {
            TrustButton.IsEnabled = true;
            FallthroughButton.IsEnabled = true;
        }
    }

    private void OnFallthroughClick(object sender, RoutedEventArgs e)
    {
        // Walking away from a nest we don't trust must NOT forget its pin —
        // only the explicit trust button does that (no silent re-pin).
        _context?.Fallthrough();
    }
}

/// <summary>
/// Navigation parameter for <see cref="LaunchIdentityChangedPage"/>.
/// <see cref="Trust"/> calls <c>LaunchMachine.TrustNestIdentity()</c> on the
/// SAME machine instance that produced the <c>IdentityChanged</c> verdict — it
/// reads the secret + nest_url off that state, so a fresh machine would
/// re-challenge from Boot and never reach the forget-the-pin branch, leaving
/// this button dead. <see cref="Fallthrough"/> drops to the wizard with the
/// identity pre-seeded, exactly like <see cref="LaunchRetryContext.Fallthrough"/>.
/// </summary>
public sealed record LaunchIdentityChangedContext(
    Func<Task> Trust,
    Action Fallthrough);
