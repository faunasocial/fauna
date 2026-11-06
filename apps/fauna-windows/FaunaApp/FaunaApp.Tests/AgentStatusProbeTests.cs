using System;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// <see cref="AgentStatusProbe"/> keeps the "never throws" contract its interface states,
/// whatever the agent handle does — the poll that calls it is an <c>async void</c>
/// <c>DispatcherTimer</c> tick on <c>MainPage</c>, where an escaped exception is an
/// unhandled crash on the UI thread (<c>apps/windows.md</c> § Client lifetime, the teardown
/// corollary: a page's work must tolerate a session object that is already destroyed).
///
/// <para>The race this pins: the probe re-resolves <c>App.CurrentSyncAgent</c> per tick,
/// and <c>SyncAgentSession.Stop</c> (sign-out, account switch, the e2e reset) can dispose
/// the provisioner between that resolve and the call on it — the UniFFI wrapper then
/// throws <see cref="ObjectDisposedException"/>.</para>
/// </summary>
public class AgentStatusProbeTests
{
    /// <summary>A provisioner whose every read throws — a disposed UniFFI wrapper, or any
    /// other fault of the agent's channel.</summary>
    private sealed class ThrowingProvisioner : FfiSyncAgentProvisionerFakeBase
    {
        private readonly Exception _fault;

        public ThrowingProvisioner(Exception fault) => _fault = fault;

        public override Task<FfiAgentStatus> AgentHealth(string localBuildVersion) => throw _fault;

        public override Task<FfiAgentSyncStatus?> SyncStatus() => throw _fault;
    }

    /// <summary>The same faults, raised from the awaited task rather than the call.</summary>
    private sealed class FaultingTaskProvisioner : FfiSyncAgentProvisionerFakeBase
    {
        public override Task<FfiAgentStatus> AgentHealth(string localBuildVersion) =>
            Task.FromException<FfiAgentStatus>(new InvalidOperationException("pipe closed"));

        public override Task<FfiAgentSyncStatus?> SyncStatus() =>
            Task.FromException<FfiAgentSyncStatus?>(new InvalidOperationException("pipe closed"));
    }

    [Fact]
    public async Task Probe_OnADestroyedProvisioner_ReadsNotRunning()
    {
        var probe = new AgentStatusProbe(
            () => new ThrowingProvisioner(new ObjectDisposedException("FfiSyncAgentProvisioner")));

        var status = await probe.ProbeAsync("0.1.0");

        Assert.Equal(FfiAgentHealthState.NotRunning, status.@state);
    }

    [Fact]
    public async Task Probe_OnAFaultedHealthRead_ReadsNotRunning()
    {
        var probe = new AgentStatusProbe(() => new FaultingTaskProvisioner());

        var status = await probe.ProbeAsync("0.1.0");

        Assert.Equal(FfiAgentHealthState.NotRunning, status.@state);
    }

    [Fact]
    public async Task SyncStatus_OnADestroyedProvisioner_ReadsUnreachable()
    {
        var probe = new AgentStatusProbe(
            () => new ThrowingProvisioner(new ObjectDisposedException("FfiSyncAgentProvisioner")));

        Assert.Null(await probe.SyncStatusAsync());
    }

    [Fact]
    public async Task SyncStatus_OnAFaultedRead_ReadsUnreachable()
    {
        var probe = new AgentStatusProbe(() => new FaultingTaskProvisioner());

        Assert.Null(await probe.SyncStatusAsync());
    }
}
