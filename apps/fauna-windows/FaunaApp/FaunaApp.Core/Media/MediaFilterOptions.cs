using uniffi.fauna_media_machine;

namespace FaunaApp.Core.Media;

/// <summary>
/// One <c>media-folder-filter</c> option as the page paints it.
/// </summary>
/// <param name="Value">What the machine is handed back, verbatim and never parsed —
/// the item's <c>Tag</c> and its UIA AutomationId, which the FlaUI bridge's
/// <c>select</c> matches first, so <c>driver.select</c> names the value on every
/// app.</param>
/// <param name="Label">The painted text — the item's <c>Content</c>.</param>
/// <param name="AutomationName">The item's UIA Name: what the bridge's <c>options</c>
/// attribute lists and what <c>get_text</c> reads back for the selected item.</param>
internal readonly record struct MediaFilterEntry(string Value, string Label, string AutomationName);

/// <summary>
/// The <c>media-folder-filter</c> option list, built from the shared-Rust
/// snapshot's value/label pairs (<c>docs/goal/ui/media.md</c> § Encryption at rest
/// → <i>The filter and the upload target address the row</i>, (a)/(b); § Followed
/// public folders → <i>Filter options are shared-Rust-owned</i>). Both halves of
/// every option are minted in shared Rust — <c>MediaPageSnapshot.folderOptions</c>
/// for the sets (an own set's name, a shared set's <c>local:&lt;id&gt;</c>, labelled
/// <c>&lt;name&gt; (&lt;owner&gt;)</c> when the caller does not own it) and
/// <c>MediaPageSnapshot.followed</c> for the followed scopes, which ride after the
/// sets — so a same-named own / shared pair is two options, never one.
///
/// <para>Each item follows the windows picker shape <c>FeedPage.FactorOption</c> and
/// <c>BackupsPage.FolderOption</c> already use: its UIA Name is the label it paints
/// (what a screen reader reads and the cross-app <c>option_texts</c> contract lists)
/// and its AutomationId the value. The one exception is the all-media sentinel,
/// whose Name stays its value: <c>get_text</c> reads the selected item's Name, and
/// every app reads the default view back as <see cref="AllValue"/>.</para>
///
/// <para>UI-free and in <c>FaunaApp.Core</c> so <c>FaunaApp.Tests</c> can pin it
/// without a WinUI window (the <see cref="UploadPathGuard"/> reasoning).</para>
/// </summary>
internal static class MediaFilterOptions
{
    /// <summary>The all-media default's value — shared Rust's
    /// <c>FILTER_ALL_VALUE</c>, mirrored by the action layer's
    /// <c>MEDIA_FILTER_ALL</c>; the page maps it to <c>SetFilter(null)</c>.</summary>
    public const string AllValue = "__all__";

    /// <summary>The sentinel, then one entry per offered set, then the followed
    /// scopes, in the snapshot's order.</summary>
    public static IReadOnlyList<MediaFilterEntry> Build(
        IReadOnlyList<MediaFolderOption> folderOptions,
        IReadOnlyList<FollowedScopeOption> followed,
        string allLabel)
    {
        var entries = new List<MediaFilterEntry>(1 + folderOptions.Count + followed.Count)
        {
            new(AllValue, allLabel, AllValue),
        };
        entries.AddRange(folderOptions.Select(o => new MediaFilterEntry(o.value, o.label, o.label)));
        entries.AddRange(followed.Select(f => new MediaFilterEntry(f.value, f.label, f.label)));
        return entries;
    }
}
