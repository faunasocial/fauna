using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Models;

namespace FaunaApp.Views.Payments;

/// <summary>
/// One offer row's price line — see the .xaml header for why it is its own removable
/// build item.
/// </summary>
/// <remarks>
/// Bound by snapshot from ProfilePage's <c>OfferPriceHost_Loaded</c>, the
/// <see cref="SubscriptionTierPriceText"/> shape. <see cref="SubscriptionOfferRow"/>'s
/// price is immutable (only its status flips in place), and the offer list is rebuilt
/// with <c>Clear</c> + <c>Add</c>.
/// </remarks>
public sealed partial class SubscriptionOfferPriceText : UserControl
{
    public SubscriptionOfferPriceText()
    {
        InitializeComponent();
    }

    public void Bind(SubscriptionOfferRow row) => PriceText.Text = row.PriceHint ?? string.Empty;
}
