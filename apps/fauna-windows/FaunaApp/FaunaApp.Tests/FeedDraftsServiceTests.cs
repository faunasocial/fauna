using System;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The feed leg's once-per-manager restore (docs/goal/behavior/reserved-folders.md §
/// Drafts Sync; docs/goal/ui/feed.md § Persistence). The feed manager lives for the
/// session (<c>FeedManagerHost</c>), so a Feed-page load that re-ran the restore would
/// write the nest's older (or another device's) draft over the live compose;
/// <see cref="FeedDraftsService.EnsureAsync"/> restores once per (manager, sync handle)
/// pair.
/// </summary>
public class FeedDraftsServiceTests
{
    private sealed class FakeDraftsSync : FfiDraftsSyncFakeBase
    {
        public byte[]? LoadResult;
        public bool LoadThrows;
        public int LoadCount;

        public override Task<byte[]?> Load()
        {
            LoadCount++;
            if (LoadThrows) throw new InvalidOperationException("transport down");
            return Task.FromResult(LoadResult);
        }

        public override Task<bool> SaveIfChanged(byte[] snapshot) => Task.FromResult(false);
    }

    private sealed class FakeStore : IFeedDraftStore
    {
        public int RestoreCount;
        public byte[] SnapshotBytes() => Array.Empty<byte>();
        public void Restore(byte[] bytes) => RestoreCount++;
    }

    [Fact]
    public async Task SecondLoadOverTheSameManagerAndSync_DoesNotRestoreAgain()
    {
        // Mutation: drop the reuse check in EnsureAsync -> the nest's older draft is
        // written over the live compose on every Feed-page load.
        var sync = new FakeDraftsSync { LoadResult = new byte[] { 1 } };
        var store = new FakeStore();
        var manager = new object();

        var (first, firstRestored) = await FeedDraftsService.EnsureAsync(null, sync, manager, () => store);
        var (second, secondRestored) = await FeedDraftsService.EnsureAsync(first, sync, manager, () => store);

        Assert.True(firstRestored);
        Assert.False(secondRestored);
        Assert.Same(first, second);
        Assert.Equal(1, sync.LoadCount);
        Assert.Equal(1, store.RestoreCount);
    }

    [Fact]
    public async Task AFreshManager_RestoresAgain_AndRetiresTheOldService()
    {
        var sync = new FakeDraftsSync { LoadResult = new byte[] { 1 } };
        var store = new FakeStore();

        var (first, _) = await FeedDraftsService.EnsureAsync(null, sync, new object(), () => store);
        var (second, restored) = await FeedDraftsService.EnsureAsync(first, sync, new object(), () => store);

        Assert.True(restored);
        Assert.NotSame(first, second);
        Assert.Equal(2, store.RestoreCount);
    }

    [Fact]
    public async Task ALateArrivingSyncHandle_RestoresWhenItArrives()
    {
        // The e2e sync handle can arrive after the first Feed load; the pair is what
        // is keyed, so the load that finally sees the handle restores, once.
        var sync = new FakeDraftsSync { LoadResult = new byte[] { 1 } };
        var store = new FakeStore();
        var manager = new object();

        var (first, _) = await FeedDraftsService.EnsureAsync(null, sync, manager, () => store);
        var otherSync = new FakeDraftsSync { LoadResult = new byte[] { 2 } };
        var (second, restored) = await FeedDraftsService.EnsureAsync(first, otherSync, manager, () => store);

        Assert.True(restored);
        Assert.NotSame(first, second);
        Assert.Equal(1, otherSync.LoadCount);
    }

    [Fact]
    public async Task AFailedLoad_IsRetriedOnTheNextLoad()
    {
        // A swallowed Load failure leaves the save gate closed; the next page load
        // must try again rather than treat the pair as restored.
        var sync = new FakeDraftsSync { LoadThrows = true };
        var store = new FakeStore();
        var manager = new object();

        var (first, firstRestored) = await FeedDraftsService.EnsureAsync(null, sync, manager, () => store);
        sync.LoadThrows = false;
        sync.LoadResult = new byte[] { 1 };
        var (second, secondRestored) = await FeedDraftsService.EnsureAsync(first, sync, manager, () => store);

        Assert.False(firstRestored);
        Assert.True(secondRestored);
        Assert.Same(first, second);
        Assert.Equal(1, store.RestoreCount);
    }
}
