using System.Collections.Generic;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using uniffi.fauna_launch_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The wizard-exit resume slots (<c>docs/goal/behavior/onboarding.md</c> § Long-term
/// store contract), exercised end-to-end through the REAL shared
/// <c>FfiAccountRegistry</c> + <c>LaunchMachine</c> across the
/// <see cref="LogicalSecretStore"/> seam — the windows twin of apple's
/// <c>KeychainSecretStoreTests.swift</c> ("Wizard-exit slot writers") and
/// android's <c>OnboardingWizardExitPersistenceTest.kt</c>.
///
/// <para><c>OnboardingViewModel</c> writes both slots per-actor through
/// <see cref="IFfiAccountRegistry.PersistAwaitingDns"/> /
/// <see cref="IFfiAccountRegistry.PersistPendingInvite"/>, and spends them at the
/// <c>LoggedIn</c> terminal through <see cref="IFfiAccountRegistry.PersistLoggedIn"/>
/// — the ruled clearing moment (ratified 2026-09-21). The per-actor slot composes a
/// handle-less deferred-DNS exit (the <c>test_smoke_i</c> shape) and is visible on a
/// multi-account install, which is what the pre-registry global keys could not
/// do.</para>
///
/// <para>These pin the REGISTRY MECHANISM only (not the VM wiring — the VM's
/// <c>HandleWizardOutcome</c> is a thin pass-through of the same calls proven
/// here, mirroring apple/android's own test scope). Constructs a real
/// <see cref="FfiAccountRegistry"/> (the native fauna_ffi dll loads in the test
/// host — memory <c>reference_windows_dotnet_test_loads_native_ffi</c>).</para>
/// </summary>
public class OnboardingWizardExitPersistenceTests
{
    /// <summary>An in-memory stand-in for Credential Manager — mirrors
    /// <c>AccountRegistryStoreTests.MemoryBackend</c>.</summary>
    private sealed class MemoryBackend : ISecretBackend
    {
        private readonly Dictionary<string, string> _rows = new();
        public string? Get(string resource, string user) =>
            _rows.TryGetValue(resource, out var v) ? v : null;
        public void Set(string resource, string user, string value) => _rows[resource] = value;
        public void Delete(string resource, string user) => _rows.Remove(resource);
    }

    // Two distinct valid 32-byte identity secrets (hex) — same values android uses.
    private const string SecretA =
        "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";
    private const string SecretB =
        "202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f";

    private static FfiAccountRegistry NewRegistry() => new(new LogicalSecretStore(new MemoryBackend()));

    /// <summary>
    /// <b>The <c>test_smoke_i</c> regression.</b> The deferred-DNS exit is reached
    /// with NO handle yet — the wizard goes identity → provisioning →
    /// <c>dns_post_instructions</c> with no handle stage — and the resume must
    /// still survive a force-quit + relaunch. The legacy-global write this
    /// replaced could not express that: an empty required field drops the whole
    /// row, so the relaunch fell through to the handle stage, losing a
    /// half-provisioned nest.
    /// </summary>
    [Fact]
    public async Task DeferredDnsSlotWithNoHandleYetSurvivesTheRelaunch()
    {
        using var registry = NewRegistry();

        registry.PersistAwaitingDns(
            SecretA, "https://nest.example.com",
            "", // the exit has none yet — this is the whole point
            "[{\"record_type\":\"A\",\"name\":\"@\",\"value\":\"203.0.113.7\"}]",
            "DNS-CODE");

        var rec = registry.LaunchPersistence().LoadAwaitingDns();
        Assert.NotNull(rec);
        Assert.Equal("", rec!.handle);
        Assert.Equal("DNS-CODE", rec.claimCode);
        Assert.Contains("203.0.113.7", rec.dnsRecordsJson);

        // The mechanism that actually matters in production: a fresh LaunchMachine
        // over the SAME store routes the relaunch straight to the "Almost ready"
        // wizard entry, off this exact record — not falling through to
        // HandleEntry the way the legacy layout's dropped-record bug did.
        var machine = new LaunchMachine(new NullLaunchObserver(), registry.LaunchPersistence());
        await machine.Start();
        var wizardAt = Assert.IsType<LaunchPhase.WizardAt>(machine.Snapshot().phase);
        Assert.Equal(LaunchWizardEntry.AwaitingManualDns, wizardAt.entry);
    }

    /// <summary>
    /// On a multi-account install the per-actor slot is written under — and
    /// activates — the identity being onboarded, so the launch machine finds it;
    /// the <c>LoggedIn</c> terminal (<c>PersistLoggedIn</c>) spends it.
    /// </summary>
    [Fact]
    public void DeferredDnsSlotSurvivesOnAMultiAccountInstall()
    {
        using var registry = NewRegistry();
        registry.AddAccount(SecretB, "https://b.example", null);

        registry.PersistAwaitingDns(
            SecretA, "https://nest.example.com", "alice@example.com", "[]", "DNS-CODE");
        Assert.Equal("DNS-CODE", registry.LaunchPersistence().LoadAwaitingDns()?.claimCode);

        // …and the LoggedIn terminal spends it.
        registry.PersistLoggedIn(SecretA, "https://nest.example.com", null, null);
        Assert.Null(registry.LaunchPersistence().LoadAwaitingDns());
    }

    /// <summary>The pending-invite twin, for the same multi-account reason.</summary>
    [Fact]
    public void PendingInviteSlotSurvivesOnAMultiAccountInstall()
    {
        using var registry = NewRegistry();
        registry.AddAccount(SecretB, null, null);

        registry.PersistPendingInvite(
            SecretA, "https://nest.example.com", "alice@example.com", "req-1", "{}");
        Assert.Equal("req-1", registry.LaunchPersistence().LoadPendingInvite()?.requestId);

        // …and the LoggedIn terminal spends it.
        registry.PersistLoggedIn(SecretA, "https://nest.example.com", null, null);
        Assert.Null(registry.LaunchPersistence().LoadPendingInvite());
    }
}
