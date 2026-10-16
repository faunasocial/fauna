using FaunaApp.Core.ViewModels;
using uniffi.fauna_feed;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The Bluesky thread page's two windows readings of shared Rust
/// (<c>docs/goal/behavior/bridges.md</c> § Bluesky-native thread view): whether a post
/// detail offers <c>feed-post-view-thread-button</c> is the exported
/// <c>post_offers_thread_view</c>, and each <c>feed-thread-post</c>'s author is the
/// exported <c>thread_post_author_label</c> — windows re-derives neither "source is
/// bluesky" nor the display-name-else-@handle rule (priority #2). The native
/// <c>fauna_ffi</c> dll loads in the test host, so these call the real exports.
/// </summary>
[Collection("StringsGlobal")]
public class FeedThreadPageTests
{
    [Theory]
    [InlineData("bluesky", true)]
    [InlineData("fauna", false)]
    [InlineData("activitypub", false)]
    public void OffersThreadView_FollowsTheSharedPredicate(string source, bool offered)
    {
        Assert.Equal(offered, FeedPostItem.ForSourceTest(source).OffersThreadView);
    }

    [Fact]
    public void ThreadPostRow_ReadsTheDisplayNameThenTheText()
    {
        var row = ThreadPostRow.From(Post("alice.test", "Alice", "in a text file, honestly", isFocal: true));

        Assert.Equal("Alice in a text file, honestly", row.Line);
        Assert.Equal("true", row.Selected);
    }

    [Fact]
    public void ThreadPostRow_WithoutADisplayName_ReadsTheHandle()
    {
        var row = ThreadPostRow.From(Post("carol.test", null, "same here", isFocal: false));

        Assert.Equal("@carol.test same here", row.Line);
        Assert.Equal("false", row.Selected);
    }

    private static ThreadPostView Post(string handle, string? displayName, string text, bool isFocal) =>
        new(
            $"at://did:plc:{handle}/app.bsky.feed.post/3k",
            handle,
            displayName,
            text,
            "2026-10-10T00:00:00Z",
            0, 0, 0,
            isFocal);
}
