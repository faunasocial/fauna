using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views;

/// <summary>
/// Settings → Subscriptions sub-page (<c>subscription-settings</c>;
/// <c>monetization.md</c> § Pillar 1 consumer path; <c>ui.yaml subscription-settings</c>).
/// The CONSUMER side: the tiers this user subscribes to, across all creators. Author-side
/// tier management lives on the Profile page (Tiers tab). A dumb renderer of
/// <see cref="SubscriptionsSettingsViewModel"/> (FaunaApp.Core) over the
/// <see cref="INestRpcClient"/> seam — observer-free; each mutation triggers a full re-read
/// (<c>HydrateAsync</c>). Hosted by <see cref="SettingsShellPage"/>. Mirrors apple
/// <c>SubscriptionsSettingsVM</c> / linux <c>src/settings/subscriptions.rs</c>.
///
/// No <c>ConfigureAwait(false)</c> in these handlers (off-thread bound-state mutation
/// throws a silent COMException — reference_windows_vm_configureawait_comexception).
/// </summary>
public sealed partial class SettingsSubscriptionsPage : Page
{
    private SubscriptionsSettingsViewModel? _vm;

#if PAYMENTS
    /// <summary>The claim-redemption input, built in code and dropped into
    /// <c>ClaimRedeemHost</c>. Not named from markup: FaunaApp.csproj removes
    /// Views\Payments\** from the store-safe build.</summary>
    private Views.Payments.ClaimRedeemPanel? _claimRedeem;
#endif

    public SettingsSubscriptionsPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients && clients.Rpc is not null)
        {
            _vm = new SubscriptionsSettingsViewModel(clients.Rpc);
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;

        // Bind the list to the VM's observable collection (auto-updates on each re-read).
        SubscriptionsList.ItemsSource = _vm.Subscriptions;

#if PAYMENTS
        // The redeem input lives in its own removable build item — bind it the same
        // way, one seam further out (dynamic-features.md § Platform-family surface
        // excision). Its post-mutation render pass stays here: a redeemed claim lands
        // as a queued grant in the list above, which the panel does not own.
        _claimRedeem = new Views.Payments.ClaimRedeemPanel();
        ClaimRedeemHost.Content = _claimRedeem;
        _claimRedeem.Attach(_vm, () =>
        {
            RenderError(_vm.ErrorMessage);
            UpdatePlaceholder();
        });
#endif

        await _vm.HydrateAsync();
        RenderError(_vm.ErrorMessage);
        UpdatePlaceholder();
    }

    // ── Row actions ─────────────────────────────────────────────────────────
    private async void Unsubscribe_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        if (sender is not FrameworkElement { Tag: SubscriptionMineRow row }) return;
        await _vm.UnsubscribeAsync(row.AuthorId);
        RenderError(_vm.ErrorMessage);
        UpdatePlaceholder();
    }

    // ── Render helpers ──────────────────────────────────────────────────────

    /// <summary>Show/hide the "no subscriptions" placeholder based on the current
    /// collection count. Called after load and every unsubscribe mutation.</summary>
    private void UpdatePlaceholder()
    {
        if (_vm is null) return;
        NoSubscriptionsText.Visibility =
            _vm.Subscriptions.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    /// <summary>Render the VM's <see cref="ViewModelBase.ErrorMessage"/> onto the
    /// page-level <c>error-message</c> InfoBar + the state-protocol mirror TextBlock
    /// (mirrors <see cref="ProfilePage.RenderError"/> / AdminCalendarPage).</summary>
    private void RenderError(string? msg)
    {
        var empty = string.IsNullOrEmpty(msg);
        ErrorBar.Message = msg ?? string.Empty;
        ErrorBar.IsOpen = !empty;
        ErrorTextMirror.Text = msg ?? " ";
        App.CurrentErrorMessage = empty ? null : msg;
    }
}
