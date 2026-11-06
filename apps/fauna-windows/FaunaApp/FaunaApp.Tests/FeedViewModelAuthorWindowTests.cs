using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_feed;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// The profile page's Posts tab rides a view model bound to an AUTHOR WINDOW of the
/// session's feed manager (`ui/profile.md` § Layout &amp; flow → <i>The Posts tab</i>;
/// `ui/feed.md` § State &amp; data shape → <i>The author window</i>). The window is a
/// second manager over the same seams, so its view model must read the named author
/// and must never stand in for the timeline: <see cref="FeedViewModel.Current"/> is
/// the session's timeline, which Personalization, the e2e state read and the
/// off-page reconnect re-pull all resolve through. Offline, as
/// <see cref="FeedViewModelAttachmentTests"/> is: <c>NestClient::new</c> opens no socket.
/// </summary>
public class FeedViewModelAuthorWindowTests
{
    private sealed class NullObserver : FeedSnapshotObserver
    {
        public void OnChanged() { }
    }

    private static readonly string Author = new('a', 64);

    [Fact]
    public void AnAuthorWindow_ReadsTheNamedAuthor()
    {
        using var nest = new FfiNestClient("wss://127.0.0.1:0/ws", new byte[32]);
        using var timeline = nest.FeedManager(new byte[32]);
        using var windowManager = timeline.AuthorWindow(Author);

        var window = FeedViewModel.ForAuthorWindow(windowManager, new NullObserver(), new MockNestRpcClient());

        Assert.True(window.IsAuthorWindow);
        Assert.Equal(Author, window.Snapshot.authorSelected);
        // The timeline's own selection is untouched by minting a window off it.
        Assert.Null(timeline.Snapshot().authorSelected);
    }

    [Fact]
    public void AnAuthorWindow_NeverBecomesTheSessionsTimeline()
    {
        using var nest = new FfiNestClient("wss://127.0.0.1:0/ws", new byte[32]);
        using var timeline = nest.FeedManager(new byte[32]);
        using var windowManager = timeline.AuthorWindow(Author);

        var timelineVm = new FeedViewModel(timeline, new NullObserver(), new MockNestRpcClient());
        Assert.False(timelineVm.IsAuthorWindow);
        var window = FeedViewModel.ForAuthorWindow(windowManager, new NullObserver(), new MockNestRpcClient());

        // Other test classes may construct timeline VMs concurrently, so assert only
        // what no other test can produce: the window is never Current.
        Assert.False(ReferenceEquals(FeedViewModel.Current, window));
    }

    [Fact]
    public void DetachingAnAuthorWindow_IsIdempotent()
    {
        using var nest = new FfiNestClient("wss://127.0.0.1:0/ws", new byte[32]);
        using var timeline = nest.FeedManager(new byte[32]);
        using var windowManager = timeline.AuthorWindow(Author);
        var window = FeedViewModel.ForAuthorWindow(windowManager, new NullObserver(), new MockNestRpcClient());

        window.DetachFromManager();
        window.DetachFromManager();
        window.CleanupReconnect();
        window.CleanupStoreChange();
        Assert.False(window.IsReconnectWired);
    }
}
