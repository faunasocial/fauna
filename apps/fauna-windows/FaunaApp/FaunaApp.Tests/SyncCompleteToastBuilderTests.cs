using FaunaApp.Core.Services;
using FaunaApp.Services;
using System.Xml.Linq;
using Microsoft.Windows.AppNotifications;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The sync-complete toast's payload (`file-sync.md` § Files Appear
/// Automatically, the desktop upload notification): one quiet notice a burst of
/// uploads collapses into, as linux's and macOS's are, its text the shared
/// strings.
/// </summary>
[Collection("StringsGlobal")]
public class SyncCompleteToastBuilderTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private readonly Dictionary<string, string> _map;
        public FakeLocalizer(Dictionary<string, string> map) => _map = map;
        public string Get(string key) => _map.TryGetValue(key, out var v) ? v : key;
    }

    private static void UseRealSyncCompleteStrings() => Strings.Initialize(new FakeLocalizer(new()
    {
        ["notifications/sync_complete_title"] = "Sync Complete",
        ["notifications/sync_complete_body"] = "{filename} uploaded",
    }));

    /// <summary>Every finished upload's toast carries the one tag and group, so a
    /// second upload replaces the first notice rather than stacking beside it.</summary>
    [Fact]
    public void Build_SharesOneTagAndGroupAcrossUploads()
    {
        UseRealSyncCompleteStrings();
        var first = SyncCompleteToastBuilder.Build("report.pdf");
        var second = SyncCompleteToastBuilder.Build("photo.jpg");

        Assert.Equal("sync-complete", first.Tag);
        Assert.Equal("sync-complete", first.Group);
        Assert.Equal(first.Tag, second.Tag);
        Assert.Equal(first.Group, second.Group);
    }

    /// <summary>The notice is quiet: no sound, the lowest priority windows has,
    /// and gone after a reboot.</summary>
    [Fact]
    public void Build_IsSilentAndLowPriority()
    {
        UseRealSyncCompleteStrings();
        var toast = SyncCompleteToastBuilder.Build("report.pdf");

        // Read as XML: the builder writes its attributes in single quotes.
        var audio = XDocument.Parse(toast.Payload).Root!.Element("audio");
        Assert.NotNull(audio);
        Assert.Equal("true", (string?)audio!.Attribute("silent"));
        Assert.Equal(AppNotificationPriority.Default, toast.Priority);
        Assert.True(toast.ExpiresOnReboot);
    }

    /// <summary>The text is the shared strings, the file name filled in by name,
    /// never a hard-coded English sentence; a file name's markup is escaped once.</summary>
    [Fact]
    public void Build_SaysTheSharedStringsAndEscapesTheFileName()
    {
        UseRealSyncCompleteStrings();
        var toast = SyncCompleteToastBuilder.Build("a <b> & c.txt");

        Assert.Contains("<text>Sync Complete</text>", toast.Payload);
        Assert.Contains("<text>a &lt;b&gt; &amp; c.txt uploaded</text>", toast.Payload);
    }
}
