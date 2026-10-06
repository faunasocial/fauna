using System;
using System.Collections.Generic;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// The launch-collision chooser (<c>docs/goal/architecture/apps/account-scoping.md</c>
/// § Concurrent instances → "the colliding instance's surface"; ui.yaml page
/// <c>launch_instance_chooser</c>, platforms <c>[windows, linux, tui]</c>).
///
/// <para>A <b>plain interactive</b> launch that finds the account it would open
/// already served by a live instance renders this instead of authenticating —
/// "already running as X; launch as…". Three ways forward, and only three:</para>
///
/// <list type="bullet">
/// <item><b>Pick an account</b> → this process becomes that account's bound
/// instance (<c>bind_account</c> + the bound launch seam) and re-enters the
/// ordinary launch. <b>No third process</b> — spawning a sibling is the running
/// instance's own affordance (<c>account-open-new-instance-button</c>), not this
/// one's.</item>
/// <item><b>Switch to the open window</b> → raise the running instance and exit.
/// This is the raise-on-relaunch UX the single-instance mutex used to give, now
/// reached explicitly because a collided launch deliberately skips
/// <c>SingleInstanceManager.TryClaim</c>.</item>
/// <item><b>Log in as a new user</b> → <i>forward</i> an add-account intent to the
/// running instance and exit. The colliding process never runs the wizard: the
/// onboarding scratchpad belongs to the primary (the same ownership rule as the
/// bound-wizard refusal).</item>
/// </list>
///
/// <para><b>A <c>FAUNA_BOUND_ACCOUNT</c> launch never reaches this page.</b> A wired
/// binding that collides stays terminally refused — the chooser is strictly a human
/// affordance, and wired IPC must be deterministic.</para>
///
/// <para><b>Focus-existing reaches a bound sibling</b> (account-scoping.md
/// § Concurrent instances → <i>The per-(OS login, account) raise channel</i>): it
/// targets the served account's own activation endpoint, which every serving
/// instance claims — plain and bound alike — rather than the app-wide name only a
/// plain instance owns. Its failure is no longer one state but two, and the page
/// distinguishes them: the sibling <i>exited</i> (no error — the launch simply
/// continues as a plain one) versus it is alive but <i>endpoint-less</i> (a tui
/// server, or a client from before this leg), which is surfaced on
/// <c>error-message</c>.</para>
///
/// <para>The add-account exit stays on the app-wide channel and can still fail
/// outright — the onboarding scratchpad belongs to the primary, so there is
/// deliberately no per-account endpoint for it to fall back on.</para>
/// </summary>
public sealed partial class LaunchInstanceChooserPage : Page
{
    private LaunchInstanceChooserContext? _context;

    public LaunchInstanceChooserPage()
    {
        InitializeComponent();
        TitleText.Text = S.Get("onboarding/instance_chooser/title");
        ChooseAccountText.Text = S.Get("onboarding/instance_chooser/choose_account");
        NoneAvailableText.Text = S.Get("onboarding/instance_chooser/none_available");
        FocusExistingButton.Content = S.Get("onboarding/instance_chooser/focus_existing");
        AddAccountButton.Content = S.Get("onboarding/instance_chooser/add_account");
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is not LaunchInstanceChooserContext ctx)
        {
            return;
        }
        _context = ctx;

        // `{account}` names the account the running instance serves, through the
        // same shared formatter the switcher uses — the collision must name an
        // account the user recognises.
        SubtitleText.Text = S.Format("onboarding/instance_chooser/subtitle", ctx.ServedLabel);

        ChoicesList.ItemsSource = ctx.Choices;
        var hasChoices = ctx.Choices.Count > 0;
        ChooseAccountText.Visibility = hasChoices ? Visibility.Visible : Visibility.Collapsed;
        // Every account already open somewhere is a legitimate outcome, not an
        // error: the explanation replaces the list, and the two exits still work.
        NoneAvailableText.Visibility = hasChoices ? Visibility.Collapsed : Visibility.Visible;
    }

    private void ShowError(string key)
    {
        ErrorBar.Message = S.Get(key);
        ErrorBar.Severity = InfoBarSeverity.Error;
        ErrorBar.IsOpen = true;
    }

    private void ClearError() => ErrorBar.IsOpen = false;

    /// <summary>
    /// Pick a row. The list was a <b>display-only</b> probe, so the account can have
    /// been taken between render and click — the pick therefore goes through the
    /// real gate and reports the loss instead of proceeding on a stale list.
    /// </summary>
    private void ChoiceClick(object sender, RoutedEventArgs e)
    {
        if (_context is null || sender is not Button { Tag: string actorId })
        {
            return;
        }
        ClearError();
        if (!_context.Pick(actorId))
        {
            ShowError("onboarding/instance_chooser/account_taken");
        }
        // On success the launch flow owns the process from here: it resumes at the
        // bound branch of App.OnLaunched and navigates away from this page itself.
    }

    /// <summary>
    /// Raise the instance serving the collided account, over its <b>per-account</b>
    /// activation endpoint. Three outcomes, and the two failures mean opposite
    /// things to the user — which is why this is not a <c>bool</c>
    /// (<c>account-scoping.md</c> § Concurrent instances → <i>The per-(OS login,
    /// account) raise channel</i>).
    /// </summary>
    private void FocusExistingClick(object sender, RoutedEventArgs e)
    {
        if (_context is null) return;
        ClearError();
        switch (_context.FocusExisting())
        {
            case uniffi.fauna_ffi.FfiFocusExistingOutcome.Raised:
                // The process exits inside the callback — nothing follows.
                break;

            case uniffi.fauna_ffi.FfiFocusExistingOutcome.NoLongerServed:
                // The serving instance exited between the collision and the click,
                // so there is no error to report: the launch flow resumes as an
                // ordinary plain launch and navigates away from this page itself.
                break;

            case uniffi.fauna_ffi.FfiFocusExistingOutcome.StillServedNoChannel:
                // Alive, but owns no reachable endpoint (a tui server, which claims
                // none by design, or one from before the per-account raise channel).
                // Say so rather than exit into nothing. Same shared string linux
                // shows for this case — `no_running_instance` was repurposed to mean
                // exactly "still served, couldn't switch to it" (the NoLongerServed
                // case is no longer an error: it just continues as a plain launch).
                ShowError("onboarding/instance_chooser/no_running_instance");
                break;
        }
    }

    private void AddAccountClick(object sender, RoutedEventArgs e)
    {
        if (_context is null) return;
        ClearError();
        if (!_context.AddAccount())
        {
            ShowError("onboarding/instance_chooser/no_running_instance");
        }
    }
}

/// <summary>
/// Navigation parameter for <see cref="LaunchInstanceChooserPage"/> — the same
/// shape as <see cref="LaunchRetryContext"/>: the launch flow constructs the
/// callbacks so the page needs to know nothing about App internals.
///
/// <para>Every callback reports its outcome rather than throwing or silently
/// exiting, because all three can legitimately fail and the page owns the
/// user-visible answer (<c>error-message</c>).</para>
/// </summary>
/// <param name="ServedLabel">Display label of the already-served account.</param>
/// <param name="Choices">The not-currently-served accounts, registry order.</param>
/// <param name="Pick">
/// Bind this process to the chosen account. <c>false</c> ⇒ the gate refused (the
/// account was taken between render and click, or is otherwise unlaunchable).
/// </param>
/// <param name="FocusExisting">
/// Raise the instance serving the collided account over its per-account activation
/// endpoint. Tri-state rather than <c>bool</c>: an unreachable endpoint means
/// either "it exited" (proceed) or "it's alive and unreachable" (report), and
/// collapsing the two turns the first into a dead end.
/// </param>
/// <param name="AddAccount">
/// Forward an add-account intent to the running instance and exit. <c>false</c> ⇒
/// nothing was listening. Stays on the <b>app-wide</b> channel — the wizard belongs
/// to the primary — so it has no per-account fallback by design.
/// </param>
internal sealed record LaunchInstanceChooserContext(
    string ServedLabel,
    IReadOnlyList<LaunchInstanceChoice> Choices,
    Func<string, bool> Pick,
    Func<uniffi.fauna_ffi.FfiFocusExistingOutcome> FocusExisting,
    Func<bool> AddAccount);
