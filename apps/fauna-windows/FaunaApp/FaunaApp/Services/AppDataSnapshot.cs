using System.Text.Json;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;

namespace FaunaApp.Services;

/// <summary>
/// Thread-safe snapshot of app data for state protocol serialization.
/// ViewModels push data here after loading; SerializeState reads it on any thread.
/// </summary>
internal static class AppDataSnapshot
{
    private static readonly object _lock = new();

    private static List<FeedPostSnapshot>? _feedPosts;
    private static int _unreadNotificationCount;
    private static List<ConversationSnapshot>? _conversations;
    private static List<ContactSnapshot>? _contacts;
    private static List<KnockSnapshot>? _knocks;
    private static List<EventSnapshot>? _events;
    private static List<SyncFileSnapshot>? _syncFiles;

    /// <summary>
    /// Push feed posts after body decoding is complete.
    /// Called from FeedViewModel on the UI thread.
    /// </summary>
    internal static void SetFeedPosts(IEnumerable<FeedPostItem> items)
    {
        var snapshot = items.Select(item => new FeedPostSnapshot(
            PostId: item.PostId,
            Author: item.AuthorHex,
            Body: item.BodyText,
            Timestamp: item.Timestamp,
            Tags: item.Tags.ToList(),
            HasMedia: item.HasMedia,
            MediaHash: item.MediaHashHex,
            IsReply: item.IsReplyPost,
            LikeCount: item.LikeCount,
            ReplyCount: item.ReplyCount,
            RepostCount: item.RepostCount,
            QuoteCount: item.QuoteCount,
            ViewerLiked: item.ViewerLiked,
            RepostedPostId: item.RepostedPostId,
            ViewerRepostId: item.ViewerRepostId,
            // Every link preview in the body with its state, in body order, off the
            // shared face (`RenderDocument::link_previews` — render-model.md § D4,
            // FeedPostItem.LinkPreviewStates). A card is absent while a preview is
            // still resolving too, so this is what lets a test wait until a preview
            // has FAILED before it reads "no card".
            LinkPreviews: item.LinkPreviewStates
                .Select(lp => new Dictionary<string, object?>
                {
                    ["url"] = lp.Url,
                    ["state"] = lp.State,
                })
                .ToList()
        )).ToList();

        lock (_lock)
        {
            _feedPosts = snapshot;
        }
    }

    /// <summary>
    /// Set unread notification count. Called from NotificationsPage or WebSocket handler.
    /// </summary>
    internal static void SetUnreadCount(int count)
    {
        lock (_lock)
        {
            _unreadNotificationCount = count;
        }
    }

    /// <summary>
    /// Read feed posts for state serialization. Returns null if never populated.
    /// </summary>
    internal static List<Dictionary<string, object?>>? GetFeedPostsForState()
    {
        lock (_lock)
        {
            if (_feedPosts is null) return null;
            return _feedPosts.Select(p => new Dictionary<string, object?>
            {
                ["post_id"] = p.PostId,
                ["author"] = p.Author,
                ["body"] = p.Body,
                ["timestamp"] = p.Timestamp,
                ["tags"] = p.Tags,
                ["has_media"] = p.HasMedia,
                ["media_hash"] = p.MediaHash,
                ["is_reply"] = p.IsReply,
                // The four interaction counts the bar renders (feed.md § Interaction
                // bar). Emitted so the count is *assertable*: without them the e2e
                // reader answers null, which reads like "no activity" rather than
                // "this app never told you".
                ["like_count"] = p.LikeCount,
                ["reply_count"] = p.ReplyCount,
                ["repost_count"] = p.RepostCount,
                ["quote_count"] = p.QuoteCount,
                // The toggle's own state (feed.md § Interaction bar → Repost, ratified
                // 2026-08-10) — what routes the next tap to like vs. unlike.
                ["viewer_liked"] = p.ViewerLiked,
                // The repost carrier + per-viewer pair (feed.md § Interaction bar →
                // Repost, ratified 2026-08-10). reposted_post_id
                // is how the harness tells a repost row from an empty quote until it
                // reads the repost-attribution element directly; viewer_repost_id is
                // the toggle's state (and unrepost's argument) — mirrors linux/web's
                // own state-dump fields, closing the gap those apps' own repost legs
                // found in their state-dump surfaces (feed.md § Implementation status
                // today).
                ["reposted_post_id"] = p.RepostedPostId,
                ["viewer_repost_id"] = p.ViewerRepostId,
                // `{url, state}` per link preview (see SetFeedPosts) — tui's key.
                ["link_previews"] = p.LinkPreviews,
            }).ToList();
        }
    }

    /// <summary>
    /// Read notification data for state serialization.
    /// </summary>
    internal static Dictionary<string, object?> GetNotificationsForState()
    {
        lock (_lock)
        {
            return new Dictionary<string, object?>
            {
                ["unread_count"] = _unreadNotificationCount,
            };
        }
    }

    /// <summary>
    /// Push conversations after loading. Called from ConversationsViewModel.
    /// </summary>
    internal static void SetConversations(IEnumerable<ConversationSnapshot> items)
    {
        lock (_lock) { _conversations = items.ToList(); }
    }

    /// <summary>
    /// Push contacts after loading. Called from ContactsViewModel.
    /// </summary>
    internal static void SetContacts(IEnumerable<ContactSnapshot> items)
    {
        lock (_lock) { _contacts = items.ToList(); }
    }

    /// <summary>
    /// Push knocks after loading. Called from ContactsViewModel.
    /// </summary>
    internal static void SetKnocks(IEnumerable<KnockSnapshot> items)
    {
        lock (_lock) { _knocks = items.ToList(); }
    }

    /// <summary>
    /// Push events after loading. Called from EventsViewModel.
    /// </summary>
    internal static void SetEvents(IEnumerable<EventSnapshot> items)
    {
        lock (_lock) { _events = items.ToList(); }
    }

    /// <summary>
    /// Push sync files after loading. Called from SyncViewModel.
    /// </summary>
    internal static void SetSyncFiles(IEnumerable<SyncFileSnapshot> items)
    {
        lock (_lock) { _syncFiles = items.ToList(); }
    }

    /// <summary>
    /// Read the unified conversations snapshot for the test bridge — one
    /// entry per thread with the fields the cross-app e2e tests assert
    /// against (label, flavor, snippet, message_count, participant_count,
    /// participant_actor_ids). Bypasses the legacy <see cref="_conversations"/>
    /// cache; the unified page reads directly from
    /// <c>ConversationsManagerHost.Instance</c>.
    ///
    /// Re-parses the shared <c>ConversationsManager.ConversationThreadsJson()</c>
    /// UniFFI passthrough (<c>fauna_conversations::state_json::
    /// conversation_threads_json</c>) rather than hand-rolling the row shape —
    /// the same <c>App.MlsFoldedCommitsForSerialization</c>-style JSON-passthrough
    /// idiom, and the one apple's <c>serializeData()</c> switched to for this
    /// exact field (previously this hand-roll omitted
    /// <c>participant_actor_ids</c> entirely, same gap apple had).
    /// </summary>
    internal static List<Dictionary<string, object?>> GetConversationsThreadsForState()
    {
        try
        {
            var manager = FaunaApp.Conversations.ConversationsManagerHost.Instance;
            var sw = FaunaApp.Core.Logs.E2eTrace.Enabled ? System.Diagnostics.Stopwatch.StartNew() : null;
            var json = manager.ConversationThreadsJson();
            var ffiMs = sw?.ElapsedMilliseconds ?? 0;
            var rows = JsonSerializer.Deserialize<List<Dictionary<string, object?>>>(json)
                ?? new List<Dictionary<string, object?>>();
            if (sw is not null)
                FaunaApp.Core.Logs.E2eTrace.Write(
                    $"[state] conversation_threads ffi={ffiMs}ms parse={sw.ElapsedMilliseconds - ffiMs}ms "
                        + $"json_chars={json.Length} rows={rows.Count}");
            return rows;
        }
        catch
        {
            return new List<Dictionary<string, object?>>();
        }
    }

    /// <summary>
    /// Read the unified conversations page's active sort order for the test
    /// bridge (<c>data.conversation_sort</c>) — the rows alone can't name it.
    /// Re-parses the shared <c>ConversationsManager.ConversationSortJson()</c>
    /// UniFFI passthrough (<c>fauna_conversations::state_json::
    /// conversation_sort_json</c>), a bare JSON string fragment
    /// (<c>"LatestActivity"</c> / <c>"OldestFirst"</c> / <c>"Unread"</c>), the
    /// same shape tui and linux publish under this key.
    /// </summary>
    internal static string? GetConversationSortForState()
    {
        try
        {
            var manager = FaunaApp.Conversations.ConversationsManagerHost.Instance;
            var json = manager.ConversationSortJson();
            return JsonSerializer.Deserialize<string>(json);
        }
        catch
        {
            return null;
        }
    }

    /// <summary>
    /// Read conversations for state serialization. Returns null if never populated.
    /// </summary>
    internal static List<Dictionary<string, object?>>? GetConversationsForState()
    {
        lock (_lock)
        {
            if (_conversations is null) return null;
            return _conversations.Select(c => new Dictionary<string, object?>
            {
                ["post_id"] = c.ActorId,
                ["from"] = c.ActorId,
                ["to"] = (object?)null,
                ["body"] = c.LastMessage,
                ["timestamp"] = c.LastTimestamp,
                ["read"] = c.UnreadCount == 0,
            }).ToList();
        }
    }

    /// <summary>
    /// Read contacts for state serialization. Returns null if never populated.
    /// </summary>
    internal static List<Dictionary<string, object?>>? GetContactsForState()
    {
        lock (_lock)
        {
            if (_contacts is null) return null;
            return _contacts.Select(c => new Dictionary<string, object?>
            {
                ["peer_id"] = c.ActorId,
                ["status"] = c.Status,
                ["handle"] = c.Handle,
            }).ToList();
        }
    }

    /// <summary>
    /// Read knocks for state serialization. Returns null if never populated.
    /// </summary>
    internal static List<Dictionary<string, object?>>? GetKnocksForState()
    {
        lock (_lock)
        {
            if (_knocks is null) return null;
            return _knocks.Select(k => new Dictionary<string, object?>
            {
                ["peer_id"] = k.ActorId,
                ["summary"] = k.Summary,
                ["timestamp"] = k.Timestamp,
            }).ToList();
        }
    }

    /// <summary>
    /// Read events for state serialization. Returns null if never populated.
    /// </summary>
    internal static List<Dictionary<string, object?>>? GetEventsForState()
    {
        lock (_lock)
        {
            if (_events is null) return null;
            return _events.Select(e => new Dictionary<string, object?>
            {
                ["id"] = e.Id,
                ["summary"] = e.Summary,
                ["start"] = e.Start,
                ["end"] = e.End,
                ["rsvp_status"] = e.RsvpStatus,
            }).ToList();
        }
    }

    /// <summary>
    /// Read the <c>data.sync</c> e2e block: the sync-files list (null if never populated)
    /// plus the agent's live <c>running</c> flag and folder map — the same three keys
    /// linux reports (<c>main.rs::sync_state_json</c>), so a real-agent assertion is
    /// platform-agnostic (`testing.md` § conventions point 11).
    ///
    /// **Both live keys come from the shared Rust surface**, not a C# probe:
    /// <c>running</c> is <c>sync_agent_any_engine_serving_cached()</c> (a cache read, not
    /// a socket round trip — see this method's own body for why), and
    /// <c>folders</c> is the login-scoped binding model's <c>rendered()</c> union, exactly as
    /// linux's <c>sync_agent::current_locations()</c> reads its own model. Read through
    /// <c>App.CurrentLocationBindings</c>, NOT through the agent session — the session does not
    /// exist for the first several seconds of a login, and a folder bound in that window is
    /// still a folder this block must report. The former pipe
    /// probe (<c>PipeIsServed</c> + a hand-rolled <c>ListEngines</c>/<c>ListLocations</c>
    /// round-trip) retired with the C# codec.
    ///
    /// **Deliberately the REAL session model, never the page's injected fake.**
    /// <c>sync_inject_locations</c> swaps the Folders page onto an in-memory channel and a
    /// throwaway model so the folder list renders without an agent; reading this block
    /// through that would let a test assert "an engine is serving" on a box where no agent
    /// exists — a green that cannot fail. Linux draws the same line: <c>sync_add_location</c>
    /// moves the agent binding model and <c>sync_inject_locations</c> — a pure render
    /// fixture — does not.
    ///
    /// Blocking is safe: the serializer runs on a thread-pool thread, never the UI thread
    /// (see <c>App.xaml.cs</c>'s <c>_testCurrentView</c> note).
    /// </summary>
    internal static Dictionary<string, object?>? GetSyncForState()
    {
        List<Dictionary<string, object?>>? files;
        lock (_lock)
        {
            files = _syncFiles?.Select(f => new Dictionary<string, object?>
            {
                ["path"] = f.Path,
                ["folder"] = f.Folder,
                ["state"] = f.State,
            }).ToList();
        }

        // Never hold _lock across the agent reads below — it would serialize every
        // snapshot writer behind an IPC round-trip.
        var locations = new List<Dictionary<string, object?>>();
        foreach (var row in App.CurrentLocationBindings?.Rendered() ?? System.Array.Empty<FfiBindingRow>())
        {
            locations.Add(new Dictionary<string, object?>
            {
                ["path"] = row.@path,
                ["folder"] = row.@folder,
            });
        }

        return new Dictionary<string, object?>
        {
            ["files"] = files,
            // A cache read, not an IPC round trip: the state provider is the
            // TestAgent's ack path and must do no blocking I/O (convention 11's
            // second corollary — e2e-latency-independent-assertions.md §
            // Implementation status today; the `provider=6007ms` history that
            // motivated this is recorded there and at TestAgent.cs's barrier-command
            // comment). `SyncAgentAnyEngineServingCached` is the synchronous UniFFI
            // twin of the shared `fauna_client_sync::agent::any_engine_serving_cached`
            // cache, callable directly from this non-async context (added 2026-08-22,
            // macOS leg) — no local cache needed on top of it.
            ["running"] = FaunaFfiMethods.SyncAgentAnyEngineServingCached(),
            ["locations"] = locations,
            // The causal barrier for "nothing re-provisions after a teardown": while this
            // is true a session install could still go on to provision the agent, so a
            // negative assert taken before it clears is a bet on timing rather than a
            // verdict (e2e-conventions.md § point 14 — the shape row 57 hid behind).
            ["agent_install_in_flight"] = App.SyncAgentInstallInFlight,
        };
    }

    /// <summary>
    /// Clear all snapshot data (on reset/logout).
    /// </summary>
    internal static void Clear()
    {
        lock (_lock)
        {
            _feedPosts = null;
            _unreadNotificationCount = 0;
            _conversations = null;
            _contacts = null;
            _knocks = null;
            _events = null;
            _syncFiles = null;
        }
    }

    // Immutable snapshot records — no observable bindings, pure data.
    internal record FeedPostSnapshot(
        string PostId,
        string Author,
        string Body,
        long Timestamp,
        List<string> Tags,
        bool HasMedia,
        string MediaHash,
        bool IsReply,
        long LikeCount,
        long ReplyCount,
        long RepostCount,
        long QuoteCount,
        bool ViewerLiked,
        string? RepostedPostId,
        string? ViewerRepostId,
        List<Dictionary<string, object?>> LinkPreviews);

    internal record ConversationSnapshot(
        string ActorId,
        string? Handle,
        string? LastMessage,
        ulong? LastTimestamp,
        uint UnreadCount);

    internal record ContactSnapshot(
        string ActorId,
        string? Handle,
        string Status);

    internal record KnockSnapshot(
        string ActorId,
        string? Summary,
        ulong Timestamp);

    internal record EventSnapshot(
        string Id,
        string Summary,
        string Start,
        string? End,
        string? RsvpStatus);

    internal record SyncFileSnapshot(
        string Path,
        string Folder,
        string State);
}
