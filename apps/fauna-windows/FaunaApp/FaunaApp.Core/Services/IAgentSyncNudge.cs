using System;
using System.Threading.Tasks;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The remote-change nudge: tell the local agent's resident engine for a folder to pull
/// <b>now</b>, off its rescan cadence (file-sync.md § Remote-change nudge). Driven by
/// <see cref="NestRpcClient"/> when a <c>SyncChanged</c> push arrives — the windows twin of
/// linux's <c>sync_agent::pull_set_now</c> and tui's <c>SyncAgentState::pull_set_now</c>,
/// all three landing on the one shared <c>PullFolderNow</c> verb (priority #3).
///
/// <para>Its own seam rather than a method on <see cref="ILocationControlChannel"/>: this is
/// keyed by file <i>set</i> and is about sync freshness, not folder bindings, and its one
/// consumer is the push pump rather than the bindings UI.</para>
///
/// <para><b>"Best-effort" here means latency, not optional.</b> Without this nudge the only
/// delivery path left is the agent's 300 s rescan tick, so a second device's save takes
/// minutes instead of seconds to appear — the regression a later fix closed. An absent agent or a
/// set with no resident engine is genuinely a no-op, which is why the pump swallows.</para>
/// </summary>
internal interface IAgentSyncNudge
{
    /// <summary>Nudge the engine serving <paramref name="folder"/>. Throws if the agent is
    /// unreachable; the caller (a push pump) swallows, since a throw there would kill every
    /// other push kind with it. <paramref name="folderHash"/> is the push's own hash address,
    /// relayed as received — the agent matches its bindings by it, so a sealed set's nudge
    /// (blank <paramref name="folder"/>) still reaches its engine.</summary>
    Task PullFolderNowAsync(string folder, byte[]? folderHash);
}

/// <summary>
/// Production <see cref="IAgentSyncNudge"/>. Resolves the agent session per call for the
/// same reason <see cref="AgentHealthProbe"/> does: the RPC client is built once per login
/// while the session comes and goes across sign-out and account switch.
/// </summary>
internal sealed class AgentSyncNudge : IAgentSyncNudge
{
    private readonly Func<IFfiSyncAgentProvisioner?> _current;

    public AgentSyncNudge(Func<IFfiSyncAgentProvisioner?> current) => _current = current;

    public async Task PullFolderNowAsync(string folder, byte[]? folderHash)
    {
        var agent = _current() ?? throw new InvalidOperationException("no sync-agent session");
        await agent.PullFolderNow(folder, folderHash);
    }
}
