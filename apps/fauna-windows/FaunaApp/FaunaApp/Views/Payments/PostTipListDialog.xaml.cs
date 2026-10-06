using System.Collections.Generic;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Payments;

/// <summary>
/// The flat tip-attribution window (monetization.md § Tips), lifted out of
/// <see cref="FeedPage"/> so the whole plane is one removable build item — see the
/// header of the .xaml.
/// </summary>
public sealed partial class PostTipListDialog : UserControl
{
    public PostTipListDialog()
    {
        InitializeComponent();
    }

    /// <summary>Populate and reveal the window for one post's tips — called from
    /// either the list card's <c>post-tip-list-button</c> or the detail dialog's own
    /// tip row, both funneled through the host page.</summary>
    public void Show(string title, IReadOnlyList<TipSenderRow> senders)
    {
        TitleText.Text = title;
        ItemsList.ItemsSource = senders;
        Root.Visibility = Visibility.Visible;
    }

    private void Close_Click(object sender, RoutedEventArgs e) => Root.Visibility = Visibility.Collapsed;
}
