namespace FaunaApp.Core.Services;

/// <summary>
/// The rule a path must meet before <see cref="DirectNestClient.GetContentAsync"/>
/// fetches it with the session bearer: nest-relative, so it can only resolve against
/// the user's own nest (<c>docs/goal/architecture/render-model.md</c> § D6c).
/// <para>The paths come out of a post's document. The shared fold only ever emits a
/// nest-relative one (<c>fauna-feed</c>'s <c>proxied_media_path</c>: rooted at one
/// <c>/</c>, never <c>//</c>), and this is the same rule held again at the one place
/// where getting it wrong would hand the bearer to another origin — an absolute or
/// scheme-relative URL given to <c>HttpClient</c> is dialed as written, default
/// headers and all.</para>
/// </summary>
internal static class NestContentPath
{
    /// <summary>True iff <paramref name="path"/> is rooted at exactly one <c>/</c>. A
    /// second <c>/</c> names another host, and so does a <c>\</c> to a URL parser that
    /// reads it as one.</summary>
    public static bool IsNestRelative(string? path)
        => path is { Length: > 0 }
            && path[0] == '/'
            && (path.Length == 1 || (path[1] != '/' && path[1] != '\\'));
}
