using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Models;

namespace FaunaApp.Views.Payments;

/// <summary>
/// One tier-list row's price text — see the .xaml header for why it is its own
/// removable build item.
/// </summary>
/// <remarks>
/// Bound by snapshot, once per realized row (<see cref="Bind"/> from ProfilePage's
/// <c>TierPriceHost_Loaded</c>), the <see cref="PostTipDisplay"/> shape: the page's
/// markup cannot name this type, so it can carry no binding into it.
/// <see cref="SubscriptionTierRow"/> is an immutable record and the list is rebuilt
/// with <c>Clear</c> + <c>Add</c>, so every new row arrives in a freshly loaded
/// container.
/// </remarks>
public sealed partial class SubscriptionTierPriceText : UserControl
{
    public SubscriptionTierPriceText()
    {
        InitializeComponent();
    }

    public void Bind(SubscriptionTierRow row) => PriceText.Text = row.PriceHint ?? string.Empty;
}
