using System;
using System.Threading.Tasks;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The app's read-only view of the local sync agent: its process health (the global
/// <c>sync-agent-status</c> shell element) and its work signal (the Status page's
/// Connected / Syncing / queue-depth readout). Narrow seam so the VMs — which live in
/// FaunaApp.Core and therefore cannot reach <c>App.CurrentSyncAgent</c> — stay
/// unit-testable, exactly like <see cref="ILocationControlChannel"/>.
///
/// <para><b>Two questions, two calls, deliberately.</b> "Is the agent process healthy" and
/// "what is it doing" have different answers and different consumers
/// (<c>MainViewModel</c> vs <c>StatusViewModel</c>); a single call returning the union
/// would make each consumer carry fields it must ignore.</para>
///
/// <para>The health tri-state itself is <b>shared Rust</b>
/// (<c>fauna_client_sync::agent::agent_health_state</c>, via
/// <c>FfiSyncAgentProvisioner.AgentHealth</c>): the client no longer compares versions
/// itself, which is what kept windows, linux and macOS from drifting on what
/// "restart pending" means.</para>
/// </summary>
internal interface IAgentStatusProbe
{
    /// <summary>The agent's process health. Never throws: an unreachable agent (not
    /// started, not installed — the normal case, since on-demand sync is opt-in) is
    /// <see cref="FfiAgentHealthState.NotRunning"/>, not an error.</summary>
    Task<FfiAgentStatus> ProbeAsync(string localBuildVersion);

    /// <summary>The agent's per-device sync signal, or <c>null</c> when it is unreachable
    /// — the caller then keeps its defaults rather than rendering an error.</summary>
    Task<FfiAgentSyncStatus?> SyncStatusAsync();
}

/// <summary>
/// Production <see cref="IAgentStatusProbe"/>. Resolves the current agent session
/// <b>lazily, per probe</b> rather than capturing it at construction: the VMs are built
/// once per login, while the agent session comes and goes across sign-in, sign-out and
/// account switch, so a captured handle would report a dead agent forever.
/// </summary>
internal sealed class AgentStatusProbe : IAgentStatusProbe
{
    private readonly Func<IFfiSyncAgentProvisioner?> _current;

    public AgentStatusProbe(Func<IFfiSyncAgentProvisioner?> current) => _current = current;

    /// <summary>The live agent, or null — never throwing, since a faulted resolver must
    /// read as "no agent" rather than take down a poll.</summary>
    private IFfiSyncAgentProvisioner? Agent()
    {
        try
        {
            return _current();
        }
        catch (Exception)
        {
            return null;
        }
    }

    public async Task<FfiAgentStatus> ProbeAsync(string localBuildVersion)
    {
        // No session at all (pre-login, post-sign-out, or never provisioned) reads
        // exactly as an unreachable one — the indicator's job is to say whether THIS
        // device's agent is serving, and in both cases it is not.
        var agent = Agent();
        if (agent is null)
        {
            return new FfiAgentStatus(FfiAgentHealthState.NotRunning, "", 0);
        }

        return await agent.AgentHealth(localBuildVersion);
    }

    public async Task<FfiAgentSyncStatus?> SyncStatusAsync()
    {
        var agent = Agent();
        if (agent is null) return null;
        return await agent.SyncStatus();
    }
}
