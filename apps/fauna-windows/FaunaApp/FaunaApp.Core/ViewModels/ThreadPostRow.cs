using uniffi.fauna_feed;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// One <c>feed-thread-post</c> of the Bluesky thread page (ui.yaml
/// <c>feed.sub_pages.thread</c>, <c>bridges.md</c> § Bluesky-native thread view), read off
/// one <see cref="ThreadPostView"/> of the shared <c>FeedSnapshot.thread</c>. The author is
/// the exported <c>thread_post_author_label</c> (display name, else <c>@handle</c>) — a
/// UniFFI record carries no methods, and windows never re-derives the rule; whether the
/// row is the opened post is the nest's <c>focal_index</c>, carried as
/// <see cref="ThreadPostView.isFocal"/>, never a guess here.
/// </summary>
internal sealed record ThreadPostRow(string Author, string Text, bool IsFocal)
{
    internal static ThreadPostRow From(ThreadPostView post) =>
        new(FaunaFeedMethods.ThreadPostAuthorLabel(post), post.text, post.isFocal);

    /// <summary>The row's one text, <c>‹author› ‹post text›</c> — what every app's
    /// <c>feed-thread-post</c> reads.</summary>
    public string Line => $"{Author} {Text}";

    /// <summary>The <c>selected</c> attribute's wire value: <c>"true"</c> on the opened
    /// post only, <c>"false"</c> on every other — always present.</summary>
    public string Selected => IsFocal ? "true" : "false";
}
