using System.Threading.Tasks;
using uniffi.fauna_conversations;

namespace FaunaApp.Conversations;

/// <summary>
/// Re-reads the composer's rooms (`FeedSnapshot.own_rooms`) on every conversations-plane
/// change — the windows twin of linux's `attach_own_rooms_refresh`
/// (<c>apps/fauna-linux/src/conversations/conv_backend.rs</c>). Registered once per manager,
/// inside <see cref="ConversationsManagerHost"/>'s own factory, so it lives for the
/// PROCESS's whole lifetime — unlike the page-scoped <see cref="ConversationsNotifyObserver"/>,
/// which exists only while <c>ConversationsPage</c> is loaded. A room joined, bound or left
/// must reach the composer with no `ConversationsPage` open (`ui/feed.md` § Encryption at
/// rest → Room-restricted — the app half, *The rooms offered*).
///
/// <para>A local read on the feed manager's side (no WS-RPC); idempotent —
/// <c>FfiFeedManager.RefreshOwnRooms</c> notifies only when the list actually changed, which
/// is what lets this fire on every unrelated conversations change (a message arriving, a
/// thread read) with no spurious feed-page repaint. Best-effort: no feed manager built yet
/// (pre-login, or the feed page has never loaded) or the call throws ⇒ silently skipped,
/// mirroring `refresh_own_rooms`'s own documented "never a page error" contract.</para>
/// </summary>
internal sealed class RoomsRefreshObserver : SnapshotObserver
{
    public void OnChanged()
    {
        if (FaunaApp.Core.FeedManagerHost.Current is not { } feed) return;
        _ = RefreshAsync(feed);
    }

    /// <summary>Fire-and-forget <c>RefreshOwnRooms</c>, swallowing a throw — shared with
    /// <c>App.BuildE2eConvSessionAsync</c>'s own backfill of a feed manager the e2e
    /// session finished building AFTER (the seam's second meeting point).</summary>
    internal static async Task RefreshAsync(uniffi.fauna_ffi.FfiFeedManager feed)
    {
        try { await feed.RefreshOwnRooms(); }
        catch { /* best-effort — mirrors refresh_own_rooms's own never-a-page-error contract */ }
    }
}
