using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The locked launch surface (<c>docs/goal/ui/sessions.md</c> § Layout &amp; flow →
/// <i>The locked surface (leg 2)</i>; <c>devices.md</c> § The locked state) and its
/// stolen-identity step, over a fake ceremony. Pins the notice's three sentences,
/// the type-to-confirm gate re-checked in the action arm, and the ceremony's arms:
/// a persisted landing switches to the successor, every other arm speaks on
/// <c>error-message</c> and switches nothing — the persist failure carrying the
/// only copy of the successor's key.
///
/// <para>Serialized via <c>StringsGlobal</c>: it registers a localizer, and the
/// succession handoff is process-global.</para>
/// </summary>
[Collection("StringsGlobal")]
public class LockedLaunchViewModelTests : IDisposable
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private readonly Dictionary<string, string> _map;
        public FakeLocalizer(Dictionary<string, string> map) => _map = map;
        public string Get(string key) => _map.TryGetValue(key, out var v) ? v : key;
    }

    private const string Predecessor = "aaaa";

    public LockedLaunchViewModelTests()
    {
        SuccessionHandoff.ClearOnCredentialWipe();
        Strings.Initialize(new FakeLocalizer(new()
        {
            ["onboarding/launch/account_locked"] = "This account is locked.",
            ["onboarding/launch/account_locked_until"] = "It unlocks at {time}.",
            ["onboarding/launch/account_locked_not_yours"] = "If you did not lock it, somebody has your secret key.",
            ["settings/recovery_kit/stolen_confirm_placeholder"] = "Type SUCCEED to confirm",
            ["settings/recovery_kit/kit_phrase_required"] = "Paste the recovery phrase first.",
            ["settings/recovery_kit/stolen_persist_failed"] = "Write this secret key down NOW: {secret}",
            ["settings/recovery_kit/stolen_ceremony_failed"] = "Couldn't recover your account: {message}",
        }));
    }

    public void Dispose() => SuccessionHandoff.ClearOnCredentialWipe();

    private sealed class FakeCeremony
    {
        public FfiStolenOutcome Next = MockNestRpcClient.MakeStolenLanded(MockNestRpcClient.MakeLandedSuccession());
        public List<string> Inputs { get; } = new();
        public Task<FfiStolenOutcome> Run(string kit)
        {
            Inputs.Add(kit);
            return Task.FromResult(Next);
        }
    }

    private static LockedLaunchViewModel Vm(FakeCeremony ceremony) =>
        new(1_800_000_000UL, ceremony.Run, Predecessor, formatTime: secs => $"T{secs}");

    [Fact]
    public void The_notice_says_locked_when_it_unlocks_and_that_an_unset_lock_means_theft()
    {
        var vm = Vm(new FakeCeremony());
        Assert.Equal(
            "This account is locked. It unlocks at T1800000000. "
            + "If you did not lock it, somebody has your secret key.",
            vm.NoticeText);
        Assert.False(vm.InStolenEntry);
    }

    [Fact]
    public async Task The_ceremony_is_armed_by_the_literal_word_and_rechecked_in_the_action_arm()
    {
        var ceremony = new FakeCeremony();
        var vm = Vm(ceremony);
        vm.OpenStolenEntry();
        Assert.True(vm.InStolenEntry);
        Assert.False(vm.Armed);

        vm.Phrase = "kit";
        vm.Confirm = "succeed";
        Assert.False(vm.Armed);
        await vm.SucceedAsync(_ => Task.CompletedTask);
        Assert.Empty(ceremony.Inputs);
        Assert.Equal("Type SUCCEED to confirm", vm.Error);

        vm.Confirm = "SUCCEED";
        Assert.True(vm.Armed);
        vm.Phrase = "";
        await vm.SucceedAsync(_ => Task.CompletedTask);
        Assert.Empty(ceremony.Inputs);
        Assert.Equal("Paste the recovery phrase first.", vm.Error);
    }

    [Fact]
    public async Task A_persisted_landing_records_the_handoff_and_switches_to_the_successor()
    {
        var ceremony = new FakeCeremony
        {
            Next = MockNestRpcClient.MakeStolenLanded(
                MockNestRpcClient.MakeLandedSuccession(newActorIdHex: "successor")),
        };
        var vm = Vm(ceremony);
        vm.OpenStolenEntry();
        vm.Phrase = "the kit";
        vm.Confirm = "SUCCEED";
        string? switchedTo = null;

        await vm.SucceedAsync(actor => { switchedTo = actor; return Task.CompletedTask; });

        Assert.Equal(new[] { "the kit" }, ceremony.Inputs);
        Assert.Equal("successor", switchedTo);
        Assert.Equal(Predecessor, SuccessionHandoff.PredecessorActorIdHex);
        Assert.Equal("", vm.Phrase);
        Assert.Equal("", vm.Confirm);
        Assert.Null(vm.Error);
        Assert.False(vm.Busy);
    }

    [Fact]
    public async Task A_landing_this_device_could_not_save_puts_the_key_on_screen_and_switches_nothing()
    {
        var ceremony = new FakeCeremony
        {
            Next = MockNestRpcClient.MakeStolenLanded(
                MockNestRpcClient.MakeLandedSuccession(successorSecretHex: "the-only-copy", persisted: false)),
        };
        var vm = Vm(ceremony);
        vm.OpenStolenEntry();
        vm.Phrase = "kit";
        vm.Confirm = "SUCCEED";
        var switched = false;

        await vm.SucceedAsync(_ => { switched = true; return Task.CompletedTask; });

        Assert.False(switched);
        Assert.Equal("Write this secret key down NOW: the-only-copy", vm.Error);
        // The succession landed either way, so what it owes is recorded either way.
        Assert.Equal(Predecessor, SuccessionHandoff.PredecessorActorIdHex);
    }

    [Fact]
    public async Task An_unlanded_ceremony_speaks_its_shared_sentence_and_switches_nothing()
    {
        var ceremony = new FakeCeremony
        {
            Next = MockNestRpcClient.MakeStolenUnlanded(
                "not-landed", "settings.recovery_kit.stolen_ceremony_failed",
                new Dictionary<string, string> { ["message"] = "the kit does not match" }),
        };
        var vm = Vm(ceremony);
        vm.OpenStolenEntry();
        vm.Phrase = "kit";
        vm.Confirm = "SUCCEED";
        var switched = false;

        await vm.SucceedAsync(_ => { switched = true; return Task.CompletedTask; });

        Assert.False(switched);
        Assert.Equal("Couldn't recover your account: the kit does not match", vm.Error);
        Assert.Null(SuccessionHandoff.PredecessorActorIdHex);
        Assert.True(vm.InStolenEntry);
    }

    [Fact]
    public async Task A_ceremony_that_could_not_start_says_why()
    {
        var vm = new LockedLaunchViewModel(
            1UL, _ => throw new InvalidOperationException("no route to the nest"), Predecessor,
            formatTime: s => s.ToString());
        vm.OpenStolenEntry();
        vm.Phrase = "kit";
        vm.Confirm = "SUCCEED";

        await vm.SucceedAsync(_ => Task.CompletedTask);

        Assert.False(string.IsNullOrEmpty(vm.Error));
        Assert.False(vm.Busy);
    }

    [Fact]
    public void Back_drops_everything_typed_on_the_step()
    {
        var vm = Vm(new FakeCeremony());
        vm.OpenStolenEntry();
        vm.Phrase = "kit";
        vm.Confirm = "SUCC";

        vm.CloseStolenEntry();

        Assert.False(vm.InStolenEntry);
        Assert.Equal("", vm.Phrase);
        Assert.Equal("", vm.Confirm);
        Assert.Null(vm.Error);
    }
}
