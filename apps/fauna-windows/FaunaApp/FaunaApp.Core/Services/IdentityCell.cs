using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The Settings Status sub-page's identity cell (<c>settings.md</c> § Layout &amp; flow
/// item 1): <c>settings-handle-label</c> — the account's own address, <c>handle@domain</c>,
/// undecorated, through the shared display twin <c>fauna_client_accounts::display_handle</c>
/// (<see cref="FaunaFfiMethods.AccountDisplayHandle"/>), the text the profile SELF header
/// shows — and <c>settings-tier-label</c>, the bare tier. Both come from the account
/// registry's server-data cache (<c>long-term-store.md</c> § Cross-app server-data cache),
/// never the quota reply and never the session's own handle, so the cell is on the
/// sub-page's first frame and stays while the nest is unreachable.
///
/// <para>FaunaKit's <c>IdentityCell.read</c>, linux's <c>identity_cell</c>, android's
/// <c>readStatusAccount</c> and tui's <c>session_identity_cell</c> are the twins.</para>
/// </summary>
internal sealed record IdentityCell(string Handle, string? Tier)
{
    /// <summary>The cell for the served account's cache — null (the cell absent, not
    /// empty) while it holds no handle.</summary>
    internal static IdentityCell? Read(ISessionAccount account) =>
        Read(account.Handle, account.Domain, account.Tier);

    /// <summary>The cell over a cached <c>(handle, domain, tier)</c>: null while the cache
    /// holds no handle, or no address resolves (never the bare handle); a blank tier
    /// leaves <see cref="Tier"/> null and its label absent.</summary>
    internal static IdentityCell? Read(string? cachedHandle, string? cachedDomain, string? cachedTier)
    {
        var handle = cachedHandle?.Trim();
        if (string.IsNullOrEmpty(handle)) return null;
        var address = FaunaFfiMethods.AccountDisplayHandle(handle, handle, cachedDomain);
        if (address is null) return null;
        var tier = cachedTier?.Trim();
        return new IdentityCell(address, string.IsNullOrEmpty(tier) ? null : tier);
    }
}
