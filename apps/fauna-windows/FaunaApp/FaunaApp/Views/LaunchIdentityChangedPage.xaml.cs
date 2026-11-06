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
///
/// <para><b>Fork variant</b> (<see cref="LaunchIdentityChangedContext.IsFork"/>, from
/// the shared <c>LaunchSnapshot.identity_fork</c>; <c>box-recovery.md</c> § Client
/// acceptance): rotation-chain fork evidence paints <c>nest-identity-fork-warning</c>
/// — the shared <c>identity_fork_warning</c> sentence naming both heads — and the
/// fallthrough ONLY. No trust button: no path re-trusts a forked chain. The twin of
/// tui's <c>LaunchSurface::IdentityForked</c> and apple's <c>isFork</c> arm.</para>
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
        {
            _context = ctx;
            ShowVariant(ctx);
        }
    }

    /// <summary>The ordinary warning + trust button, or — on fork evidence — the fork
    /// sentence alone. The shared string's two named heads are filled by name
    /// (<c>Strings.Resolve</c>), so a locale that orders them differently still names
    /// each one right.</summary>
    private void ShowVariant(LaunchIdentityChangedContext ctx)
    {
        if (ctx.IsFork)
        {
            // A fork always carries a seen head (only the Withdrawn arm leaves it
            // null), so the empty fallback is unreachable — and still never trustable.
            ForkWarningText.Text = S.Resolve(new uniffi.fauna_core.LocalizedText(
                "onboarding.launch.identity_fork_warning",
                new System.Collections.Generic.Dictionary<string, string>
                {
                    ["pinned"] = ctx.PinnedHex,
                    ["seen"] = ctx.SeenHex ?? "",
                }));
            ForkWarningText.Visibility = Visibility.Visible;
            WarningText.Visibility = Visibility.Collapsed;
            TrustButton.Visibility = Visibility.Collapsed;
        }
        else
        {
            ForkWarningText.Visibility = Visibility.Collapsed;
            WarningText.Visibility = Visibility.Visible;
            TrustButton.Visibility = Visibility.Visible;
        }
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
/// <see cref="IsFork"/> is the snapshot's <c>identity_fork</c> bit; the two heads are
/// the <c>IdentityChanged</c> phase's own <c>pinned_hex</c>/<c>seen_hex</c> (the bit
/// carries no heads of its own).
/// </summary>
public sealed record LaunchIdentityChangedContext(
    Func<Task> Trust,
    Action Fallthrough,
    bool IsFork = false,
    string PinnedHex = "",
    string? SeenHex = null);
