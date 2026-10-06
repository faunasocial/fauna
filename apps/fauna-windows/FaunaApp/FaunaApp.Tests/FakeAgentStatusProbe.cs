using System.Collections.Generic;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// In-memory <see cref="IAgentStatusProbe"/> for the VM unit tests — canned answers for
/// the agent's two read questions, with no live agent.
///
/// <para>The health tri-state is NOT derived here: it is shared Rust
/// (<c>agent_health_state</c>), so a fake that re-derived it would pin the fake. Tests set
/// the state they want to render and assert the label mapping, which is the only part the
/// VM still owns.</para>
/// </summary>
internal sealed class FakeAgentStatusProbe : IAgentStatusProbe
{
    /// <summary>What <see cref="ProbeAsync"/> returns. Defaults to an unreachable agent —
    /// the ordinary state on a box where on-demand sync was never set up.</summary>
    public FfiAgentStatus Status { get; set; } =
        new(FfiAgentHealthState.NotRunning, "", 0);

    /// <summary>What <see cref="SyncStatusAsync"/> returns; <c>null</c> (the default) is an
    /// unreachable agent.</summary>
    public FfiAgentSyncStatus? SyncStatus { get; set; }

    public Task<FfiAgentStatus> ProbeAsync(string localBuildVersion) =>
        Task.FromResult(Status);

    public Task<FfiAgentSyncStatus?> SyncStatusAsync() => Task.FromResult(SyncStatus);
}

/// <summary>
/// In-memory <see cref="IAgentSyncNudge"/> — records every set nudged, in order. There is
/// no engine behind it, so recording the call IS the observable, which is exactly what the
/// push-pump tests assert (file-sync.md § Remote-change nudge).
/// </summary>
internal sealed class FakeAgentSyncNudge : IAgentSyncNudge
{
    public List<string> PulledNow { get; } = new();

    /// <summary>When true every nudge throws — the absent-agent case the pump must swallow
    /// without taking the other push kinds down with it.</summary>
    public bool Unreachable { get; set; }

    public Task PullFolderNowAsync(string folder, byte[]? folderHash)
    {
        PulledNow.Add(folder);
        return Unreachable
            ? Task.FromException(new System.InvalidOperationException("no sync-agent session"))
            : Task.CompletedTask;
    }
}
