using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using uniffi.fauna_launch_machine;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// The <c>launch_account_index_unreadable</c> surface — the saved account
/// index at <c>fauna/index</c> is present and this build cannot use it
/// (<c>docs/goal/architecture/version-compatibility.md</c> § 5 item 9;
/// <c>docs/goal/behavior/onboarding.md</c> § App-launch routing — the row
/// checked before every other). Terminal and non-retryable: no retry
/// reparses a blob, and there is no "use a different nest" fallthrough — the
/// nest is not the problem. The <c>Offline</c> arm of
/// <c>App.xaml.cs:DispatchLaunchSnapshotAsync</c>, checked ahead of the
/// ordinary Offline arms, builds a
/// <see cref="LaunchAccountIndexUnreadableContext"/> and navigates here.
///
/// Two verdicts, told apart by <see cref="AccountIndexRefusal"/>:
/// <list type="bullet">
/// <item><c>NewerBuild</c>: the accounts are intact and an update restores
/// them, so NOTHING else is offered — a start-over here would destroy
/// exactly what an update would have restored.</item>
/// <item><c>Malformed</c>: updating cannot help, so the documented
/// client-side floor (<c>long-term-store.md</c> § Cleanup contract) is
/// reachable, but only through a confirm that states the residual
/// first.</item>
/// </list>
///
/// Reference implementation: tui <c>apps/fauna-tui/src/launch.rs</c>
/// <c>LaunchSurface::AccountIndexUnreadable</c> (<c>route()</c>'s element
/// list, <c>reveal_start_over</c>/<c>start_over</c>); the shared FaunaKit
/// <c>LaunchAccountIndexUnreadableView</c> apple's two targets render.
/// </summary>
public sealed partial class LaunchAccountIndexUnreadablePage : Page
{
    private LaunchAccountIndexUnreadableContext? _context;

    public LaunchAccountIndexUnreadablePage()
    {
        InitializeComponent();
        ResetButton.Content = S.Get("onboarding/launch/index_malformed_reset");
        ResetConfirmButton.Content = S.Get("onboarding/launch/index_malformed_reset_confirm");
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is not LaunchAccountIndexUnreadableContext ctx) return;
        _context = ctx;
        switch (ctx.Refusal)
        {
            case AccountIndexRefusal.NewerBuild:
                // The accounts are intact and an update restores them —
                // nothing else is offered, so both buttons stay collapsed.
                MessageText.Text = S.Get("onboarding/launch/index_newer_build");
                break;
            case AccountIndexRefusal.Malformed:
                MessageText.Text = S.Get("onboarding/launch/index_malformed");
                ResetButton.Visibility = Visibility.Visible;
                break;
        }
    }

    /// <summary>
    /// "Start over on this device": reveal the confirm, which states the
    /// residual before anything is erased. Only reachable from the malformed
    /// verdict — <see cref="ResetButton"/> stays collapsed on the version
    /// verdict, so this can never become a second way to reach the reset
    /// from it.
    /// </summary>
    private void OnResetClick(object sender, RoutedEventArgs e)
    {
        MessageText.Text = S.Get("onboarding/launch/index_malformed_reset_residual");
        ResetButton.Visibility = Visibility.Collapsed;
        ResetConfirmButton.Visibility = Visibility.Visible;
    }

    private void OnResetConfirmClick(object sender, RoutedEventArgs e)
    {
        // Ask BEFORE the erase, same as sign-out's gate (account-scoping.md §
        // Concurrent instances → *An erase refuses while a sibling serves the
        // account*): OnConfirmStartOver's own first call,
        // App.ClearCredentialNamespace, releases actor-scoped state before it
        // erases, so the gate has to sit here, at the confirm click. No new
        // element — the refusal paints on this page's own verdict text (mirrors
        // linux's launch screen, which paints the same refusal onto its
        // per-phase warning label rather than a separate error-message).
        using var registry = Services.CredentialStore.Registry();
        var blocked = FaunaFfiMethods.StartOverBlocked(registry, AccountStateDir.Base, null, null);
        if (blocked is not null)
        {
            MessageText.Text = S.Resolve(blocked.line);
            return;
        }
        _context?.OnConfirmStartOver();
    }
}

/// <summary>
/// Navigation parameter for <see cref="LaunchAccountIndexUnreadablePage"/>.
/// <see cref="OnConfirmStartOver"/> is the documented client-side floor
/// (<c>long-term-store.md</c> § Cleanup contract) — the app's existing
/// factory-reset seam (<c>App.ClearCredentialNamespace</c>, the same one
/// sign-out and the test agent's <c>reset</c>/<c>logout</c> arms use), never a
/// second clearing path — landing on fresh onboarding at
/// <c>identity_choice</c>.
///
/// <c>internal</c>, not <c>public</c> — forced, not chosen:
/// <c>AccountIndexRefusal</c> is <c>uniffi-bindgen-cs</c>-generated and every
/// such type is <c>internal</c>, so a <c>public</c> member naming one is
/// CS0051 (the constructor twin of <c>AccountStateDir.EraseAll</c>'s CS0050;
/// see that method's own remark).
/// </summary>
internal sealed record LaunchAccountIndexUnreadableContext(
    AccountIndexRefusal Refusal,
    System.Action OnConfirmStartOver);
