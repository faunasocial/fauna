using System;
using System.Threading;
using System.Threading.Tasks;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The two draft operations the feed leg needs from the shared
/// <c>FfiFeedManager</c> — the feed twin of <c>IConversationDraftStore</c>, a
/// narrow seam so <see cref="FeedDraftsService"/> is faked with a 2-method
/// stub in tests.
/// </summary>
internal interface IFeedDraftStore
{
    /// <summary>The composer's canonical at-rest bytes (<c>FfiFeedManager.DraftsSnapshotBytes</c>).</summary>
    byte[] SnapshotBytes();

    /// <summary>Replace the manager's compose state with a restored snapshot
    /// (<c>FfiFeedManager.RestoreDrafts</c>).</summary>
    void Restore(byte[] bytes);
}

/// <summary>Adapts the shared <see cref="IFfiFeedManager"/> to the narrow
/// <see cref="IFeedDraftStore"/> seam.</summary>
internal sealed class ManagerFeedDraftStore : IFeedDraftStore
{
    private readonly IFfiFeedManager _manager;

    public ManagerFeedDraftStore(IFfiFeedManager manager) => _manager = manager;

    public byte[] SnapshotBytes() => _manager.DraftsSnapshotBytes();

    public void Restore(byte[] bytes) => _manager.RestoreDrafts(bytes);
}

/// <summary>
/// Draft-persistence v2, the feed leg (docs/goal/behavior/reserved-folders.md §
/// Drafts Sync; docs/goal/ui/feed.md § Persistence). The per-app glue: restore the
/// manager's compose state from the nest-backed <c>__drafts</c> reserved folder
/// (rail <c>"posts"</c>) and save (debounced) the manager's snapshot after a
/// compose change. Both go through the shared, stateful <see cref="IFfiDraftsSync"/>
/// — the same canonical <c>fauna_client_drafts::DraftsSync</c> wrapper the
/// conversations leg (<see cref="ConversationDraftsService"/>) uses, which owns the
/// launch gate and the last-saved-baseline dedup in shared Rust. Retires the feed
/// rail's use of the local-file C# <c>DraftStore</c>/<c>DraftPersistenceService</c>
/// (deleted in the same change) with nest-backed, cross-device persistence,
/// converging windows onto the same shared gate as tui/linux/web/android (priority
/// #1/#2/#4).
///
/// <para><b>Windows' own wrinkle, unlike the conversations manager: <c>FfiFeedManager</c>
/// is rebuilt on every Feed-page load</b> (<c>FeedPage.Page_Loaded</c>), not once per
/// session. A fresh <see cref="FeedDraftsService"/> instance therefore wraps each
/// freshly-built manager — <see cref="App.FeedDraftsSync"/> (the sync handle) is what
/// is held once at login and shared across instances, mirroring android's
/// <c>FeedManagerHost.manager()</c>, which re-runs <c>sync.load()</c> for every fresh
/// manager it sees rather than assuming a single load suffices for the whole
/// session.</para>
///
/// <para>A service (not a WinUI VM), so it uses <c>ConfigureAwait(false)</c> and never
/// touches bound UI state — the snapshot read + the save run off the UI thread; the
/// manager call is thread-safe (Rust <c>Arc</c> + interior lock).</para>
/// </summary>
internal sealed class FeedDraftsService : IDisposable
{
    private readonly IFfiDraftsSync _sync;
    private readonly IFeedDraftStore _store;
    private readonly TimeSpan _debounce;
    private readonly object _gate = new();
    private Timer? _timer;
    private bool _disposed;

    public FeedDraftsService(
        IFfiDraftsSync sync,
        IFeedDraftStore store,
        TimeSpan? debounce = null)
    {
        _sync = sync;
        _store = store;
        // Same ~600ms windows already uses for the conversations leg
        // (ConversationDraftsService) — reused rather than minting a second debounce
        // constant for the same platform (reserved-folders.md § Drafts Sync notes
        // AUTOSAVE_DEBOUNCE as the shared-Rust legs' 1500ms; windows' own local
        // constant predates it and stays the windows-side value).
        _debounce = debounce ?? TimeSpan.FromMilliseconds(600);
    }

    /// <summary>
    /// Fetch + unseal the actor's stored feed draft and restore it into the
    /// manager. <c>None</c> (first run, or nothing typed yet on any device) leaves
    /// the empty composer untouched. Best-effort: a transport/seal failure must
    /// never block the feed page from loading, so it is swallowed (the user keeps
    /// composing; once a later load succeeds the gate opens and saves resume).
    /// </summary>
    public async Task RestoreOnLaunchAsync()
    {
        byte[]? bytes;
        try
        {
            bytes = await _sync.Load().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            FaunaApp.Core.Logs.E2eTrace.Write($"[feed-drafts] RestoreOnLaunchAsync: Load() threw: {ex.GetType().Name}: {ex.Message}");
            return;
        }
        FaunaApp.Core.Logs.E2eTrace.Write($"[feed-drafts] RestoreOnLaunchAsync: Load() returned {(bytes is null ? "null" : $"{bytes.Length} bytes")}");
        if (bytes is not null)
            _store.Restore(bytes);
    }

    /// <summary>
    /// Schedule a debounced save. Each call (re)starts the timer, so a burst of
    /// compose edits coalesces into a single <see cref="SaveNowAsync"/> after the
    /// quiet window.
    /// </summary>
    public void ScheduleSave()
    {
        lock (_gate)
        {
            if (_disposed)
            {
                FaunaApp.Core.Logs.E2eTrace.Write($"[feed-drafts] ScheduleSave: disposed, no-op (instance {GetHashCode()})");
                return;
            }
            FaunaApp.Core.Logs.E2eTrace.Write($"[feed-drafts] ScheduleSave: armed (instance {GetHashCode()})");
            _timer?.Dispose();
            _timer = new Timer(_ => { _ = SaveNowAsync(); }, null, _debounce, Timeout.InfiniteTimeSpan);
        }
    }

    /// <summary>
    /// Hand the manager's current draft snapshot to the shared <c>DraftsSync</c> gate.
    /// The gate decides whether to actually seal + PUT: it no-ops before the launch GET
    /// has succeeded and skips an unchanged set (so this is safe to call eagerly, e.g.
    /// immediately after a post clears the composer). Best-effort: a failed save must
    /// never crash a compose/close path.
    /// </summary>
    public async Task SaveNowAsync()
    {
        byte[] snapshot = _store.SnapshotBytes();
        FaunaApp.Core.Logs.E2eTrace.Write($"[feed-drafts] SaveNowAsync: snapshot={snapshot.Length} bytes (instance {GetHashCode()})");
        try
        {
            var wrote = await _sync.SaveIfChanged(snapshot).ConfigureAwait(false);
            FaunaApp.Core.Logs.E2eTrace.Write($"[feed-drafts] SaveNowAsync: SaveIfChanged wrote={wrote}");
        }
        catch (Exception ex)
        {
            FaunaApp.Core.Logs.E2eTrace.Write($"[feed-drafts] SaveNowAsync threw: {ex.GetType().Name}: {ex.Message}");
        }
    }

    /// <summary>
    /// Fire a still-pending debounced save immediately, or no-op if nothing is
    /// armed. Unlike <see cref="SaveNowAsync"/> (unconditional — used after a
    /// deliberate state change like posting), this is for a control/page tearing
    /// down: <c>FfiFeedManager</c> is rebuilt on every Feed-page load
    /// (this type's own doc comment), so a torn-down instance this navigation
    /// away never actually typed into has nothing pending and an unconditional
    /// flush would write a spurious empty snapshot — observably indistinguishable
    /// from a real save to anything polling the rail (nest-side <c>drafts.get</c>
    /// included), which raced and masked a genuine bug the day this guard was
    /// added (docs/goal/ui/feed.md § Persistence).
    /// </summary>
    public Task FlushIfPendingAsync()
    {
        Timer? pending;
        lock (_gate)
        {
            pending = _timer;
            _timer = null;
        }
        if (pending is null)
        {
            FaunaApp.Core.Logs.E2eTrace.Write($"[feed-drafts] FlushIfPendingAsync: nothing armed, no-op (instance {GetHashCode()})");
            return Task.CompletedTask;
        }
        pending.Dispose();
        FaunaApp.Core.Logs.E2eTrace.Write($"[feed-drafts] FlushIfPendingAsync: firing pending save now (instance {GetHashCode()})");
        return SaveNowAsync();
    }

    public void Dispose()
    {
        FaunaApp.Core.Logs.E2eTrace.Write($"[feed-drafts] Dispose (instance {GetHashCode()})");
        lock (_gate)
        {
            _disposed = true;
            _timer?.Dispose();
            _timer = null;
        }
    }
}
