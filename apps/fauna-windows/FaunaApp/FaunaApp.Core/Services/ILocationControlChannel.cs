using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The folder-binding verbs <see cref="ViewModels.LocationsViewModel"/> drives on the
/// per-user sync agent, narrowed to exactly what the bindings UI needs — the C# peer of
/// macOS's <c>LocationControlChannel</c> protocol (priority #3: the same seam concept on
/// every app). Production is <see cref="AgentLocationControlChannel"/> over the SHARED
/// provisioner (<c>FfiSyncAgentProvisioner</c> = <c>fauna_client_sync::agent</c>); tests
/// and the e2e <c>sync_inject_locations</c> fixture substitute a fake.
///
/// <para>Narrow on purpose: the provisioner also owns start/poke/unprovision/health, none
/// of which the bindings UI may touch — a VM handed the whole provisioner could stop the
/// agent. It is also why a fake is four short methods rather than the provisioner's nine.</para>
///
/// <para><b>These throw; they are not the old best-effort <c>Try*</c> pipe verbs.</b> That
/// is load-bearing for the optimistic model (<c>FfiLocationBindingsModel</c>): a failed push
/// must leave its row <c>PendingBind</c> so the next reconcile re-pushes it, which a
/// swallowed <c>false</c> cannot express. The VM catches per-call and simply does not
/// confirm — see <see cref="ViewModels.LocationsViewModel.ReconcileAsync"/>.</para>
///
/// <para><c>internal</c> because it names the generated <c>FfiAgentLocation</c>, which
/// uniffi-bindgen-cs emits as an internal record; the test + WinUI assemblies see it via
/// <c>[InternalsVisibleTo]</c> — the same shape as <c>INestRpcClient</c>.</para>
/// </summary>
internal interface ILocationControlChannel
{
    /// <summary>Bind a local folder to a nest folder (<c>AddLocation</c> then the
    /// ref-keyed <c>SetLocationFolder</c>, agent-side, idempotent). The set is always
    /// contextual on the folders page, so <paramref name="folderId"/> — the set's
    /// <c>FolderRef</c> wire string, from <c>FolderRefForRow</c> — is the binding's key;
    /// <paramref name="folder"/> is its label (a name is unique only per owner). There
    /// is no name-keyed bind: a row with no ref is refused before it gets here.</summary>
    Task BindLocationAsync(string path, string folder, string folderId);

    /// <summary>Unbind a folder (<c>RemoveLocation</c>): its engine stops and the
    /// binding is forgotten. The nest folder and the engine's state DB are untouched,
    /// so re-binding resumes instead of re-uploading.</summary>
    Task UnbindLocationAsync(string path);

    /// <summary>Set a bound folder's sync mode — <c>"always"</c> or <c>"on-demand"</c>
    /// (<c>folder-location-mode-toggle</c>; binding + on-demand makes the cfapi placeholder
    /// host serve that root). Orthogonal to binding: re-binding does not reset the mode.</summary>
    Task SetLocationSyncModeAsync(string path, string mode);

    /// <summary>The agent's current sync folders (<c>ListLocations</c>) — the truth the
    /// optimistic model reconciles against.</summary>
    Task<IReadOnlyList<FfiAgentLocation>> ListLocationsAsync();

    /// <summary>Apply a folder's held deletes (the mass-delete floor's confirm affordance,
    /// <c>folder-location-apply-deletes-button</c>) — the agent re-derives what is actually
    /// still missing at click time and deletes exactly that set, never a caller-supplied
    /// count. delete-propagation.md § A wholesale-vanished folder is infrastructure
    /// failure.</summary>
    Task<FfiHeldDeletesApplied> ApplyHeldDeletesAsync(string folder);
}

/// <summary>
/// Production <see cref="ILocationControlChannel"/>: a straight forward to the shared
/// provisioner. It holds no state and makes no decisions — the union/optimistic semantics
/// live in <c>FfiLocationBindingsModel</c> (shared Rust), and the drive order lives in the VM.
/// </summary>
internal sealed class AgentLocationControlChannel : ILocationControlChannel
{
    private readonly IFfiSyncAgentProvisioner _agent;

    public AgentLocationControlChannel(IFfiSyncAgentProvisioner agent) => _agent = agent;

    public Task BindLocationAsync(string path, string folder, string folderId) =>
        _agent.BindLocation(path, folder, folderId);

    public Task UnbindLocationAsync(string path) => _agent.UnbindLocation(path);

    public Task SetLocationSyncModeAsync(string path, string mode) =>
        _agent.SetLocationSyncMode(path, mode);

    public async Task<IReadOnlyList<FfiAgentLocation>> ListLocationsAsync() =>
        await _agent.ListLocations();

    public Task<FfiHeldDeletesApplied> ApplyHeldDeletesAsync(string folder) =>
        _agent.ApplyHeldDeletes(folder);
}

/// <summary>
/// The no-session channel: every verb throws "unreachable". Used when the page opens with
/// no <c>SyncAgentSession</c> at all — pre-login, or an actor whose agent was never
/// provisioned.
///
/// <para>Throwing (rather than silently succeeding) is the point: it is indistinguishable
/// to the VM from an agent that is installed but down, so optimistic rows stay rendered and
/// pending, and the very first reconcile after a session appears pushes them. A
/// no-op channel would instead let the model <i>confirm</i> bindings that never reached
/// anything.</para>
/// </summary>
internal sealed class UnreachableLocationControlChannel : ILocationControlChannel
{
    private static Task Fail() =>
        Task.FromException(new InvalidOperationException("no sync-agent session"));

    public Task BindLocationAsync(string path, string folder, string folderId) => Fail();

    public Task UnbindLocationAsync(string path) => Fail();

    public Task SetLocationSyncModeAsync(string path, string mode) => Fail();

    public Task<IReadOnlyList<FfiAgentLocation>> ListLocationsAsync() =>
        Task.FromException<IReadOnlyList<FfiAgentLocation>>(
            new InvalidOperationException("no sync-agent session"));

    public Task<FfiHeldDeletesApplied> ApplyHeldDeletesAsync(string folder) =>
        Task.FromException<FfiHeldDeletesApplied>(
            new InvalidOperationException("no sync-agent session"));
}
