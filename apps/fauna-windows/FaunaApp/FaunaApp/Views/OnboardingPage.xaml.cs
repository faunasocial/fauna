using System;
using System.ComponentModel;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.ViewModels;
using FaunaApp.Onboarding;
using FaunaApp.Services;
using FaunaApp.Views.Onboarding;
using uniffi.fauna_ffi;
using uniffi.fauna_onboarding_machine;

namespace FaunaApp.Views;

/// <summary>
/// Windows-specific shell for the onboarding wizard. Constructs the 12 per-stage
/// <see cref="UserControl"/> instances once, subscribes to the cross-platform
/// <see cref="OnboardingViewModel"/>'s property-changed notifications, and swaps
/// the active control into <c>StageContent</c> on every change.
///
/// Caching the views here (rather than in the VM) keeps the VM free of
/// Windows-specific UI types — see FaunaApp.Core/ViewModels/OnboardingViewModel.cs
/// for the cross-platform proxy.
///
/// The <c>ServiceClients</c> nav param's <c>SeedSecret</c> field is read in
/// <see cref="OnNavigatedTo"/> when the launch-flow detects a previously
/// confirmed identity without a completed nest_login (see App.xaml.cs case 2).
/// All other ServiceClients fields are unused here; the orchestrator
/// (Tasks 11+ of the onboarding plan) will pick them up once the post-login
/// handoff flow is wired.
/// </summary>
public sealed partial class OnboardingPage : Page
{
    internal OnboardingViewModel ViewModel { get; }

    /// <summary>The per-actor registry face the VM writes the wizard-exit resume
    /// slots through (<see cref="OnboardingViewModel"/>'s <c>_registry</c> field).
    /// Disposable view over the process-wide store — built once with the page,
    /// released in <see cref="OnNavigatedFrom"/>, same lifetime pattern as
    /// <c>SettingsAccountPage._switcherRegistry</c>.</summary>
    private readonly FfiAccountRegistry _registry;

    // Cached UserControl instances per stage. Constructed once; the platform
    // shell selects the active one. Lives here (not in the VM) so the VM
    // stays free of Windows-specific UI types.
    //
    // Wave 3 / target-state-doc: nest_select / nest_connect / nest_login /
    // invite_request_pending pages are deleted. Their routing collapses into
    // handle_entry's outcome-driven Continue and invite_request's snapshot.
    private readonly IdentityChoiceView         _identityChoiceView;
    private readonly IdentityCreatedView        _identityCreatedView;
    private readonly IdentityImportView         _identityImportView;
    private readonly RecoveryKitView            _recoveryKitView;
    private readonly RecoveryEntryView          _recoveryEntryView;
    private readonly HandleEntryView            _handleEntryView;
    private readonly DnsConfigView              _dnsConfigView;
    private readonly VpsConfigView              _vpsConfigView;
    private readonly DnsPostInstructionsView    _dnsPostInstructionsView;
    private readonly InviteRequestView          _inviteRequestView;
    private readonly ClaimCodeView              _claimCodeView;
    private readonly NatModeChoiceView          _natModeChoiceView;
    private readonly TrustPromptView            _trustPromptView;
    private readonly NestProvisioningView    _nestProvisioningView;
    private readonly NestRecoveryView           _nestRecoveryView;
    private readonly RecoverSelfhostedInstructionsView _recoverSelfhostedInstructionsView;
    // NOT keyed on OnboardingStep — see UpdateStageContent below.
    private readonly AwaitingManualDnsView      _awaitingManualDnsView;
    private readonly SigningInView              _signingInView;

    public OnboardingPage()
    {
        // The registry is the long-term identity store the launch flow
        // (App.xaml.cs) routes on and every wizard moment writes through. A
        // view over the one store, so opening one here is fine; the alternative
        // would be threading a single instance through OnNavigatedTo.
        _registry = Services.CredentialStore.Registry();
        // CredentialStore.Logical is also the install-device-secret store the VM
        // derives the named sync-device-row id from — safe because it's the SAME
        // store _registry sits on, and windows' one sign-out path (ClearAll())
        // never names install/device_secret (sync-agent-credentials.md §
        // Credential model, the RULED 2026-09-20 block; CredentialStore's own
        // doc comment).
        ViewModel = new OnboardingViewModel(
            new NotifyObserver(), _registry, App.IsAppendingAccount,
            Services.CredentialStore.Logical);
        InitializeComponent();

        _identityChoiceView         = new IdentityChoiceView(ViewModel);
        _identityCreatedView        = new IdentityCreatedView(ViewModel);
        _identityImportView         = new IdentityImportView(ViewModel);
        _recoveryKitView            = new RecoveryKitView(ViewModel);
        _recoveryEntryView          = new RecoveryEntryView(ViewModel);
        _handleEntryView            = new HandleEntryView(ViewModel);
        _dnsConfigView              = new DnsConfigView(ViewModel);
        _vpsConfigView              = new VpsConfigView(ViewModel);
        _dnsPostInstructionsView    = new DnsPostInstructionsView(ViewModel);
        _inviteRequestView          = new InviteRequestView(ViewModel);
        _claimCodeView              = new ClaimCodeView(ViewModel);
        _natModeChoiceView          = new NatModeChoiceView(ViewModel);
        _trustPromptView            = new TrustPromptView(ViewModel);
        _nestProvisioningView       = new NestProvisioningView(ViewModel);
        _nestRecoveryView           = new NestRecoveryView(ViewModel);
        _recoverSelfhostedInstructionsView = new RecoverSelfhostedInstructionsView(ViewModel);
        _awaitingManualDnsView      = new AwaitingManualDnsView(ViewModel);
        _signingInView              = new SigningInView(ViewModel);

        ViewModel.PropertyChanged += OnViewModelPropertyChanged;
        UpdateStageContent();
    }

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        UpdateStageContent();
        // Mirror the wizard's (shared-machine) error onto the app-wide
        // state-protocol error surface (App.CurrentErrorMessage → the
        // `messages.error` field of /app/state). Each stage's InfoBar binds
        // ViewModel.ErrorMessage for the visible UI, but only post-login pages'
        // RenderError write App.CurrentErrorMessage — the channel the e2e state
        // protocol and any other consumer reads. Without this, onboarding
        // validation/connection errors (e.g. secret_key_invalid) never reach
        // the state protocol on Windows, unlike linux which surfaces them.
        // Idempotent: re-reads the VM's current error on every observer tick;
        // runs on the UI thread (the NotifyObserver marshals to the dispatcher,
        // same as UpdateStageContent's StageContent mutation above).
        var error = string.IsNullOrEmpty(ViewModel.ErrorMessage) ? null : ViewModel.ErrorMessage;
        if (error != App.CurrentErrorMessage)
        {
            // Which way the shared error surface moved, and on which step — never
            // the text. The one line that separates "the machine still held the
            // error" from "the mirror missed a tick" when an e2e reads a stale one.
            FaunaApp.Core.Logs.ShellLog.Info("Onboarding",
                $"error surface {(error is null ? "cleared" : "set")} at step {ViewModel.CurrentStep}");
        }
        App.CurrentErrorMessage = error;
    }

    /// <summary>
    /// Pre-seeds the wizard with an existing secret when the launch-flow
    /// detected a force-quit between identity-confirm and complete-login on
    /// a previous run (i.e. the long-term store has a secret but no nest
    /// URL). The state machine routes the seeded identity directly to
    /// <c>HandleEntry</c> so the user resumes onboarding without re-entering
    /// their secret. <see cref="ServiceClients.SeedSecret"/> is null on
    /// every other launch path; the call is a no-op there.
    ///
    /// Gated on <c>NavigationMode.New</c> so a future <c>Frame.GoBack()</c>
    /// from a child page can't yank the user out of a later wizard stage
    /// (e.g. <c>NestLogin</c>) back to <c>HandleEntry</c> and discard
    /// their progress.
    /// </summary>
    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        StartUiThreadStallWatch();
        if (e.NavigationMode != NavigationMode.New) return;
        if (e.Parameter is ServiceClients sc)
        {
            // Sign-out residue (account-scoping.md § Erasure follows scope → the
            // residue surface): handed over once here, painted by
            // IdentityChoiceView's sign-out-residue view from state the wizard's
            // machine does not own, so no observer tick wipes it. Null on every
            // navigation that neither erased nor re-checked a recorded residue.
            ViewModel.SignOutResidue = sc.SignOutResidue;
            if (sc.SeedSecret is { Length: > 0 } seed)
            {
                ViewModel.SeedIdentity(seed);
            }
            // box-recovery.md § Recovery UI (step 4), surviving-device entry:
            // launch-recover-button already read the box list off the saved
            // nest before navigating here. Mutually exclusive with SeedSecret
            // above (this carries its own secret hex) — SeedRecoveryFromLaunch
            // is a superset of SeedIdentity (same imported secret, flips
            // recovery_intent, lands on NestRecovery directly).
            if (sc.RecoverFromLaunch is { } recover)
            {
                ViewModel.SeedRecoveryFromLaunch(recover.SecretHex, recover.Boxes);
            }
            // The launch-time succeeded-identity refusal (identity-succession.md
            // § Propagation → *Own device fleet*): land on the import flow with the
            // reason. Fire-and-forget — the verified upgrade is a best-effort walk.
            // A held verified successor (a lost succession reply) is adopted
            // through an ordinary switch to it — the same one the ceremony's
            // persisted arm ends in; no re-auth prompt ran, so `confirmed: false`.
            if (sc.SupersededRefusal is { } superseded)
            {
                _ = ViewModel.RouteSupersededRefusalAsync(
                    superseded.ClaimedSuccessor, superseded.SecretHex, superseded.NestUrl,
                    adopt: App.SwitchAccountHandler is { } switchTo
                        ? successor => switchTo(successor, false)
                        : null);
            }
            if (sc.SeedPendingInvite is { } pending)
            {
                ViewModel.SeedPendingInvite(
                    pending.@nestUrl, pending.@handle, pending.@requestId, pending.@statusJson);
            }
            // Per target §App-launch routing: when the silent challenge
            // says "secret not registered on this nest," land the wizard
            // directly on invite_request rather than handle_entry. Must
            // run AFTER SeedIdentity so the machine has the identity to
            // associate with the invite request.
            if (sc.NavigateToInviteRequestForKnownNest is { } navTarget)
            {
                ViewModel.NavigateToInviteRequestForKnownNest(
                    navTarget.NestUrl, navTarget.Handle);
            }
            // Per target §App-launch routing — silent-challenge fallback
            // table, unclaimed-nest row: when the silent challenge
            // returns 404 AND setup-status reports claimed=false, land
            // the wizard directly on claim_code so the user can claim
            // the nest themselves (no admin to issue invites). Mutually
            // exclusive with NavigateToInviteRequestForKnownNest.
            if (sc.NavigateToClaimCodeForKnownNest is { } claimTarget)
            {
                ViewModel.NavigateToClaimCodeForKnownNest(
                    claimTarget.NestUrl, claimTarget.Handle);
            }
            // Factory-reset re-onboard: fauna.admin.factory_reset returned the
            // post-reset claim code to the client (the human never sees it), so
            // land the wizard on claim_code with the code pre-filled. Must run
            // AFTER SeedIdentity (the identity is unchanged — only the box was
            // wiped). Mutually exclusive with NavigateToClaimCodeForKnownNest.
            // Per docs/goal/behavior/mail-bridge-lifecycle.md § Factory reset.
            if (sc.FactoryResetReonboard is { } resetTarget)
            {
                ViewModel.NavigateToClaimCodeForKnownNestWithCode(
                    resetTarget.NestUrl, resetTarget.Handle, resetTarget.ClaimCode);
            }
            // Relaunch hydration for the deferred-DNS ("Almost ready") path
            // (onboarding.md § "Almost ready" surface): the persisted
            // awaiting-manual-DNS slot. Must run AFTER SeedIdentity so signing
            // works for the eventual claim. Lands the wizard on Done with
            // wizard_outcome() == AwaitingManualDns — the same state the
            // same-session dns_post_instructions exit produces — so
            // UpdateStageContent renders _awaitingManualDnsView identically
            // either way.
            if (sc.SeedAwaitingManualDns is { } awaitingDns)
            {
                ViewModel.SeedAwaitingManualDns(
                    awaitingDns.NestUrl, awaitingDns.Handle, awaitingDns.DnsRecordsJson, awaitingDns.ClaimCode);
            }
            // Wired to the VM directly: HandleWizardOutcome invokes this
            // only on WizardOutcome.LoggedIn, so seed-only / re-onboard
            // paths (where the launch flow passes null) don't accidentally
            // navigate, and the AwaitingManualDns terminal
            // states that previously also fired this hand-off no longer do.
            ViewModel.OnLoggedIn = sc.OnOnboardingCompleted;
            // Pending-invite's append-mode adoption seam (onboarding.md
            // § Multi-account) — see ServiceClients.OnPendingInvitePersisted.
            ViewModel.OnPendingInvitePersisted = sc.OnPendingInvitePersisted;
            // The deferred-DNS exit's twin seam — see ServiceClients.OnAwaitingDnsPersisted.
            ViewModel.OnAwaitingDnsPersisted = sc.OnAwaitingDnsPersisted;
        }
    }

    /// <summary>
    /// Every exit path — LoggedIn (fresh boot or append-switch) and an abandoned append
    /// (the <c>onboarding-cancel-button</c> path) alike — navigates this page away, so this
    /// is the single place to retire <see cref="OnboardingViewModel.Current"/>. See
    /// <see cref="OnboardingViewModel.ClearIfCurrent"/> for why a dangling reference matters.
    /// </summary>
    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        base.OnNavigatedFrom(e);
        ViewModel.ClearIfCurrent();
        // Every exit navigates this page away — the single place that
        // guarantees App's append latch never survives past this wizard
        // instance, regardless of which specific exit path fired. A no-op outside append mode.
        App.ClearAppendingAccount();
        _stallTimer?.Stop();
        _stallTimer = null;
        _registry.Dispose();
    }

    // ── Is the UI thread actually pumping? ───────────────────────────────────
    // A timer ON the UI thread that reports its own lateness. This is the only
    // witness in the app that does not observe itself through the automation
    // surface, which matters because the surface's own health is what is in
    // doubt: when UIA cannot resolve this app's window on `vps_config`, the
    // standing reading is "the UI thread is not pumping" — and that has never
    // been measured, only inferred from UIA failing. If UIA times out while these
    // ticks stay on time, the thread is fine and the fault is elsewhere.
    //
    // Cheap and self-silencing: one queued no-op per interval, and it writes
    // only when a tick is late, so a healthy run produces no output at all.
    private Microsoft.UI.Dispatching.DispatcherQueueTimer? _stallTimer;

    private void StartUiThreadStallWatch()
    {
        if (_stallTimer is not null) return;
        const double intervalMs = 250;
        var watch = new FaunaApp.Core.Helpers.DispatcherLatencyWatch(intervalMs);
        var clock = System.Diagnostics.Stopwatch.StartNew();

        _stallTimer = DispatcherQueue.CreateTimer();
        _stallTimer.Interval = TimeSpan.FromMilliseconds(intervalMs);
        _stallTimer.IsRepeating = true;
        _stallTimer.Tick += (_, _) =>
        {
            var stall = watch.Tick(clock.Elapsed.TotalMilliseconds);
            if (stall is null) return;
            FaunaApp.Core.Logs.ShellLog.Warn("Onboarding", stall.Value.ToString());
        };
        _stallTimer.Start();

        // Say so ON ARMING. This instrument is read for its silence — "UIA timed
        // out and no stall was reported, so the thread was pumping" — and that
        // reading is worthless unless the absence of stall lines can be told apart
        // from the absence of the instrument. One line at Info makes the negative
        // result an assertion instead of an inference.
        FaunaApp.Core.Logs.ShellLog.Info(
            "Onboarding",
            $"UI-thread stall watch armed ({intervalMs:N0}ms tick, reporting gaps over "
            + $"{intervalMs + FaunaApp.Core.Helpers.DispatcherLatencyWatch.DefaultLateMs:N0}ms)");
    }

    /// <summary>Maps the current Rust stage to its cached UserControl.</summary>
    private void UpdateStageContent()
    {
        StageContent.Content = ViewModel.CurrentStep switch
        {
            OnboardingStep.IdentityChoice        => _identityChoiceView,
            OnboardingStep.IdentityCreated       => _identityCreatedView,
            OnboardingStep.IdentityImport        => _identityImportView,
            // onboarding.md § 1 Identity: the kit offer (reached only because the
            // VM declares SetRendersRecoveryKit — the two land together) and the
            // phrase-only restore (restore-from-recovery-kit-button).
            OnboardingStep.RecoveryKit           => _recoveryKitView,
            OnboardingStep.RecoveryEntry         => _recoveryEntryView,
            OnboardingStep.HandleEntry           => _handleEntryView,
            OnboardingStep.DnsConfig             => _dnsConfigView,
            OnboardingStep.VpsConfig             => _vpsConfigView,
            OnboardingStep.DnsPostInstructions   => _dnsPostInstructionsView,
            OnboardingStep.InviteRequest         => _inviteRequestView,
            OnboardingStep.ClaimCode             => _claimCodeView,
            // nat_mode_choice (docs/goal/behavior/onboarding.md §3b-bis) — the
            // terminal admin-path step, reached directly on a successful admin
            // claim (Phase-4 "no-modes", S8.7).
            OnboardingStep.NatModeChoice          => _natModeChoiceView,
            // 3b-ter (onboarding.md § 3b-ter): reached only when the machine
            // was constructed with set_renders_trust_prompt(true) — see
            // OnboardingViewModel's constructor. Until this leg landed the
            // step routed straight past to Done; the flow is unchanged for
            // any build that hasn't declared the capability.
            OnboardingStep.TrustPrompt            => _trustPromptView,
            // Box-recovery step-4 (Task E): the two recovery pages. MANDATORY
            // arms — this is a throwing switch expression, so an unmapped step
            // (reached the moment recover-lost-box-button fires
            // begin_recover_lost_box) would throw. Per box-recovery.md §
            // Recovery UI (step 4).
            OnboardingStep.NestRecovery                  => _nestRecoveryView,
            OnboardingStep.RecoverSelfhostedInstructions => _recoverSelfhostedInstructionsView,
            // Done: NOT itself the "Almost ready" trigger — that surface is
            // keyed on wizard_outcome() == AwaitingManualDns (not an
            // OnboardingStep), true on both the same-session
            // dns_post_instructions exit and the relaunch-hydration seed.
            // LoggedIn means OnLoggedIn already fired and App.StartMainAppAsync
            // is connecting in the background (WS-RPC + MLS session build, ~1-3s)
            // before it navigates to MainPage — render the transient
            // signing-in spinner for that gap (was: a wrong, momentary flash of
            // IdentityChoice's "Create a new identity / Import" page). The
            // identity-choice fallback is now only a never-blank guard: the one
            // outcome that used to land on it, InviteSubmitted, retired
            // 2026-08-12 (that journey no longer reaches Done at all — it stays
            // on invite_request and polls).
            OnboardingStep.Done => ViewModel.IsAwaitingManualDns
                ? _awaitingManualDnsView
                : ViewModel.IsSigningIn
                    ? _signingInView
                    : _identityChoiceView,
            OnboardingStep.NestProvisioning      => _nestProvisioningView,
            _ => throw new InvalidOperationException($"unknown OnboardingStep: {ViewModel.CurrentStep}"),
        };
    }
}
