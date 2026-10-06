using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Payments;

/// <summary>
/// The consumer-side claim-redemption input of Settings → Subscriptions
/// (<c>monetization.md</c> § Pillar 3 Q4), lifted out of
/// <see cref="SettingsSubscriptionsPage"/> so the payments render is one removable
/// build item. A dumb renderer over the page's own view model.
/// </summary>
/// <remarks>⚠ Nothing may name this type from markup — see the header of the .xaml.</remarks>
public sealed partial class ClaimRedeemPanel : UserControl
{
    private SubscriptionsSettingsViewModel? _vm;
    private Action? _afterRedeem;

    public ClaimRedeemPanel()
    {
        InitializeComponent();
    }

    /// <summary>Bind to the host page's view model. <paramref name="afterRedeem"/> is the
    /// page's own post-mutation render pass (its <c>error-message</c> + the "no
    /// subscriptions" placeholder), which stays the page's business: a redeemed claim
    /// lands as a queued grant in the list this control does not own.</summary>
    public void Attach(SubscriptionsSettingsViewModel vm, Action afterRedeem)
    {
        _vm = vm;
        _afterRedeem = afterRedeem;
    }

    /// <summary>Redeem the pasted claim code (<c>fauna.payments.claims.redeem</c>
    /// — <c>monetization.md</c> § Pillar 3 Q4). Success clears the input + re-reads
    /// the mine list, where the queued grant renders as a pending row; a typed
    /// error surfaces via the page <c>error-message</c>.</summary>
    private async void RedeemClaim_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.RedeemClaimAsync(ClaimRedeemInput.Text);
        if (_vm.ErrorMessage is null)
            ClaimRedeemInput.Text = "";
        _afterRedeem?.Invoke();
    }
}
