// testing.md convention 15 — automation surface, compiled out of release
// artifacts. Reached ONLY from Testing/TestAgent's command table, and its
// handlers call `FfiFeedManager.InjectPostsForTest` / `.SetCueRollupForTest`,
// seams the production FFI flavor does not export. Gated with the agent.
#if DEBUG || FAUNA_E2E_AGENT
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Threading.Tasks;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Feed;

/// <summary>
/// E2E bridge command handler for the unified Feed page. Mirrors
/// <see cref="FaunaApp.Conversations.ConversationsCommands"/>: the test process
/// posts a <c>feed_inject_posts</c> command, the running app polls it via
/// <see cref="FaunaApp.Testing.TestAgent"/>, and this seeds the live
/// <see cref="FeedViewModel"/>'s shared <c>FfiFeedManager</c> with the
/// cross-app <c>TestPostSpec</c> list — the only way a tier_2 test reaches the
/// unverified-source-badge <c>Failed</c> arm (a real nest serves only
/// <c>Unchecked</c>/<c>Verified</c>; see actions/feed.py +
/// <c>fauna_feed::test_support</c>).
///
/// Available only when the windows-ffi <c>test-helpers</c> build exposes
/// <c>FfiFeedManager.InjectPostsForTest</c> (the same gate as
/// <c>ConversationsManager.InjectInboundForTest</c>).
/// </summary>
internal static class FeedCommands
{
    public static void InjectPosts(Dictionary<string, object?> command)
    {
        if (!command.TryGetValue("posts", out var raw) || raw is not JsonElement posts
            || posts.ValueKind != JsonValueKind.Array)
        {
            return;
        }
        // Forward the raw `posts` JSON array verbatim — shared Rust deserializes it
        // into Vec<TestPostSpec> (the SAME payload linux's handle_feed_inject_posts
        // parses), so no spec shape lives in C#. A no-op if the Feed VM isn't live
        // yet (pre-nav); the test's seed_posts re-injects until the count holds.
        FeedViewModel.Current?.Manager.InjectPostsForTest(posts.GetRawText());
    }

    /// <summary>
    /// Seed the live engagement-cue engine with a real <c>cues:v1</c> nest row
    /// (a real network round trip, unlike <see cref="InjectPosts"/>), so a
    /// capture-less test can reach "Clear activity data" with something to
    /// actually delete. Windows twin of tui/web's same-named seam. Expects
    /// <c>{content_ids:[string...]}</c>.
    /// </summary>
    public static async Task SeedCueRollupForTest(Dictionary<string, object?> command)
    {
        var manager = FeedViewModel.Current?.Manager;
        if (manager is null)
        {
            return;
        }
        List<string> contentIds = new();
        if (command.TryGetValue("content_ids", out var raw) && raw is JsonElement ids
            && ids.ValueKind == JsonValueKind.Array)
        {
            contentIds = ids.EnumerateArray()
                .Select(e => e.GetString() ?? string.Empty)
                .Where(s => s.Length > 0)
                .ToList();
        }
        // The generated binding takes `string[]` for a Rust `Vec<String>` (same shape
        // as ConversationsCommands' CreateMlsGroup call).
        await manager.SetCueRollupForTest(contentIds.ToArray());
    }

    /// <summary>
    /// Drive the feed page's <c>error-message</c> directly — the windows twin of
    /// tui's <c>feed_inject_error</c> agent arm and apple's same-named seam.
    /// There is no <em>product</em> path that fails a feed fetch on demand (a
    /// real failure needs the nest's own query to error), so this reaches
    /// <c>fauna_feed::FeedManager::inject_error_for_test</c> through
    /// <c>FfiFeedManager.InjectErrorForTest</c>. That FFI face builds the
    /// <c>LocalizedText::key_arg</c> carrier a genuinely failed fetch uses, so
    /// the app resolves the text through its own i18n pipeline exactly as it
    /// would a real failure — which is what makes the assertion on
    /// <c>error-message</c> honest rather than a painted string. Expects
    /// <c>{key: string, message: string}</c>; see actions/feed.py's
    /// <c>inject_error_for_test</c> for the defaults mirrored below.
    /// </summary>
    public static void InjectError(Dictionary<string, object?> command)
    {
        var key = ReadString(command, "key");
        if (key.Length == 0)
        {
            key = "feed.error_load";
        }
        var message = ReadString(command, "message");
        if (message.Length == 0)
        {
            message = "feed load failed";
        }
        // A no-op if the Feed VM isn't live yet (pre-nav/pre-auth) — the same
        // guard InjectPosts takes, and what tui's arm logs-and-returns for.
        FeedViewModel.Current?.Manager.InjectErrorForTest(key, message);
    }

    /// <summary>
    /// Arm (<paramref name="hold"/> true) or release the session manager's
    /// one-shot reload hold (<c>FeedManager::hold_next_reload_for_test</c>): the
    /// NEXT reload publishes the list it kept or cleared, then parks before its
    /// fetch until the release, so a test can read the page while a refresh is
    /// in flight (<c>feed.md</c> § The read model). The page starts its reload
    /// from <c>Page_Loaded</c>, an <c>async void</c> nothing here awaits, so no
    /// agent command parks behind a held reload. Twins of tui's, web's and
    /// linux's <c>feed_hold_next_reload</c> / <c>feed_release_held_reload</c>.
    /// Returns <c>false</c> when there is no manager yet (pre-auth, or before the
    /// Feed page first built one) — the caller says so rather than ack a hold
    /// that was never armed.
    /// </summary>
    public static bool HoldReload(bool hold)
    {
        var manager = Core.FeedManagerHost.Current;
        if (manager is null)
        {
            return false;
        }
        if (hold)
        {
            manager.HoldNextReloadForTest();
        }
        else
        {
            manager.ReleaseHeldReloadForTest();
        }
        return true;
    }

    private static string ReadString(Dictionary<string, object?> command, string name)
    {
        if (command.TryGetValue(name, out var raw) && raw is JsonElement el
            && el.ValueKind == JsonValueKind.String)
        {
            return el.GetString() ?? string.Empty;
        }
        return string.Empty;
    }
}
#endif
