using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Tests;

/// <summary>
/// In-memory <see cref="ICaldavPolicyMachine"/> for view-model unit tests — the
/// machine-as-seam peer of <see cref="FakeMailPolicyMachine"/>. Records dispatched
/// actions and returns a configurable <see cref="NextSnapshot"/>; set
/// <see cref="NextError"/> to make Dispatch/Hydrate throw (error-path tests).
/// </summary>
internal sealed class FakeCaldavPolicyMachine : CaldavPolicyMachineFakeBase
{
    public CaldavPolicySnapshot NextSnapshot { get; set; } = Snap();
    public List<CaldavPolicyAction> Dispatched { get; } = new();
    public string? NextError { get; set; }
    public int HydrateCalls { get; private set; }

    public override Task Hydrate()
    {
        HydrateCalls++;
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    public override Task Dispatch(CaldavPolicyAction action)
    {
        Dispatched.Add(action);
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    public override CaldavPolicySnapshot Snapshot() => NextSnapshot;

    /// <summary>A catalog-plausible snapshot (Idle, no error). caldavPort defaults to
    /// 8443 — the field CaldavPolicySnapshot gained with the admin CalDAV-port lift.</summary>
    public static CaldavPolicySnapshot Snap(bool caldavEnabled = false, string? error = null, ushort caldavPort = 8443) =>
        new(caldavEnabled, caldavPort, CaldavPolicyStatus.Idle, error);
}

/// <summary>
/// Deterministic unit tests for the admin <c>admin-calendar</c> page VM, over the
/// <see cref="FakeCaldavPolicyMachine"/> (the UniFFI <c>ICaldavPolicyMachine</c>
/// seam — the e2e flow made deterministic; no live nest / FlaUI, which flakes on
/// windows). The CalDAV-enable sibling of <see cref="AdminMailViewModelTests"/>:
/// a single deployment-wide toggle hydrated from <c>get_mail_config.caldav_enabled</c>
/// and saved via <c>set_caldav_enabled</c>.
/// </summary>
public class AdminCalendarViewModelTests
{
    [Fact]
    public async Task Load_ProjectsCaldavEnabled()
    {
        var fake = new FakeCaldavPolicyMachine
        {
            NextSnapshot = FakeCaldavPolicyMachine.Snap(caldavEnabled: true),
        };
        var vm = new AdminCalendarViewModel(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(1, fake.HydrateCalls);
        Assert.Null(vm.Error);
        Assert.True(vm.CaldavEnabled);
    }

    [Fact]
    public async Task SetCaldavEnabled_DispatchesFlag()
    {
        var fake = new FakeCaldavPolicyMachine();
        var vm = new AdminCalendarViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.SetCaldavEnabledAsync(true);

        Assert.Contains(fake.Dispatched,
            a => a is CaldavPolicyAction.SetCaldavEnabled { enabled: true });
    }

    [Fact]
    public async Task SetCaldavEnabled_ReflectsRereadPersistedState()
    {
        // After the write the machine re-reads persisted state; the VM reflects the
        // re-projected snapshot (here the fake reports it now enabled), not the local
        // gesture — mirrors the shared CaldavPolicyMachine's re-read-after-write.
        var fake = new FakeCaldavPolicyMachine();
        var vm = new AdminCalendarViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);
        Assert.False(vm.CaldavEnabled);

        fake.NextSnapshot = FakeCaldavPolicyMachine.Snap(caldavEnabled: true);
        await vm.SetCaldavEnabledAsync(true);

        Assert.True(vm.CaldavEnabled);
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task Dispatch_Failure_RoutesToError()
    {
        var fake = new FakeCaldavPolicyMachine();
        var vm = new AdminCalendarViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        // The machine throws (and, in production, also captures snapshot.error). The
        // VM swallows the throw and surfaces an error rather than letting it escape.
        fake.NextError = "boom";
        await vm.SetCaldavEnabledAsync(true);

        Assert.False(string.IsNullOrEmpty(vm.Error));
    }

    [Fact]
    public async Task Load_ProjectsCaldavPort()
    {
        // The admin-set CalDAV port (admin.md § 8 Calendar) hydrates from the
        // snapshot's caldav_port as an editable string.
        var fake = new FakeCaldavPolicyMachine
        {
            NextSnapshot = FakeCaldavPolicyMachine.Snap(caldavPort: 9443),
        };
        var vm = new AdminCalendarViewModel(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal("9443", vm.CaldavPort);
    }

    [Fact]
    public async Task SaveCaldavPort_Valid_DispatchesPort_AndRereads()
    {
        var fake = new FakeCaldavPolicyMachine();
        var vm = new AdminCalendarViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        // After the write the machine re-reads persisted state (here 8444).
        fake.NextSnapshot = FakeCaldavPolicyMachine.Snap(caldavPort: 8444);
        vm.CaldavPort = "8444";
        await vm.SaveCaldavPortAsync();

        Assert.Contains(fake.Dispatched,
            a => a is CaldavPolicyAction.SetCaldavPort { port: 8444 });
        Assert.Equal("8444", vm.CaldavPort);
        Assert.Null(vm.Error);
    }

    [Theory]
    [InlineData("0")]        // below [1, 65535]
    [InlineData("65536")]    // above [1, 65535]
    [InlineData("abc")]      // not a number
    [InlineData("")]         // empty
    [InlineData("-1")]       // negative (shared parse_port rejection)
    [InlineData("84.43")]    // fractional (shared parse_port rejection)
    [InlineData("8 443")]    // interior whitespace (shared parse_port rejection)
    public async Task SaveCaldavPort_Invalid_SetsError_AndSkipsDispatch(string bad)
    {
        var fake = new FakeCaldavPolicyMachine();
        var vm = new AdminCalendarViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        vm.CaldavPort = bad;
        await vm.SaveCaldavPortAsync();

        Assert.DoesNotContain(fake.Dispatched, a => a is CaldavPolicyAction.SetCaldavPort);
        Assert.False(string.IsNullOrEmpty(vm.Error));
    }
}
