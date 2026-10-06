using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Tests;

/// <summary>
/// In-memory <see cref="ICarddavPolicyMachine"/> for view-model unit tests — the
/// machine-as-seam peer of <see cref="FakeCaldavPolicyMachine"/>. Records dispatched
/// actions and returns a configurable <see cref="NextSnapshot"/>; set
/// <see cref="NextError"/> to make Dispatch/Hydrate throw (error-path tests).
/// </summary>
internal sealed class FakeCarddavPolicyMachine : CarddavPolicyMachineFakeBase
{
    public CarddavPolicySnapshot NextSnapshot { get; set; } = Snap();
    public List<CarddavPolicyAction> Dispatched { get; } = new();
    public string? NextError { get; set; }
    public int HydrateCalls { get; private set; }

    public override Task Hydrate()
    {
        HydrateCalls++;
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    public override Task Dispatch(CarddavPolicyAction action)
    {
        Dispatched.Add(action);
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    public override CarddavPolicySnapshot Snapshot() => NextSnapshot;

    /// <summary>A catalog-plausible snapshot (Idle, no error).</summary>
    public static CarddavPolicySnapshot Snap(bool carddavEnabled = false, string? error = null) =>
        new(carddavEnabled, CarddavPolicyStatus.Idle, error);
}

/// <summary>
/// Deterministic unit tests for the admin <c>admin-contacts</c> page VM, over the
/// <see cref="FakeCarddavPolicyMachine"/> (the UniFFI <c>ICarddavPolicyMachine</c>
/// seam — the e2e flow made deterministic; no live nest / FlaUI, which flakes on
/// windows). The CardDAV-enable sibling of <see cref="AdminCalendarViewModelTests"/>:
/// a single deployment-wide toggle hydrated from <c>get_mail_config.carddav_enabled</c>
/// and saved via <c>set_carddav_enabled</c>. No port surface (CardDAV rides
/// admin-calendar's shared port field).
/// </summary>
public class AdminContactsViewModelTests
{
    [Fact]
    public async Task Load_ProjectsCarddavEnabled()
    {
        var fake = new FakeCarddavPolicyMachine
        {
            NextSnapshot = FakeCarddavPolicyMachine.Snap(carddavEnabled: true),
        };
        var vm = new AdminContactsViewModel(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(1, fake.HydrateCalls);
        Assert.Null(vm.Error);
        Assert.True(vm.CarddavEnabled);
    }

    [Fact]
    public async Task SetCarddavEnabled_DispatchesFlag()
    {
        var fake = new FakeCarddavPolicyMachine();
        var vm = new AdminContactsViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.SetCarddavEnabledAsync(true);

        Assert.Contains(fake.Dispatched,
            a => a is CarddavPolicyAction.SetCarddavEnabled { enabled: true });
    }

    [Fact]
    public async Task SetCarddavEnabled_ReflectsRereadPersistedState()
    {
        // After the write the machine re-reads persisted state; the VM reflects the
        // re-projected snapshot (here the fake reports it now enabled), not the local
        // gesture — mirrors the shared CarddavPolicyMachine's re-read-after-write.
        var fake = new FakeCarddavPolicyMachine();
        var vm = new AdminContactsViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);
        Assert.False(vm.CarddavEnabled);

        fake.NextSnapshot = FakeCarddavPolicyMachine.Snap(carddavEnabled: true);
        await vm.SetCarddavEnabledAsync(true);

        Assert.True(vm.CarddavEnabled);
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task Dispatch_Failure_RoutesToError()
    {
        var fake = new FakeCarddavPolicyMachine();
        var vm = new AdminContactsViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        // The machine throws (and, in production, also captures snapshot.error). The
        // VM swallows the throw and surfaces an error rather than letting it escape.
        fake.NextError = "boom";
        await vm.SetCarddavEnabledAsync(true);

        Assert.False(string.IsNullOrEmpty(vm.Error));
    }
}
