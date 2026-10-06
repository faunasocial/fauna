using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Controls;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_atproto_settings_machine;

namespace FaunaApp.Views;

/// <summary>
/// The dedicated AT Protocol settings page (docs/goal/ui/atproto.md, ratified 2026-07-22),
/// closing the last "windows UNBUILT" row in that doc's implementation-status table.
/// A dumb renderer of the testable <see cref="AtprotoViewModel"/> over the shared
/// <c>AtprotoSettingsMachine</c> — no new Rust, no client-side level logic (priority #2).
///
/// <para>This file's whole job is <see cref="ApplyVm"/>: project the VM's already-decided
/// booleans onto the visual tree. It must not decide anything itself — every reveal rule,
/// the gate verdict and the card's copy are machine-composed. In particular the transition
/// card's lines are added VERBATIM, in order, with no re-authoring: a card promising
/// something nest did not do is the one failure a confirm-before-anything-happens UX
/// cannot absorb.</para>
///
/// <para>The e2e reads two attrs off this page, both via <c>AutomationProperties.HelpText</c>
/// (the windows bridge maps any non-name/disabled attr to HelpText): the CURRENT LEVEL from
/// <c>atproto-depth-selector</c>'s <c>state</c>, and <c>gated</c>/<c>ok</c> from each hosted
/// rung's <c>reason</c>. They never collide because no rung exposes <c>state</c>.</para>
///
/// <para>No <c>ConfigureAwait(false)</c> anywhere here or in the VM: off-thread mutation of
/// bound state throws a silent <c>COMException</c> in WinUI (memory
/// <c>reference_windows_vm_configureawait_comexception</c>).</para>
/// </summary>
public sealed partial class AtprotoPage : Page, IAsyncLoadedPage
{
    private ServiceClients? _clients;
    private AtprotoViewModel? _vm;

    /// <summary>Capture suppression while an app-credential secret is painted — rule 2 of
    /// <c>docs/goal/architecture/security.md</c> § On-screen secret exposure. A Bluesky app
    /// credential is <em>minted and revocable</em>, so suppressing costs a revoke-and-re-mint
    /// at worst; that is what puts it in rule 2's scope and out of rule 1's.
    ///
    /// <para>⚠ <b>This page has no hide path, and that is what makes the release non-obvious.</b>
    /// <c>atproto-app-credential-reveal</c>'s own Button text <em>becomes</em> the secret
    /// (F1's approved ui.yaml surface has no separate secret element), and
    /// <c>AtprotoViewModel</c> never clears <c>_revealedSecrets</c> — <c>CanReveal</c> just
    /// goes false. So a hold keyed on "the revealed secret went away" would NEVER release and
    /// would leave the whole app permanently uncapturable. Rule 2's contract is "off on hide
    /// <em>or navigate-away</em>"; with no hide, navigate-away is the release, and
    /// <see cref="OnNavigatedFrom"/> is where it happens.</para></summary>
    private readonly ScreenCaptureHold _captureHold =
        FaunaApp.Helpers.SuppressScreenCapture.ForMainWindow();

    /// <summary>The e2e rehydrate hook — the windows twin of linux's
    /// <c>settings::notify_atproto_rehydrate</c> and web's page-installed hook.
    ///
    /// <para>Set while this page is mounted, cleared when it is navigated away from.
    /// <c>TestAgent</c>'s <c>atproto_delegation_advance_clock</c> arm invokes it after
    /// moving the process-wide delegation render clock: the offset alone changes
    /// nothing, because liveness is computed when the machine refreshes, so without
    /// this the row would keep reporting the pre-advance state and the lapse journey
    /// would be unreachable — or, worse, chased with a sleep against a ~90-day
    /// window.</para>
    ///
    /// <para>Deliberately a hook rather than a direct machine poke: it re-runs the
    /// SAME <c>LoadAsync</c> + <c>ApplyVm</c> pair every gesture on this page runs,
    /// against the SAME memoized machine instance the page observes
    /// (<c>AtprotoSettingsMachineHost</c>), so the test drives the production render
    /// path rather than a test-only one.</para></summary>
    internal static Func<Task>? RehydrateForTest;

    /// <summary>Backs the Linked-account panel (§ 3a): a SECOND
    /// <see cref="BridgesViewModel"/> scoped by <c>SingleBridgeId = "bluesky"</c>,
    /// reading the same unfiltered <c>fauna.bridges.list</c> reply the Bridges page
    /// reads — so the two surfaces can never disagree about the bridge's status.
    /// Apple scopes its <c>BridgeManagerVM</c> the same way; android runs a second
    /// <c>BridgesVM</c>. The panel is not a second settings machine — it is the
    /// existing bridges surface, focused (ui/atproto.md § Layout &amp; flow item 3).</summary>
    private BridgesViewModel? _linkedVm;

    /// <summary>Set while <see cref="ApplyVm"/> writes <c>IsChecked</c>/<c>IsOn</c>, so the
    /// resulting events are not mistaken for user intent and re-sent to the machine.</summary>
    private bool _applying;

    /// <summary>The provider id this page owns. It is the id the shared
    /// <c>is_unified_bridges_page_bridge</c> predicate excludes from the generic
    /// Bridges page, which is precisely why this page must render its card.</summary>
    private const string BlueskyBridgeId = "bluesky";

    /// <summary>The four depth-selector radio buttons, keyed by their wire level —
    /// built once in <see cref="BuildDepthRungs"/> from the shared catalog, so
    /// <see cref="ApplyVm"/> can still paint them by level without re-spelling any
    /// of the four names.</summary>
    private readonly Dictionary<string, RadioButton> _depthRungButtons = new();

    public AtprotoPage()
    {
        this.InitializeComponent();
        BuildDepthRungs();
    }

    /// <summary>Build the four depth-selector rungs from the shared catalog
    /// (<c>fauna_atproto_settings_machine::depth::depth_level_options</c>) — wire
    /// level, element id, title and description all come from there, none
    /// re-spelled here (atproto.md § Where logic lives → *The rung catalog*).
    /// Mirrors linux's/android's/apple's per-row dynamic construction (a `ForEach`/
    /// loop over the catalog) rather than a hand-listed set of RadioButtons.</summary>
    private void BuildDepthRungs()
    {
        foreach (var opt in FaunaAtprotoSettingsMachineMethods.DepthLevelOptions())
        {
            var radio = new RadioButton
            {
                GroupName = "AtprotoDepth",
                Tag = opt.@level,
                Content = Strings.Resolve(opt.@title),
            };
            AutomationProperties.SetAutomationId(radio, opt.@uiId);
            radio.Click += Depth_Click;

            var desc = new TextBlock
            {
                Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
                Margin = new Thickness(28, -4, 0, 4),
                Text = Strings.Resolve(opt.@description),
                TextWrapping = TextWrapping.Wrap,
            };

            DepthRungsHost.Children.Add(radio);
            DepthRungsHost.Children.Add(desc);
            _depthRungButtons[opt.@level] = radio;
        }
    }

    private TaskCompletionSource _loadComplete =
        new(TaskCreationOptions.RunContinuationsAsynchronously);

    /// <inheritdoc />
    public Task LoadComplete => _loadComplete.Task;

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _clients = clients;
        }
        // Re-arm for a reused (cached) page instance so a second navigation waits on
        // this load, not the previous one's already-completed barrier.
        if (_loadComplete.Task.IsCompleted)
            _loadComplete = new(TaskCreationOptions.RunContinuationsAsynchronously);
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        try
        {
            await LoadAsync();
        }
        finally
        {
            // ALWAYS complete, so the agent's bounded await never hangs a navigation.
            _loadComplete.TrySetResult();
        }
    }

    private async Task LoadAsync()
    {
        if (_clients?.Rpc is null) return;
        try
        {
            _vm ??= new AtprotoViewModel(_clients.Rpc);
            _linkedVm ??= new BridgesViewModel(_clients.Rpc) { SingleBridgeId = BlueskyBridgeId };
            // Paint the VM's PRE-FETCH state before the first await. Without this the
            // tree keeps its XAML statics for the whole fetch — the two hosted rungs
            // carry no IsEnabled and no HelpText (so: enabled and unmarked) and
            // GateReasonText is statically Collapsed — i.e. exactly the "greyed with no
            // reason" screen ui/README.md rule 5 forbids, only worse: not greyed at all.
            // The VM's own defaults are the shared Rust pre-fetch default (gate closed +
            // its reason), so one early projection is all the pre-fetch window needs.
            // Safe this early: ApplyVm is null-safe on _linkedVm, and every panel it
            // could scroll into view is collapsed at level "off".
            ApplyVm();
            await _vm.LoadAsync();
            await _linkedVm.LoadCommand.ExecuteAsync(null);
            ApplyVm();
            RehydrateForTest = async () =>
            {
                if (_vm is null) return;
                await _vm.LoadAsync();
                ApplyVm();
            };
        }
        catch (Exception ex)
        {
            ShowError(Strings.Error(ex));
        }
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        // The reveal's only end (see _captureHold): this page cannot un-reveal a secret, so
        // walking away from it is what lifts the suppression. Skipping this is the stuck-on
        // failure — an app that has silently stopped taking screenshots.
        _captureHold.Sync(false);

        // Clear the hook with the page: a closure over a navigated-away page's `_vm`
        // would repaint a tree nobody is looking at, and would keep it alive.
        RehydrateForTest = null;
        base.OnNavigatedFrom(e);
    }

    /// <summary>Project the VM onto the tree. Called after load and after every gesture —
    /// the page is non-optimistic, so what is shown is what the nest persisted.</summary>
    private void ApplyVm()
    {
        if (_vm is null) return;
        _applying = true;
        try
        {
            // ── Recovery-fork contest ceremony — leads the page. ──
            ContestCard.Visibility = Vis(_vm.ShowContestCard);
            AutomationProperties.SetHelpText(ContestCard, _vm.ContestState);
            ContestDetailText.Text = _vm.ContestDetail;
            ContestDeadlineText.Text = _vm.ContestDeadline;
            ContestDeadlineText.Visibility = Vis(_vm.ShowContestDeadline);
            ContestBtn.Visibility = Vis(_vm.ShowContestButton);

            ContestConfirmCard.Visibility = Vis(_vm.ShowContestConfirmCard);
            ContestConfirmLines.Children.Clear();
            foreach (var line in _vm.ContestConfirmLines)
            {
                ContestConfirmLines.Children.Add(new TextBlock { Text = line, TextWrapping = TextWrapping.Wrap });
            }
            ContestConfirmBtn.IsEnabled = _vm.ContestConfirmActionsEnabled;
            ContestCancelBtn.IsEnabled = _vm.ContestConfirmActionsEnabled;

            // ── Selector. The container's `state` attr IS the level, for the e2e. ──
            AutomationProperties.SetHelpText(DepthSelector, _vm.Level);
            // Greyed with a reason, never hidden. A rung the user is already AT stays
            // enabled so a step-down survives the domain going non-public, and
            // Off/Linked are never gated (§ Reveal/greying rules) — the VM decides
            // all of that per rung; this just applies it.
            foreach (var rung in _vm.DepthRungs)
            {
                if (!_depthRungButtons.TryGetValue(rung.Level, out var radio)) continue;
                radio.IsChecked = rung.Checked;
                radio.IsEnabled = rung.Enabled;
                AutomationProperties.SetHelpText(radio, rung.GateMarker);
            }

            GateReasonText.Text = _vm.GateReason;
            GateReasonText.Visibility = Vis(_vm.ShowGateReason);

            // ── Transition card: the machine's lines, verbatim and in order. ──
            TransitionCard.Visibility = Vis(_vm.ShowCard);
            CardLines.Children.Clear();
            foreach (var line in _vm.CardLines)
            {
                CardLines.Children.Add(new TextBlock { Text = line, TextWrapping = TextWrapping.Wrap });
            }
            HistoryBackfill.Visibility = Vis(_vm.ShowHistoryBackfill);
            HistoryBackfill.IsChecked = _vm.HistoryBackfill;
            ConfirmBtn.IsEnabled = _vm.CardActionsEnabled;
            CancelBtn.IsEnabled = _vm.CardActionsEnabled;

            // ── Per-level panels. ──
            // The Linked panel is the shared bridge card, focused on this page's own
            // provider (§ 3a). `_linkedVm` always yields exactly one row — a synthetic
            // unlinked one when the provider has no row at all — so the panel offers
            // the link rather than vanishing.
            LinkedPanel.Visibility = Vis(_vm.ShowLinkedPanel);
            LinkedBridgeCard.Bridge = _linkedVm?.Bridges.Count > 0 ? _linkedVm.Bridges[0] : null;
            if (_vm.ShowLinkedPanel) BringIntoViewAfterLayout(LinkedPanel);

            HostedPanel.Visibility = Vis(_vm.ShowHostedPanel);
            DidMethodBox.Visibility = Vis(_vm.ShowDidMethodRadio);
            DidPlc.IsChecked = _vm.DidMethodPlc;
            DidWeb.IsChecked = _vm.DidMethodWeb;
            HandlePreviewText.Text = _vm.HandlePreview;
            HandlePreviewText.Visibility = Vis(_vm.ShowHandlePreview);
            HostedHandleText.Text = _vm.HostedHandle;
            HostedHandleText.Visibility = Vis(_vm.ShowHostedHandle);

            DeletePresenceBtn.Visibility = Vis(_vm.ShowDeletePresence);

            DeleteConfirmCard.Visibility = Vis(_vm.ShowDeleteConfirmCard);
            DeleteConfirmLines.Children.Clear();
            foreach (var line in _vm.DeleteConfirmLines)
            {
                DeleteConfirmLines.Children.Add(new TextBlock { Text = line, TextWrapping = TextWrapping.Wrap });
            }
            DeleteConfirmBtn.IsEnabled = _vm.DeleteConfirmActionsEnabled;
            DeleteCancelBtn.IsEnabled = _vm.DeleteConfirmActionsEnabled;

            FullPdsPanel.Visibility = Vis(_vm.ShowFullPds);
            if (_vm.ShowFullPds) BringIntoViewAfterLayout(FullPdsPanel);
            CredentialsList.ItemsSource = _vm.Credentials;
            // Every gesture re-renders through here, so this is the one place that sees a
            // reveal land. Idempotent: N re-renders with a secret showing take one hold.
            _captureHold.Sync(_vm.Credentials.Any(c => c.RevealedSecret is not null));
            ExternalAppsToggle.IsOn = _vm.ExternalAppsEnabled;
            AutomationProperties.SetHelpText(ExternalAppsToggle, _vm.ExternalAppsState);

            // ── The D10 authoring-delegation row. ──
            // Two shapes, one always-present control: `-authorize` renders either way
            // (re-authorizing IS renewal — hiding it once authorized would force the
            // revoke-then-re-mint flow the ruling forbids), `-revoke` only alongside a
            // live row, and the four leaves are WITHHELD with no verified delegation.
            DelegationRow.Visibility = Vis(_vm.ShowDelegationRow);
            DelegationEmptyText.Visibility = Vis(!_vm.ShowDelegationRow);
            DelegationRevokeBtn.Visibility = Vis(_vm.ShowDelegationRow);
            DelegationAuthorizeBtn.Content = _vm.DelegationAuthorizeText;
            DelegationScopeText.Text = _vm.DelegationScope;
            DelegationLastsUntilText.Text = _vm.DelegationLastsUntil;
            DelegationStatusText.Text = _vm.DelegationStatus;
            // The liveness WIRE spelling, in the slot the driver's get_attr(id, "state")
            // reads — the e2e asserts the state, never the prose (convention 14).
            AutomationProperties.SetHelpText(DelegationStatusText, _vm.DelegationStatusState);
            DelegationLastUsedText.Text = _vm.DelegationLastUsed;

            // LAST, so it wins over the FullPdsPanel scroll above: this page is taller
            // than the 600x600 DIP viewport, the delegation row sits below the
            // credential and session lists, and the driver's is_visible is
            // `!IsOffscreen` — so a row that rendered correctly would still read as
            // absent. Only when it is actually shown; scrolling to a collapsed row
            // would drag the panel away from the groups above for nothing.
            if (_vm.ShowDelegationRow) BringIntoViewAfterLayout(DelegationRow);

            // The settings machine owns the page error; a Linked-panel failure (the
            // bridges fetch, a refused link) must surface too rather than be dropped —
            // a silently-swallowed command is what testing.md rule 11 forbids.
            ShowError(_vm.ErrorMessage ?? _linkedVm?.ErrorMessage);
        }
        finally
        {
            _applying = false;
        }
    }

    private static Visibility Vis(bool shown) => shown ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>Scroll <paramref name="panel"/> into the viewport once layout has run.
    ///
    /// <para>This page is taller than the 600x600 DIP viewport the e2e drives: below the
    /// four rungs and their descriptions, a revealed panel's controls are in the UIA tree
    /// but OFFSCREEN — and the driver's <c>is_visible</c> is <c>!IsOffscreen</c>, so the
    /// panel reads as ABSENT. Several action-layer reads (<c>is_fullpds_visible</c>,
    /// <c>is_linked_panel_visible</c>) use a bare <c>is_visible</c> with no scroll of their
    /// own, so the page has to put the panel it just revealed on screen itself.</para>
    ///
    /// <para>⚠ A bare <c>StartBringIntoView</c> in the same pass that sets
    /// <c>Visibility = Visible</c> is a silent no-op: the panel has not been measured yet,
    /// so it has no size or position to scroll to. <c>UpdateLayout()</c> forces that
    /// measure/arrange FIRST, which is what gives the request a rect to act on.
    /// (Diagnosed 2026-07-31 — without it, <c>atproto-app-credential-mint</c> read
    /// offscreen at <c>hosted_full</c>.)</para>
    ///
    /// <para>⚠ And it must land BEFORE <see cref="ApplyVm"/> returns, not on a later tick.
    /// A deferred-only version left a race the ladder test lost: the driver polls the
    /// level off <c>atproto-depth-selector</c>'s <c>state</c> attr and then reads panel
    /// visibility immediately, so a scroll still queued at that moment reads as "panel
    /// absent". Doing the work synchronously inside <c>ApplyVm</c> closes it by
    /// construction — cross-process UIA reads cannot observe the tree until the UI thread
    /// leaves <c>ApplyVm</c>, so level-flipped implies already-scrolled. The low-priority
    /// enqueue stays as a follow-up for the animated settle. Per testing.md convention 14
    /// this is a causal fix, NOT a sleep.</para></summary>
    private void BringIntoViewAfterLayout(FrameworkElement panel)
    {
        panel.UpdateLayout();
        panel.StartBringIntoView();
        DispatcherQueue?.TryEnqueue(
            Microsoft.UI.Dispatching.DispatcherQueuePriority.Low,
            () => panel.StartBringIntoView());
    }

    private void ShowError(string? message)
    {
        ErrorBar.Message = message ?? string.Empty;
        ErrorBar.IsOpen = !string.IsNullOrEmpty(message);
    }

    // ── Gestures. Each forwards to the machine, then re-projects. ────────────

    /// <summary>A rung was clicked. The machine decides what the move means — Off→Linked
    /// applies immediately with no card, a gated hosted selection is refused loudly on
    /// the error banner, everything else stages the card. This page must not pre-judge
    /// any of that.</summary>
    private async void Depth_Click(object sender, RoutedEventArgs e)
    {
        if (_applying || _vm is null) return;
        if (sender is not FrameworkElement { Tag: string level }) return;
        await _vm.SelectLevelAsync(level);
        ApplyVm();
    }

    /// <summary>The Linked panel's card action — the same link/unlink ceremony the
    /// Bridges page runs, through the shared <see cref="BridgeCardActions"/>. The page
    /// stays non-optimistic: the VM re-loads itself on success, and re-projecting
    /// afterwards repaints the card from what the nest persisted.</summary>
    private async void LinkedBridgeAction_Click(object? sender, BridgeInfo bridge)
    {
        if (_applying || _linkedVm is null) return;
        await BridgeCardActions.RunAsync(this.XamlRoot, _linkedVm, bridge);
        ApplyVm();
    }

    /// <summary>The Linked panel's card settings row committed a new value
    /// (bridges.md § Bridge settings) — same
    /// dispatch-then-reproject shape as <see cref="LinkedBridgeAction_Click"/>.
    /// </summary>
    private async void LinkedBridgeSetting_Changed(object? sender, BridgeSettingChangedEventArgs e)
    {
        if (_applying || _linkedVm is null) return;
        await _linkedVm.SetSettingAsync(e.BridgeId, e.Value);
        ApplyVm();
    }

    /// <summary>Open the contest confirm card (`atproto-contest`) — a sync,
    /// pure-local machine mutation; no wire call.</summary>
    private void Contest_Click(object sender, RoutedEventArgs e)
    {
        if (_applying || _vm is null) return;
        _vm.OpenContestConfirm();
        ApplyVm();
    }

    /// <summary>Record the contest intent and run the converge (`atproto-contest-confirm`)
    /// — client-direct HTTPS to the public PLC directory, never a nest call.</summary>
    private async void ContestConfirm_Click(object sender, RoutedEventArgs e)
    {
        if (_applying || _vm is null) return;
        await _vm.RequestContestAsync();
        ApplyVm();
    }

    /// <summary>Close the contest confirm card (`atproto-contest-cancel`) — nothing
    /// signed, no intent recorded.</summary>
    private void ContestCancel_Click(object sender, RoutedEventArgs e)
    {
        if (_applying || _vm is null) return;
        _vm.CancelContest();
        ApplyVm();
    }

    private async void Confirm_Click(object sender, RoutedEventArgs e)
    {
        if (_applying || _vm is null) return;
        await _vm.ConfirmTransitionAsync();
        ApplyVm();
    }

    /// <summary>Open the delete ceremony's confirm card (`atproto-delete-presence`)
    /// — a sync, pure-local machine mutation; no wire call.</summary>
    private void DeletePresence_Click(object sender, RoutedEventArgs e)
    {
        if (_applying || _vm is null) return;
        _vm.OpenDeleteConfirm();
        ApplyVm();
    }

    /// <summary>Run the delete-presence sweep (`atproto-delete-confirm`) —
    /// `fauna.bridges.atproto.delete_presence`, the one network round trip in the
    /// ceremony.</summary>
    private async void DeleteConfirm_Click(object sender, RoutedEventArgs e)
    {
        if (_applying || _vm is null) return;
        await _vm.ConfirmDeleteAsync();
        ApplyVm();
    }

    /// <summary>Close the delete confirm card (`atproto-delete-cancel`) — nothing
    /// deleted.</summary>
    private void DeleteCancel_Click(object sender, RoutedEventArgs e)
    {
        if (_applying || _vm is null) return;
        _vm.CancelDelete();
        ApplyVm();
    }

    private void Cancel_Click(object sender, RoutedEventArgs e)
    {
        if (_applying || _vm is null) return;
        _vm.CancelTransition();
        ApplyVm();
    }

    private void DidMethod_Click(object sender, RoutedEventArgs e)
    {
        if (_applying || _vm is null) return;
        if (sender is not FrameworkElement { Tag: string method }) return;
        _vm.SetDidMethod(method);
        ApplyVm();
    }

    private void HistoryBackfill_Click(object sender, RoutedEventArgs e)
    {
        if (_applying || _vm is null) return;
        _vm.SetHistoryBackfill(HistoryBackfill.IsChecked == true);
        ApplyVm();
    }

    private async void ExternalApps_Toggled(object sender, RoutedEventArgs e)
    {
        if (_applying || _vm is null) return;
        await _vm.SetExternalAppsEnabledAsync(ExternalAppsToggle.IsOn);
        ApplyVm();
    }

    /// <summary>Mint an app credential. The secret comes back exactly once — the nest
    /// custodies only a verifier and can never re-serve it — so the VM records it
    /// against the freshly minted row, and re-rendering shows it as that row's own
    /// reveal-button text (the F1 get_text contract; no banner).</summary>
    private async void Mint_Click(object sender, RoutedEventArgs e)
    {
        if (_applying || _vm is null) return;
        await _vm.MintCredentialAsync(Strings.Get("atproto_settings/default_credential_label"));
        ApplyVm();
    }

    private async void Reveal_Click(object sender, RoutedEventArgs e)
    {
        if (_applying || _vm is null) return;
        if (sender is not FrameworkElement { Tag: AtprotoViewModel.CredentialRowVm row }) return;
        await _vm.RevealSecretAsync(row.CredentialId);
        ApplyVm();
    }

    private async void RevokeCredential_Click(object sender, RoutedEventArgs e)
    {
        if (_applying || _vm is null) return;
        if (sender is not FrameworkElement { Tag: AtprotoViewModel.CredentialRowVm row }) return;
        await _vm.RevokeCredentialAsync(row.CredentialId);
        ApplyVm();
    }

    /// <summary>Authorize — or RE-authorize — external apps to post as this account.
    /// ONE control for both: provisioning overwrites the stored cert with a freshly
    /// dated one, so a lapsed delegation recovers with no revoke first
    /// (atproto-pds-full.md &#167; App surface — "re-minting IS renewal").</summary>
    private async void AuthorizeDelegation_Click(object sender, RoutedEventArgs e)
    {
        if (_applying || _vm is null) return;
        await _vm.AuthorizeExternalAppsAsync();
        ApplyVm();
    }

    /// <summary>Revoke — the separate DESTRUCTIVE action, never the renewal path: it
    /// destroys the signing sub-key K. Already-published posts stay verifiable forever
    /// (their cert is embedded in their own bytes), so this stops future writes rather
    /// than un-writing past ones (atproto-pds-full.md &#167; Revocation).</summary>
    private async void RevokeDelegation_Click(object sender, RoutedEventArgs e)
    {
        if (_applying || _vm is null) return;
        await _vm.DeauthorizeExternalAppsAsync();
        ApplyVm();
    }
}
