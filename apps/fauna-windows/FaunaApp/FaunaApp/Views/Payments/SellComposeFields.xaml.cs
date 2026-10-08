using System;
using Microsoft.UI.Xaml.Controls;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views.Payments;

/// <summary>
/// The sell-this-post composer's three fields — see the .xaml header for why they are
/// one removable build item. Dumb holders: <c>FeedComposeBar</c> reads them into its
/// <c>ComposeAudience.Sell</c> answer and owns when they show; the shared FeedManager
/// owns parsing (an empty or unparseable asking price means no machine price).
/// </summary>
/// <remarks>⚠ Nothing outside <c>Views\Payments\</c> may name this type from markup —
/// see the .xaml header.</remarks>
public sealed partial class SellComposeFields : UserControl
{
    /// <summary>Fires on every user edit of any of the three fields — what lets the
    /// composer stage the sale on the draft as it is typed.</summary>
    public event Action? Changed;

    public SellComposeFields()
    {
        InitializeComponent();
        PriceBox.PlaceholderText = S.Get("feed/post/sell_price_placeholder");
        AskingPriceBox.PlaceholderText = S.Get("feed/post/sell_asking_price_placeholder");
        SubscribersFreeCheck.Content = S.Get("feed/post/sell_subscribers_free");
        PriceBox.TextChanged += (_, _) => Changed?.Invoke();
        AskingPriceBox.TextChanged += (_, _) => Changed?.Invoke();
        SubscribersFreeCheck.Checked += (_, _) => Changed?.Invoke();
        SubscribersFreeCheck.Unchecked += (_, _) => Changed?.Invoke();
    }

    public string Price
    {
        get => PriceBox.Text;
        set => PriceBox.Text = value;
    }

    public string AskingPrice
    {
        get => AskingPriceBox.Text;
        set => AskingPriceBox.Text = value;
    }

    /// <summary>Whether the author's subscribers get the sold post free — the ratified
    /// rank knob, ON by default.</summary>
    public bool SubscribersGetItFree
    {
        get => SubscribersFreeCheck.IsChecked ?? true;
        set => SubscribersFreeCheck.IsChecked = value;
    }

    /// <summary>True while all three still stand at their fresh defaults (empty, empty,
    /// ON) — the never-clobber test a restored draft's sale is written in under.</summary>
    public bool IsAtDefaults
        => string.IsNullOrEmpty(Price) && string.IsNullOrEmpty(AskingPrice) && SubscribersGetItFree;

    /// <summary>Back to the fresh defaults: empty, empty, ON.</summary>
    public void Reset()
    {
        Price = string.Empty;
        AskingPrice = string.Empty;
        SubscribersGetItFree = true;
    }
}
