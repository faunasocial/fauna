using FaunaApp.Core.Media;
using uniffi.fauna_media_machine;
using Xunit;

namespace FaunaApp.Tests;

// `media-folder-filter` renders the shared-Rust value/label pairs
// (docs/goal/ui/media.md § Encryption at rest → *The filter and the upload
// target address the row*, (a)/(b)): an option's value is the set's identity —
// an own set's name, a shared set's `local:<id>`, a followed scope's minted
// value — and its label the name, with the owner on a set the caller does not
// own. The page builds its ComboBox from `MediaFilterOptions.Build`, so these
// pin what the picker offers without a WinUI window.
public class MediaFilterOptionsTests
{
    private const string AllLabel = "All media";

    private static MediaFolderOption Own(string name, long id) =>
        new(@name: name, @folderId: $"local:{id}", @value: name, @label: name);

    private static MediaFolderOption Shared(string name, long id, string owner) =>
        new(@name: name, @folderId: $"local:{id}", @value: $"local:{id}", @label: $"{name} ({owner})");

    [Fact]
    public void A_shared_sets_option_paints_its_owner_labelled_name_and_keys_on_its_row()
    {
        var entries = MediaFilterOptions.Build(
            new[] { Shared("photos", 7, "alice") }, Array.Empty<FollowedScopeOption>(), AllLabel);

        var shared = Assert.Single(entries, e => e.Value == "local:7");
        Assert.Equal("photos (alice)", shared.Label);
        // The bridge's `options` attr answers each item's UIA Name: the painted
        // label, which is what `folder_filter_labels()` reads on every app.
        Assert.Equal("photos (alice)", shared.AutomationName);
    }

    [Fact]
    public void A_same_named_pair_is_two_options_never_merged()
    {
        var entries = MediaFilterOptions.Build(
            new[] { Own("photos", 3), Shared("photos", 7, "alice") },
            Array.Empty<FollowedScopeOption>(), AllLabel);

        Assert.Equal(
            new[] { MediaFilterOptions.AllValue, "photos", "local:7" },
            entries.Select(e => e.Value));
        Assert.Equal(
            new[] { AllLabel, "photos", "photos (alice)" },
            entries.Select(e => e.Label));
    }

    [Fact]
    public void The_all_media_sentinel_leads_and_reads_back_as_its_value()
    {
        var entries = MediaFilterOptions.Build(
            new[] { Own("photos", 3) }, Array.Empty<FollowedScopeOption>(), AllLabel);

        var all = entries[0];
        Assert.Equal(MediaFilterOptions.AllValue, all.Value);
        Assert.Equal(AllLabel, all.Label);
        // `get_text("media-folder-filter")` reads the selected item's UIA Name,
        // and the cross-app contract reads the all-media default back as the
        // sentinel (`test_select_refuses_unoffered_option.py`).
        Assert.Equal(MediaFilterOptions.AllValue, all.AutomationName);
    }

    [Fact]
    public void Followed_scopes_ride_after_the_set_options_by_their_minted_label()
    {
        var entries = MediaFilterOptions.Build(
            new[] { Own("photos", 3) },
            new[] { new FollowedScopeOption(@value: "followed:abc", @label: "trip (bob)", @available: true) },
            AllLabel);

        var last = entries[^1];
        Assert.Equal("followed:abc", last.Value);
        Assert.Equal("trip (bob)", last.Label);
        Assert.Equal("trip (bob)", last.AutomationName);
    }
}
