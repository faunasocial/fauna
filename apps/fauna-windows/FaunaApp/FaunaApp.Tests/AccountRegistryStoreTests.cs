using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using uniffi.fauna_launch_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Windows' leg of the shared multi-account registry
/// (<c>docs/goal/architecture/long-term-store.md</c> § Multi-account evolution) —
/// the only long-term store windows reads or writes.
///
/// <para>These run against the <b>real</b> Rust seam — the real
/// <c>FfiAccountRegistry</c> over a real <see cref="LogicalSecretStore"/>, and the
/// real <c>LaunchMachine</c> — with only the raw key/value backend faked. That is
/// the point: the platform's entire contribution is the key→key map plus the
/// backend, so a test that stubs the registry would prove nothing. The Rust twins
/// live in <c>fauna-client-accounts::launch_persistence::tests</c>.</para>
/// </summary>
public class AccountRegistryStoreTests
{
    /// <summary>An in-memory stand-in for Credential Manager, keyed by native resource.</summary>
    private sealed class MemoryBackend : ISecretBackend
    {
        private readonly Dictionary<string, string> _rows = new();

        public IReadOnlyDictionary<string, string> Rows => _rows;

        public string? Get(string resource, string user) =>
            _rows.TryGetValue(resource, out var v) ? v : null;

        public void Set(string resource, string user, string value) => _rows[resource] = value;

        public void Delete(string resource, string user) => _rows.Remove(resource);

        /// <summary>Write a raw native row (the old single-identity shape).</summary>
        public void SeedNative(string resource, string value) => _rows[resource] = value;
    }

    private const string SecretA = "1111111111111111111111111111111111111111111111111111111111111111";
    private const string SecretB = "2222222222222222222222222222222222222222222222222222222222222222";

    private static (LogicalSecretStore Store, MemoryBackend Backend) NewStore()
    {
        var backend = new MemoryBackend();
        return (new LogicalSecretStore(backend), backend);
    }

    // ── The key map: the platform's whole contribution ──

    /// <summary>
    /// Every logical key is stored verbatim — the same rule as linux's
    /// <c>account_for</c>. A test seeding <c>fauna/index</c> depends on this, and
    /// so does the retirement of the pre-registry single slot: a <c>legacy/*</c>
    /// key is no longer remapped onto the old <c>FaunaIdentity</c>-style rows
    /// (<c>long-term-store.md</c> § Downgrade mirror + abandoned-append recovery).
    /// </summary>
    [Fact]
    public void EveryLogicalKeyIsStoredVerbatim()
    {
        Assert.Equal(("fauna/index", "fauna"), SecretKeyMap.Resolve("fauna/index"));
        Assert.Equal(($"fauna/{SecretA}/secret", "fauna"), SecretKeyMap.Resolve($"fauna/{SecretA}/secret"));
        Assert.Equal(("legacy/secret", "fauna"), SecretKeyMap.Resolve("legacy/secret"));
    }

    /// <summary>
    /// The pre-registry Credential Manager rows are retired: a vault holding only
    /// the old single-identity <c>FaunaIdentity</c> / <c>FaunaNestUrl</c> rows
    /// reads as identity-less, so launch routes to fresh onboarding. (The
    /// 2026-09-24 baseline reset: no installation predates the registry, so there
    /// is no upgrade door left to keep open — <c>version-compatibility.md</c>
    /// § Dimension 2.)
    /// </summary>
    [Fact]
    public async Task ThePreRegistryVaultRowsAreNoLongerRead()
    {
        var (store, backend) = NewStore();
        backend.SeedNative("FaunaIdentity", SecretA);
        backend.SeedNative("FaunaNestUrl", "https://legacy.example");

        using var registry = new FfiAccountRegistry(store);
        Assert.Empty(registry.List());
        var machine = new LaunchMachine(new NullLaunchObserver(), registry.LaunchPersistence());
        await machine.Start();

        var wizardAt = Assert.IsType<LaunchPhase.WizardAt>(machine.Snapshot().phase);
        Assert.Equal(LaunchWizardEntry.IdentityChoice, wizardAt.entry);
    }

    // ── The onboarding moments windows' wizard now runs (onboarding.md § Long-term
    // store contract) — the shared Rust through the real FFI seam ──

    /// <summary>
    /// Moment 1, first-run mode: the confirmed identity becomes a registered,
    /// ACTIVE account whose secret reads back through the registry.
    /// </summary>
    [Fact]
    public void ConfirmIdentityRegistersAndActivatesOnAFirstRun()
    {
        var (store, _) = NewStore();
        using var registry = new FfiAccountRegistry(store);

        var actor = registry.ConfirmIdentity(SecretA, false);

        Assert.Equal(actor, registry.Active());
        Assert.Equal(SecretA, registry.SessionMaterial(actor)?.secretHex);
    }

    /// <summary>
    /// Moment 1, append mode ("Add account" over a live session): the shared
    /// moment writes NOTHING — the appended identity stays in the wizard machine
    /// until its own terminal registers it, so an abandoned append can neither
    /// leave a half-account nor move the live session's active pointer.
    /// </summary>
    [Fact]
    public void ConfirmIdentityInAppendModeWritesNothing()
    {
        var (store, backend) = NewStore();
        using var registry = new FfiAccountRegistry(store);
        var live = registry.AddAccount(SecretA, "https://a.example", null);
        var before = new Dictionary<string, string>(backend.Rows);

        registry.ConfirmIdentity(SecretB, true);

        Assert.Equal(before, backend.Rows);
        Assert.Equal(live, registry.Active());
        Assert.Single(registry.List());
    }

    /// <summary>
    /// Moment 4, the <c>LoggedIn</c> terminal: the home nest is recorded
    /// PER-ACTOR — the only place it can be recorded — so the next launch's
    /// silent-challenge row finds it; the device id and the pending-invite spend
    /// ride the same call.
    /// </summary>
    [Fact]
    public void PersistLoggedInRecordsTheHomeNestPerActor()
    {
        var (store, _) = NewStore();
        using var registry = new FfiAccountRegistry(store);
        registry.ConfirmIdentity(SecretA, false);
        registry.PersistPendingInvite(SecretA, "https://a.example", "alice", "req-1", "{}");

        var actor = registry.PersistLoggedIn(SecretA, "https://a.example", "ab".PadRight(64, '0'), null);

        var persistence = registry.LaunchPersistence();
        Assert.Equal("https://a.example", persistence.LoadNestUrl());
        Assert.Null(persistence.LoadPendingInvite());
        Assert.Equal("ab".PadRight(64, '0'), registry.SessionMaterial(actor)?.deviceId);
    }

    // ── Cleanup contract ──

    /// <summary>
    /// Sign-out must erase the WHOLE namespace: on a two-account install a
    /// narrower wipe would leave account #2's secret sitting in Credential Manager
    /// referenced by nothing — an identity the user believes they signed out of
    /// (<c>long-term-store.md</c> § Cleanup contract). This pins the behavior
    /// <c>App.ClearCredentialNamespace</c> routes to.
    /// </summary>
    [Fact]
    public void ClearAllErasesEveryAccountNotJustTheActiveOne()
    {
        var (store, backend) = NewStore();
        using var registry = new FfiAccountRegistry(store);
        registry.AddAccount(SecretA, "https://a.example", null);
        registry.AddAccount(SecretB, "https://b.example", null);
        Assert.Equal(2, registry.List().Length);

        var credentials = registry.ClearAll();

        // The read-back found nothing — the clean arm the sign-out's residue line
        // stays silent on (account-scoping.md § Erasure follows scope).
        Assert.Empty(credentials.survivors);
        Assert.Empty(registry.List());
        Assert.False(backend.Rows.Values.Any(v => v == SecretA),
            "the active account's secret must be gone");
        Assert.False(backend.Rows.Values.Any(v => v == SecretB),
            "the NON-active account's secret must be gone too — a narrower wipe " +
            "would strand it in Credential Manager, referenced by nothing");
    }

    // ── Device id derivation (sync-agent-credentials.md § Credential model, the
    // RULED 2026-09-20 block) — windows' leg of the shared secret-store face,
    // `FfiAccountRegistry.DeviceIdForActor`. Mirrors apple's
    // `FaunaAccountsDeviceIdTests.swift` (the same shared Rust, a different FFI
    // seam): the real registry over the real key map, only the raw backend
    // faked — the same reason the rest of this file runs against the real seam.

    /// <summary>
    /// <b>The row's own definition of success.</b> A sign-out (<c>ClearAll()</c> —
    /// what <c>App.ClearCredentialNamespace</c> calls) erases the account's
    /// per-actor slot, but never <c>install/device_secret</c>
    /// (<c>DeviceIdForActor</c>'s own doc: "no erase in this crate names it") —
    /// windows has exactly one store, so <c>installStore</c> and the registry's
    /// own store are the same <see cref="LogicalSecretStore"/>. The next sign-in
    /// re-derives the SAME id for the same actor, so it comes back to the same
    /// named <c>sync_devices</c> row instead of accruing a new one every cycle
    /// (the 2026-09-20 ruling this row builds).
    /// </summary>
    [Fact]
    public void DeviceIdForActorSurvivesASignOutClearAndReDerivesTheSameId()
    {
        var (store, _) = NewStore();
        using var registry = new FfiAccountRegistry(store);
        var actorA = registry.AddAccount(SecretA, "https://a.example", null);

        var first = registry.DeviceIdForActor(store, actorA);
        Assert.False(string.IsNullOrEmpty(first));

        registry.ClearAll();

        var second = registry.DeviceIdForActor(store, actorA);
        Assert.Equal(first, second);
    }

    /// <summary>
    /// Decision 5 (§ Credential model): two accounts on one install never share a
    /// device id — the derivation is salted by the actor, not install-flat.
    /// </summary>
    [Fact]
    public void DeviceIdForActorNeverSharesAnIdBetweenTwoAccounts()
    {
        var (store, _) = NewStore();
        using var registry = new FfiAccountRegistry(store);
        var actorA = registry.AddAccount(SecretA, null, null);
        var actorB = registry.AddAccount(SecretB, null, null);

        var idA = registry.DeviceIdForActor(store, actorA);
        var idB = registry.DeviceIdForActor(store, actorB);

        Assert.NotEqual(idA, idB);
    }
}
