using System.IO;
using System.Text.RegularExpressions;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The census pin for <see cref="FaunaApp.Core.Services.DeviceOffset"/>
/// (`value-formatting.md` § Absolute local timestamp display — the app-side
/// one-door rule): windows
/// derives the device UTC offset in exactly ONE named function, and every
/// other site calls it — never a second inline
/// <c>TimeZoneInfo.Local.GetUtcOffset(...)</c> / <c>DateTimeOffset.Now.Offset</c>
/// / <c>DateTimeOffset.UtcNow.Offset</c> read. Mirrors the tree-walking guard
/// pattern <c>fauna-tui</c>/<c>fauna-linux</c> use for their own local-date
/// hand-roll rule, and the cross-language dev-fleet local-date-hand-rolls
/// ratchet for a sibling claim.
///
/// <para><b>Ranges over the INVARIANT, not the three sites found at write
/// time</b>: the regex matches any offset-derivation
/// expression anywhere in the scanned tree, not the literal strings the three
/// original sites happened to spell — a FOURTH site spelled differently would
/// still be caught. <see cref="FaunaApp.Core.Services.DeviceOffset"/>'s own
/// file is excluded (it is the door; it legitimately calls
/// <c>TimeZoneInfo</c>), and this test's own directory is never in the scan
/// roots (trap (a) — a needle string living in a file the scan also reads
/// would make the assert trivially satisfiable by the test itself).</para>
/// </summary>
public class DeviceOffsetCensusTests
{
    // Any UTC-offset derivation outside the one door. Two independent .NET
    // APIs are named because that IS the historical defect: the
    // display sites used DateTimeOffset, the nest-facing site used a
    // different DateTimeOffset member — two producers that could silently
    // disagree even though both compile.
    private static readonly Regex OffsetDerivation = new(
        @"TimeZoneInfo\s*\.\s*Local\s*\.\s*GetUtcOffset|DateTimeOffset\s*\.\s*(Now|UtcNow)\s*\.\s*Offset",
        RegexOptions.Compiled);

    // Scan roots: the two hand-written windows projects that can call the
    // door. FaunaApp.Tests (this project) is deliberately never a root.
    private static readonly string[] ScanDirs = { "FaunaApp", "FaunaApp.Core" };

    private static readonly string[] ExcludedPathParts =
    {
        Path.DirectorySeparatorChar + "obj" + Path.DirectorySeparatorChar,
        Path.DirectorySeparatorChar + "bin" + Path.DirectorySeparatorChar,
        Path.DirectorySeparatorChar + "Generated" + Path.DirectorySeparatorChar,
    };

    /// <summary>Every offending (path, line, text) outside the door, across
    /// both scan roots.</summary>
    private static List<(string Path, int Line, string Text)> FindSites()
    {
        var repoRoot = RepoRoot.Find();
        var windowsRoot = Path.Combine(repoRoot, "apps", "fauna-windows", "FaunaApp");
        var deviceOffsetFile = Path.Combine(
            windowsRoot, "FaunaApp.Core", "Services", "DeviceOffset.cs");
        var sites = new List<(string, int, string)>();

        foreach (var scanDir in ScanDirs)
        {
            var root = Path.Combine(windowsRoot, scanDir);
            if (!Directory.Exists(root)) continue;
            foreach (var file in Directory.EnumerateFiles(root, "*.cs", SearchOption.AllDirectories))
            {
                if (ExcludedPathParts.Any(p => file.Contains(p))) continue;
                if (string.Equals(file, deviceOffsetFile, StringComparison.OrdinalIgnoreCase)) continue;

                var lines = File.ReadAllLines(file);
                for (var i = 0; i < lines.Length; i++)
                {
                    if (OffsetDerivation.IsMatch(lines[i]))
                        sites.Add((Path.GetRelativePath(repoRoot, file), i + 1, lines[i].Trim()));
                }
            }
        }
        return sites;
    }

    [Fact]
    public void ExactlyOneDoorExists_NoOtherSiteDerivesTheUtcOffset()
    {
        var sites = FindSites();
        Assert.True(sites.Count == 0,
            "windows must derive the device UTC offset in exactly ONE named "
            + "function (FaunaApp.Core.Services.DeviceOffset — value-formatting.md "
            + "§ Absolute local timestamp display, the app-side one-door rule). "
            + "Found a second derivation:\n"
            + string.Join("\n", sites.Select(s => $"  {s.Path}:{s.Line}: {s.Text}")));
    }

    /// <summary>The door itself really is reachable from the tree this test
    /// scans — a sanity check that <see cref="RepoRoot.Find"/> and
    /// <see cref="ScanDirs"/> resolve to real paths, so a silently-empty scan
    /// (e.g. a moved project directory) cannot masquerade as "0 sites found".</summary>
    [Fact]
    public void ScanRootsAreNonEmpty()
    {
        var repoRoot = RepoRoot.Find();
        var windowsRoot = Path.Combine(repoRoot, "apps", "fauna-windows", "FaunaApp");
        foreach (var scanDir in ScanDirs)
        {
            var root = Path.Combine(windowsRoot, scanDir);
            Assert.True(Directory.Exists(root), $"expected scan root to exist: {root}");
            Assert.True(
                Directory.EnumerateFiles(root, "*.cs", SearchOption.AllDirectories).Any(),
                $"expected at least one .cs file under: {root}");
        }
    }
}
