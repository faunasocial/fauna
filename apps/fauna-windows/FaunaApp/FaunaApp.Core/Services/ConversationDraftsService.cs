using System;
using System.Threading;
using System.Threading.Tasks;
using uniffi.fauna_conversations;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The two draft operations the conversations leg needs from the shared
/// <c>ConversationsManager</c> — a narrow seam over the (large) generated
/// <c>IConversationsManager</c> so <see cref="ConversationDraftsService"/> is faked with a
/// 2-method stub in tests (the repo convention — every fake is a narrow interface).
/// </summary>
internal interface IConversationDraftStore
{
    /// <summary>The whole conversations-rail draft set as canonical at-rest bytes
    /// (<c>ConversationsManager.drafts_snapshot_bytes</c>); byte-stable so an unchanged
    /// set re-uploads identically.</summary>
    byte[] SnapshotBytes();

    /// <summary>The manager's identity epoch (<c>ConversationsManager.identity_epoch</c>),
    /// read synchronously and handed back to <see cref="RestoreAsync"/> so a launch
    /// restore the outgoing account started is refused rather than filling the
    /// manager the incoming account uses (docs/goal/ui/conversations.md § Persistence →
    /// <i>A restore the outgoing account started fills nothing after an identity
    /// change</i>; account-scoping.md § The scoping taxonomy).</summary>
    ulong IdentityEpoch();

    /// <summary>Replace the manager's in-memory draft set with a restored snapshot at
    /// <paramref name="epoch"/> (<c>ConversationsManager.restore_drafts_at</c>), which
    /// refuses the fill if the identity has moved since <paramref name="epoch"/> was
    /// read. Asynchronous because a fill that is not refused also owes a <i>restored</i>
    /// recipient its rail probe — a restored picker would otherwise hold an address at
    /// <c>idle</c> with nothing left to resolve it.</summary>
    Task RestoreAsync(ulong epoch, byte[] bytes);
}

/// <summary>Adapts the shared <see cref="IConversationsManager"/> to the narrow
/// <see cref="IConversationDraftStore"/> seam.</summary>
internal sealed class ManagerDraftStore : IConversationDraftStore
{
    private readonly IConversationsManager _manager;

    public ManagerDraftStore(IConversationsManager manager) => _manager = manager;

    public byte[] SnapshotBytes() => _manager.DraftsSnapshotBytes();

    public ulong IdentityEpoch() => _manager.IdentityEpoch();

    public Task RestoreAsync(ulong epoch, byte[] bytes) => _manager.RestoreDraftsAt(epoch, bytes);
}

/// <summary>
/// Draft-persistence v2, the conversations leg (docs/goal/behavior/reserved-folders.md §
/// Drafts Sync; docs/goal/ui/conversations.md § Persistence). The per-app glue: restore the
/// manager's draft set from the nest-backed <c>__drafts</c> reserved folder on launch,
/// and save (debounced) the manager's snapshot after a compose change. Both go through the
/// shared, stateful <see cref="IFfiDraftsSync"/> — the canonical <c>fauna_client_drafts::DraftsSync</c>
/// wrapper (one per session, built at login via <c>FfiNestClient.DraftsSync("conversations")</c>),
/// which seals under the owner's <c>BackupKey</c>, rides the WS-RPC transport, and owns the two
/// no-data-loss safety properties in shared Rust: the <b>launch gate</b> (never PUT before the
/// launch GET has succeeded) and the <b>last-saved-baseline dedup</b> (skip a re-upload of an
/// unchanged set). So this service is pure trigger glue — only the debounce is per-platform; the
/// seal, WS call, gate, and dedup are NOT re-implemented in C#. Converges windows onto the same
/// shared gate as web/linux/android (priority #1/#2/#4). This supersedes the conversations use of
/// the local-file C# <c>DraftStore</c> with nest-backed, cross-device persistence — the feed rail's
/// own leg is <see cref="FeedDraftsService"/>, the same shape.
///
/// <para>A service (not a WinUI VM), so it uses <c>ConfigureAwait(false)</c> and never
/// touches bound UI state — the snapshot read + the save run off the UI thread; the
/// manager call is thread-safe (Rust <c>Arc</c> + interior lock).</para>
/// </summary>
internal sealed class ConversationDraftsService : IDisposable
{
    private readonly IFfiDraftsSync _sync;
    private readonly IConversationDraftStore _store;
    private readonly TimeSpan _debounce;
    private readonly object _gate = new();
    private Timer? _timer;
    private bool _disposed;

    public ConversationDraftsService(
        IFfiDraftsSync sync,
        IConversationDraftStore store,
        TimeSpan? debounce = null)
    {
        _sync = sync;
        _store = store;
        // Read from the shared `fauna_client_drafts::AUTOSAVE_DEBOUNCE` (via
        // `FaunaFfiMethods.AutosaveDebounceMs()`) so windows coalesces typing
        // bursts on the same cadence as every other app (reserved-folders.md §
        // Drafts Sync) — no re-declared literal. The `debounce` ctor parameter
        // stays injectable for tests.
        _debounce = debounce ?? TimeSpan.FromMilliseconds(FaunaFfiMethods.AutosaveDebounceMs());
    }

    /// <summary>
    /// Fetch + unseal the actor's stored conversations drafts and restore them into the
    /// manager. <c>None</c> (first run) leaves the empty store untouched. <see cref="IFfiDraftsSync.Load"/>
    /// also records the dedup baseline and lifts the save gate; on a transport/seal failure the
    /// gate stays closed (so a later <see cref="SaveNowAsync"/> can't clobber the unread blob).
    /// Best-effort: a failure must never block app launch, so it is swallowed (the user keeps
    /// composing; once a launch GET eventually succeeds the gate opens and saves resume).
    ///
    /// <para>The identity epoch is read **here, synchronously, before <see cref="IFfiDraftsSync.Load"/>
    /// is awaited** — not after it resolves — and handed to <see cref="IConversationDraftStore.RestoreAsync"/>,
    /// which refuses the fill once the epoch has moved. Reading it after the load would defeat this:
    /// by the time an unbounded fetch resolves, the identity may already have switched, and the
    /// outgoing account's reply would otherwise fill the manager the incoming account uses
    /// (docs/goal/ui/conversations.md § Persistence; account-scoping.md § The scoping taxonomy —
    /// the native twin of apple's <c>restoreDraftsOnLaunch</c>, linux's <c>restore_when_loaded</c>).</para>
    /// </summary>
    public async Task RestoreOnLaunchAsync()
    {
        ulong epoch = _store.IdentityEpoch();
        byte[]? bytes;
        try
        {
            bytes = await _sync.Load().ConfigureAwait(false);
        }
        catch
        {
            return;
        }
        if (bytes is not null)
            await _store.RestoreAsync(epoch, bytes).ConfigureAwait(false);
    }

    /// <summary>
    /// Schedule a debounced save. Each call (re)starts the timer, so a burst of compose
    /// edits coalesces into a single <see cref="SaveNowAsync"/> after the quiet window.
    /// </summary>
    public void ScheduleSave()
    {
        lock (_gate)
        {
            if (_disposed)
                return;
            _timer?.Dispose();
            _timer = new Timer(_ => { _ = SaveNowAsync(); }, null, _debounce, Timeout.InfiniteTimeSpan);
        }
    }

    /// <summary>
    /// Hand the manager's current draft snapshot to the shared <c>DraftsSync</c> gate.
    /// The gate decides whether to actually seal + PUT: it no-ops before the launch GET
    /// has succeeded and skips an unchanged set (so this is safe to call eagerly). Best-effort:
    /// a failed save must never crash a compose/close path.
    /// </summary>
    public async Task SaveNowAsync()
    {
        byte[] snapshot = _store.SnapshotBytes();
        try
        {
            await _sync.SaveIfChanged(snapshot).ConfigureAwait(false);
        }
        catch
        {
            // best-effort persistence; the next compose change re-saves.
        }
    }

    public void Dispose()
    {
        lock (_gate)
        {
            _disposed = true;
            _timer?.Dispose();
            _timer = null;
        }
    }
}
