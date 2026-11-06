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
/// <para><b>Lifetime: one instance per (manager, sync handle) pair.</b>
/// <c>FfiFeedManager</c> lives for the session (<c>FeedManagerHost</c>), as the
/// conversations manager does, so the restore runs once per pair —
/// <see cref="EnsureAsync"/> — not on every Feed-page load: a re-restore would
/// write the nest's older (or another device's) draft over the live compose, since
/// <c>FeedManager::restore_drafts</c> overwrites a live compose unguarded. The pair
/// is keyed, not just the manager, because the e2e sync handle
/// (<c>App.FeedDraftsSync</c>) can arrive after the first Feed load; a Load that
/// failed is retried on the next load. A page load repaints its (uncached) compose
/// bar from the manager's snapshot itself. Android's
/// <c>FeedManagerHost.manager()</c> restores per new manager the same way.</para>
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
    private readonly object? _managerKey;
    private bool _disposed;
    private bool _restored;

    public FeedDraftsService(
        IFfiDraftsSync sync,
        IFeedDraftStore store,
        TimeSpan? debounce = null,
        object? managerKey = null)
    {
        _sync = sync;
        _store = store;
        _managerKey = managerKey;
        // Shared `fauna_client_drafts::autosave_debounce` via
        // `FaunaFfiMethods.AutosaveDebounceMs()` — the one window every app and every
        // rail reads (reserved-folders.md § Drafts Sync), as the conversations and
        // events legs here already do. This rail kept a windows-only 600 ms literal
        // after the others converged, which also left it out of reach of the e2e
        // window seam.
        _debounce = debounce ?? TimeSpan.FromMilliseconds(FaunaFfiMethods.AutosaveDebounceMs());
    }

    /// <summary>
    /// The service for (<paramref name="managerKey"/>, <paramref name="sync"/>): the
    /// <paramref name="existing"/> one when it is live, bound to that same pair and
    /// has restored, else a new one (the old one is retired) that restores now.
    /// <c>Restored</c> is true when this call ran a restore that reached the nest.
    /// </summary>
    public static async Task<(FeedDraftsService Service, bool Restored)> EnsureAsync(
        FeedDraftsService? existing, IFfiDraftsSync sync, object managerKey,
        Func<IFeedDraftStore> makeStore)
    {
        var sameBinding = existing is { } e
            && !e._disposed
            && ReferenceEquals(e._sync, sync)
            && ReferenceEquals(e._managerKey, managerKey);
        if (sameBinding && existing!._restored) return (existing, false);

        var service = sameBinding ? existing! : new FeedDraftsService(sync, makeStore(), managerKey: managerKey);
        if (!sameBinding) existing?.Dispose();
        var restored = await service.RestoreOnLaunchAsync().ConfigureAwait(false);
        return (service, restored);
    }

    /// <summary>
    /// Fetch + unseal the actor's stored feed draft and restore it into the
    /// manager. <c>None</c> (first run, or nothing typed yet on any device) leaves
    /// the empty composer untouched. Best-effort: a transport/seal failure must
    /// never block the feed page from loading, so it is swallowed (the user keeps
    /// composing; once a later load succeeds the gate opens and saves resume).
    /// Returns whether the Load reached the nest.
    /// </summary>
    public async Task<bool> RestoreOnLaunchAsync()
    {
        byte[]? bytes;
        try
        {
            bytes = await _sync.Load().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            FaunaApp.Core.Logs.E2eTrace.Write($"[feed-drafts] RestoreOnLaunchAsync: Load() threw: {ex.GetType().Name}: {ex.Message}");
            return false;
        }
        FaunaApp.Core.Logs.E2eTrace.Write($"[feed-drafts] RestoreOnLaunchAsync: Load() returned {(bytes is null ? "null" : $"{bytes.Length} bytes")}");
        if (bytes is not null)
            _store.Restore(bytes);
        _restored = true;
        return true;
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
    /// down: the session's one instance outlives each page, so a
    /// navigation away that never typed has nothing pending and an unconditional
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
