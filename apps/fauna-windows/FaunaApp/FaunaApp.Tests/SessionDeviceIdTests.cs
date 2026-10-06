using System.Collections.Generic;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// <see cref="SessionDeviceId.Resolve"/> — the device id the SESSION account
/// registers under, read by every launch path through
/// <c>RegistrySessionAccount.DeviceId</c>. It must apply the shared
/// get-or-create rule (<c>FfiAccountRegistry.DeviceIdForActor</c>,
/// <c>sync-agent-credentials.md</c> § Credential model, the RULED 2026-09-20
/// block), never serve the raw per-actor slot: a slot that is not a 32-byte-hex
/// sync id is no id at all, and serving it verbatim left the account-store
/// runtime with no enrollment target — its assembly failed with "this device
/// has no own device id to enroll on", so a stored-identity launch never
/// mounted the account store. Real registry over the real key map, only the raw
/// backend faked (the <see cref="AccountRegistryStoreTests"/> convention).
/// </summary>
public class SessionDeviceIdTests
{
    private sealed class MemoryBackend : ISecretBackend
    {
        private readonly Dictionary<string, string> _rows = new();

        public string? Get(string resource, string user) =>
            _rows.TryGetValue(resource, out var v) ? v : null;

        public void Set(string resource, string user, string value) => _rows[resource] = value;

        public void Delete(string resource, string user) => _rows.Remove(resource);
    }

    private const string SecretA = "1111111111111111111111111111111111111111111111111111111111111111";
    private const string SyncId = "abababababababababababababababababababababababababababababababab";

    private static bool IsSyncShaped(string? id) =>
        id is { Length: 64 } && System.Text.RegularExpressions.Regex.IsMatch(id, "^[0-9a-f]{64}$");

    [Fact]
    public void ASlotThatIsNotASyncIdResolvesToADerivedSyncIdAndPersistsIt()
    {
        var store = new LogicalSecretStore(new MemoryBackend());
        using var registry = new FfiAccountRegistry(store);
        var actor = registry.AddAccount(SecretA, "https://a.example", "smoke-e2e-device");

        var id = SessionDeviceId.Resolve(registry, store, actor);

        Assert.True(IsSyncShaped(id), $"expected a 32-byte-hex sync id, got '{id}'");
        Assert.Equal(id, registry.SessionMaterial(actor)?.@deviceId);
    }

    [Fact]
    public void AStoredSyncIdIsServedVerbatim()
    {
        var store = new LogicalSecretStore(new MemoryBackend());
        using var registry = new FfiAccountRegistry(store);
        var actor = registry.AddAccount(SecretA, null, SyncId);

        Assert.Equal(SyncId, SessionDeviceId.Resolve(registry, store, actor));
    }

    [Fact]
    public void AnActorTheRegistryDoesNotHoldHasNoDeviceId()
    {
        var store = new LogicalSecretStore(new MemoryBackend());
        using var registry = new FfiAccountRegistry(store);

        Assert.Null(SessionDeviceId.Resolve(registry, store, new string('c', 64)));
    }
}
