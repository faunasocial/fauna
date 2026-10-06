namespace FaunaApp.Core.Media;

/// <summary>
/// The path a Media <c>upload-button</c> press should act on, or nothing when the
/// box is empty — in which case the caller must SAY so on the page
/// <c>error-message</c> rather than return silently.
///
/// <para>
/// The windows peer of linux's <c>upload_path_of</c>
/// (<c>apps/fauna-linux/src/views/media/mod.rs</c>) and of the same guard in tui's
/// upload arm. Pressing Upload with an empty box used to <c>return</c> with no
/// visible effect on all three, which presents as a <b>dead button</b>: a live
/// user pressed tui's Upload, saw nothing happen, and reported the upload feature
/// as missing entirely. tui fixed it 2026-08-02, linux 2026-08-06; windows was the
/// last app still carrying the silent arm.
/// </para>
///
/// <para>
/// <c>docs/goal/ui/media.md</c> § Client glue: "<c>upload-button</c> with an empty
/// path box | Surface <c>media.file_required</c> ("Choose a file first…") on
/// <c>error-message</c> — never a silent no-op, which presents as a dead button."
/// That row rules the guard <b>app glue</b> — the shared
/// <c>MediaMachine::upload_selected</c> gesture takes bytes, never a path, so it
/// cannot host this check.
/// </para>
///
/// <para>
/// UI-free and in <c>FaunaApp.Core</c> on purpose: the precondition is the whole
/// bug, and <c>FaunaApp.Tests</c> references <c>FaunaApp.Core</c> alone — a helper
/// left inside <c>MediaPage.xaml.cs</c> could not be pinned without a WinUI window.
/// Same reasoning as <see cref="FaunaApp.Core.Calendar.DayCellClickArbiter"/>,
/// extracted from <c>EventsPage.xaml.cs</c>.
/// </para>
/// </summary>
public static class UploadPathGuard
{
    /// <summary>
    /// The i18n key for the prompt shown when the box is empty.
    ///
    /// <para>
    /// The <b>picker-flavoured</b> wording ("Choose a file first…"), deliberately
    /// NOT tui's typed-path <c>media/file_path_required</c> ("Type the path…"):
    /// windows has a real native picker, so picker language is the true one here.
    /// The split mirrors the existing <c>choose_file</c> / <c>type_file_path</c>
    /// pair — see the note above both keys in <c>i18n/strings/en.yaml</c>.
    /// </para>
    /// </summary>
    public const string FileRequiredKey = "media/file_required";

    /// <summary>
    /// Returns the trimmed path to upload, or <c>null</c> when the box holds
    /// nothing usable — the caller then surfaces <see cref="FileRequiredKey"/> on
    /// <c>error-message</c> instead of uploading.
    /// </summary>
    /// <remarks>
    /// Whitespace counts as empty: a stray space is what a box looks like to a
    /// user who believes they typed a path. Trimming here also means the upload
    /// glue never sees the padding, matching linux, where the same trim feeds
    /// <c>std::fs::read</c>.
    /// </remarks>
    public static string? UploadPathOf(string? typed)
    {
        var picked = typed?.Trim();
        return string.IsNullOrEmpty(picked) ? null : picked;
    }
}
