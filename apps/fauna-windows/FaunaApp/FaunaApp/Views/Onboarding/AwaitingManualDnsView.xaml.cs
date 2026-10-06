using System;
using System.ComponentModel;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_onboarding_machine;

namespace FaunaApp.Views.Onboarding;

/// <summary>
/// The post-provisioning "Almost ready" surface (onboarding.md § "Almost ready"
/// surface). NOT an <c>OnboardingStep</c> — rendered by <see cref="OnboardingPage"/>
/// whenever <c>ViewModel.IsAwaitingManualDns</c> is true while
/// <c>CurrentStep == OnboardingStep.Done</c>, on both the same-session exit from
/// <c>dns_post_instructions</c> and the relaunch-hydration path.
///
/// Auto-probes the nest every 10s while shown (mirrors
/// apps/fauna-linux/src/views/onboarding/mod.rs's start_awaiting_dns_poll and
/// android's AwaitingManualDnsScreen POLL_INTERVAL_MS) — recheck_manual_dns() is
/// single-shot by contract, so the client owns the cadence. Self-terminating:
/// stops once the wizard leaves the AwaitingManualDns outcome (a successful claim
/// advances the step past Done and OnboardingPage swaps this view out).
/// </summary>
public sealed partial class AwaitingManualDnsView : UserControl
{
    // Typed VM property used by x:Bind. Defaulted to null! since the
    // parameterless ctor (used by the XAML loader's design-time path) never
    // gets a VM.
    internal OnboardingViewModel ViewModel { get; private set; } = null!;

    private DispatcherTimer? _pollTimer;

    public AwaitingManualDnsView()
    {
        InitializeComponent();
    }

    internal AwaitingManualDnsView(OnboardingViewModel vm) : this()
    {
        ViewModel = vm;
        DataContext = vm;

        // x:Bind generates one-time evaluations for OneWay-on-non-INPC
        // sources; the VM raises PropertyChanged with empty PropertyName on
        // every observer tick and Bindings.Update() re-runs them all — same
        // pattern as NestProvisioningView (this page's status text and
        // recheck-enabled state both change while shown, unlike the
        // one-shot DnsPostInstructionsView).
        ViewModel.PropertyChanged += OnViewModelPropertyChanged;

        // Reconcile once at construction in case relaunch hydration already
        // seeded the AwaitingManualDns outcome before this view rendered.
        ReconcilePollTimer();
    }

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        Bindings.Update();
        ReconcilePollTimer();
    }

    private void ReconcilePollTimer()
    {
        if (!ViewModel.IsAwaitingManualDns)
        {
            _pollTimer?.Stop();
            return;
        }
        _pollTimer ??= new DispatcherTimer
        {
            Interval = TimeSpan.FromMilliseconds(FaunaOnboardingMachineMethods.AwaitingDnsPollMs())
        };
        _pollTimer.Tick -= OnPollTick;
        _pollTimer.Tick += OnPollTick;
        _pollTimer.Start();
    }

    private void OnPollTick(object? sender, object e)
    {
        // Guard mirrors the recheck button's own IsEnabled: a probe or claim
        // already in flight shouldn't get a second concurrent one queued behind it.
        if (ViewModel.AwaitingDnsRecheckEnabled)
            ViewModel.RecheckManualDnsCommand.Execute(null);
    }

    /// <summary>
    /// Copies the DNS records text to the system clipboard. Codebehind because it
    /// uses the Windows-specific <c>Windows.ApplicationModel.DataTransfer</c> APIs
    /// that FaunaApp.Core (net10.0) can't reference.
    /// </summary>
    private void OnCopyClick(object sender, RoutedEventArgs e) =>
        FaunaApp.Helpers.ClipboardHelper.CopyText(ViewModel?.AwaitingDnsRecordsText);
}
