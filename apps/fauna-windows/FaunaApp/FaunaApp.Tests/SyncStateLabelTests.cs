using FaunaApp.Core.Models;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Unit test for the computed <see cref="FolderFileInfo.SyncStateLabel"/> in-app
/// media-badge text. The windows media page is a CONTROL-PLANE surface
/// (file-sync.md § Per-file sync-status display): it lists fauna.sync.files (no
/// per-file status) and treats every listed file as present, so the badge renders
/// the shared "Synced" label off fauna_core::format::sync_display_state_label,
/// matching the web + linux media-page precedent (priority #1/#3). The test host
/// has no localizer, so Strings.Resolve falls back to the dotted i18n key — the
/// same missing-key fallback the ContactInfo.StatusLabel / device-status lifts use.
/// </summary>
public class SyncStateLabelTests
{
    [Fact]
    public void SyncStateLabel_ResolvesSharedSyncedKey()
    {
        var f = new FolderFileInfo("photos/a.jpg", 1024, 0);
        Assert.Equal("media.status_label.synced", f.SyncStateLabel);
    }

    /// <summary>
    /// Same claim as above, for <see cref="MediaItem.SyncStateLabel"/> — the
    /// cross-set MediaPage explorer's row type, distinct from
    /// <see cref="FolderFileInfo"/> (the Settings → Folders row type). Both
    /// resolve the same shared "Synced" label independently (mirrors the
    /// AttendeeInfo.RsvpLabel / ContactInfo.StatusLabel per-record pattern).
    /// </summary>
    [Fact]
    public void MediaItem_SyncStateLabel_ResolvesSharedSyncedKey()
    {
        var item = new MediaItem(
            Name: "a.jpg", Path: "photos/a.jpg", Folder: "camera-roll",
            SizeBytes: 1024, UpdatedAt: 0, ThumbnailHash: null, SourceStatus: "online");
        Assert.Equal("media.status_label.synced", item.SyncStateLabel);
    }
}
