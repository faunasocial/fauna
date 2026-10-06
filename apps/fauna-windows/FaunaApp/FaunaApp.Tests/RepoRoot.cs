namespace FaunaApp.Tests;

/// <summary>
/// The repository root, for a test that reads a checked-in file (a shared e2e fixture) or
/// scans the source tree. One door so the walk-up rule is not re-derived per test file.
/// </summary>
internal static class RepoRoot
{
    /// <summary>Walks up from the test binary's directory to the first ancestor holding a
    /// <c>.git</c>.</summary>
    internal static string Find()
    {
        // `.git` (ordinarily a directory, sometimes a redirect FILE instead) is
        // the one marker every clone of this repo carries, public or private —
        // unlike a repo-internal doc, it names no private content.
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null && !Path.Exists(Path.Combine(dir.FullName, ".git")))
            dir = dir.Parent;
        if (dir is null)
            throw new InvalidOperationException(
                "RepoRoot: could not find the repo root (no .git "
                + $"found walking up from {AppContext.BaseDirectory})");
        return dir.FullName;
    }
}
