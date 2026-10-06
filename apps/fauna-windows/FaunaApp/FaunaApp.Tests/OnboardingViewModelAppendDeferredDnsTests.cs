using System.Collections.Generic;
using System.Linq;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using uniffi.fauna_onboarding_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The append-mode ("Add account") deferred-DNS exit: the
/// appended identity lives only in the wizard machine until its own terminal
/// adopts it (<c>long-term-store.md</c> § Downgrade mirror + abandoned-append
/// recovery), and for this exit that terminal is <c>persist_awaiting_dns</c>
/// followed by the shell's switch (<c>onboarding.md</c> § Multi-account,
/// *Append-mode deferred/incomplete states*). A skipped write leaves the secret
/// — client-only key material — and the parked box's claim code in process
/// memory for an "Almost ready" wait measured in hours.
///
/// <para>Drives the VM's own <c>HandleWizardOutcome</c> over the REAL shared
/// <see cref="FfiAccountRegistry"/> (only the Credential Manager backend is
/// faked), so the pin covers the VM wiring the registry-only
/// <see cref="OnboardingWizardExitPersistenceTests"/> leave out.</para>
/// </summary>
public class OnboardingViewModelAppendDeferredDnsTests
{
    private sealed class FakeOnboardingObserver : OnboardingObserver
    {
        public void OnChanged() { }
    }

    private sealed class MemoryBackend : ISecretBackend
    {
        private readonly Dictionary<string, string> _rows = new();
        public IReadOnlyDictionary<string, string> Rows => _rows;
        public string? Get(string resource, string user) =>
            _rows.TryGetValue(resource, out var v) ? v : null;
        public void Set(string resource, string user, string value) => _rows[resource] = value;
        public void Delete(string resource, string user) => _rows.Remove(resource);
    }

    private const string LiveSecret =
        "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f";
    private const string AppendedSecret =
        "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";

    // The seeder's snake_case serde shape (see OnboardingViewModelAwaitingManualDnsTests).
    private const string OneRecordJson =
        "[{\"record_type\":\"A\",\"name\":\"@\",\"value\":\"203.0.113.7\",\"ttl\":300,\"priority\":null}]";

    private static Dictionary<string, string> RowsOf(MemoryBackend backend, string actor) =>
        backend.Rows.Where(kv => kv.Key.StartsWith($"fauna/{actor}/"))
            .ToDictionary(kv => kv.Key, kv => kv.Value);

    [Fact]
    public void AppendModeDeferredDnsExitRegistersActivatesAndHandsTheActorToTheShell()
    {
        var backend = new MemoryBackend();
        using var registry = new FfiAccountRegistry(new LogicalSecretStore(backend));
        var live = registry.AddAccount(LiveSecret, "https://live.example", null);
        var liveRowsBefore = RowsOf(backend, live);
        Assert.NotEmpty(liveRowsBefore);

        var vm = new OnboardingViewModel(new FakeOnboardingObserver(), registry, isAppendMode: true);
        string? handedToShell = null;
        vm.OnAwaitingDnsPersisted = actor => handedToShell = actor;

        // The deferred-DNS exit: identity on the machine, no handle stage.
        vm.SeedIdentity(AppendedSecret);
        vm.SeedAwaitingManualDns("https://nest.example.com", "", OneRecordJson, "DNS-CODE");
        vm.HandleWizardOutcome();

        // Registered and active, with its secret readable back from the store.
        Assert.NotNull(handedToShell);
        Assert.NotEqual(live, handedToShell);
        Assert.Equal(handedToShell, registry.Active());
        Assert.Equal(AppendedSecret, registry.SessionMaterial(handedToShell!)?.secretHex);
        Assert.Equal(2, registry.List().Length);

        // Its awaiting-DNS slot is what a relaunch reads — claim code included.
        var slot = registry.LaunchPersistence().LoadAwaitingDns();
        Assert.NotNull(slot);
        Assert.Equal("DNS-CODE", slot!.claimCode);
        Assert.Equal("https://nest.example.com", slot.nestUrl);
        Assert.Contains("203.0.113.7", slot.dnsRecordsJson);

        // The previously active account's own rows are untouched.
        Assert.Equal(liveRowsBefore, RowsOf(backend, live));
    }
}
