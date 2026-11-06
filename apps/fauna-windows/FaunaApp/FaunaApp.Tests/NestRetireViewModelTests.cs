using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using uniffi.fauna_onboarding_machine;

namespace FaunaApp.Tests;

/// <summary>
/// In-memory <see cref="INestRetireMachine"/>: records every call the view-model
/// makes, in order, and answers <see cref="Snapshot"/> with <see cref="Snap"/>.
/// </summary>
internal sealed class FakeNestRetireMachine : NestRetireMachineFakeBase
{
    public List<string> Calls { get; } = new();
    public RetireSnapshot Snap { get; set; } = NestRetireViewModelTests.SnapshotAt(RetirePhase.Credentials);
    public string? Provider { get; set; }

    public override void ExpectAppInputs() => Calls.Add("expect_app_inputs");
    public override void AppInputsLanded() => Calls.Add("app_inputs_landed");
    public override void AddCandidateDomains(string[] domains)
        => Calls.Add($"add_candidate_domains:{string.Join(",", domains)}");
    public override void SetHeldDnsJson(string[] ids, string[] json)
        => Calls.Add($"set_held_dns_json:{string.Join(",", ids)}|{string.Join(",", json)}");
    public override void SetRememberedSlotsJson(string dkim, string atproto, string self)
        => Calls.Add($"set_remembered_slots_json:{dkim}|{atproto}|{self}");
    public override void SetInputError(string message) => Calls.Add($"set_input_error:{message}");
    public override void SetCurrentIpv4(string? ipv4) => Calls.Add($"set_current_ipv4:{ipv4}");
    public override void Back() => Calls.Add("back");
    public override void Cancel() => Calls.Add("cancel");
    public override string? SelectedProvider() => Provider;
    public override void SelectProvider(string providerId)
    {
        Calls.Add($"select_provider:{providerId}");
        Provider = providerId;
    }
    public override void SetCredentialField(string fieldId, string value) => Calls.Add($"set_cred:{fieldId}");
    public override RetireSnapshot Snapshot() => Snap;
}

public class NestRetireViewModelTests
{
    internal static RetireSnapshot SnapshotAt(RetirePhase phase, ManagedServerRow? retired = null)
        => new(
            phase,
            Array.Empty<ManagedServerRow>(),
            null,
            new[]
            {
                new RetireStepRow(RetireStep.Dns, new StepState.Pending()),
                new RetireStepRow(RetireStep.Server, new StepState.Pending()),
            },
            Array.Empty<DnsPlanLine>(),
            Array.Empty<LeftoverLine>(),
            "",
            false,
            false,
            null,
            retired);

    private static ManagedServerRow Row(bool current)
        => new("101", "example-test", "203.0.113.5", null, "example.test",
            Array.Empty<string>(), true, current, new TransferCodeState.Unsupported());

    private static FfiRetireHeldInputs Held(string? notReady = null)
        => new(new[] { "hetzner" }, new[] { "{\"api-token\":\"held\"}" },
            "{\"example.test\":[\"a._domainkey.example.test\"]}", "{}", "{}", notReady);

    private sealed record Harness(
        NestRetireViewModel Vm, FakeNestRetireMachine Machine,
        List<RetireMachineInputs> Built, List<bool> HeldWaits);

    private static Harness Make(FfiRetireHeldInputs? held = null, string? resolved = null)
    {
        var machine = new FakeNestRetireMachine();
        var built = new List<RetireMachineInputs>();
        var waits = new List<bool>();
        var vm = new NestRetireViewModel(
            buildMachine: inputs => { built.Add(inputs); return machine; },
            readHeldInputs: wait => { waits.Add(wait); return Task.FromResult(held ?? Held()); },
            resolveIpv4: _ => Task.FromResult(resolved));
        return new Harness(vm, machine, built, waits);
    }

    private static Task<IReadOnlyList<string>> Domains(params string[] d)
        => Task.FromResult<IReadOnlyList<string>>(d);

    [Fact]
    public async Task Admin_entry_lands_the_shared_held_read_and_the_active_domains_between_expect_and_landed()
    {
        var h = Make(Held("account plane not ready"));
        await h.Vm.Open(RetireEntry.Admin, "https://nest.example.test", "http://fake/vps", "http://fake/doh",
            () => Domains("example.test", "second.test"));

        Assert.Equal(new[] { true }, h.HeldWaits);
        Assert.Equal(
            new RetireMachineInputs(new[] { "nest.example.test" }, "http://fake/vps", "http://fake/doh"),
            h.Built[0],
            new InputsComparer());
        var landed = h.Machine.Calls.IndexOf("app_inputs_landed");
        Assert.Equal(0, h.Machine.Calls.IndexOf("expect_app_inputs"));
        Assert.True(landed > 0, string.Join(" / ", h.Machine.Calls));
        Assert.Contains("add_candidate_domains:example.test,second.test", h.Machine.Calls);
        Assert.Contains("set_held_dns_json:hetzner|{\"api-token\":\"held\"}", h.Machine.Calls);
        Assert.Contains(
            "set_remembered_slots_json:{\"example.test\":[\"a._domainkey.example.test\"]}|{}|{}",
            h.Machine.Calls);
        Assert.Contains("set_input_error:account plane not ready", h.Machine.Calls);
        foreach (var call in new[] { "add_candidate_domains", "set_held_dns_json",
                                     "set_remembered_slots_json", "set_input_error" })
        {
            var at = h.Machine.Calls.FindIndex(c => c.StartsWith(call));
            Assert.True(at > 0 && at < landed, $"{call} must land between expect and landed: "
                + string.Join(" / ", h.Machine.Calls));
        }
    }

    [Fact]
    public async Task Launch_entry_reads_no_live_domains_and_does_not_wait_for_a_runtime()
    {
        var h = Make();
        var asked = false;
        await h.Vm.Open(RetireEntry.Launch, "http://127.0.0.1:4100", null, null,
            () => { asked = true; return Domains("never.test"); });

        Assert.False(asked, "the launch entry has no session to read active domains from");
        Assert.Equal(new[] { false }, h.HeldWaits);
        Assert.Empty(h.Built[0].CandidateDomains); // an IP literal is never a domain
        Assert.DoesNotContain(h.Machine.Calls, c => c.StartsWith("add_candidate_domains"));
        Assert.Contains("app_inputs_landed", h.Machine.Calls);
        Assert.DoesNotContain(h.Machine.Calls, c => c.StartsWith("set_input_error"));
    }

    [Fact]
    public async Task The_saved_nests_address_reaches_the_machine_for_the_current_badge()
    {
        var h = Make(resolved: "127.0.0.1");
        await h.Vm.Open(RetireEntry.Admin, "http://127.0.0.1:4100", null, null, () => Domains());
        await Task.Delay(50); // the resolve is fire-and-forget beside the inputs
        Assert.Contains("set_current_ipv4:127.0.0.1", h.Machine.Calls);
    }

    [Theory]
    [InlineData("Credentials", true)]
    [InlineData("Running", true)]
    [InlineData("Done", true)]
    [InlineData("List", false)]
    [InlineData("Confirm", false)]
    [InlineData("Listing", false)]
    public async Task Back_leaves_from_credentials_and_a_stopped_run_and_steps_back_from_list_and_confirm(
        string phaseName, bool leaves)
    {
        var phase = Enum.Parse<RetirePhase>(phaseName);
        var h = Make();
        await h.Vm.Open(RetireEntry.Launch, null, null, null, () => Domains());
        h.Machine.Snap = SnapshotAt(phase);

        Assert.Equal(leaves, h.Vm.Back());
        var steppedBack = phase is RetirePhase.List or RetirePhase.Confirm;
        Assert.Equal(steppedBack, h.Machine.Calls.Contains("back"));
    }

    [Theory]
    [InlineData("Admin", true, true)]
    [InlineData("Admin", false, false)]
    [InlineData("Launch", true, false)]
    public async Task Leaving_drops_the_session_only_after_retiring_the_current_box_from_admin(
        string entryName, bool current, bool drops)
    {
        var entry = Enum.Parse<RetireEntry>(entryName);
        var h = Make();
        await h.Vm.Open(entry, null, null, null, () => Domains());
        h.Machine.Snap = SnapshotAt(RetirePhase.Done, retired: Row(current));

        Assert.Equal(drops, h.Vm.Leave());
        Assert.Contains("cancel", h.Machine.Calls); // the token never outlives the visit
        Assert.Null(h.Vm.Machine);
    }

    [Fact]
    public async Task Switching_provider_forgets_what_was_typed_for_the_old_one()
    {
        var h = Make();
        await h.Vm.Open(RetireEntry.Launch, null, null, null, () => Domains());
        h.Vm.SelectProvider("hetzner");
        h.Vm.SetCred("api-token", "secret");
        Assert.Equal("secret", h.Vm.GetCred("api-token"));
        h.Vm.SelectProvider("vultr");
        Assert.Equal("", h.Vm.GetCred("api-token"));
    }

    private sealed class InputsComparer : IEqualityComparer<RetireMachineInputs>
    {
        public bool Equals(RetireMachineInputs? a, RetireMachineInputs? b)
            => a is not null && b is not null
               && string.Join(",", a.CandidateDomains) == string.Join(",", b.CandidateDomains)
               && a.VpsBaseUrl == b.VpsBaseUrl && a.DohBaseUrl == b.DohBaseUrl;
        public int GetHashCode(RetireMachineInputs o) => 0;
    }
}
