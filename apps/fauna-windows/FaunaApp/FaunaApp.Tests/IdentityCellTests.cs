using System.Collections.Generic;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using uniffi.fauna_launch_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The account registry's server-data cache on windows' two paint surfaces
/// (<c>long-term-store.md</c> § Cross-app server-data cache → <i>Where the cache
/// paints</i>): the Settings Status sub-page's identity cell
/// (<see cref="IdentityCell"/>; <c>settings.md</c> § Layout &amp; flow item 1) and the
/// launch surfaces' <c>launch-identity-label</c> (<c>FfiAccountRegistry.LaunchIdentityLine</c>;
/// <c>onboarding.md</c> § App-launch routing → <i>The cached identity on the launch
/// surfaces</i>) — and the session's identity follow that keeps the cache current
/// (<see cref="SelfIdentityCache"/>). Against the real Rust seam: the real registry over a
/// <see cref="LogicalSecretStore"/> with only the key/value backend faked, so the composed
/// text is the shared <c>display_handle</c> / <c>identity_line</c>, never a C# copy.
/// FaunaKit's <c>IdentityCell.read</c>, linux's <c>identity_cell</c>, tui's
/// <c>session_identity_cell</c> and its <c>apply_self_identity</c> are the twins.
/// </summary>
public class IdentityCellTests
{
    private sealed class MemoryBackend : ISecretBackend
    {
        private readonly Dictionary<string, string> _rows = new();
        public string? Get(string resource) => _rows.TryGetValue(resource, out var v) ? v : null;
        public void Set(string resource, string value) => _rows[resource] = value;
        public void Delete(string resource) => _rows.Remove(resource);
    }

    private static readonly string SecretA = new('1', 64);

    private static FfiAccountRegistry NewRegistry() =>
        new(new LogicalSecretStore(new MemoryBackend()));

    // ── The identity cell (settings-handle-label + settings-tier-label) ──

    [Fact]
    public void TheCellIsTheCachedAddressAndTheBareTier()
    {
        var cell = IdentityCell.Read("alice", "nest.test", " pro ");

        Assert.NotNull(cell);
        Assert.Equal("alice@nest.test", cell!.Handle);
        Assert.Equal("pro", cell.Tier);
    }

    [Fact]
    public void TheCellIsAbsentWhileTheCacheHoldsNoHandle()
    {
        // A first launch or an import: nothing cached, nothing painted — absent, not empty.
        Assert.Null(IdentityCell.Read(null, "nest.test", "pro"));
        Assert.Null(IdentityCell.Read("  ", "nest.test", "pro"));
    }

    [Fact]
    public void TheCellIsAbsentWithoutACachedDomain()
    {
        // Never the bare handle: the address is handle@domain or nothing (display_handle).
        Assert.Null(IdentityCell.Read("alice", null, "pro"));
    }

    [Fact]
    public void ABlankTierLeavesTheTierLabelAbsent()
    {
        var cell = IdentityCell.Read("alice", "nest.test", "");

        Assert.Equal("alice@nest.test", cell!.Handle);
        Assert.Null(cell.Tier);
    }

    // ── The launch surfaces' line over the same cache ──

    [Fact]
    public void TheLaunchLineIsTheActiveAccountsCachedIdentity()
    {
        using var registry = NewRegistry();
        var actor = registry.AddAccount(SecretA, "https://nest.test", null);
        registry.SetActive(actor);
        Assert.Null(registry.LaunchIdentityLine());   // nothing cached yet: the label is absent

        registry.UpdateCache(actor, "alice", "nest.test", "pro");

        Assert.Equal("alice@nest.test · pro", registry.LaunchIdentityLine());
        // The cell and the line show one address (a primary rename moves both at once).
        var entry = Assert.Single(registry.List());
        Assert.Equal("alice@nest.test", IdentityCell.Read(entry.@handle, entry.@domain, entry.@tier)!.Handle);
    }

    // ── The session's identity follow: the cache's second writer path ──

    [Fact]
    public void TheFollowWritesAllThreeFieldsOfTheConfirmedIdentity()
    {
        using var registry = NewRegistry();
        var actor = registry.AddAccount(SecretA, "https://nest.test", null);
        registry.UpdateCache(actor, "alice", null, null);   // the e2e door's handle-only write

        Assert.True(SelfIdentityCache.Apply(registry, actor, new LaunchIdentity("alice", "nest.test", "pro")));

        var entry = Assert.Single(registry.List());
        Assert.Equal(("alice", "nest.test", "pro"), (entry.@handle, entry.@domain, entry.@tier));
    }

    [Fact]
    public void AnEmptyFieldOfTheConfirmedIdentityKeepsTheCachedOne()
    {
        using var registry = NewRegistry();
        var actor = registry.AddAccount(SecretA, "https://nest.test", null);
        registry.UpdateCache(actor, "alice", "nest.test", "pro");

        Assert.True(SelfIdentityCache.Apply(registry, actor, new LaunchIdentity("", "", "witness-plus")));

        var entry = Assert.Single(registry.List());
        Assert.Equal(("alice", "nest.test", "witness-plus"), (entry.@handle, entry.@domain, entry.@tier));
    }

    [Fact]
    public void AnAccountNoLongerInTheIndexIsNeverResurrected()
    {
        // A follow outliving a sign-out: the row is gone, so nothing is written.
        using var registry = NewRegistry();

        Assert.False(SelfIdentityCache.Apply(
            registry, "00".PadRight(64, '0'), new LaunchIdentity("alice", "nest.test", "pro")));
        Assert.Empty(registry.List());
    }
}
