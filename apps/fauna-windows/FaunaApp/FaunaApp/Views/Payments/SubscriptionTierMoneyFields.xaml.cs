using Microsoft.UI.Xaml.Controls;

namespace FaunaApp.Views.Payments;

/// <summary>
/// The tier editor's three money fields — see the .xaml header for why they are one
/// removable build item. Dumb text holders: <c>ProfilePage.SyncFormFromVm</c> /
/// <c>SyncFormToVm</c> move them to and from <c>ProfileViewModel</c>'s form state,
/// which owns parsing (an empty or unparseable asking price means no machine price).
/// </summary>
/// <remarks>⚠ Nothing outside <c>Views\Payments\</c> may name this type from markup —
/// see the .xaml header.</remarks>
public sealed partial class SubscriptionTierMoneyFields : UserControl
{
    public SubscriptionTierMoneyFields()
    {
        InitializeComponent();
    }

    public string PriceHint
    {
        get => PriceHintBox.Text;
        set => PriceHintBox.Text = value;
    }

    public string AskingPriceSats
    {
        get => AskingPriceBox.Text;
        set => AskingPriceBox.Text = value;
    }

    public string PaymentUrl
    {
        get => PaymentUrlBox.Text;
        set => PaymentUrlBox.Text = value;
    }
}
