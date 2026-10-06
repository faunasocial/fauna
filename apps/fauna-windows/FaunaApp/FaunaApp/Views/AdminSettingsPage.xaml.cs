using System.ComponentModel;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views;

/// <summary>
/// Admin settings page (admin.md § 3 Settings — renamed "Tiers" per the
/// per-page-services redesign, 2026-06-04). A dumb renderer of the shared
/// <see cref="AdminSettingsViewModel"/> (FaunaApp.Core, over the
/// <see cref="INestRpcClient"/> WS-RPC seam → <c>FfiAdminClient</c>): the
/// tier-*definition* in-place cap editor (<c>admin-settings-tiers-section</c>).
/// The Factory Reset danger zone moved to the new <c>admin-nest</c> page
/// (admin.md § N Nest, which also carried the storage-mode indicator until it
/// was retired entirely with the no-modes cutover, Phase-4 S8.7); invite-code
/// minting moved to the admin-users hub (§ 2); email-domain management moved to
/// admin-dns (§ 3).
/// </summary>
public sealed partial class AdminSettingsPage : Page
{
    private AdminSettingsViewModel? _vm;

    public AdminSettingsPage()
    {
        this.InitializeComponent();
    }

    // Per-row tier-cap input labels + the save-button label (admin.md § 3 in-place
    // tier-cap editing). Static so the DataTemplate can {x:Bind} them once — the
    // controls live inside the ItemsControl template, where the page-level
    // {loc:Localize} markup resolves against the row DataType, not the page, so the
    // app-string keys are surfaced through these static page properties instead.
    public static string CapInboxLabel => S.Get("admin/settings_page/cap_inbox_bytes");
    public static string CapStorageLabel => S.Get("admin/settings_page/cap_storage_bytes");
    public static string CapDevicesLabel => S.Get("admin/settings_page/cap_devices");
    public static string CapBlobSizeLabel => S.Get("admin/settings_page/cap_blob_size");
    public static string CapFeedsLabel => S.Get("admin/settings_page/cap_feeds");
    public static string SaveTierLabel => S.Get("admin/settings_page/save_tier");

    // Membership designation row labels (monetization.md § Pillar 4), surfaced the
    // same way and for the same reason as the cap labels above.
    public static string MembershipAdmitsAtLabel => S.Get("admin/settings_page/membership_admits_at");
    public static string MembershipLapsesToLabel => S.Get("admin/settings_page/membership_lapses_to");
    public static string MembershipSaveLabel => S.Get("admin/settings_page/membership_save");
    public static string MembershipClearLabel => S.Get("admin/settings_page/membership_clear");

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            if (_vm is null)
            {
                _vm = new AdminSettingsViewModel(clients.Rpc!);
                _vm.PropertyChanged += ViewModel_PropertyChanged;
                _vm.Tiers.CollectionChanged += (_, _) => UpdateEmptyState();
                _vm.MembershipTiers.CollectionChanged += (_, _) => UpdateEmptyState();
            }
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        TiersList.ItemsSource = _vm.Tiers;
        MembershipList.ItemsSource = _vm.MembershipTiers;
        await _vm.LoadCommand.ExecuteAsync(null);
        UpdateEmptyState();
    }

    private void ViewModel_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (_vm is null) return;
        switch (e.PropertyName)
        {
            case nameof(AdminSettingsViewModel.IsLoading):
                LoadProgress.IsActive = _vm.IsLoading;
                LoadProgress.Visibility = _vm.IsLoading ? Visibility.Visible : Visibility.Collapsed;
                break;
            case nameof(AdminSettingsViewModel.ErrorMessage):
                if (_vm.ErrorMessage is { } msg)
                {
                    ShowError(msg);
                }
                else
                {
                    ErrorBar.IsOpen = false;
                    App.CurrentErrorMessage = null;
                }
                break;
        }
    }

    private void UpdateEmptyState()
    {
        NoTiersText.Visibility = (_vm?.Tiers.Count ?? 0) == 0 ? Visibility.Visible : Visibility.Collapsed;
        // No owned subscription tiers ⇒ nothing to designate. That is the normal
        // out-of-the-box state (nothing about the nest is monetized), so it shows the
        // "create one in your Tiers tab" pointer, never an error — designating
        // creates neither kind of tier (monetization.md § Pillar 4).
        NoMembershipTiersText.Visibility =
            (_vm?.MembershipTiers.Count ?? 0) == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>
    /// A per-row <c>admin-settings-tier-save-button</c> → persist that tier's edited
    /// caps via <c>fauna.admin.tiers.update</c> then refetch (admin.md § 3 in-place
    /// tier-cap editing). The button's DataContext is the row; the two-way-bound cap
    /// inputs have already pushed their text into it. Mirrors the per-row Click
    /// handlers on AdminUsersPage; errors surface via the VM's ErrorMessage.
    /// </summary>
    private async void SaveTier_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        if (sender is FrameworkElement { DataContext: AdminTierRow row })
        {
            await _vm.SaveTierAsync(row);
        }
    }

    /// <summary>
    /// A per-row <c>admin-settings-membership-save-button</c> → designate or re-point
    /// that row via <c>fauna.admin.membership_tiers.set</c> (an upsert), then refetch
    /// (monetization.md § Pillar 4). The button's DataContext is the row; the
    /// two-way-bound selects have already pushed their picks into it, so the VM sends
    /// the row's current selection. Errors surface via the VM's ErrorMessage — a
    /// tier the admin doesn't own is <c>fauna.admin.not_found</c>, an unknown quota
    /// tier is <c>fauna.admin.invalid_params</c>.
    /// </summary>
    private async void SaveMembership_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        if (sender is FrameworkElement { DataContext: MembershipTierRow row })
        {
            await _vm.SaveMembershipAsync(row);
        }
    }

    /// <summary>
    /// A per-row <c>admin-settings-membership-clear-button</c> → drop that row's
    /// designation via <c>fauna.admin.membership_tiers.clear</c>, then refetch. The
    /// subscription tier itself survives; it reverts to an ordinary content tier.
    /// The button stays enabled on an undesignated row (a disabled WinUI button can't
    /// be driven by InvokePattern), so clearing an undesignated row is a harmless
    /// no-op rather than a blocked interaction.
    /// </summary>
    private async void ClearMembership_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        if (sender is FrameworkElement { DataContext: MembershipTierRow row })
        {
            await _vm.ClearMembershipAsync(row);
        }
    }

    private void ShowError(string msg)
    {
        ErrorBar.Message = msg;
        ErrorBar.IsOpen = true;
        App.CurrentErrorMessage = msg;
    }
}
