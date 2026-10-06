using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Tests;

/// <summary>
/// In-memory <see cref="IWebdavPolicyMachine"/> for view-model unit tests — the
/// machine-as-seam peer of <see cref="FakeCarddavPolicyMachine"/>. Records dispatched
/// actions and returns a configurable <see cref="NextSnapshot"/>; set
/// <see cref="NextError"/> to make Dispatch/Hydrate throw (error-path tests).
/// </summary>
internal sealed class FakeWebdavPolicyMachine : WebdavPolicyMachineFakeBase
{
    public WebdavPolicySnapshot NextSnapshot { get; set; } = Snap();
    public List<WebdavPolicyAction> Dispatched { get; } = new();
    public string? NextError { get; set; }
    public int HydrateCalls { get; private set; }

    public override Task Hydrate()
    {
        HydrateCalls++;
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    public override Task Dispatch(WebdavPolicyAction action)
    {
        Dispatched.Add(action);
        if (NextError is not null) throw new InvalidOperationException(NextError);
        return Task.CompletedTask;
    }

    public override WebdavPolicySnapshot Snapshot() => NextSnapshot;

    /// <summary>A catalog-plausible snapshot (Idle, no error).</summary>
    public static WebdavPolicySnapshot Snap(bool webdavEnabled = false, string? error = null) =>
        new(webdavEnabled, WebdavPolicyStatus.Idle, error);
}

/// <summary>
/// Deterministic unit tests for the admin <c>admin-files</c> page VM, over the
/// <see cref="FakeWebdavPolicyMachine"/> (the UniFFI <c>IWebdavPolicyMachine</c>
/// seam — the e2e flow made deterministic; no live nest / FlaUI, which flakes on
/// windows). The WebDAV-enable sibling of <see cref="AdminContactsViewModelTests"/>:
/// a single deployment-wide toggle hydrated from <c>get_mail_config.webdav_enabled</c>
/// and saved via <c>set_webdav_enabled</c>. No port surface (WebDAV rides
/// admin-calendar's shared port field).
/// </summary>
public class AdminFilesViewModelTests
{
    [Fact]
    public async Task Load_ProjectsWebdavEnabled()
    {
        var fake = new FakeWebdavPolicyMachine
        {
            NextSnapshot = FakeWebdavPolicyMachine.Snap(webdavEnabled: true),
        };
        var vm = new AdminFilesViewModel(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal(1, fake.HydrateCalls);
        Assert.Null(vm.Error);
        Assert.True(vm.WebdavEnabled);
    }

    [Fact]
    public async Task SetWebdavEnabled_DispatchesFlag()
    {
        var fake = new FakeWebdavPolicyMachine();
        var vm = new AdminFilesViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.SetWebdavEnabledAsync(true);

        Assert.Contains(fake.Dispatched,
            a => a is WebdavPolicyAction.SetWebdavEnabled { enabled: true });
    }

    [Fact]
    public async Task SetWebdavEnabled_ReflectsRereadPersistedState()
    {
        // After the write the machine re-reads persisted state; the VM reflects the
        // re-projected snapshot (here the fake reports it now enabled), not the local
        // gesture — mirrors the shared WebdavPolicyMachine's re-read-after-write.
        var fake = new FakeWebdavPolicyMachine();
        var vm = new AdminFilesViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);
        Assert.False(vm.WebdavEnabled);

        fake.NextSnapshot = FakeWebdavPolicyMachine.Snap(webdavEnabled: true);
        await vm.SetWebdavEnabledAsync(true);

        Assert.True(vm.WebdavEnabled);
        Assert.Null(vm.Error);
    }

    [Fact]
    public async Task Dispatch_Failure_RoutesToError()
    {
        var fake = new FakeWebdavPolicyMachine();
        var vm = new AdminFilesViewModel(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        // The machine throws (and, in production, also captures snapshot.error). The
        // VM swallows the throw and surfaces an error rather than letting it escape.
        fake.NextError = "boom";
        await vm.SetWebdavEnabledAsync(true);

        Assert.False(string.IsNullOrEmpty(vm.Error));
    }
}
