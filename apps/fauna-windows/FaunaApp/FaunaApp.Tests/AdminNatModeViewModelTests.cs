using System;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_core;
using uniffi.fauna_onboarding_machine;

namespace FaunaApp.Tests;

/// <summary>
/// In-memory <see cref="IAdminNatModeMachine"/> for view-model unit tests — the
/// machine-as-seam peer of <c>FakeWebdavPolicyMachine</c>. Tracks the last
/// dispatched action and returns a configurable <see cref="NextSnapshot"/>; set
/// <see cref="NextError"/> to make Hydrate/Submit throw (error-path tests).
/// </summary>
internal sealed class FakeAdminNatModeMachine : AdminNatModeMachineFakeBase
{
    public NatModeSnapshot NextSnapshot { get; set; } = Snap();
    public string? NextError { get; set; }
    public int HydrateCalls { get; private set; }
    public int SubmitCalls { get; private set; }
    public NodeMode? LastSelected { get; private set; }

    public override Task Hydrate()
    {
        HydrateCalls++;
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    public override void Select(NodeMode mode)
    {
        LastSelected = mode;
    }

    public override NatModeSnapshot Snapshot() => NextSnapshot;

    public override Task Submit()
    {
        SubmitCalls++;
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    /// <summary>A catalog-plausible snapshot (Choosing, submit enabled).</summary>
    public static NatModeSnapshot Snap(
        NodeMode selectedMode = NodeMode.Public,
        bool submitEnabled = true,
        string messageKey = "") =>
        new(new NatModeState.Choosing(), selectedMode,
            new LocalizedText(messageKey, new()), submitEnabled);
}

/// <summary>
/// Deterministic unit tests for the <c>admin-nest</c> page's NAT-mode section VM,
/// over the <see cref="FakeAdminNatModeMachine"/> (the UniFFI
/// <c>IAdminNatModeMachine</c> seam — the e2e flow made deterministic; no live nest
/// / FlaUI, which flakes on win-arm64). admin.md § Nest → NAT-mode control.
/// </summary>
public class AdminNatModeViewModelTests
{
    [Fact]
    public async Task Hydrate_ProjectsSelectedModeAndStatus()
    {
        var fake = new FakeAdminNatModeMachine
        {
            NextSnapshot = FakeAdminNatModeMachine.Snap(
                NodeMode.Private, messageKey: "admin.nest_page.nat_mode_choosing"),
        };
        var vm = new AdminNatModeViewModel(fake);

        await vm.HydrateCommand.ExecuteAsync(null);

        Assert.Equal(1, fake.HydrateCalls);
        Assert.False(vm.PublicSelected);
        Assert.True(vm.PrivateSelected);
        Assert.True(vm.SubmitEnabled);
        // No localizer in the test host → Resolve falls back to the dotted key.
        Assert.Equal("admin.nest_page.nat_mode_choosing", vm.StatusText);
    }

    [Fact]
    public void Select_ForwardsModeToMachine_AndReprojects()
    {
        var fake = new FakeAdminNatModeMachine
        {
            NextSnapshot = FakeAdminNatModeMachine.Snap(NodeMode.Public),
        };
        var vm = new AdminNatModeViewModel(fake);

        fake.NextSnapshot = FakeAdminNatModeMachine.Snap(NodeMode.Private);
        vm.Select(NodeMode.Private);

        Assert.Equal(NodeMode.Private, fake.LastSelected);
        Assert.True(vm.PrivateSelected);
        Assert.False(vm.PublicSelected);
    }

    [Fact]
    public async Task Submit_CommitsThenReprojects()
    {
        var fake = new FakeAdminNatModeMachine
        {
            NextSnapshot = FakeAdminNatModeMachine.Snap(NodeMode.Private),
        };
        var vm = new AdminNatModeViewModel(fake);

        fake.NextSnapshot = FakeAdminNatModeMachine.Snap(
            NodeMode.Private, messageKey: "admin.nest_page.nat_mode_saved");
        await vm.SubmitCommand.ExecuteAsync(null);

        Assert.Equal(1, fake.SubmitCalls);
        // The set is mutable — save stays enabled after a successful commit
        // (an immediate re-flip is allowed), unlike the wizard's terminal Done.
        Assert.True(vm.SubmitEnabled);
        Assert.Equal("admin.nest_page.nat_mode_saved", vm.StatusText);
    }

    [Fact]
    public async Task Submit_Failure_StillReprojectsFromSnapshot()
    {
        // The machine records the cause into the snapshot's message
        // (nat_mode_error_transient / _terminal); the VM swallows the throw
        // rather than letting it escape (mirrors AdminAliasesViewModel).
        var fake = new FakeAdminNatModeMachine
        {
            NextSnapshot = FakeAdminNatModeMachine.Snap(
                NodeMode.Public, submitEnabled: true,
                messageKey: "admin.nest_page.nat_mode_error_transient"),
        };
        var vm = new AdminNatModeViewModel(fake);

        fake.NextError = "boom";
        await vm.SubmitCommand.ExecuteAsync(null);

        Assert.Equal(1, fake.SubmitCalls);
        Assert.True(vm.SubmitEnabled); // resubmit always allowed
        Assert.Equal("admin.nest_page.nat_mode_error_transient", vm.StatusText);
    }

    [Fact]
    public async Task Hydrate_Failure_StillReprojectsFromSnapshot()
    {
        var fake = new FakeAdminNatModeMachine
        {
            NextSnapshot = FakeAdminNatModeMachine.Snap(
                NodeMode.Public, submitEnabled: true,
                messageKey: "admin.nest_page.nat_mode_error_load"),
        };
        var vm = new AdminNatModeViewModel(fake);

        fake.NextError = "boom";
        await vm.HydrateCommand.ExecuteAsync(null);

        Assert.Equal(1, fake.HydrateCalls);
        // A read failure leaves save enabled — the set is safe to submit
        // without a successful read (mutable upsert).
        Assert.True(vm.SubmitEnabled);
        Assert.Equal("admin.nest_page.nat_mode_error_load", vm.StatusText);
    }
}
