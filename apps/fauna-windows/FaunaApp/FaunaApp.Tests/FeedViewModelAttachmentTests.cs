using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_feed;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// Red-first pin for `ui/feed.md` § Persistence → *Attachments by content
/// address*: a content-only compose edit (typing, the post-restore sync, the
/// compose dialog) must carry the manager's already-staged <c>attached_file</c>
/// through <c>update_compose</c> rather than dropping it as <c>null</c> — the
/// defect windows shipped with. Builds fully offline (mirrors <see cref="FeedObserverVtableTests"/>):
/// <c>NestClient::new</c> opens no socket.
/// </summary>
public class FeedViewModelAttachmentTests
{
    private sealed class NullObserver : FeedSnapshotObserver
    {
        public void OnChanged() { }
    }

    private static FeedViewModel MakeViewModel(out FfiFeedManager manager, out FfiNestClient nest)
    {
        nest = new FfiNestClient("wss://127.0.0.1:0/ws", new byte[32]);
        manager = nest.FeedManager(new byte[32]);
        return new FeedViewModel(manager, new NullObserver(), new MockNestRpcClient());
    }

    [Fact]
    public void UpdateComposeText_CarriesForwardAnAlreadyStagedAttachment()
    {
        var vm = MakeViewModel(out var manager, out var nest);
        using (nest) using (manager)
        {
            // A restored draft's hash-less handle, staged exactly as
            // FeedDraftsService.RestoreOnLaunchAsync would leave it.
            var restored = new AttachedFile("photo.png", 2048UL, null, null);
            vm.UpdateCompose("draft text", "tag1", restored);

            // A content-only edit — the keystroke / post-restore-sync /
            // compose-dialog shape — must not drop the handle it never touched.
            vm.UpdateComposeText("draft text edited", "tag1 tag2");

            Assert.Equal(restored, manager.Snapshot().compose.attachedFile);
        }
    }

    [Fact]
    public void UpdateCompose_StillStagesAnExplicitRemoval()
    {
        var vm = MakeViewModel(out var manager, out var nest);
        using (nest) using (manager)
        {
            vm.UpdateCompose("draft text", "", new AttachedFile("photo.png", 2048UL, null, null));

            // The user's own remove gesture — the one path allowed to clear it.
            vm.UpdateCompose("draft text", "", null);

            Assert.Null(manager.Snapshot().compose.attachedFile);
        }
    }
}
