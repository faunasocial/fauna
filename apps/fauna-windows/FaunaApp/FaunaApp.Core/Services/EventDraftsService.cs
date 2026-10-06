using System;
using System.Threading;
using System.Threading.Tasks;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The two draft operations the events leg needs — the events twin of
/// <see cref="IConversationDraftStore"/>/<see cref="IFeedDraftStore"/>, but typed
/// (<see cref="FfiEventDrafts"/>) rather than raw bytes: the Events page has no
/// shared manager anywhere to hang a canonical encoding off (reserved-folders.md
/// § Drafts Sync's 2026-08-17 ruling), so this seam carries the five
/// <c>event-form</c> fields directly.
/// </summary>
internal interface IEventDraftStore
{
    /// <summary>The five <c>event-form</c> fields as the caller's compose surface
    /// holds them right now.</summary>
    FfiEventDrafts Snapshot();

    /// <summary>Write a restored draft into the compose surface.</summary>
    void Restore(FfiEventDrafts draft);

    /// <summary>True if the user has already typed into any of the five fields.
    /// The non-destructive-resume guard (reserved-folders.md § Drafts Sync's
    /// fourth rule) reads this before calling <see cref="Restore"/> — declining
    /// whenever the compose already holds authored text, exactly the caller-side
    /// decision the rule assigns, never the accessor's own.</summary>
    bool HasAuthoredText();
}

/// <summary>
/// Draft-persistence v2, the events leg (docs/goal/behavior/reserved-folders.md §
/// Drafts Sync; docs/goal/ui/events.md § Persistence — the windows arm; six of
/// seven apps landed first). Pure trigger glue over the shared, stateful
/// <see cref="IFfiEventDraftsSync"/> (<c>FfiNestClient.EventDrafts()</c>): the seal,
/// the wire call, the launch gate, and the last-saved-baseline dedup all stay in
/// Rust (<c>fauna_client_caldav::drafts::EventDrafts</c> +
/// <c>fauna_client_drafts::DraftsSync</c>), never re-implemented here — only the
/// debounce and the compose-surface read/write are per-platform.
///
/// <para>Deliberately NOT shaped like <see cref="ConversationDraftsService"/>/
/// <see cref="FeedDraftsService"/> (a store that re-snapshots a shared manager):
/// <c>EventsPage</c> IS the store (its five <c>TextBox</c>es hold the live draft
/// directly, per <see cref="IEventDraftStore"/>), and — because the page is
/// <c>NavigationCacheMode.Required</c> and therefore never torn down within a
/// session — reading it live at save time already satisfies "the rail holds the
/// live draft" (reserved-folders.md's non-destructive-resume rule) with no extra
/// state to keep in sync.</para>
///
/// <para><b>Identity seam</b> (account-scoping.md § The scoping taxonomy, the
/// in-memory corollary): this service is built fresh per login
/// (<c>App.WireEventDraftsAsync</c>) rather than reused across an account switch
/// like the manager-backed rails, so a superseded actor's in-flight
/// <see cref="RestoreOnLaunchAsync"/> can only ever mutate ITS OWN (already
/// discarded) instance — there is no shared singleton for it to corrupt. The
/// remaining gap the other three apps' legs hit (a restore already suspended
/// inside <c>RestoreDrafts()</c> is not cancelled by disposal alone, so it still
/// returns and would still assign) is closed here with a per-instance
/// cancellation check AFTER the await, at the write — never trusted to happen
/// only at launch.</para>
///
/// <para>A service (not a WinUI VM), so it uses <c>ConfigureAwait(false)</c> and
/// never touches bound UI state directly — restore/save call
/// <see cref="IEventDraftStore"/> off the caller's thread; <c>EventsPage</c>'s own
/// implementation marshals back to the UI thread where WinUI requires it.</para>
/// </summary>
internal sealed class EventDraftsService : IDisposable
{
    private readonly IFfiEventDraftsSync _sync;
    private readonly IEventDraftStore _store;
    private readonly TimeSpan _debounce;
    private readonly object _gate = new();
    private readonly CancellationTokenSource _generation = new();
    private Timer? _timer;
    private int _disposedFlag;

    public EventDraftsService(
        IFfiEventDraftsSync sync,
        IEventDraftStore store,
        TimeSpan? debounce = null)
    {
        _sync = sync;
        _store = store;
        // Shared `fauna_client_drafts::AUTOSAVE_DEBOUNCE` via
        // `FaunaFfiMethods.AutosaveDebounceMs()` — the same window every app now
        // reads (reserved-folders.md § Drafts Sync), never a re-declared literal
        // (unlike FeedDraftsService's pre-existing 600ms, which predates the
        // shared constant and stays as its own windows-side value).
        _debounce = debounce ?? TimeSpan.FromMilliseconds(FaunaFfiMethods.AutosaveDebounceMs());
    }

    /// <summary>
    /// Fetch + unseal the actor's stored event draft and write it into the compose
    /// surface. <c>None</c> (first run, an all-empty record, or an undecodable
    /// blob) leaves the surface untouched — the caller's own default already is
    /// empty. Best-effort: a transport/seal failure must never block login, so it
    /// is swallowed (the save gate stays closed until a later load succeeds).
    ///
    /// <para>Checks the identity-seam token AFTER the await, not before: a call
    /// already suspended inside <see cref="IFfiEventDraftsSync.RestoreDrafts"/>
    /// when <see cref="Dispose"/> runs is not itself cancelled (UniFFI's async
    /// export has no cooperative cancellation), so it still returns — this is
    /// what stops that stale result from ever reaching <see cref="IEventDraftStore.Restore"/>.</para>
    /// </summary>
    public async Task RestoreOnLaunchAsync()
    {
        var token = _generation.Token;
        FfiEventDrafts? draft;
        try
        {
            draft = await _sync.RestoreDrafts().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            FaunaApp.Core.Logs.E2eTrace.Write($"[event-drafts] RestoreOnLaunchAsync: RestoreDrafts() threw: {ex.GetType().Name}: {ex.Message}");
            return;
        }
        FaunaApp.Core.Logs.E2eTrace.Write($"[event-drafts] RestoreOnLaunchAsync: RestoreDrafts() returned {(draft is null ? "null" : $"summary={draft.summary.Length} chars")}");
        if (token.IsCancellationRequested) return;
        if (draft is { } d && !_store.HasAuthoredText())
            _store.Restore(d);
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
            if (Volatile.Read(ref _disposedFlag) != 0) return;
            _timer?.Dispose();
            _timer = new Timer(_ => { _ = SaveNowAsync(); }, null, _debounce, Timeout.InfiniteTimeSpan);
        }
    }

    /// <summary>
    /// Hand the compose surface's CURRENT five fields to the shared
    /// <c>DraftsSync</c> gate. The gate decides whether to actually seal + PUT:
    /// it no-ops before the launch GET has succeeded and skips an unchanged
    /// record (so this is safe to call eagerly). No-ops after
    /// <see cref="Dispose"/> — a timer callback racing teardown must not fire a
    /// save against a torn-down connection. Best-effort: a failed save must
    /// never crash a compose/close path.
    /// </summary>
    public async Task SaveNowAsync()
    {
        if (Volatile.Read(ref _disposedFlag) != 0) return;
        var d = _store.Snapshot();
        FaunaApp.Core.Logs.E2eTrace.Write($"[event-drafts] SaveNowAsync: summary={d.summary.Length} chars");
        try
        {
            await _sync.SaveDrafts(d.summary, d.dtstart, d.dtend, d.description, d.location)
                .ConfigureAwait(false);
            FaunaApp.Core.Logs.E2eTrace.Write("[event-drafts] SaveNowAsync: SaveDrafts succeeded");
        }
        catch (Exception ex)
        {
            FaunaApp.Core.Logs.E2eTrace.Write($"[event-drafts] SaveNowAsync threw: {ex.GetType().Name}: {ex.Message}");
        }
    }

    /// <summary>
    /// Clear the rail immediately (not debounced) — a successful create or a
    /// day-cell fresh start (docs/goal/ui/events.md § Persistence's second and
    /// third resume rules), never on the submit CLICK: a failed create must
    /// leave the user's text where they can retry it. Cancels any pending
    /// debounced save first, so a stale in-flight edit can't resurrect the
    /// just-cleared rail a moment later.
    /// </summary>
    public Task ClearAsync()
    {
        lock (_gate)
        {
            _timer?.Dispose();
            _timer = null;
        }
        return SaveEmptyAsync();
    }

    private async Task SaveEmptyAsync()
    {
        if (Volatile.Read(ref _disposedFlag) != 0) return;
        try
        {
            await _sync.SaveDrafts("", "", "", "", "").ConfigureAwait(false);
        }
        catch
        {
            // best-effort; see SaveNowAsync.
        }
    }

    /// <summary>
    /// Fire a still-pending debounced save immediately, or no-op if nothing is
    /// armed — the leave-door flush (reserved-folders.md § The leave-flush
    /// promise), mirroring <see cref="FeedDraftsService.FlushIfPendingAsync"/>
    /// exactly (same reasoning: an unconditional flush from a page the user
    /// never actually typed into would write a spurious empty record).
    /// </summary>
    public Task FlushIfPendingAsync()
    {
        Timer? pending;
        lock (_gate)
        {
            pending = _timer;
            _timer = null;
        }
        return pending is null ? Task.CompletedTask : FireAsync(pending);

        Task FireAsync(Timer t)
        {
            t.Dispose();
            return SaveNowAsync();
        }
    }

    public void Dispose()
    {
        lock (_gate)
        {
            Volatile.Write(ref _disposedFlag, 1);
            _timer?.Dispose();
            _timer = null;
        }
        try { _generation.Cancel(); } catch (AggregateException) { /* a callback's own fault, not this drop's */ }
        _generation.Dispose();
        (_sync as IDisposable)?.Dispose();
    }
}
