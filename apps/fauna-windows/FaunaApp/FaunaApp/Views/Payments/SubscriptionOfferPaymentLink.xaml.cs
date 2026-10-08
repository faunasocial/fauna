using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Models;

namespace FaunaApp.Views.Payments;

/// <summary>
/// One offer row's external checkout link — see the .xaml header for why it is its
/// own removable build item.
/// </summary>
/// <remarks>
/// Bound by snapshot from ProfilePage's <c>OfferPaymentLinkHost_Loaded</c>, the
/// <see cref="SubscriptionOfferPriceText"/> shape. The link shows only when the tier
/// carries a URL (<see cref="SubscriptionOfferRow.HasPaymentUrl"/>, linux
/// <c>offers.rs</c>); the open itself, and its https-only guard, are the page's.
/// </remarks>
public sealed partial class SubscriptionOfferPaymentLink : UserControl
{
    /// <summary>Fires on click with the row whose <c>PaymentUrl</c> the host should
    /// open.</summary>
    public event EventHandler<SubscriptionOfferRow>? OpenRequested;

    private SubscriptionOfferRow? _row;

    public SubscriptionOfferPaymentLink()
    {
        InitializeComponent();
    }

    public void Bind(SubscriptionOfferRow row)
    {
        _row = row;
        LinkButton.Visibility = row.HasPaymentUrl ? Visibility.Visible : Visibility.Collapsed;
    }

    private void LinkButton_Click(object sender, RoutedEventArgs e)
    {
        if (_row is not null) OpenRequested?.Invoke(this, _row);
    }
}
