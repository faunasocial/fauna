using System;
using System.ComponentModel;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_onboarding_machine;

namespace FaunaApp.Views.Onboarding;

/// <summary>
/// Stage 3 of handle-first onboarding (Wave 3 / Spec 1): admin-flow
/// invite-request submit + status display + recheck on the top row, and
/// an independent out-of-band code path on the bottom row. Both rows
/// converge on the shared Continue button at the bottom — Continue's
/// behavior depends on snapshot state per the target-state doc:
///   - state == Approved or oob_code_state == Valid → redeem_invite()
///   - state == PendingReview                       → (no Continue; the page polls)
///
/// Replaces the legacy invite_request + invite_request_pending pair: the
/// machine now folds the pending-review state into the same snapshot,
/// surfaced by the optional `invite-request-recheck-button`.
/// </summary>
public sealed partial class InviteRequestView : UserControl
{
    internal OnboardingViewModel ViewModel { get; private set; } = null!;

    // "Poll is the channel — structurally, not provisionally" (onboarding.md
    // § The pending-invite surface): an unregistered actor has no bearer, so
    // no notification plane can reach it, and the client must ask. Mirrors
    // apps/fauna-linux/src/views/onboarding/mod.rs::start_pending_invite_poll
    // and web's +page.svelte pendingInviteReview $effect — same page-lifetime
    // timer + state-gated tick shape as AwaitingManualDnsView's DNS poll.
    private DispatcherTimer? _pollTimer;
    private bool _isPolling;

    public InviteRequestView()
    {
        InitializeComponent();
    }

    internal InviteRequestView(OnboardingViewModel vm) : this()
    {
        ViewModel = vm;
        DataContext = vm;
        // PropertyChanged with empty PropertyName fires on every observer
        // tick from the OnboardingMachine; Bindings.Update() re-runs all
        // x:Bind expressions so InviteRequestStatusText / continue-enabled
        // / recheck-visible / oob status all refresh as the snapshot evolves.
        vm.PropertyChanged += OnViewModelPropertyChanged;
        // Reconcile once at construction in case relaunch hydration already
        // seeded PendingReview before this view rendered.
        ReconcilePollTimer();
    }

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        Bindings.Update();
        ReconcilePollTimer();
    }

    /// <summary>
    /// Arms the pending-invite poll for as long as the page reads
    /// `PendingReview` — `InviteRequestRecheckVisible` is exactly that state
    /// test (shared Rust's `recheck_visible = matches!(state, PendingReview)`),
    /// so it doubles as both the button's visibility and this timer's guard;
    /// no separate "recheck in flight" check is needed since the state leaves
    /// `PendingReview` for `Rechecking` for the duration of a call. Fires the
    /// FIRST poll immediately on entry — a relaunch hydrates straight into
    /// `PendingReview` and should not stare at a stale page for a full
    /// interval — then ticks at `InviteRecheckPollMs()`.
    /// </summary>
    private void ReconcilePollTimer()
    {
        if (!ViewModel.InviteRequestRecheckVisible)
        {
            _pollTimer?.Stop();
            _isPolling = false;
            return;
        }
        if (!_isPolling)
        {
            _isPolling = true;
            ViewModel.RecheckInviteStatusCommand.Execute(null);
        }
        _pollTimer ??= new DispatcherTimer
        {
            Interval = TimeSpan.FromMilliseconds(FaunaOnboardingMachineMethods.InviteRecheckPollMs())
        };
        _pollTimer.Tick -= OnPollTick;
        _pollTimer.Tick += OnPollTick;
        _pollTimer.Start();
    }

    private void OnPollTick(object? sender, object e)
    {
        if (ViewModel.InviteRequestRecheckVisible)
            ViewModel.RecheckInviteStatusCommand.Execute(null);
    }

    public static Visibility BoolToVisibility(bool value)
        => value ? Visibility.Visible : Visibility.Collapsed;
}
