using FaunaApp.Core.ViewModels;
using uniffi.fauna_feed;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// <c>feed-like-button</c>'s <c>state</c> word on windows (<c>account-offline-mutation.md</c>
/// § The offline-mutation contract → <i>The unsent-changes surface</i>, point 2):
/// the card carries <c>PostSummary.like_state</c> spelled by the shared
/// <c>like_state_word</c>, so a like waiting in the device outbox reads <c>queued</c>, one
/// the drain parked reads <c>unsent</c>, and a sent one reads <c>on</c> / <c>off</c> — and a
/// changed word is a changed card, so the list repaints it.
/// </summary>
[Collection("StringsGlobal")]
public class FeedLikeStateTests
{
    private static FeedPostItem Card(LikeState state, bool viewerLiked = false) =>
        new(PostSummaryFixture.Make(postId: "p1", viewerLiked: viewerLiked, likeState: state));

    [Fact]
    public void AQueuedLikeReadsQueued()
    {
        Assert.Equal("queued", Card(LikeState.Queued).LikeStateWord);
    }

    [Fact]
    public void AParkedLikeReadsUnsent()
    {
        Assert.Equal("unsent", Card(LikeState.Unsent).LikeStateWord);
    }

    [Fact]
    public void ASentLikeReadsOnOrOff()
    {
        Assert.Equal("on", Card(LikeState.On, viewerLiked: true).LikeStateWord);
        Assert.Equal("off", Card(LikeState.Off).LikeStateWord);
    }

    [Fact]
    public void AChangedStateWordIsAChangedCard()
    {
        Assert.False(Card(LikeState.Queued).ContentEquals(Card(LikeState.Off)));
        Assert.True(Card(LikeState.Queued).ContentEquals(Card(LikeState.Queued)));
    }
}
