using System;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FaunaApp.Views.Payments;

/// <summary>
/// The claim-redemption pair (<c>subscription-claim-redeem-input</c> +
/// <c>subscription-claim-redeem-button</c>, <c>monetization.md</c> § Pillar 3 Q4) —
/// one control for both doors: Settings → Subscriptions (no creator, this nest) and
/// another actor's profile Tiers tab (that creator's code, redeemed at their home;
/// <c>profile.md</c> § Layout &amp; flow → <i>Another's profile</i>). Lifted out of the
/// host pages so the payments render is one removable build item. A dumb renderer:
/// the host's view model does the redeem.
/// </summary>
/// <remarks>⚠ Nothing may name this type from markup — see the header of the .xaml.</remarks>
public sealed partial class ClaimRedeemPanel : UserControl
{
    private Func<string, Task<bool>>? _redeem;
    private Action? _afterRedeem;

    public ClaimRedeemPanel()
    {
        InitializeComponent();
    }

    /// <summary>Bind to the host page. <paramref name="redeem"/> is the host view
    /// model's redeem over the typed text, answering whether the nest took the code
    /// (a blank code and a refusal both answer <c>false</c>, so a refused paste stays
    /// to correct). <paramref name="afterRedeem"/> is the page's own post-mutation
    /// render pass (its <c>error-message</c> and whatever list the grant lands in),
    /// which stays the page's business.</summary>
    public void Attach(Func<string, Task<bool>> redeem, Action afterRedeem)
    {
        _redeem = redeem;
        _afterRedeem = afterRedeem;
    }

    /// <summary>Redeem the pasted claim code (<c>fauna.payments.claims.redeem</c>
    /// — <c>monetization.md</c> § Pillar 3 Q4). Success clears the input; a typed
    /// error surfaces via the page <c>error-message</c>.</summary>
    private async void RedeemClaim_Click(object sender, RoutedEventArgs e)
    {
        if (_redeem is null) return;
        if (await _redeem(ClaimRedeemInput.Text))
            ClaimRedeemInput.Text = "";
        _afterRedeem?.Invoke();
    }
}
