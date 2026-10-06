using System;
using System.Collections.ObjectModel;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using uniffi.fauna_client_mail_settings;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// Admin page: mail-bridge approval + the approved-bridge roster
/// (`admin-bridges-pending`).
///
/// Dumb renderer of the shared <c>BridgeApprovalMachine</c>
/// (libs/fauna-client-mail-settings, exposed over UniFFI by
/// libs/fauna-ffi/src/mail_admin.rs) — builds the machine over the shared,
/// already-connected WS-RPC client (the <c>INestRpcClient</c> seam), hydrates,
/// renders one card per pending bridge plus the approved-bridge roster below, and
/// dispatches Approve / Reject / Rotate. Behavior: docs/goal/behavior/mail-bridge-lifecycle.md
/// § Pending approval + § Service-user re-keying; docs/goal/behavior/admin.md
/// § Approved-bridges roster. IDs: tests/e2e-unified/ui.yaml. Prior art: linux
/// apps/fauna-linux/src/views/admin.rs (build_pending_bridge_card,
/// build_approved_bridge_card, open_rotate_confirm).
/// </summary>
public sealed partial class AdminBridgesPendingPage : Page
{
    private ServiceClients? _clients;
    private BridgeApprovalMachine? _machine;
    private readonly ObservableCollection<PendingBridgeItem> _cards = new();
    private readonly ObservableCollection<ApprovedBridgeItem> _approved = new();

    /// <summary>Pubkey of the approved bridge the open rotate-confirm overlay acts on.</summary>
    private string? _rotateTarget;

    public AdminBridgesPendingPage()
    {
        this.InitializeComponent();
        CardsList.ItemsSource = _cards;
        ApprovedList.ItemsSource = _approved;
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _clients = clients;
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        await LoadAsync();
    }

    private async Task LoadAsync()
    {
        if (_clients is null) return;

        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            await EnsureMachineAsync();
            await _machine!.Hydrate();
            RenderSnapshot(_machine.Snapshot());
        }
        catch (Exception ex)
        {
            ShowError(Strings.Error(ex));
        }
        finally
        {
            LoadProgress.IsActive = false;
            LoadProgress.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>Build the machine over the shared, already-connected,
    /// auto-reconnecting WS-RPC client (the <see cref="INestRpcClient"/> seam;
    /// idempotent). Riding the shared client — rather than a one-shot per-page
    /// <c>FfiNestClient.Connect()</c> — means a transient WS blip recovers instead
    /// of pinning the page in an error.</summary>
    private async Task EnsureMachineAsync()
    {
        if (_machine is not null || _clients?.Rpc is null) return;
        _machine = await _clients.Rpc.BuildBridgeApprovalMachineAsync();
    }

    private void RenderSnapshot(BridgeApprovalSnapshot snap)
    {
        _cards.Clear();
        var roleCaption = S.Get("admin/bridges_pending/role");
        var pubkeyCaption = S.Get("admin/bridges_pending/pubkey");
        var sourceIpCaption = S.Get("admin/bridges_pending/source_ip");
        var firstSeenCaption = S.Get("admin/bridges_pending/first_seen");
        var sourceIpUnknown = S.Get("admin/bridges_pending/source_ip_unknown");
        var approveLabel = S.Get("admin/bridges_pending/approve");
        var rejectLabel = S.Get("admin/bridges_pending/reject");

        foreach (var v in snap.pending)
        {
            _cards.Add(new PendingBridgeItem
            {
                // Friendly per-role display name via the canonical shared Rust fn
                // `bridge_display_name` (fauna_client_mail_settings::bridge_approval;
                // bridges.md § Active bridges). Mirrors linux bridge_display_name +
                // CL1 MemberStatusLabel precedent — resolves LocalizedText through
                // the windows i18n runtime (priority #1/#2).
                DisplayName = S.Resolve(FaunaClientMailSettingsMethods.BridgeDisplayName(v.requestedRole)),
                RoleCaption = roleCaption,
                RequestedRole = v.requestedRole,
                PubkeyCaption = pubkeyCaption,
                PubkeyHex = v.pubkeyHex,
                SourceIpCaption = sourceIpCaption,
                SourceIp = v.sourceIp ?? sourceIpUnknown,
                FirstSeenCaption = firstSeenCaption,
                FirstSeenAt = FormatFirstSeen(v.firstSeenAt),
                ApproveLabel = approveLabel,
                RejectLabel = rejectLabel,
            });
        }

        EmptyText.Visibility = _cards.Count == 0 ? Visibility.Visible : Visibility.Collapsed;

        // Approved roster (admin.md § Approved-bridges roster) — the rotate surface.
        _approved.Clear();
        var approvedAtCaption = S.Get("admin/bridges_pending/approved_at");
        var rotateLabel = S.Get("admin/bridges_pending/rotate");
        foreach (var v in snap.approved)
        {
            _approved.Add(new ApprovedBridgeItem
            {
                DisplayName = S.Resolve(FaunaClientMailSettingsMethods.BridgeDisplayName(v.role)),
                RoleCaption = roleCaption,
                Role = v.role,
                PubkeyCaption = pubkeyCaption,
                PubkeyHex = v.pubkeyHex,
                ApprovedAtCaption = approvedAtCaption,
                // `approved_at` is optional (None on a pending row) → dash
                // placeholder, as linux does.
                ApprovedAt = v.approvedAt is { } ms ? FormatFirstSeen(ms) : sourceIpUnknown,
                RotateLabel = rotateLabel,
            });
        }
        ApprovedEmptyText.Visibility = _approved.Count == 0 ? Visibility.Visible : Visibility.Collapsed;

        if (snap.error is { } err)
        {
            ShowError(err);
        }
        else
        {
            ClearError();
        }
    }

    /// <summary>Format an epoch-millis timestamp via the shared ms door; renders the
    /// raw number out of range, never throws (value-formatting.md § Absolute local
    /// timestamp display).</summary>
    private static string FormatFirstSeen(ulong epochMillis) =>
        uniffi.fauna_ffi.FaunaFfiMethods.FormatUnixLocalMs((long)epochMillis);

    private async void ApproveButton_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null) return;
        if ((sender as FrameworkElement)?.DataContext is not PendingBridgeItem item) return;
        ((Button)sender).IsEnabled = false;
        try
        {
            await _machine.Dispatch(new BridgeApprovalAction.Approve(item.PubkeyHex, item.RequestedRole));
            RenderSnapshot(_machine.Snapshot());
        }
        catch (Exception ex)
        {
            ShowError(Strings.Error(ex));
        }
    }

    private async void RejectButton_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null) return;
        if ((sender as FrameworkElement)?.DataContext is not PendingBridgeItem item) return;
        ((Button)sender).IsEnabled = false;
        try
        {
            await _machine.Dispatch(new BridgeApprovalAction.Reject(item.PubkeyHex));
            RenderSnapshot(_machine.Snapshot());
        }
        catch (Exception ex)
        {
            ShowError(Strings.Error(ex));
        }
    }

    /// <summary>Open the `admin-bridges-rotate-confirm` overlay for this card
    /// (mail-bridge-lifecycle.md § Service-user re-keying). Mirrors linux
    /// open_rotate_confirm.</summary>
    private void RotateButton_Click(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not ApprovedBridgeItem item) return;
        _rotateTarget = item.PubkeyHex;
        RotateOverlay.Visibility = Visibility.Visible;
    }

    private void RotateCancel_Click(object sender, RoutedEventArgs e)
    {
        CloseRotateOverlay();
    }

    /// <summary>Confirm the rotation: dispatch the shared machine's Rotate action
    /// (→ `fauna.bridges.revoke_service_user`), which re-refreshes, so the rotated
    /// bridge drops out of the approved roster.</summary>
    private async void RotateConfirm_Click(object sender, RoutedEventArgs e)
    {
        if (_machine is null || _rotateTarget is not { } pubkeyHex) return;
        CloseRotateOverlay();
        try
        {
            await _machine.Dispatch(new BridgeApprovalAction.Rotate(pubkeyHex));
            RenderSnapshot(_machine.Snapshot());
        }
        catch (Exception ex)
        {
            ShowError(Strings.Error(ex));
        }
    }

    private void CloseRotateOverlay()
    {
        RotateOverlay.Visibility = Visibility.Collapsed;
        _rotateTarget = null;
    }

    private void ShowError(string msg)
    {
        ErrorBar.Message = msg;
        ErrorBar.IsOpen = true;
        App.CurrentErrorMessage = msg;
    }

    private void ClearError()
    {
        ErrorBar.IsOpen = false;
        App.CurrentErrorMessage = null;
    }
}

/// <summary>Row item for one pending-bridge approval card.</summary>
public sealed class PendingBridgeItem
{
    /// <summary>Friendly per-role display name (admin.md § Bridge display naming):
    /// "Mail &amp; calendar bridge" (mda) / "Mail bridge" (mta) / "Bridge".</summary>
    public string DisplayName { get; init; } = "";
    public string RoleCaption { get; init; } = "";
    public string RequestedRole { get; init; } = "";
    public string PubkeyCaption { get; init; } = "";
    public string PubkeyHex { get; init; } = "";
    public string SourceIpCaption { get; init; } = "";
    public string SourceIp { get; init; } = "";
    public string FirstSeenCaption { get; init; } = "";
    public string FirstSeenAt { get; init; } = "";
    public string ApproveLabel { get; init; } = "";
    public string RejectLabel { get; init; } = "";
}

/// <summary>Row item for one approved-bridge roster card
/// (`admin-bridges-approved-card`) — carries the rotate-service-user-key
/// affordance. Projected from the shared machine's
/// <c>BridgeApprovalSnapshot.approved</c>.</summary>
public sealed class ApprovedBridgeItem
{
    /// <summary>Friendly per-role display name — the same shared
    /// <c>bridge_display_name</c> map the pending card uses.</summary>
    public string DisplayName { get; init; } = "";
    public string RoleCaption { get; init; } = "";
    public string Role { get; init; } = "";
    public string PubkeyCaption { get; init; } = "";
    public string PubkeyHex { get; init; } = "";
    public string ApprovedAtCaption { get; init; } = "";
    public string ApprovedAt { get; init; } = "";
    public string RotateLabel { get; init; } = "";
}
