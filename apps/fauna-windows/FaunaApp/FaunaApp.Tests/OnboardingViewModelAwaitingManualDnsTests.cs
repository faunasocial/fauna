using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_onboarding_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Windows leg of the "Almost ready" (awaiting-manual-DNS) surface
/// (onboarding.md § "Almost ready" surface). The OnboardingViewModel is a thin
/// proxy over the shared UniFFI OnboardingMachine (the seeder/snapshot/recheck
/// logic itself is tested in libs/fauna-onboarding-machine), so these pin only
/// the <b>windows glue</b>: the relaunch-hydration seed
/// (<see cref="OnboardingViewModel.SeedAwaitingManualDns"/>) and the derived
/// display properties <see cref="AwaitingManualDnsView"/> binds.
///
/// Constructs a real <see cref="OnboardingMachine"/> (the native fauna_ffi dll
/// loads in the test host — memory <c>reference_windows_dotnet_test_loads_native_ffi</c>).
/// </summary>
public class OnboardingViewModelAwaitingManualDnsTests
{
    private sealed class FakeOnboardingObserver : OnboardingObserver
    {
        public void OnChanged() { }
    }

    private static OnboardingViewModel NewVm() =>
        new(new FakeOnboardingObserver(), new FakeAccountRegistry());

    // Matches fauna_onboarding_machine::state::DnsRecordPlain's serde shape
    // (snake_case) — this is the wire format seed_awaiting_manual_dns_json
    // actually parses; it is NOT what a naive JsonSerializer.Serialize of the
    // UniFFI-bound (camelCase) type would produce, which is exactly the bug
    // this surface's HandleWizardOutcome fix avoids re-introducing.
    private const string OneRecordJson =
        "[{\"record_type\":\"A\",\"name\":\"@\",\"value\":\"1.2.3.4\",\"ttl\":300,\"priority\":null}]";

    [Fact]
    public void IsAwaitingManualDns_IsFalse_BeforeSeeding()
    {
        var vm = NewVm();

        Assert.False(vm.IsAwaitingManualDns);
    }

    [Fact]
    public void SeedAwaitingManualDns_SetsIsAwaitingManualDns_True()
    {
        var vm = NewVm();

        vm.SeedAwaitingManualDns("https://nest.example.com", "alice", OneRecordJson, "claim-123");

        Assert.True(vm.IsAwaitingManualDns);
    }

    [Fact]
    public void SeedAwaitingManualDns_PopulatesRecordsText_FromTheSnakeCaseJson()
    {
        var vm = NewVm();

        vm.SeedAwaitingManualDns("https://nest.example.com", "alice", OneRecordJson, "claim-123");

        // The exact rendering is the machine's own formatter (awaiting_dns_records_text) —
        // this only pins that the record actually parsed (a camelCase/JsonSerializer bug
        // would silently produce an empty list, and this text would be empty too).
        Assert.False(string.IsNullOrWhiteSpace(vm.AwaitingDnsRecordsText));
        Assert.Contains("1.2.3.4", vm.AwaitingDnsRecordsText);
    }

    [Fact]
    public void SeedAwaitingManualDns_LeavesStatusText_NonEmpty()
    {
        var vm = NewVm();

        vm.SeedAwaitingManualDns("https://nest.example.com", "alice", OneRecordJson, "claim-123");

        Assert.False(string.IsNullOrWhiteSpace(vm.AwaitingDnsStatusText));
    }

    [Fact]
    public void AwaitingDnsRecheckEnabled_IsTrue_InThePendingState()
    {
        var vm = NewVm();

        vm.SeedAwaitingManualDns("https://nest.example.com", "alice", OneRecordJson, "claim-123");

        // Freshly seeded lands in AwaitingDnsState.Pending (never Checking/Claiming),
        // so the recheck button must not be disabled out of the gate.
        Assert.True(vm.AwaitingDnsRecheckEnabled);
    }

    [Fact]
    public void AwaitingDnsCopyEnabled_IsFalse_WithNoRecords()
    {
        var vm = NewVm();

        vm.SeedAwaitingManualDns("https://nest.example.com", "alice", "[]", "claim-123");

        // A resumed standard-path run with no records has nothing to copy —
        // the button is disabled, never removed (ui.yaml scopes the ID to this
        // page's required elements).
        Assert.False(vm.AwaitingDnsCopyEnabled);
    }

    [Fact]
    public void AwaitingDnsCopyEnabled_IsTrue_WithRecords()
    {
        var vm = NewVm();

        vm.SeedAwaitingManualDns("https://nest.example.com", "alice", OneRecordJson, "claim-123");

        Assert.True(vm.AwaitingDnsCopyEnabled);
    }

    // ── The exit: "Use a different nest" (onboarding-provisioning.md § "Almost
    // ready" surface → Exit). The door is the shared machine's; these pin that the
    // windows VM forwards to it and binds its enablement rule, nothing more.

    private const string Secret =
        "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20";

    [Fact]
    public void AwaitingDnsFallthroughEnabled_IsTrue_AtRest()
    {
        var vm = NewVm();

        vm.SeedAwaitingManualDns("https://nest.example.com", "alice", OneRecordJson, "claim-123");

        Assert.True(vm.AwaitingDnsFallthroughEnabled);
    }

    [Fact]
    public void AbandonAwaitingManualDns_ClearsTheSlot_AndLandsHandleEntryHoldingTheIdentity()
    {
        var registry = new FakeAccountRegistry();
        var vm = new OnboardingViewModel(new FakeOnboardingObserver(), registry);
        vm.SeedIdentity(Secret);
        vm.SeedAwaitingManualDns("https://nest.example.com", "alice", OneRecordJson, "claim-123");

        vm.AbandonAwaitingManualDnsCommand.Execute(null);

        Assert.False(vm.IsAwaitingManualDns);
        Assert.Equal(OnboardingStep.HandleEntry, vm.CurrentStep);
        Assert.Equal(new[] { Secret }, registry.ClearedAwaitingDns);
    }
}
