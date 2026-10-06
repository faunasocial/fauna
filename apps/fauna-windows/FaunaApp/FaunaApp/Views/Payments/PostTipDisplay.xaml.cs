using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Payments;

/// <summary>
/// The tip display surface on one post card (monetization.md § Tips), lifted out of
/// <see cref="FeedPage"/>'s per-card DataTemplate so the whole plane is one removable
/// build item — see the header of the .xaml.
/// </summary>
/// <remarks>
/// Bound imperatively, once per realized ListView container, rather than via
/// <c>x:Bind</c>/<c>DataContext</c>: the host page's
/// <c>PostsList_ContainerContentChanging</c> calls <see cref="Bind"/> directly with the
/// item, since the type can't be named from the page's own markup (see the .xaml
/// header) and so can carry no compile-time binding into it.
/// </remarks>
public sealed partial class PostTipDisplay : UserControl
{
    /// <summary>Fires when <c>post-tip-list-button</c> is clicked, carrying the post
    /// whose attribution window the host should open (there is only ever one
    /// <c>post-tip-list</c> dialog, flat, not per-card).</summary>
    public event EventHandler<FeedPostItem>? OpenTipListRequested;

    private FeedPostItem? _item;

    public PostTipDisplay()
    {
        InitializeComponent();
    }

    /// <summary>Paint this card's tip state. Same guard shape as the markup this
    /// replaced: <c>post-tip-total</c> iff <c>HasTipTotal</c>, <c>post-tip-count</c> +
    /// the list button iff <c>HasTips</c> (never the same condition — a post can have
    /// real tips with no summable amount).</summary>
    public void Bind(FeedPostItem item)
    {
        _item = item;
        TipTotalText.Text = item.TipTotalText;
        TipTotalText.Visibility = FeedPage.BoolToVisibility(item.HasTipTotal);
        TipCountText.Text = item.TipCountText;
        TipCountText.Visibility = FeedPage.BoolToVisibility(item.HasTips);
        TipListButton.Visibility = FeedPage.BoolToVisibility(item.HasTips);
    }

    private void TipListButton_Click(object sender, RoutedEventArgs e)
    {
        if (_item is not null) OpenTipListRequested?.Invoke(this, _item);
    }
}
