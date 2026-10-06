using System.Net.Http;
using System.Net.Http.Json;
using System.Reflection;
using System.Text.Json;

namespace FaunaApp.Core.Services;

/// <summary>
/// Polls GitHub Releases API to check whether a newer version is available.
/// </summary>
public sealed class UpdateService
{
    private static readonly string CurrentVersion =
        Assembly.GetExecutingAssembly()
            .GetCustomAttribute<AssemblyInformationalVersionAttribute>()
            ?.InformationalVersion ?? "0.0.0";

    // Release source. Canonical single source of truth is the shared Rust
    // constant `fauna_core::version::RELEASE_REPO` ("faunasocial/fauna"); kept
    // in sync by hand here because UniFFI exposes functions, not consts, and a
    // runtime FFI call for a build-time slug isn't worth the binding churn. The
    // hyphenated `fauna-social/fauna` is a recurring typo (it points at a repo
    // that does not exist) — do not reintroduce it.
    private const string GitHubApi =
        "https://api.github.com/repos/faunasocial/fauna/releases/latest";

    private readonly HttpClient _http;

    public UpdateService(HttpClient http)
    {
        _http = http;
        _http.DefaultRequestHeaders.UserAgent.ParseAdd($"fauna-windows/{CurrentVersion}");
    }

    /// <summary>
    /// Returns the tag name of the latest release if it is newer than the
    /// running version, or <c>null</c> if already up to date (or on error).
    /// </summary>
    public async Task<string?> CheckForUpdateAsync()
    {
        try
        {
            using var response = await _http.GetAsync(GitHubApi);
            response.EnsureSuccessStatusCode();
            using var doc = await JsonDocument.ParseAsync(
                await response.Content.ReadAsStreamAsync());
            var tag = doc.RootElement.GetProperty("tag_name").GetString();
            if (tag is null) return null;
            var remote = tag.TrimStart('v');
            // Shared, spec-correct semver compare (`fauna_core::version::is_newer`
            // via UniFFI) — one implementation across every app (priority
            // #2/#1). Replaces the hand-rolled int-tuple parse that mishandled
            // pre-release tags / build metadata. `is_newer(current, candidate)`
            // returns true when `candidate` (remote) is strictly newer.
            return uniffi.fauna_ffi.FaunaFfiMethods.IsNewer(CurrentVersion, remote) ? tag : null;
        }
        catch
        {
            return null;
        }
    }
}
