using System;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit check for the draft-persistence v2 events leg
/// (docs/goal/behavior/reserved-folders.md § Drafts Sync; docs/goal/ui/events.md §
/// Persistence). <see cref="EventDraftsService"/> restores the compose surface's
/// five fields from the nest-backed `__drafts` plane on launch and saves (debounced)
/// after a compose change — over the shared, stateful `fauna.drafts.{get,put}`
/// autosync seam (<see cref="IFfiEventDraftsSync"/>, the typed events twin of
/// <see cref="IFfiDraftsSync"/>). The seal, WS call, gate, and dedup are NOT
/// re-implemented in C#; these tests only exercise the per-app trigger glue.
/// Mirrors <c>ConversationDraftsServiceTests</c>'s shape.
/// </summary>
public class EventDraftsServiceTests
{
    private static FfiEventDrafts Drafts(string summary = "", string dtstart = "",
        string dtend = "", string description = "", string location = "") =>
        new(summary, dtstart, dtend, description, location);

    private sealed class FakeEventDraftsSync : FfiEventDraftsSyncFakeBase
    {
        public FfiEventDrafts? RestoreResult;
        public TaskCompletionSource<FfiEventDrafts?>? RestoreGate;
        public int RestoreCount;
        public (string summary, string dtstart, string dtend, string description, string location)? Saved;
        public int SaveCount;
        public Action? OnSaved;

        public override Task<FfiEventDrafts?> RestoreDrafts()
        {
            RestoreCount++;
            return RestoreGate?.Task ?? Task.FromResult(RestoreResult);
        }

        public override Task SaveDrafts(string summary, string dtstart, string dtend, string description, string location)
        {
            Saved = (summary, dtstart, dtend, description, location);
            SaveCount++;
            OnSaved?.Invoke();
            return Task.CompletedTask;
        }
    }

    private sealed class FakeEventDraftStore : IEventDraftStore
    {
        public FfiEventDrafts CurrentSnapshot = Drafts();
        public FfiEventDrafts? Restored;
        public bool RestoreCalled;
        public bool AuthoredText;

        public FfiEventDrafts Snapshot() => CurrentSnapshot;

        public void Restore(FfiEventDrafts draft)
        {
            RestoreCalled = true;
            Restored = draft;
        }

        public bool HasAuthoredText() => AuthoredText;
    }

    [Fact]
    public async Task RestoreOnLaunch_WithStoredDraft_RestoresIt()
    {
        var sync = new FakeEventDraftsSync { RestoreResult = Drafts("Standup", "2026-09-08T09:00") };
        var store = new FakeEventDraftStore();
        using var svc = new EventDraftsService(sync, store);

        await svc.RestoreOnLaunchAsync();

        Assert.Equal(1, sync.RestoreCount);
        Assert.True(store.RestoreCalled);
        Assert.Equal("Standup", store.Restored?.summary);
    }

    [Fact]
    public async Task RestoreOnLaunch_WithNoStoredDraft_DoesNotRestore()
    {
        var sync = new FakeEventDraftsSync { RestoreResult = null };
        var store = new FakeEventDraftStore();
        using var svc = new EventDraftsService(sync, store);

        await svc.RestoreOnLaunchAsync();

        Assert.False(store.RestoreCalled);
    }

    [Fact]
    public async Task RestoreOnLaunch_WhenComposeAlreadyHasAuthoredText_Declines()
    {
        // The non-destructive-resume rule (reserved-folders.md § Drafts Sync's
        // fourth rule): a late-arriving restore must never clobber text the user
        // has already started typing.
        var sync = new FakeEventDraftsSync { RestoreResult = Drafts("Restored") };
        var store = new FakeEventDraftStore { AuthoredText = true };
        using var svc = new EventDraftsService(sync, store);

        await svc.RestoreOnLaunchAsync();

        Assert.False(store.RestoreCalled);
    }

    [Fact]
    public async Task SaveNow_PassesTheComposeSurfaceSnapshotThroughTheSharedGate()
    {
        var sync = new FakeEventDraftsSync();
        var store = new FakeEventDraftStore
        {
            CurrentSnapshot = Drafts("Retro", "2026-09-08T10:00", "2026-09-08T11:00", "notes", "Room 1"),
        };
        using var svc = new EventDraftsService(sync, store);

        await svc.SaveNowAsync();

        Assert.Equal(1, sync.SaveCount);
        Assert.Equal(("Retro", "2026-09-08T10:00", "2026-09-08T11:00", "notes", "Room 1"), sync.Saved);
    }

    [Fact]
    public async Task ClearAsync_SavesFiveEmptyStringsImmediately()
    {
        var sync = new FakeEventDraftsSync();
        var store = new FakeEventDraftStore { CurrentSnapshot = Drafts("leftover") };
        using var svc = new EventDraftsService(sync, store);

        await svc.ClearAsync();

        Assert.Equal(1, sync.SaveCount);
        Assert.Equal(("", "", "", "", ""), sync.Saved);
    }

    [Fact]
    public async Task ClearAsync_CancelsAPendingDebouncedSaveFirst()
    {
        // A day-cell fresh start right after a burst of typing must not let the
        // stale debounced save resurrect the just-cleared rail a moment later.
        var sync = new FakeEventDraftsSync();
        var store = new FakeEventDraftStore { CurrentSnapshot = Drafts("about to be cleared") };
        using var svc = new EventDraftsService(sync, store, TimeSpan.FromSeconds(3));

        svc.ScheduleSave();
        await svc.ClearAsync();

        // Give the (should-be-cancelled) 3s timer no chance to still fire during
        // this test's own lifetime — a short settle window, not the timer's window.
        await Task.Delay(TimeSpan.FromMilliseconds(300));

        Assert.Equal(1, sync.SaveCount);
        Assert.Equal(("", "", "", "", ""), sync.Saved);
    }

    [Fact]
    public async Task ScheduleSave_CoalescesRapidEditsIntoOneSave()
    {
        var sync = new FakeEventDraftsSync();
        var store = new FakeEventDraftStore { CurrentSnapshot = Drafts("final") };
        using var svc = new EventDraftsService(sync, store, TimeSpan.FromSeconds(3));

        var saveCompleted = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        sync.OnSaved = () => saveCompleted.TrySetResult(true);

        svc.ScheduleSave();
        svc.ScheduleSave();
        svc.ScheduleSave();

        var winner = await Task.WhenAny(saveCompleted.Task, Task.Delay(TimeSpan.FromSeconds(30)));
        Assert.True(winner == saveCompleted.Task, "debounced save never fired within 30s");

        await Task.Delay(TimeSpan.FromMilliseconds(300));

        Assert.Equal(1, sync.SaveCount);
    }

    [Fact]
    public async Task Dispose_DuringAnInFlightRestore_DiscardsItsLateResult()
    {
        // The identity seam (account-scoping.md § The scoping taxonomy, the
        // in-memory corollary). UniFFI's async export gives
        // no cooperative cancellation, so a RestoreDrafts() call already suspended
        // when Dispose() runs is NOT itself cancelled — it still completes and
        // returns a value. Without the post-await token check this test pins, that
        // stale result would still reach IEventDraftStore.Restore() and repaint a
        // superseded actor's draft into the (by then reassigned) compose surface —
        // exactly the leak android/tui/web each shipped and had to fix.
        var sync = new FakeEventDraftsSync
        {
            RestoreGate = new TaskCompletionSource<FfiEventDrafts?>(TaskCreationOptions.RunContinuationsAsynchronously),
        };
        var store = new FakeEventDraftStore();
        var svc = new EventDraftsService(sync, store);

        var restoring = svc.RestoreOnLaunchAsync();

        // Dispose while RestoreDrafts() is still suspended — the generation token
        // is cancelled now, before the call has returned anything.
        svc.Dispose();

        // The call finally "returns" (as it would on a real slow nest) — with a
        // real draft, which a naive implementation would apply unconditionally.
        sync.RestoreGate.SetResult(Drafts("a superseded actor's leftover draft"));
        await restoring;

        Assert.False(store.RestoreCalled,
            "a restore that outlived Dispose() must never reach the compose surface");
    }

    [Fact]
    public void Dispose_DisposesTheUnderlyingSyncHandleToo()
    {
        // Review correction #5: the sibling rails
        // (ConversationDraftsService/FeedDraftsService) null their sync handle on
        // teardown without disposing it; this leg does not silently inherit that.
        var sync = new DisposableFakeEventDraftsSync();
        var store = new FakeEventDraftStore();
        var svc = new EventDraftsService(sync, store);

        svc.Dispose();

        Assert.True(sync.Disposed);
    }

    private sealed class DisposableFakeEventDraftsSync : FfiEventDraftsSyncFakeBase, IDisposable
    {
        public bool Disposed;
        public void Dispose() => Disposed = true;
    }
}
