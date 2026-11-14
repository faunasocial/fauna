using System;
using System.Linq;
using FaunaApp.Core.Logs;
using uniffi.fauna_ffi;
using uniffi.fauna_launch_machine;

namespace FaunaApp.Core.Services;

/// <summary>
/// Writes an identity the session's own bearer mint confirmed into the account
/// registry's server-data cache — the session's identity follow, the cache's writer
/// beside the launch's silent challenge (<c>long-term-store.md</c> § Cross-app
/// server-data cache: "the verify reply — the launch's silent challenge and the
/// session's identity follow"). <see cref="NestRpcClient.StartSelfIdentityFollow"/>
/// feeds it from <c>FfiNestClient.SubscribeSelfIdentity</c>.
///
/// <para>A returned empty field keeps whatever the cache already holds (a launch, or an
/// earlier refresh, wrote it) — tui's <c>apply_self_identity</c> and FaunaKit's
/// <c>applySelfIdentity</c> make the same merge.</para>
/// </summary>
internal static class SelfIdentityCache
{
    /// <summary>Merge <paramref name="identity"/> into <paramref name="actorId"/>'s cached
    /// handle/domain/tier. False — nothing written, logged — when the account is no longer
    /// in the index (a follow that outlived a sign-out never resurrects a row).</summary>
    internal static bool Apply(FfiAccountRegistry registry, string actorId, LaunchIdentity identity)
    {
        var cur = registry.List().FirstOrDefault(e =>
            string.Equals(e.@actorId, actorId, StringComparison.OrdinalIgnoreCase));
        if (cur is null) return false;
        try
        {
            registry.UpdateCache(
                cur.@actorId,
                Pick(identity.@handle, cur.@handle),
                Pick(identity.@domain, cur.@domain),
                Pick(identity.@tier, cur.@tier));
            return true;
        }
        catch (Exception ex)
        {
            ShellLog.Warn("SelfIdentityCache", $"[self-identity] cache update failed: {ex.Message}");
            return false;
        }
    }

    private static string? Pick(string? confirmed, string? cached) =>
        string.IsNullOrEmpty(confirmed) ? cached : confirmed;
}
