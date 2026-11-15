using Microsoft.UI.Xaml;
using FaunaApp.Feed;

namespace FaunaApp.Controls;

/// <summary>
/// Code-behind of the shared post-card template (<c>PostCardTemplates.xaml</c>). It
/// holds no behaviour: every handler the template names forwards to the
/// <see cref="PostCardHost"/> of the list the card sits in, which acts on that list's
/// own feed manager (the feed's timeline, or the profile's author window). A card with
/// no host above it (being torn down, say) does nothing.
/// </summary>
public sealed partial class PostCardTemplates : ResourceDictionary
{
    public PostCardTemplates()
    {
        this.InitializeComponent();
    }

    private void PostCard_Click(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.PostCard_Click(sender, e);
    private void PostCard_Loaded(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.PostCard_Loaded(sender, e);
    private void PostCard_Unloaded(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.PostCard_Unloaded(sender, e);
    private void RegionPlaceholder_Loaded(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.RegionPlaceholder_Loaded(sender, e);
    private void RegionPlaceholder_Unloaded(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.RegionPlaceholder_Unloaded(sender, e);
    private void NestPlaceholder_Loaded(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.NestPlaceholder_Loaded(sender, e);
    private void NestPlaceholder_Unloaded(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.NestPlaceholder_Unloaded(sender, e);
    private void RegionRevealButton_Click(object sender, RoutedEventArgs e) => PostCardHost.RevealContent(sender);
    private void NestRevealButton_Click(object sender, RoutedEventArgs e) => PostCardHost.RevealContent(sender);
    private void ContentRevealButton_Click(object sender, RoutedEventArgs e) => PostCardHost.RevealContent(sender);
    private void MutedRevealButton_Click(object sender, RoutedEventArgs e) => PostCardHost.RevealMuted(sender);
    private void ShowAgainButton_Click(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.ShowAgain_Click(sender, e);
    private void PostAuthor_Click(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.PostAuthor_Click(sender, e);
    private void SoldPostTeaserHost_Loaded(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.SoldPostTeaserHost_Loaded(sender, e);
    private void TipDisplayHost_Loaded(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.TipDisplayHost_Loaded(sender, e);
    private void PostActionsButton_Click(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.PostActionsButton_Click(sender, e);
    private void LoadRemoteContent_Click(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.LoadRemoteContent_Click(sender, e);
    private void PostImage_Click(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.PostImage_Click(sender, e);
    private void LikeButton_Click(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.LikeButton_Click(sender, e);
    private void ReplyButton_Click(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.ReplyButton_Click(sender, e);
    private void RepostButton_Click(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.RepostButton_Click(sender, e);
    private void QuoteButton_Click(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.QuoteButton_Click(sender, e);
    private void ReactionPill_Click(object sender, RoutedEventArgs e) => PostCardHost.Of(sender)?.ReactionPill_Click(sender, e);
}
