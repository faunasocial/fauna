namespace FaunaApp.Core.Media;

/// <summary>
/// The two <c>get_attr(post-image, "state")</c> strings the e2e paint-read contract
/// answers with — the same <c>painted</c> / <c>placeholder</c> pair linux's agent
/// answers off its live <c>gtk::Picture</c> and FaunaKit's <c>PostImage</c> answers
/// off the view it is actually showing (<c>tests/e2e-unified/actions/feed.py</c>
/// § <c>_post_image_states_in</c>). Every one of those legs derives the string from
/// the element's OWN current paint, never from a marker written beside the call that
/// assigns it — a literal <c>"painted"</c> next to <c>img.Source = bmp</c> would still
/// answer <c>painted</c> after that one assignment line was deleted.
///
/// <para>
/// UI-free and in <c>FaunaApp.Core</c> on purpose, mirroring
/// <see cref="UploadPathGuard"/>: <c>ImageHashBind</c> — the WinUI attached property
/// that owns the actual <c>Image.Source</c> assignment — lives in the WinUI
/// <c>FaunaApp</c> project, which <c>FaunaApp.Tests</c> cannot reference
/// (<c>FaunaApp.Tests.csproj</c> references <c>FaunaApp.Core</c> alone), so the two
/// literal strings and the boolean-to-string rule are pinned here instead.
/// </para>
/// </summary>
public static class ImagePaintState
{
    public const string Painted = "painted";
    public const string Placeholder = "placeholder";

    /// <summary><see cref="Painted"/> once the element actually holds a picture,
    /// <see cref="Placeholder"/> otherwise.</summary>
    public static string From(bool hasPicture) => hasPicture ? Painted : Placeholder;
}
