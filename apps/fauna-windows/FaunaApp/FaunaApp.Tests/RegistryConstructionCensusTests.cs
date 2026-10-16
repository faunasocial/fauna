using System.IO;
using System.Text.RegularExpressions;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The census pin for windows' one account-registry builder,
/// <c>FaunaApp.Services.CredentialStore.Registry()</c>
/// (<c>long-term-store.md</c> § Multi-account evolution → <i>Cross-process
/// mutation lock</i>, which asks every adopting app to sweep its constructions
/// through one choke point and pin the result against regrowth). That builder
/// is the only one that hands the registry the install-scoped mutation lock: a
/// registry built anywhere else rewrites <c>fauna/index</c> unlocked, and
/// nothing observable fails — the write lands while skipping the lock a sibling
/// instance's writer takes. The windows twin of linux's
/// <c>account_registry_census_test</c>.
///
/// <para><b>Ranges over the invariant, not the sites found at write time</b>: the
/// pattern matches every constructor the bindings expose (<c>new
/// FfiAccountRegistry(…)</c> and any <c>FfiAccountRegistry.New…(…)</c> secondary
/// constructor, qualified or not), so a second builder spelled differently is
/// still caught. The builder's own file is excluded, and this test's project is
/// never a scan root (the needle would otherwise live in a file the scan reads).
/// Unit tests may build registries over in-memory stores freely — they are not
/// app writers.</para>
/// </summary>
public class RegistryConstructionCensusTests
{
    private static readonly Regex RegistryConstruction = new(
        @"new\s+(uniffi\.fauna_ffi\.)?FfiAccountRegistry\s*\(|FfiAccountRegistry\s*\.\s*New\w*\s*\(",
        RegexOptions.Compiled);

    private static readonly string[] ScanDirs = { "FaunaApp", "FaunaApp.Core" };

    private static readonly string[] ExcludedPathParts =
    {
        Path.DirectorySeparatorChar + "obj" + Path.DirectorySeparatorChar,
        Path.DirectorySeparatorChar + "bin" + Path.DirectorySeparatorChar,
        Path.DirectorySeparatorChar + "Generated" + Path.DirectorySeparatorChar,
    };

    private static string WindowsRoot() =>
        Path.Combine(RepoRoot.Find(), "apps", "fauna-windows", "FaunaApp");

    private static string BuilderFile() =>
        Path.Combine(WindowsRoot(), "FaunaApp", "Services", "CredentialStore.cs");

    private static List<(string Path, int Line, string Text)> FindSites(bool includeBuilder)
    {
        var repoRoot = RepoRoot.Find();
        var sites = new List<(string, int, string)>();
        foreach (var scanDir in ScanDirs)
        {
            var root = Path.Combine(WindowsRoot(), scanDir);
            if (!Directory.Exists(root)) continue;
            foreach (var file in Directory.EnumerateFiles(root, "*.cs", SearchOption.AllDirectories))
            {
                if (ExcludedPathParts.Any(p => file.Contains(p))) continue;
                if (!includeBuilder
                    && string.Equals(file, BuilderFile(), StringComparison.OrdinalIgnoreCase)) continue;

                var lines = File.ReadAllLines(file);
                for (var i = 0; i < lines.Length; i++)
                {
                    if (RegistryConstruction.IsMatch(lines[i]))
                        sites.Add((Path.GetRelativePath(repoRoot, file), i + 1, lines[i].Trim()));
                }
            }
        }
        return sites;
    }

    [Fact]
    public void NoRegistryIsBuiltOutsideTheOneLockedBuilder()
    {
        var sites = FindSites(includeBuilder: false);
        Assert.True(sites.Count == 0,
            "windows builds its account registry in exactly ONE place, "
            + "CredentialStore.Registry(), the only builder that hands it the "
            + "install-scoped mutation lock (long-term-store.md § Multi-account "
            + "evolution → Cross-process mutation lock). Call that instead of:\n"
            + string.Join("\n", sites.Select(s => $"  {s.Path}:{s.Line}: {s.Text}")));
    }

    /// <summary>The builder itself is found by the same pattern and builds the
    /// LOCKED registry — so neither a moved file nor an unlocked builder can
    /// pass as "no other site found".</summary>
    [Fact]
    public void TheOneBuilderIsFoundAndTakesTheLock()
    {
        var builder = FindSites(includeBuilder: true)
            .Where(s => s.Path.EndsWith(Path.Combine("Services", "CredentialStore.cs"), StringComparison.OrdinalIgnoreCase))
            .ToList();
        Assert.True(builder.Count == 1,
            $"expected exactly one registry construction in CredentialStore.cs, found {builder.Count}");
        Assert.Contains("NewWithLockDir", builder[0].Text);
    }
}
