using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Payments;

/// <summary>
/// The sold-post buyer teaser on one post card — see the .xaml header for why it is
/// its own removable build item.
/// </summary>
/// <remarks>
/// Bound by snapshot, the <see cref="PostTipDisplay"/> shape and for the same reasons:
/// the page's own markup cannot name this type, so <see cref="FeedPage"/> calls
/// <see cref="Bind"/> from the host's <c>Loaded</c> and again after every snapshot pass
/// (an offer resolved later reaches a card whose container loaded earlier).
/// </remarks>
public sealed partial class SoldPostTeaser : UserControl
{
    /// <summary>Fires when <c>gated-post-payment-link</c> is clicked, carrying the post
    /// whose offer URL the host should open (behind the shared https-only guard).</summary>
    public event EventHandler<FeedPostItem>? PaymentLinkRequested;

    /// <summary>Fires when <c>gated-post-buy-button</c> is clicked, carrying the post
    /// whose resolved offer the host should buy.</summary>
    public event EventHandler<FeedPostItem>? BuyRequested;

    private FeedPostItem? _item;

    public SoldPostTeaser()
    {
        InitializeComponent();
    }

    /// <summary>Paint this card's offer: the price and the buy button iff an offer
    /// resolved (<c>HasUnlockOffer</c>), the payment link iff it also carries a URL
    /// (<c>HasUnlockPaymentLink</c>) — the guards the card markup used before the
    /// move.</summary>
    public void Bind(FeedPostItem item)
    {
        _item = item;
        PriceText.Text = item.UnlockOfferPriceText;
        PriceText.Visibility = FeedPage.BoolToVisibility(item.HasUnlockOffer);
        PaymentLinkButton.Visibility = FeedPage.BoolToVisibility(item.HasUnlockPaymentLink);
        BuyButton.Visibility = FeedPage.BoolToVisibility(item.HasUnlockOffer);
    }

    private void PaymentLinkButton_Click(object sender, RoutedEventArgs e)
    {
        if (_item is not null) PaymentLinkRequested?.Invoke(this, _item);
    }

    private void BuyButton_Click(object sender, RoutedEventArgs e)
    {
        if (_item is not null) BuyRequested?.Invoke(this, _item);
    }
}
