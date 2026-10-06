using System;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit check for the draft-persistence v2 conversations leg
/// (docs/goal/behavior/file-sync.md § Drafts Sync). <see cref="ConversationDraftsService"/>
/// restores the manager's draft set from the nest-backed `__drafts` plane on launch
/// and saves (debounced) the manager's snapshot after a compose change — both over the
/// shared, stateful <c>fauna.drafts.{get,put}</c> autosync seam (<see cref="IFfiDraftsSync"/>,
/// the canonical <c>DraftsSync</c> wrapper that owns the launch gate + last-saved-baseline
/// dedup in shared Rust). The seal, WS call, gate, and dedup are NOT re-implemented in C#;
/// these tests only exercise the per-app trigger glue (restore-on-launch + debounced
/// save). Fakes both narrow seams (the full <c>IConversationsManager</c> is huge — the
/// service depends only on a 2-method draft view, <see cref="IConversationDraftStore"/>).
/// </summary>
public class ConversationDraftsServiceTests
{
    private sealed class FakeDraftsSync : FfiDraftsSyncFakeBase
    {
        public byte[]? LoadResult;
        public int LoadCount;
        public byte[]? SavedBytes;
        public int SaveCount;
        public bool SaveResult = true;
        public Action? OnSaved;
        public Action? OnLoadInvoked;

        public override Task<byte[]?> Load()
        {
            LoadCount++;
            OnLoadInvoked?.Invoke();
            return Task.FromResult(LoadResult);
        }

        public override Task<bool> SaveIfChanged(byte[] snapshot)
        {
            SavedBytes = snapshot;
            SaveCount++;
            OnSaved?.Invoke();
            return Task.FromResult(SaveResult);
        }
    }

    private sealed class FakeDraftStore : IConversationDraftStore
    {
        public byte[] Snapshot = Array.Empty<byte>();
        public ulong Epoch;
        public byte[]? Restored;
        public ulong? RestoredEpoch;
        public bool RestoreCalled;

        public byte[] SnapshotBytes() => Snapshot;

        public ulong IdentityEpoch() => Epoch;

        public Task RestoreAsync(ulong epoch, byte[] bytes)
        {
            RestoreCalled = true;
            RestoredEpoch = epoch;
            Restored = bytes;
            return Task.CompletedTask;
        }
    }

    [Fact]
    public async Task RestoreOnLaunch_WithStoredBytes_RestoresThem()
    {
        var drafts = new FakeDraftsSync { LoadResult = new byte[] { 1, 2, 3 } };
        var store = new FakeDraftStore();
        using var svc = new ConversationDraftsService(drafts, store);

        await svc.RestoreOnLaunchAsync();

        Assert.Equal(1, drafts.LoadCount);
        Assert.True(store.RestoreCalled);
        Assert.Equal(new byte[] { 1, 2, 3 }, store.Restored);
    }

    [Fact]
    public async Task RestoreOnLaunch_WithNoStoredBytes_DoesNotRestore()
    {
        var drafts = new FakeDraftsSync { LoadResult = null };
        var store = new FakeDraftStore();
        using var svc = new ConversationDraftsService(drafts, store);

        await svc.RestoreOnLaunchAsync();

        Assert.Equal(1, drafts.LoadCount);
        Assert.False(store.RestoreCalled);
        Assert.Null(store.Restored);
    }

    [Fact]
    public async Task RestoreOnLaunch_ReadsTheIdentityEpochBeforeTheLoad_NotAfter()
    {
        // Mutation: move the `_store.IdentityEpoch()` read to after `_sync.Load()` is
        // awaited, and this reds. An identity change landing while the (unbounded)
        // fetch is in flight must not be visible to the epoch this restore hands back —
        // otherwise a launch restore the outgoing account started could pass the
        // manager's moved-epoch refusal and fill the incoming account's manager
        // (docs/goal/ui/conversations.md § Persistence → *A restore the outgoing
        // account started fills nothing after an identity change*).
        var drafts = new FakeDraftsSync { LoadResult = new byte[] { 1, 2, 3 } };
        var store = new FakeDraftStore { Epoch = 1 };
        drafts.OnLoadInvoked = () => store.Epoch = 2; // identity change lands mid-flight
        using var svc = new ConversationDraftsService(drafts, store);

        await svc.RestoreOnLaunchAsync();

        Assert.True(store.RestoreCalled);
        Assert.Equal(1UL, store.RestoredEpoch);
    }

    [Fact]
    public async Task SaveNow_PassesManagerSnapshotThroughTheSharedGate()
    {
        var drafts = new FakeDraftsSync();
        var store = new FakeDraftStore { Snapshot = new byte[] { 9, 8, 7 } };
        using var svc = new ConversationDraftsService(drafts, store);

        await svc.SaveNowAsync();

        // The service is pure trigger glue: it hands the manager's snapshot to the
        // shared DraftsSync gate (which decides whether to actually PUT) — it does
        // not itself dedup or own a rail path.
        Assert.Equal(new byte[] { 9, 8, 7 }, drafts.SavedBytes);
        Assert.Equal(1, drafts.SaveCount);
    }

    [Fact]
    public async Task ScheduleSave_CoalescesRapidEditsIntoOneSave()
    {
        // THIRD widening, and the last one that should ever be needed: 40ms →
        // 200ms → 3s. 200ms still went red in a full-suite run on 2026-07-23
        // (`Expected: 1, Actual: 2`) while passing solo, because the debounce window is raced
        // by the gap between the three ADJACENT ScheduleSave() statements below — a
        // thread-pool-starved or GC-paused scheduler can stall the calling thread past the
        // window, an earlier timer fires (a real, elapsed 200ms), and two saves is then
        // CORRECT production behavior wrongly asserted.
        //
        // The window is not a "how long is long enough" guess to keep shaving: it is the
        // ceiling on inter-statement stall this assertion tolerates, so it belongs far above
        // any non-pathological delay rather than just above the last observed one (e2e
        // conventions point 14 — this is its tier_1 analogue). At 3s a split needs a
        // 3-second pause between two adjacent statements.
        //
        // ⚠ This test genuinely PAYS the window once (~3s): the debounce fires one window
        // after the last schedule, and waiting for it is the point. That is the honest cost
        // of covering a real debounce without a fake clock — and it runs in parallel with
        // the other test classes. The permanent fix is to inject the timer so the window can
        // be driven rather than waited out; until then, do not shave this back down.
        var drafts = new FakeDraftsSync();
        var store = new FakeDraftStore { Snapshot = new byte[] { 1 } };
        using var svc = new ConversationDraftsService(drafts, store, TimeSpan.FromSeconds(3));

        // Widening 40ms/600ms -> 200ms/600ms only shrank the flake rate: it still
        // raced a fixed wall-clock wait against a System.Threading.Timer callback that must
        // acquire a ThreadPool thread to fire SaveNowAsync, and the same full-suite run can
        // starve the pool for longer than any fixed guess on a busy dev machine running many
        // builds concurrently. Wait on the save's own completion signal instead —
        // latency-independent, bounded by a ceiling far above any pathological stall rather
        // than a guess at "long enough".
        var saveCompleted = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        drafts.OnSaved = () => saveCompleted.TrySetResult(true);

        svc.ScheduleSave();
        svc.ScheduleSave();
        svc.ScheduleSave();

        var winner = await Task.WhenAny(saveCompleted.Task, Task.Delay(TimeSpan.FromSeconds(30)));
        Assert.True(winner == saveCompleted.Task, "debounced save never fired within 30s");

        // Bounded settle window to catch a would-be duplicate save landing just after the
        // first — guards the coalescing assertion below, not the primary wait.
        await Task.Delay(TimeSpan.FromMilliseconds(300));

        Assert.Equal(1, drafts.SaveCount);
        Assert.Equal(new byte[] { 1 }, drafts.SavedBytes);
    }
}
