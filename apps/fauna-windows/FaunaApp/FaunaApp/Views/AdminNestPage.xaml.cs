using System;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Helpers;
using uniffi.fauna_ffi;
using uniffi.fauna_onboarding_machine;
using NodeMode = uniffi.fauna_core.NodeMode;
using S = FaunaApp.Core.Services.Strings;
// Aliased rather than a plain `using uniffi.fauna_launch_machine;` — only the one
// static-method class is needed here.
using FaunaLaunchMachineMethods = uniffi.fauna_launch_machine.FaunaLaunchMachineMethods;
using FaunaApp.UiIds;

namespace FaunaApp.Views;

/// <summary>
/// The admin Nest page (admin.md § N Nest) — nest-wide settings that aren't a
/// feature page. The per-page-services redesign (2026-06-04, admin.md § Admin IA
/// redesign) removed the standalone admin-services page; its one live flag (the
/// admin pairing toggle) and Factory Reset (off admin-settings) consolidate
/// here. A dumb renderer of the shared <see cref="AdminNestViewModel"/>
/// (FaunaApp.Core, over the <see cref="INestRpcClient"/> WS-RPC seam →
/// <c>FfiAdminClient</c> / <c>fauna.setup.status</c>):
/// <c>admin-service-pairing-toggle</c>. The pairing toggle is reflective — every
/// set is proven by a refetch, never an optimistic flip, with
/// <see cref="_syncing"/> guarding the programmatic <c>IsOn</c> so it doesn't
/// re-dispatch. Factory Reset (Danger zone) stays a page-owned imperative action
/// over the shared WS-RPC client (the <c>INestRpcClient</c> seam), not VM state.
/// Page-level failures route to
/// <c>error-message</c>. Mirrors linux <c>build_nest_page</c>. The read-only
/// storage-mode indicator this page once also carried was retired with the
/// no-modes cutover (Phase-4 S8.7).
/// </summary>
public sealed partial class AdminNestPage : Page
{
    private ServiceClients? _clients;
    private AdminNestViewModel? _vm;
    private AdminNatModeViewModel? _natVm;

    /// <summary>Guards the programmatic <c>ToggleSwitch.IsOn</c> set during a
    /// reflective render so its <c>Toggled</c> handler doesn't re-dispatch an
    /// update (matches linux's per-toggle guard cell).</summary>
    private bool _syncing;

    /// <summary>Guards the programmatic <c>RadioButton.IsChecked</c> set during
    /// a NAT-mode render so the <c>Checked</c> handlers don't re-dispatch
    /// (mirrors <c>NatModeChoiceView</c>'s <c>_syncingRadios</c>).</summary>
    private bool _syncingNatMode;

    public AdminNestPage()
    {
        this.InitializeComponent();
        // Declaring is a genuine Admin-class wire write (kind OnlineOnly,
        // offline_class.rs); withdrawing rides the same kind. Mirrors apple's
        // AdminNestView.regionSection.
        RegionSaveButton.FaunaGate("fauna.admin.region.set");
        RegionWithdrawButton.FaunaGate("fauna.admin.region.set");
        // The admin pairing master switch — one `fauna.admin.services.update`
        // row (name "pairing"), the same kind linux declares on its own toggle
        // (apps/fauna-linux/src/views/admin.rs:2844). Found MISSING here by
        // the whole-frame red-verify: the per-control offline-gate
        // assertion in test_offline_gate.py::test_the_admin_plane_desensitizes_with_no_nest
        // had never run on windows before (blocked on the same missing
        // `/registry` route), so this gap was never caught.
        PairingToggle.FaunaGate("fauna.admin.services.update");
        // Arming reads the roster (fauna.admin.admins.list) — Read class, which
        // never desensitizes (only OnlineOnly does), so the arm button gets no
        // gate at all: greying it while offline would say nothing true. The
        // rotation itself (fauna.admin.deployment_seed.rotate, OnlineOnly) is
        // the confirm button's gate — the "deployment-mutating half of every
        // admin action is OnlineOnly" rule this page's own offline-gate test
        // states.
        SeedRotateConfirmButton.FaunaGate("fauna.admin.deployment_seed.rotate");
        // The dispatching control, not the arm: arming reads nothing over the
        // wire, exactly as the seed-rotation ceremony gates its confirm alone.
        TakedownConfirmButton.FaunaGate("fauna.moderation.legal_takedown");
        // Outside-app sign-in keys (authorization-server.md § The issuer → Two
        // rotation arms). The ordinary rotate is the one control whose kind
        // never changes; the two arm buttons and cancel dispatch nothing over
        // the wire and declare no kind. OauthConfirmButton is gated per-arm,
        // re-declared with its own literal in each arm's click handler below
        // (OfflineGate.Declare's documented "declare it again after the paint
        // decides" shape — the same control paints two different ceremonies).
        OauthRotateButton.FaunaGate("fauna.oauth.rotate_issuer_key");
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _clients = clients;
            _vm ??= new AdminNestViewModel(clients.Rpc!);
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;

        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            await _vm.LoadCommand.ExecuteAsync(null);
            ServingPortBox.Text = _vm.ServingPort;
            RenderServingPortFronted();
            RenderPairingToggle();
            RenderOsMaintenance();
            RenderRegion();
            RenderSeedRotate();
            // Fold the (empty) form once so the arm control opens with the
            // shared `arm_label` and its blocked reason, not a blank button.
            _vm.RefreshTakedownArm();
            RenderTakedown();
            RenderOauth();

            await LoadNatModeAsync();
            RenderNatMode();

            RenderError(_vm.Error);
        }
        catch (Exception ex)
        {
            RenderError(Strings.Error(ex));
        }
        finally
        {
            LoadProgress.IsActive = false;
            LoadProgress.Visibility = Visibility.Collapsed;
        }
    }

    // ── Admin pairing toggle (fauna.admin.services.update name="pairing") ──

    private async void PairingToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_syncing || _vm is null) return;
        await _vm.SetPairingAsync(PairingToggle.IsOn);
        // Snap the toggle back to server truth (handles the failure-revert case
        // where the VM flag is unchanged so a value-change event wouldn't fire).
        RenderPairingToggle();
        RenderError(_vm.Error);
    }

    /// <summary>Reflectively sync the pairing toggle + status badge from the VM,
    /// guarding the programmatic <c>IsOn</c> set against re-dispatch.</summary>
    private void RenderPairingToggle()
    {
        if (_vm is null) return;
        _syncing = true;
        PairingToggle.IsOn = _vm.PairingEnabled;
        PairingStatus.Text = S.Get(_vm.PairingEnabled
            ? "admin/services_page/enabled" : "admin/services_page/disabled");
        _syncing = false;
    }

    // ── Serving port (fauna.admin.set_serving_port) ──────────────────────────

    /// <summary>
    /// Gate the <c>admin-nest-serving-port</c> field on the deployment wiring
    /// (<c>fauna.setup.status</c> <c>fronted_by_router</c>): on a router-fronted
    /// Docker/cloud box the client-facing port is the SNI router's fixed
    /// <c>443</c> and a <c>set_serving_port</c> write is rejected nest-side, so the
    /// input + save button render <b>read-only</b> and the "served on 443 by this
    /// deployment" hint shows. On a direct-listener box (the default) the field
    /// stays editable. Pure UX polish (the nest rejection is the floor); matches
    /// web/linux/android. <c>nest/common.md</c> § Serving ports.
    /// </summary>
    private void RenderServingPortFronted()
    {
        if (_vm is null) return;
        bool editable = !_vm.FrontedByRouter;
        ServingPortBox.IsEnabled = editable;
        SaveServingPortButton.IsEnabled = editable;
        ServingPortFrontedHint.Visibility =
            _vm.FrontedByRouter ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>
    /// admin-nest-serving-port-save-button → validate + commit the client-facing
    /// serving port via the VM (which writes <c>fauna.admin.set_serving_port</c>
    /// then re-reads <c>fauna.setup.status</c>). The box is re-rendered from the VM
    /// so it reflects the persisted port (a malformed value leaves the box as-typed
    /// and surfaces the error). No <c>ConfigureAwait(false)</c> — off-thread
    /// bound-state mutation throws a silent COMException (the windows VM rule).
    /// </summary>
    private async void SaveServingPortButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.ServingPort = ServingPortBox.Text;
        await _vm.SaveServingPortAsync();
        ServingPortBox.Text = _vm.ServingPort;
        RenderError(_vm.Error);
    }

    // ── Host-OS maintenance (installers/vps.md § Host OS Maintenance § 4) ────

    /// <summary>
    /// Reflectively render the host-OS-maintenance surface from the VM: the always-
    /// present <c>nest-os-maintenance-status</c> line (the shared
    /// <c>os_maintenance_status_label</c>), the <c>nest-os-updates-count</c> badge
    /// (shown only when <c>os_security_updates_pending &gt; 0</c>, the raw integer),
    /// and the <c>nest-os-restart-now-button</c> (shown only when
    /// <c>os_reboot_pending</c>). Visibility-gated (not added/removed) — a collapsed
    /// element gets no UIA peer, so the e2e <c>count</c> reads 0 (badge/button absent)
    /// while present, matching web/linux/android/apple's conditional render.
    /// </summary>
    private void RenderOsMaintenance()
    {
        if (_vm is null) return;
        OsMaintenanceStatusText.Text = _vm.OsMaintenanceStatus;
        OsUpdatesCountText.Text = _vm.OsSecurityUpdatesPending.ToString();
        OsUpdatesCountText.Visibility =
            _vm.OsSecurityUpdatesPending > 0 ? Visibility.Visible : Visibility.Collapsed;
        OsRestartNowButton.Visibility =
            _vm.OsRebootPending ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>
    /// nest-os-restart-now-button → dispatch the admin "restart now"
    /// (<c>fauna.admin.request_host_restart</c>) via the VM, which writes the
    /// <c>restart-requested</c> flag the host reboot-coordinator consumes then re-reads
    /// <c>fauna.setup.status</c>. Re-render the os surface from the refreshed VM state
    /// and route any <c>no_host</c> rejection to <c>error-message</c>. No
    /// <c>ConfigureAwait(false)</c> — off-thread bound-state mutation throws a silent
    /// COMException (the windows VM rule).
    /// </summary>
    private async void OsRestartNowButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.RestartNowAsync();
        RenderOsMaintenance();
        RenderError(_vm.Error);
    }

    // ── Declared region (fauna.admin.region.{get,set}, admin.md § N Nest →
    //     Declared region) ──────────────────────────────────────────────────

    /// <summary>
    /// Reflectively render the declared-region section from the VM: the
    /// always-present <c>admin-nest-region-status</c> line (a NORMAL state when
    /// undeclared, never <c>error-message</c> — the shared fold routes the
    /// unreadable-declaration arm here too), the optional authority/staleness
    /// lines, and the withdraw button (shown only while a region is declared). The
    /// input box re-seeds from the persisted declaration ONLY when it doesn't
    /// already match — so a withdrawal empties it rather than leaving the
    /// withdrawn code sitting in it looking declared, and an in-progress edit
    /// isn't clobbered by a stale re-render. Mirrors linux <c>set_nest_region</c> /
    /// apple's <c>onChange(of: vm.regionView?.declared)</c>.
    /// </summary>
    private void RenderRegion()
    {
        if (_vm is null) return;
        var declared = _vm.RegionDeclared ?? string.Empty;
        if (RegionInputBox.Text != declared)
        {
            RegionInputBox.Text = declared;
        }
        RegionStatusText.Text = _vm.RegionStatus;
        RegionAuthorityText.Text = _vm.RegionAuthority ?? string.Empty;
        RegionAuthorityText.Visibility =
            _vm.RegionAuthority is not null ? Visibility.Visible : Visibility.Collapsed;
        RegionStalenessText.Text = _vm.RegionStaleness ?? string.Empty;
        RegionStalenessText.Visibility =
            _vm.RegionStaleness is not null ? Visibility.Visible : Visibility.Collapsed;
        RegionWithdrawButton.Visibility =
            _vm.RegionCanWithdraw ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>
    /// admin-nest-region-save-button → validate the typed code client-side via the
    /// shared <c>FaunaFfiMethods.AdminParseRegionCode</c> (the serving-port shape:
    /// invalid → <c>error-message</c>, no dispatch — never even case-folds,
    /// DECLARED NEVER DETECTED), then declare via the VM. No
    /// <c>ConfigureAwait(false)</c> — off-thread bound-state mutation throws a
    /// silent COMException (the windows VM rule).
    /// </summary>
    private async void RegionSaveButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        string code;
        try
        {
            code = FaunaFfiMethods.AdminParseRegionCode(RegionInputBox.Text);
        }
        catch
        {
            RenderError(S.Get("admin/nest_page/region_invalid"));
            return;
        }
        await _vm.SetRegionAsync(code);
        RenderRegion();
        RenderError(_vm.Error);
    }

    /// <summary>
    /// admin-nest-region-withdraw-button → <c>fauna.admin.region.set</c> with the
    /// region absent; nothing to validate. Withdrawing also retires the previous
    /// region's feature-policy document nest-side.
    /// </summary>
    private async void RegionWithdrawButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.SetRegionAsync(null);
        RenderRegion();
        RenderError(_vm.Error);
    }

    // ── NAT mode (fauna.setup.nat_mode via the shared AdminNatModeMachine,
    //     admin.md § Nest → NAT-mode control) ─────────────────────────────────

    /// <summary>
    /// Lazily build the <see cref="AdminNatModeViewModel"/> over the raw
    /// <c>(nest_url, secret_hex)</c> session pair — this machine rides the
    /// pre-identity transport, not the bearer <see cref="INestRpcClient"/> seam,
    /// so it needs the same local creds source as <see cref="RunFactoryResetAsync"/>
    /// — then hydrate it. A missing creds pair (should not happen for an already
    /// admin-authenticated session) surfaces via the caller's try/catch, same as
    /// every other Page_Loaded failure.
    /// </summary>
    private async Task LoadNatModeAsync()
    {
        if (_natVm is null)
        {
            var url = _clients?.Account.NestUrl
                ?? throw new InvalidOperationException("nest URL not available");
            var secretHex = _clients?.Account.SecretHex
                ?? throw new InvalidOperationException("identity secret not available");
            _natVm = new AdminNatModeViewModel(new AdminNatModeMachine(url, secretHex));
        }
        await _natVm.HydrateCommand.ExecuteAsync(null);
    }

    /// <summary>Reflectively sync the two radios + status + save button from the
    /// VM, guarding the programmatic <c>IsChecked</c> set against re-dispatch
    /// (mirrors <c>NatModeChoiceView.SyncRadios</c>).</summary>
    private void RenderNatMode()
    {
        if (_natVm is null) return;
        _syncingNatMode = true;
        if (NatModePublicRadio.IsChecked != _natVm.PublicSelected)
            NatModePublicRadio.IsChecked = _natVm.PublicSelected;
        if (NatModePrivateRadio.IsChecked != _natVm.PrivateSelected)
            NatModePrivateRadio.IsChecked = _natVm.PrivateSelected;
        _syncingNatMode = false;
        NatModeStatusText.Text = _natVm.StatusText;
        NatModeSaveButton.IsEnabled = _natVm.SubmitEnabled;
    }

    private void NatModePublicRadio_Checked(object sender, RoutedEventArgs e)
    {
        if (_syncingNatMode || _natVm is null) return;
        _natVm.Select(NodeMode.Public);
        RenderNatMode();
    }

    private void NatModePrivateRadio_Checked(object sender, RoutedEventArgs e)
    {
        if (_syncingNatMode || _natVm is null) return;
        _natVm.Select(NodeMode.Private);
        RenderNatMode();
    }

    /// <summary>admin-nest-nat-mode-save-button → sign + commit the selected
    /// mode via the VM. No defer button — navigating away is the defer.</summary>
    private async void NatModeSaveButton_Click(object sender, RoutedEventArgs e)
    {
        if (_natVm is null) return;
        await _natVm.SubmitCommand.ExecuteAsync(null);
        RenderNatMode();
    }

    // ── Deployment-identity rotation (box-recovery.md § Deployment-seed
    //     rotation) ────────────────────────────────────────────────────────────

    /// <summary>admin-nest-seed-rotate-button → arm the ceremony via the VM
    /// (paints Loading synchronously, then resolves to Ready/Failed).</summary>
    private async void SeedRotateArmButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        // Calling (not yet awaiting) runs the VM's synchronous arm — it sets
        // Stage to Loading before its own first await — so the render right
        // after this line already paints the confirm surface (disabled while
        // the roster is unknown, never absent), the same frame the click's
        // Invoke() returns on; the awaited roster then resolves it.
        var arming = _vm.ArmSeedRotateAsync();
        RenderSeedRotate();
        await arming;
        RenderSeedRotate();
    }

    /// <summary>admin-nest-seed-rotate-cancel-button → disarm, touching
    /// nothing nest-side.</summary>
    private void SeedRotateCancelButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.CancelSeedRotate();
        RenderSeedRotate();
    }

    /// <summary>admin-nest-seed-rotate-confirm-button → dispatch the ceremony.
    /// The VM disarms synchronously before its own await, so the surface is
    /// already gone by the time this handler's first await point is reached;
    /// re-rendering after the awaited call paints the verdict. This call
    /// outlives the click by design (box-recovery.md's ceremony survives the
    /// box's mid-flight serving-generation restart) — no timeout, no
    /// cancellation tied to this handler.</summary>
    private async void SeedRotateConfirmButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        // Calling (not yet awaiting) runs the VM's synchronous disarm — it sets
        // Stage back to None and paints "working…" before its own first await
        // — so the render right after this line already shows the disarmed
        // surface, the same frame the click's Invoke() returns on.
        var ceremony = _vm.ConfirmSeedRotateAsync();
        RenderSeedRotate();
        await ceremony;
        RenderSeedRotate();
    }

    /// <summary>
    /// Reflectively render the rotation ceremony from the VM: the confirm box
    /// is present ONLY while armed (Stage != None — absent, not merely
    /// disabled, when un-armed); the roster rows are rebuilt from
    /// <see cref="AdminNestViewModel.SeedRotateRoster"/> (painted ONLY in the
    /// Ready stage — box-recovery.md's ordering rule forbids an empty list
    /// beside a live confirm); the reason line shows only when the VM carries
    /// one; the confirm button's enabled state is <c>view.can_confirm</c>
    /// verbatim; the status line shows only once a ceremony has been
    /// attempted. Mirrors linux's <c>render_seed_rotate</c>.
    /// </summary>
    private void RenderSeedRotate()
    {
        if (_vm is null) return;
        bool armed = _vm.SeedRotateStage != AdminNestViewModel.SeedRotateArmStage.None;
        SeedRotateConfirmBox.Visibility = armed ? Visibility.Visible : Visibility.Collapsed;

        SeedRotateRosterBox.Children.Clear();
        for (int i = 0; i < _vm.SeedRotateRoster.Count; i++)
        {
            var row = new TextBlock
            {
                Text = _vm.SeedRotateRoster[i].Label,
                TextWrapping = TextWrapping.Wrap,
            };
            AutomationProperties.SetAutomationId(row, $"admin-nest-seed-rotate-roster-item-{i}");
            AutomationProperties.SetName(row, _vm.SeedRotateRoster[i].Label);
            SeedRotateRosterBox.Children.Add(row);
        }

        SeedRotateReasonText.Text = _vm.SeedRotateReasonText ?? string.Empty;
        SeedRotateReasonText.Visibility =
            _vm.SeedRotateReasonText is not null ? Visibility.Visible : Visibility.Collapsed;
        SeedRotateConfirmButton.IsEnabled = _vm.SeedRotateCanConfirm;

        SeedRotateStatusText.Text = _vm.SeedRotateStatus ?? string.Empty;
        SeedRotateStatusText.Visibility =
            _vm.SeedRotateStatus is not null ? Visibility.Visible : Visibility.Collapsed;
    }

    // ── Legal takedown console (moderation.md § Legal takedown →
    //     Invocation surface) ────────────────────────────────────────────────

    /// <summary>admin-nest-takedown-content-id-input → push the text into the VM,
    /// whose own change hook re-folds the shared form view and repaints the arm
    /// control. Per keystroke, like linux's <c>connect_changed</c>.</summary>
    private void TakedownContentIdInput_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_vm is null) return;
        _vm.TakedownContentId = TakedownContentIdInput.Text;
        RenderTakedown();
    }

    /// <summary>admin-nest-takedown-reference-input → same, for the citation.</summary>
    private void TakedownReferenceInput_TextChanged(object sender, TextChangedEventArgs e)
    {
        if (_vm is null) return;
        _vm.TakedownReference = TakedownReferenceInput.Text;
        RenderTakedown();
    }

    /// <summary>admin-nest-takedown-type-{post,conversation}-radio → which kind of
    /// content the obligation names. Only the newly-CHECKED radio raises, so
    /// reading the conversation radio is enough to know the pair's state.</summary>
    private void TakedownType_Changed(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.TakedownConversation = TakedownTypeConversationRadio.IsChecked == true;
        RenderTakedown();
    }

    /// <summary>admin-nest-takedown-restore-checkbox → overturn instead of issue.
    /// Both Checked and Unchecked route here: the arm label and the citation
    /// requirement both flip with it.</summary>
    private void TakedownRestore_Changed(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.TakedownRestore = TakedownRestoreCheckbox.IsChecked == true;
        RenderTakedown();
    }

    /// <summary>admin-nest-takedown-button → capture the form and paint the named
    /// confirm. The VM re-gates on the shared <c>can_submit</c>, so a press that
    /// should not arm arms nothing even if the chrome allowed it.</summary>
    private void TakedownArmButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.ArmTakedown();
        RenderTakedown();
    }

    /// <summary>admin-nest-takedown-cancel-button → disarm, touching nothing
    /// nest-side.</summary>
    private void TakedownCancelButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.CancelTakedown();
        RenderTakedown();
    }

    /// <summary>admin-nest-takedown-confirm-button → dispatch. The VM disarms
    /// synchronously before its own first await, so re-rendering immediately
    /// (before awaiting) is what makes the confirm control ABSENT the moment it is
    /// pressed — which the journey asserts as disarm-before-dispatch.</summary>
    private async void TakedownConfirmButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        var dispatch = _vm.ConfirmTakedownAsync();
        RenderTakedown();
        await dispatch;
        RenderTakedown();
    }

    /// <summary>
    /// Paint the console from the VM: the arm control's label and enabled state
    /// are the shared fold's <c>arm_label</c> / <c>can_submit</c> verbatim; the
    /// reason line shows only while the VM carries one; the confirm box is present
    /// ONLY while armed (absent, not merely disabled); the status line shows only
    /// once a dispatch has been attempted. Mirrors linux's
    /// <c>refresh_takedown_arm</c> + <c>render_takedown</c>.
    /// </summary>
    private void RenderTakedown()
    {
        if (_vm is null) return;

        TakedownArmButton.Content = _vm.TakedownArmLabel ?? string.Empty;
        // Kept ENABLED with the guard in the VM would be wrong here: this control
        // has a real confirm behind it, and ui.yaml's contract for the journey is
        // that a citation-less takedown is UN-ARMABLE — the e2e asserts
        // `is_enabled` is false. The VM re-gates anyway (belt and braces).
        TakedownArmButton.IsEnabled = _vm.TakedownCanSubmit;

        TakedownReasonText.Text = _vm.TakedownBlockedReason ?? string.Empty;
        TakedownReasonText.Visibility =
            _vm.TakedownBlockedReason is not null ? Visibility.Visible : Visibility.Collapsed;

        TakedownConfirmBox.Visibility = _vm.TakedownArmed ? Visibility.Visible : Visibility.Collapsed;
        TakedownConfirmSummaryText.Text = _vm.TakedownConfirmSummary ?? string.Empty;
        TakedownConfirmButton.Content = _vm.TakedownConfirmLabel ?? string.Empty;

        TakedownStatusText.Text = _vm.TakedownStatus ?? string.Empty;
        TakedownStatusText.Visibility =
            _vm.TakedownStatus is not null ? Visibility.Visible : Visibility.Collapsed;
    }

    // ── Outside-app sign-in keys (admin-nest-oauth-*, authorization-server.md
    //    § The issuer → Two rotation arms) ─────────────────────────────────────

    /// <summary>admin-nest-oauth-rotate-button → the ordinary rotation via the
    /// VM (which disarms any forced confirm first, then dispatches and
    /// re-reads the key set in one state update).</summary>
    private async void OauthRotateButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.RotateIssuerKeyAsync();
        RenderOauth();
    }

    /// <summary>admin-nest-oauth-force-rotate-button → arm the issuer-key
    /// forced confirm. Re-gates <c>OauthConfirmButton</c> to THIS arm's own
    /// kind, spelled as a literal at this call site — the shared confirm
    /// button paints two different ceremonies, and <c>OfflineGate.Declare</c>'s
    /// documented shape for that is "declare it again after the paint
    /// decides".</summary>
    private void OauthForceRotateButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.OpenOauthForcedConfirm(FfiIssuerForcedArm.IssuerKey);
        OauthConfirmButton.FaunaGate("fauna.oauth.force_rotate_issuer_key");
        RenderOauth();
    }

    /// <summary>admin-nest-oauth-secret-force-rotate-button → arm the
    /// session-secret forced confirm, this arm's sibling.</summary>
    private void OauthSecretForceRotateButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.OpenOauthForcedConfirm(FfiIssuerForcedArm.SessionSecret);
        OauthConfirmButton.FaunaGate("fauna.oauth.force_rotate_session_secret");
        RenderOauth();
    }

    /// <summary>admin-nest-oauth-cancel-button → disarm, touching nothing.</summary>
    private void OauthCancelButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        _vm.CancelOauthForced();
        RenderOauth();
    }

    /// <summary>admin-nest-oauth-confirm-button → dispatch the armed arm. The
    /// confirm surface (including this button) is gone by the time this
    /// handler's first await point is reached — the VM disarms synchronously
    /// before its own await, the same shape as
    /// <see cref="SeedRotateConfirmButton_Click"/>.</summary>
    private async void OauthConfirmButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || _vm.OauthArmedArm is not { } arm) return;
        var ceremony = _vm.ConfirmOauthForcedAsync(arm);
        RenderOauth();
        await ceremony;
        RenderOauth();
    }

    /// <summary>
    /// Reflectively render the outside-app sign-in keys section from the VM:
    /// the key rows are rebuilt from <see cref="AdminNestViewModel.OauthKeyRows"/>
    /// (painted ONLY from an answered read — ui.yaml's "never an empty list
    /// that would read as 'no keys'" rule); the reason line shows only while
    /// the read hasn't answered; the rotate cost shows only once it has; all
    /// three sign-in-key controls are enabled exactly when
    /// <see cref="AdminNestViewModel.OauthControlsLive"/>; the confirm box is
    /// present ONLY while armed; the status line shows only once a control
    /// has been used. Mirrors <see cref="RenderSeedRotate"/>.
    /// </summary>
    private void RenderOauth()
    {
        if (_vm is null) return;

        OauthKeyRowsBox.Children.Clear();
        for (int i = 0; i < _vm.OauthKeyRows.Count; i++)
        {
            var row = new TextBlock
            {
                Text = _vm.OauthKeyRows[i],
                TextWrapping = TextWrapping.Wrap,
            };
            AutomationProperties.SetAutomationId(row, $"admin-nest-oauth-key-item-{i}");
            AutomationProperties.SetName(row, _vm.OauthKeyRows[i]);
            OauthKeyRowsBox.Children.Add(row);
        }

        OauthKeyReasonText.Text = _vm.OauthKeyReason ?? string.Empty;
        OauthKeyReasonText.Visibility =
            _vm.OauthKeyReason is not null ? Visibility.Visible : Visibility.Collapsed;

        OauthRotateCostText.Text = _vm.OauthRotateCost ?? string.Empty;
        OauthRotateCostText.Visibility =
            _vm.OauthRotateCost is not null ? Visibility.Visible : Visibility.Collapsed;

        bool live = _vm.OauthControlsLive;
        OauthRotateButton.IsEnabled = live;
        OauthForceRotateButton.IsEnabled = live;
        OauthSecretForceRotateButton.IsEnabled = live;

        OauthConfirmBox.Visibility = _vm.OauthConfirmArmed ? Visibility.Visible : Visibility.Collapsed;
        OauthConfirmSummaryText.Text = _vm.OauthConfirmSummary ?? string.Empty;
        OauthConfirmButton.Content = _vm.OauthConfirmLabel ?? string.Empty;

        OauthStatusText.Text = _vm.OauthStatus ?? string.Empty;
        OauthStatusText.Visibility =
            _vm.OauthStatus is not null ? Visibility.Visible : Visibility.Collapsed;
    }

    // ── Factory reset (Danger zone — moved off admin-settings) ───────────────

    /// <summary>
    /// admin-factory-reset-button → a confirm <see cref="ContentDialog"/> whose
    /// confirm control carries the <c>admin-factory-reset-confirm-button</c>
    /// AutomationId (placed in the dialog's content so the id is directly
    /// queryable by the e2e bridge; Cancel rides the dialog's close button).
    /// On confirm, run the reset. Per mail-bridge-lifecycle.md § Factory reset.
    /// </summary>
    private async void FactoryResetButton_Click(object sender, RoutedEventArgs e)
    {
        var confirmButton = new Button
        {
            Content = S.Get("admin/settings_page/factory_reset_confirm_button"),
            HorizontalAlignment = HorizontalAlignment.Right,
        };
        AutomationProperties.SetAutomationId(confirmButton, Ids.AdminFactoryResetConfirmButton);

        var body = new TextBlock
        {
            Text = S.Get("admin/settings_page/factory_reset_confirm_body"),
            TextWrapping = TextWrapping.Wrap,
        };
        var panel = new StackPanel { Spacing = 16 };
        panel.Children.Add(body);
        panel.Children.Add(confirmButton);

        var dialog = new ContentDialog
        {
            XamlRoot = this.XamlRoot,
            Title = S.Get("admin/settings_page/factory_reset_confirm_title"),
            Content = panel,
            CloseButtonText = S.Get("admin/settings_page/factory_reset_cancel"),
        };
        confirmButton.Click += async (_, _) =>
        {
            dialog.Hide();
            await RunFactoryResetAsync();
        };
        await Controls.Dialogs.ShowAsync(dialog);
    }

    /// <summary>
    /// Call the Admin-gated <c>fauna.admin.factory_reset</c>, then hand off to
    /// <see cref="App.FactoryResetReonboardHandler"/> to tear down the session
    /// (keeping local creds — the box was wiped, not the client) and re-seed
    /// onboarding at <c>claim_code</c> with the code pre-filled (the human never sees
    /// it). The nest is mid-restart (~1-2s); the claim-code page's transient-retry
    /// covers the WS drop. Per mail-bridge-lifecycle.md § Factory reset.
    /// </summary>
    /// <remarks>
    /// Ordering is the whole of gap CR-1 (<c>nest/common.md</c> § Client-state
    /// recoverability): <b>mint + durably persist the claim code, then dispatch with
    /// it pinned.</b> The code used to exist only in the synchronous reply, so a
    /// client killed between dispatch and reply-render lost it — the box landed
    /// fresh/unclaimed but nobody could claim it. <c>MintAndPersistPendingFactoryReset</c>
    /// hands the code back only once the row is verifiably in the vault, so the
    /// crash-unsafe ordering is unrepresentable; if it cannot persist, we refuse to
    /// dispatch at all (a wipe against a code nobody holds is unrecoverable; refusing
    /// to start never is). A relaunch after a crash resumes the pre-filled claim via
    /// <c>LaunchWizardEntry.PendingFactoryReset</c> (see <c>App.xaml.cs</c>).
    /// </remarks>
    private async Task RunFactoryResetAsync()
    {
        if (_clients?.Rpc is null) return;
        try
        {
            var url = _clients.Account.NestUrl
                ?? throw new InvalidOperationException("nest URL not available");
            var secretHex = _clients.Account.SecretHex
                ?? throw new InvalidOperationException("identity secret not available");
            var handle = _clients.Account.Handle ?? "";
            // Re-qualify the bare cached localpart to localpart@domain before the
            // re-claim: the nest stores bare localparts and auto-registers the
            // primary mail domain from the handle's @domain at claim, so a
            // custom-domain box needs the qualified handle (mail-bridge-lifecycle.md
            // § Factory reset → re-claim handle sourcing). The qualification rule +
            // nest-URL-host parse live once in shared Rust — adopt the shared helper,
            // never re-derive inline (that would re-introduce the per-app drift).
            // Computed BEFORE the mint so the persisted row carries the handle the
            // re-claim will actually use.
            handle = FaunaOnboardingMachineMethods.QualifyReclaimHandle(
                handle, _clients.Account.Domain, url);

            // CR-1 step 1 — mint + persist, through the SAME LaunchPersistence the
            // launch machine branches on, so the row written here is the row the
            // relaunch routes on. The helper writes it, reads it back, and returns
            // null if it did not land (credential-store failures are swallowed, so
            // "saved" is a claim to verify, not to trust).
            //
            // Since CR-3 the row is PER-IDENTITY: the registry adapter writes it to
            // the ACTIVE account's slot, so on a multi-account install this admin's
            // outstanding reset can no longer overwrite another account's pending
            // claim code (nest/common.md § Client-state recoverability, CR-3). The
            // reads stay LIVE store reads through the adapter — the mint reads its
            // own write back, which a constructor-preloaded snapshot would defeat.
            using var registry = Services.CredentialStore.Registry();
            var pinnedCode = FaunaLaunchMachineMethods.MintAndPersistPendingFactoryReset(
                registry.LaunchPersistence(), url, handle);
            if (pinnedCode is null)
            {
                // Do NOT dispatch. Wiping the box against a code we failed to persist
                // is CR-1 again — and worse, because we would believe it was safe.
                // The nest is untouched.
                RenderError(S.Get("admin/settings_page/factory_reset_persist_failed"));
                return;
            }

            // CR-1 step 2 — dispatch with the (already-durable) code pinned. The nest
            // honors it verbatim, so this is the code the wiped box boots with; take
            // it from the reply anyway so the nest stays the authority.
            var claimCode = await _clients.Rpc.AdminFactoryResetAsync(pinnedCode);

            App.FactoryResetReonboardHandler?.Invoke(url, handle, claimCode, secretHex);
        }
        catch (Exception ex)
        {
            RenderError(Strings.Error(ex));
        }
    }

    // ── Error surface (page-level error-message, admin.md § N Nest) ──────────

    private void RenderError(string? msg)
    {
        if (string.IsNullOrEmpty(msg))
        {
            ErrorBar.IsOpen = false;
            App.CurrentErrorMessage = null;
        }
        else
        {
            ErrorBar.Message = msg;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = msg;
        }
    }
}
