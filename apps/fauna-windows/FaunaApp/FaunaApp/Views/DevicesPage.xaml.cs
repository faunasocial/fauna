using System;
using System.Collections.Generic;
using System.Linq;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Services;
using FaunaApp.Sync;
using uniffi.fauna_devices_machine;
using uniffi.fauna_client_capabilities;
using uniffi.fauna_conversations;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// Settings → Devices sub-page (devices.md; ui.yaml § devices): the device ROSTER
/// only. A thin renderer over the shared-Rust <see cref="DevicesMachine"/>
/// (libs/fauna-devices-machine, via libs/fauna-ffi) — the same page-level machine
/// the Folders sub-page (<see cref="FoldersPage"/>) reads; this page renders only
/// the roster slice (<c>DevicesSnapshot.devices</c> + <c>.error</c>). The folder /
/// conflict / wizard slices are the Folders sub-page's concern (devices.md § State
/// &amp; data shape — the two pages each render their slice of one snapshot).
///
/// Split out of the former top-level SyncPage by the 2026-06-28 sync/folder UI
/// unification (spec §§ 1, 6). Both pages read the session's ONE machine
/// (<see cref="FaunaApp.Core.DevicesMachineHost"/>), like every other app's: a
/// machine per page visit lost the followed rows' last-read memory and reset the
/// refresh barrier on every navigation.
///
/// <para>⚠ No <c>ConfigureAwait(false)</c> — a WinUI page that mutates bound state
/// off the UI thread throws a silent <c>COMException</c>
/// (reference_windows_vm_configureawait_comexception).</para>
/// </summary>
public sealed partial class DevicesPage : Page
{
    private INestRpcClient? _rpc;
    private ICryptoService? _crypto;
    private ISessionAccount? _account;
    private ConversationsSession? _convSession;

    private DevicesMachine? _machine;
    private DevicesNotifyObserver? _observer;
    /// <summary>This page's listener on the session machine's observer fan-out,
    /// disposed in <see cref="OnNavigatedFrom"/>.</summary>
    private IDisposable? _listening;

    /// <summary>The T16 custody facet (devices.md § Custody facet, pieces 1–3 +
    /// the mint): the folded rows, the last gesture's error, the open mint
    /// flow and the keyless-posture marks — see <see cref="DevicesCustodyFacet"/>.</summary>
    private readonly DevicesCustodyFacet _custody = new();

    /// <summary>The standing enrollment notice (the tier device-cap refusal;
    /// ui/devices.md § Errors &amp; edge cases) and the two rules that decide what
    /// <c>error-message</c> shows — see <see cref="DevicesEnrollmentNotice"/>.</summary>
    private readonly DevicesEnrollmentNotice _enrollmentNotice = new();

    /// <summary>The roster row the This-device badge marks, or <c>null</c> when
    /// none is known yet (<see cref="LoadThisDeviceRowAsync"/>).</summary>
    private string? _thisDeviceRow;

    public DevicesPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _rpc = clients.Rpc;
            _crypto = clients.Crypto;
            _account = clients.Account;
            _convSession = clients.ConvSession;
        }
    }

    /// <summary>The machine outlives this page (Core.DevicesMachineHost); only the
    /// repaint is ours.</summary>
    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        _listening?.Dispose();
        _listening = null;
        base.OnNavigatedFrom(e);
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_rpc is null) return;

        if (_crypto is not null && _crypto.HasKey)
            PeerActorIdText.Text = _crypto.ActorIdHex;

        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            // The session's ONE machine, shared with the Folders page
            // (Core.DevicesMachineHost); this page listens while it is showing —
            // the observer ticks the UI thread → RenderPage.
            _observer = new DevicesNotifyObserver(RenderPage);
            _listening?.Dispose();
            _listening = DevicesMachineHost.Listen(_observer);
            _machine = await DevicesMachineHost.GetOrBuildAsync(_rpc, _convSession);
            await _machine.Refresh();
        }
        catch (Exception ex)
        {
            ErrorBar.Message = Strings.Error(ex);
            ErrorBar.IsOpen = true;
        }
        LoadProgress.IsActive = false;
        LoadProgress.Visibility = Visibility.Collapsed;
        RenderPage();

        // The account runtime's standing enrollment refusal and the row the
        // This-device badge marks — local slot reads, so they ride the same
        // per-nav hydrate the machine does.
        await LoadThisDeviceRowAsync();
        await LoadEnrollmentNoticeAsync();
        await LoadKeylessPostureAsync();
        await LoadCustodyAsync();
    }

    /// <summary>Re-read piece 1's keyless-posture marks for the roster's
    /// principals (a local read of the account store — the shared
    /// <c>keyless_posture</c> join), then repaint the roster. Rides the same
    /// per-nav hydrate as the This-device row: the posture changes when the
    /// account's generation tip moves, which no observer tick announces.</summary>
    private async Task LoadKeylessPostureAsync()
    {
        if (_rpc is null || _machine is null) return;
        var principals = _machine.Snapshot().devices.Select(d => d.@principal).ToList();
        await _custody.LoadKeylessPostureAsync(_rpc, principals);
        RenderPage();
    }

    /// <summary>Re-read the standing enrollment notice, then repaint — the
    /// load's own repaint, since the read lands after the first
    /// <see cref="RenderPage"/> and would otherwise wait for the next observer
    /// tick (mirrors <see cref="LoadCustodyAsync"/>'s trailing
    /// <see cref="RenderCustody"/>). <see cref="DevicesEnrollmentNotice.LoadAsync"/>
    /// never throws and keeps the held notice on a failed read.</summary>
    /// <summary>Re-read which roster row carries <c>device-this-mark-badge</c> —
    /// the row this machine's enrollment latched on, else the app's own id
    /// (behavior/devices.md § This-device marker; the rule is shared Rust behind
    /// <see cref="INestRpcClient.ThisDeviceRowAsync"/>). A failed read keeps the
    /// row already held. The next <see cref="RenderPage"/> (the notice load's
    /// trailing repaint) paints it.</summary>
    private async Task LoadThisDeviceRowAsync()
    {
        if (_rpc is null) return;
        try
        {
            _thisDeviceRow = await _rpc.ThisDeviceRowAsync(_account?.DeviceId);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("DevicesPage", $"[this-device] read failed: {ex.GetType().Name}: {ex.Message}");
        }
        // The machine's last fallback for which row is this device's (p2p.md §
        // Per-device participation → Which row is this device's): the id the
        // badge paints from, so the participation switch and the marker agree.
        _machine?.SetThisDeviceRow(_thisDeviceRow);
    }

    private async Task LoadEnrollmentNoticeAsync()
    {
        if (_rpc is null) return;
        await _enrollmentNotice.LoadAsync(_rpc);
        RenderPage();
    }

    // ── T16 custody facet (devices.md § Custody facet, pieces 1–3 + the mint) ──
    // A thin renderer over Core.ViewModels.DevicesCustodyFacet — the windows
    // twin of linux's CustodyView + run_custody_gesture: each control raises one
    // gesture, keyed by its GRANT ID (never a row index a refold can re-point),
    // which runs the shared run_custody_act and repaints from the re-folded
    // facet; the gesture's error rides RenderPage's error-message precedence so
    // a machine tick cannot wipe it.

    /// <summary>Fire a ceremony drive pass, then refold the custody facet —
    /// rides the page's own load edge, mirroring apple/android's
    /// <c>loadCustodyFacet</c>. Best-effort: a failure keeps the painted rows
    /// rather than blanking a live list (<see cref="DevicesCustodyFacet.LoadAsync"/>).</summary>
    private async Task LoadCustodyAsync()
    {
        if (_rpc is null) return;
        await _custody.LoadAsync(_rpc, _convSession);
        RenderCustody();
    }

    /// <summary>Repaint every custody family and the page's error-message after
    /// a gesture.</summary>
    private void AfterCustodyGesture()
    {
        RenderCustody();
        RenderPage();
    }

    /// <summary>One <c>custody-holder-card</c> row. <c>GrantId</c>/<c>Holder</c>
    /// are the revoke gesture's own args — never a row index, which a refold
    /// can re-point at a different custody.</summary>
    private record CustodyRow(
        byte[] GrantId, byte[]? Holder, string Name, string ReceiptStatusText,
        string HeldBytesText, bool Stale, bool Pending)
    {
        public Brush ReceiptStatusBrush => Stale
            ? (Brush)Application.Current.Resources["SystemFillColorCriticalBrush"]
            : (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"];

        // A pending ceremony has minted nothing to revoke, so the honest-bound
        // note would over-promise there (matches RevokeEnabled below).
        public Visibility RevokeBoundNoteVisibility => Pending ? Visibility.Collapsed : Visibility.Visible;

        // A control that cannot succeed is not offered: while the ceremony is
        // pending there is no minted grant to revoke and no bound holder to
        // name (the row's own custodian_key is null there).
        public bool RevokeEnabled => !Pending;
    }

    /// <summary>One <c>custody-held-card</c> row. <see cref="Draft"/> is the
    /// budget input's seed (the shared <c>budget_draft</c>), advanced to the
    /// committed text once a commit fires so the blur that follows an Enter
    /// never commits twice.</summary>
    private sealed record HeldRow(byte[] GrantId, string Owner, string Scope, string Bytes, bool Stopped)
    {
        public string Draft { get; set; } = "";
        public string StopLabel => Strings.Get(Stopped ? "devices/custody_stopped_bytes_remain" : "devices/custody_stop");
        public bool StopEnabled => !Stopped;
    }

    /// <summary>One <c>custody-offer-card</c>. <see cref="TargetIndex"/> is the
    /// target select's two-way answer (0 = this device, 1 = my nest).</summary>
    private sealed record OfferRow(byte[] GrantId, string Title, bool ShowsTarget)
    {
        public Visibility TargetVisibility => ShowsTarget ? Visibility.Visible : Visibility.Collapsed;
        public IReadOnlyList<string> TargetOptions { get; } = new[]
        {
            Strings.Get("devices/custody_offer_target_device"),
            Strings.Get("devices/custody_offer_target_nest"),
        };
        public int TargetIndex { get; set; }
    }

    /// <summary>The counterpart account, abbreviated through the SAME shared
    /// short id every app uses.</summary>
    private static string ShortActor(byte[] id)
        => uniffi.fauna_ffi.FaunaFfiMethods.ShortId(uniffi.fauna_ffi.FaunaFfiMethods.HexFull(id));

    private void RenderCustody()
    {
        var facet = _custody.Facet;
        RenderHolders(facet?.@rows ?? Array.Empty<CustodyHolderRowView>());

        var held = (facet?.@held ?? Array.Empty<CustodyHeldRowView>())
            .Select(h => new HeldRow(
                h.@grantId,
                S.Format("devices/custody_held_owner", ShortActor(h.@owner)),
                h.@scopes is { @wholeAccount: false } scoped
                    ? string.Join(", ", scoped.@scopes)
                    : Strings.Get("devices/custody_held_scope_account"),
                S.CustodyHeldBytesText(h.@receipt),
                h.@stopped)
            { Draft = S.Resolve(h.@budgetDraft) })
            .ToList();
        HeldList.ItemsSource = held;
        HeldSection.Visibility = held.Count == 0 ? Visibility.Collapsed : Visibility.Visible;

        var offers = (facet?.@offers ?? Array.Empty<CustodyOfferRowView>())
            .Select(o => new OfferRow(
                o.@grantId,
                S.Format("devices/custody_offer_title", ShortActor(o.@owner)),
                _custody.ShowsTargetSelect(o.@grantId)))
            .ToList();
        OfferList.ItemsSource = offers;
        OfferSection.Visibility = offers.Count == 0 ? Visibility.Collapsed : Visibility.Visible;

        if (_custody.MintCandidates is null) MintFlow.Visibility = Visibility.Collapsed;
    }

    private void RenderHolders(IReadOnlyList<CustodyHolderRowView> holders)
    {
        // Skip a row whose custodian_nest_url is set — that custody belongs
        // to the Nests page's nest-trust-custody-* family instead (the
        // nest-custodian identity fact, ruled 2026-08-17); one custody never
        // renders in both places.
        var rows = holders
            .Where(r => r.@custodianNestUrl is null)
            .Select(r => new CustodyRow(
                r.@grantId,
                r.@custodianKey,
                uniffi.fauna_ffi.FaunaFfiMethods.ShortId(uniffi.fauna_ffi.FaunaFfiMethods.HexFull(r.@host)),
                S.CustodyReceiptStatusText(r.@receipt),
                S.CustodyHeldBytesText(r.@receipt),
                r.@receiptState == CustodyReceiptStateView.Stale,
                r.@pending))
            .ToList();
        CustodyList.ItemsSource = rows;
        // Hidden entirely when empty — an account with no custodians has
        // nothing to say here, and a titled-but-empty section reads as a
        // feature that failed to load.
        CustodySection.Visibility = rows.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
    }

    private async void RevokeCustody_Click(object sender, RoutedEventArgs e)
    {
        if (_rpc is null || sender is not Button { Tag: CustodyRow row }) return;
        await _custody.RevokeAsync(_rpc, row.GrantId, row.Holder);
        AfterCustodyGesture();
    }

    // ── Piece 3: held-for-others controls ──

    private async void HeldBudget_KeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key != Windows.System.VirtualKey.Enter) return;
        e.Handled = true;
        await CommitBudgetAsync(sender);
    }

    private async void HeldBudget_LostFocus(object sender, RoutedEventArgs e) => await CommitBudgetAsync(sender);

    /// <summary>Commit the budget input's text through the shared parser. A
    /// blur with nothing typed (the seed still standing — a repaint's own focus
    /// shuffle included) is not a commit.</summary>
    private async Task CommitBudgetAsync(object sender)
    {
        if (_rpc is null || sender is not TextBox { Tag: HeldRow row } box) return;
        var typed = box.Text;
        if (typed == row.Draft) return;
        row.Draft = typed;
        await _custody.SetBudgetAsync(_rpc, row.GrantId, typed);
        AfterCustodyGesture();
    }

    private async void HeldStop_Click(object sender, RoutedEventArgs e)
    {
        if (_rpc is null || sender is not Button { Tag: HeldRow row }) return;
        await _custody.StopAsync(_rpc, row.GrantId);
        AfterCustodyGesture();
    }

    private async void HeldRemove_Click(object sender, RoutedEventArgs e)
    {
        if (_rpc is null || sender is not Button { Tag: HeldRow row }) return;
        await _custody.RemoveAsync(_rpc, row.GrantId);
        AfterCustodyGesture();
    }

    // ── Piece 3: the consent card ──

    private async void OfferAccept_Click(object sender, RoutedEventArgs e)
    {
        if (_rpc is null || sender is not Button { Tag: OfferRow row }) return;
        // No select rendered → the accept binds this device.
        var onNest = row.ShowsTarget && row.TargetIndex == 1;
        await _custody.AcceptAsync(_rpc, _convSession, row.GrantId, onNest);
        AfterCustodyGesture();
    }

    private async void OfferDecline_Click(object sender, RoutedEventArgs e)
    {
        if (_rpc is null || sender is not Button { Tag: OfferRow row }) return;
        await _custody.DeclineAsync(_rpc, row.GrantId);
        AfterCustodyGesture();
    }

    // ── Offer initiation (custody-mint-*) ──

    private void MintOpen_Click(object sender, RoutedEventArgs e)
    {
        if (_rpc is null) return;
        _custody.OpenMint(_rpc, _convSession);
        if (_custody.MintCandidates is { } candidates)
        {
            // Index 0 is the placeholder: confirm is live only once a real host
            // is chosen (a control that cannot succeed is not offered).
            var labels = new List<string> { Strings.Get("devices/custody_mint_host_placeholder") };
            labels.AddRange(candidates.Select(c => c.@label));
            MintHostSelect.ItemsSource = labels;
            MintHostSelect.SelectedIndex = 0;
            MintConfirm.IsEnabled = false;
            MintFlow.Visibility = Visibility.Visible;
        }
        AfterCustodyGesture();
    }

    private void MintHostSelect_SelectionChanged(object sender, SelectionChangedEventArgs e)
        => MintConfirm.IsEnabled = MintHostSelect.SelectedIndex >= 1;

    private async void MintConfirm_Click(object sender, RoutedEventArgs e)
    {
        var index = MintHostSelect.SelectedIndex - 1;
        if (_rpc is null || _custody.MintCandidates is not { } candidates || index < 0 || index >= candidates.Count)
        {
            ShellLog.Warn("DevicesPage", $"[custody-mint] confirm dropped: index={index}");
            return;
        }
        await _custody.MintAsync(_rpc, _convSession, candidates[index]);
        AfterCustodyGesture();
    }

    private void MintCancel_Click(object sender, RoutedEventArgs e)
    {
        _custody.CloseMint();
        MintFlow.Visibility = Visibility.Collapsed;
    }

    // ── Observer-driven render: only the roster slice ──

    private void RenderPage()
    {
        string? machineError = null;
        if (_machine is not null)
        {
            var snap = _machine.Snapshot();
            RenderDevices(snap.devices, snap.ownP2pParticipation);
            machineError = snap.error is { } err ? S.Resolve(err) : null;
        }
        else if (_custody.Error is null)
        {
            // No machine (its build failed and Page_Loaded painted why) and no
            // custody gesture to answer: leave that error standing.
            return;
        }

        // Page-level error-message: the machine localizes the last read/gesture
        // failure into snapshot.error; a subsequent successful gesture clears it.
        // A custody gesture's error comes next (DevicesCustodyFacet.Error — held
        // until the next custody gesture succeeds), then the standing enrollment
        // notice as the FALLBACK. All three are folded in here rather than
        // painted by a second writer: this runs on every observer tick, so an
        // error painted anywhere else would be wiped by the very next repaint.
        // Only the slot no longer recording the refusal takes the notice down
        // (ui/devices.md § Errors & edge cases).
        var msg = _enrollmentNotice.MessageFor(machineError ?? _custody.Error);
        if (msg is not null)
        {
            ErrorBar.Message = msg;
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = msg;
        }
        else
        {
            ErrorBar.IsOpen = false;
            App.CurrentErrorMessage = null;
        }
    }

    /// <summary>One roster row. <paramref name="GuardianMarked"/> is the additive
    /// <c>fauna.sync.devices.list</c> flag (family-safety.md § Full visibility,
    /// Slice F) — the ward's OWN list renders the guardian's enrolled device, and
    /// only the guardian can clear it. <paramref name="ThisMarked"/> is true for the
    /// row this machine ENROLLED on (<see cref="_thisDeviceRow"/>; the shared rule
    /// falls back to this app's own locally-stored id) — removes the
    /// hex-comparison a user used to do by eye (devices.md; ID user-approved
    /// 2026-08-11). <paramref name="FolderRoleBadges"/> is one resolved label per
    /// folder this device carries (`device-folder-role-badge`, devices.md § Where
    /// logic lives) — empty when the device carries none, which the ItemsControl
    /// renders as nothing. <paramref name="Participation"/> is
    /// <c>device-p2p-participation-toggle</c>'s paint
    /// (<see cref="DeviceParticipationPaint"/>). <paramref name="Keyless"/> is
    /// piece 1's <c>device-keyless-posture-badge</c> (the shared
    /// <c>keyless_posture</c> join, <see cref="DevicesCustodyFacet.IsKeyless"/>).
    /// Value-only consume, so the Visibility is a computed get-only on the row
    /// (the windows row-record convention) rather than a new XAML converter
    /// type.</summary>
    private record DeviceRow(
        string Name, string Status, int Index, bool GuardianMarked, bool ThisMarked,
        IReadOnlyList<string> FolderRoleBadges, DeviceParticipationPaint Participation, bool Keyless)
    {
        public Visibility GuardianBadgeVisibility
            => GuardianMarked ? Visibility.Visible : Visibility.Collapsed;
        public Visibility ThisMarkVisibility
            => ThisMarked ? Visibility.Visible : Visibility.Collapsed;
        public Visibility KeylessVisibility
            => Keyless ? Visibility.Visible : Visibility.Collapsed;
        public bool ParticipationChecked => Participation.Checked;
        public string ParticipationLabel => Strings.Get(Participation.LabelKey);
        public bool ParticipationActionable => Participation.Actionable;
    }

    private void RenderDevices(IReadOnlyList<DeviceSummary> devices, bool? ownParticipation)
    {
        var thisRow = _thisDeviceRow;
        var rows = devices
            // device-status label single-sourced in shared Rust
            // (fauna_core::format::device_status_label; devices.md § Where logic lives) —
            // drops the hand-rolled untranslated English ternary (priority #1). The status
            // dot/indicator color stays per-app.
            .Select((d, i) =>
            {
                var own = thisRow is not null && d.deviceId == thisRow;
                return new DeviceRow(
                    d.label,
                    S.Resolve(uniffi.fauna_ffi.FaunaFfiMethods.DeviceStatusLabel(d.online)),
                    i,
                    d.guardianMarked,
                    own,
                    // device-folder-role-badge: one label per folder this device carries,
                    // stating its place (fauna_core::format::device_place_label; devices.md §
                    // Where logic lives) — composed from the SAME place labels the create
                    // wizard's checkboxes carry. Its template args are themselves i18n keys,
                    // so it goes through the NESTED resolver (priority #1).
                    d.folders
                        .Select(fs => S.ResolveNested(uniffi.fauna_ffi.FaunaFfiMethods.DevicePlaceLabel(
                            fs.originates, fs.accepts, fs.appliesDeletes)))
                        .ToList(),
                    DeviceParticipationPaint.For(d, own, ownParticipation),
                    _custody.IsKeyless(d.@principal));
            })
            .ToList();
        DevicesList.ItemsSource = rows;
        NoDevicesText.Visibility = rows.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private async void RemoveDevice_Click(object sender, RoutedEventArgs e)
    {
        // A click that cannot reach the machine says so (e2e convention 11):
        // a silent return here reads exactly like a removal that hung.
        if (_machine is null || sender is not Button b || b.Tag is not int index)
        {
            ShellLog.Warn("DevicesPage", $"[remove] click dropped: machine={_machine is not null} tag={(sender as Button)?.Tag?.GetType().Name ?? "null"}");
            return;
        }
        await _machine.RemoveDevice((uint)index);
    }

    /// <summary><c>device-p2p-participation-toggle[i]</c> — ask the machine to flip
    /// the row it paints: <c>SetP2pParticipation(index, !checked)</c>. Which arm
    /// that takes (this device's own switch, or a request that a sibling turn off)
    /// is the machine's decision; the row this app names as its own is handed in
    /// first, as FaunaKit's <c>DevicesMachineVM.setP2pParticipation</c> does. The
    /// CheckBox has already flipped itself locally; the repaint after the
    /// gesture's re-read puts it back to what the machine holds, so a refused
    /// gesture (painted on <c>error-message</c>) never leaves a false tick.</summary>
    private async void ParticipationToggle_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null || sender is not CheckBox { Tag: int index, DataContext: DeviceRow row })
        {
            ShellLog.Warn("DevicesPage", $"[p2p-participation] click dropped: machine={_machine is not null} tag={(sender as CheckBox)?.Tag?.GetType().Name ?? "null"}");
            return;
        }
        _machine.SetThisDeviceRow(_thisDeviceRow);
        await _machine.SetP2pParticipation((uint)index, !row.ParticipationChecked);
        RenderPage();
    }

    // ── Copy buttons ──

    private void CopyPeerActorId_Click(object sender, RoutedEventArgs e)
    {
        if (PeerActorIdText.Text != "--") FaunaApp.Helpers.ClipboardHelper.CopyText(PeerActorIdText.Text);
    }
}
