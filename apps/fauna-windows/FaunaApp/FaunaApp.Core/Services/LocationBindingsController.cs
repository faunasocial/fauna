using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The **session-scoped folder-binding control plane**: the shared optimistic model
/// (<see cref="FfiLocationBindingsModel"/> = <c>fauna_client_sync::agent::LocationBindingsModel</c>),
/// the agent's last-reported per-path modes, and the reconcile that pushes the model's
/// outstanding binds/unbinds at the agent. The C# peer of linux's <c>sync_agent.rs</c>
/// module surface (<c>add_binding</c> / <c>remove_binding</c> / <c>reconcile</c> /
/// <c>current_locations</c>) — priority #3, the same concept in the same place on every app.
///
/// <para><b>Why this is not the Folders page's job.</b> The page is one renderer of this
/// model among several (the e2e <c>data.sync.locations</c> block reads it too), it is usually
/// closed, and it appears and disappears with navigation. A control plane owned by a page
/// therefore drops mutations whenever the page is shut — and, worse, <b>never pushes them</b>
/// while it is: the agent-reachable edge used to be routed through a static
/// <c>FoldersPage.NotifyAgentReachable</c> (deleted 2026-08-05) that was by contract a no-op
/// with the page closed, so the edge pushed nothing in exactly the case it existed for. Linux
/// draws the boundary in the same place, which is why its <c>reconcile()</c> lives beside the
/// model and merely <i>asks</i> the page to repaint.</para>
///
/// <para><b>It outlives the provisioner, deliberately.</b> The controller is constructed
/// synchronously at login, whereas <see cref="SyncAgentSession.CreateAsync"/> takes as long as
/// its nest round-trips take — ~10 s against a disconnected nest, and longer against a slow
/// one. A binding made in that window used to hit a null session and be <b>dropped silently</b>
/// (one-shot, never retried): a real user who bound a folder just after signing in simply lost
/// the bind. Now the row is recorded and rendered immediately, and
/// <see cref="AttachChannel"/> — the session-install edge — reconciles it onto the agent when
/// the agent finally exists. Nothing here waits on a clock, so a slower nest widens no window
/// (testing.md § conventions point 14).</para>
///
/// <para>Thread-safety: <see cref="AttachChannel"/> and <see cref="ReconcileAsync"/> are
/// called from the convergence loop's tokio task, the gesture methods from the UI thread.
/// The FFI model is internally <c>Mutex</c>-guarded, and <see cref="_gate"/> serializes whole
/// reconciles so a user gesture and a reachable-edge pass cannot interleave their
/// list→reconcile→push→confirm sequences. <see cref="Changed"/> is therefore raised on an
/// arbitrary thread — a renderer subscribing to it owns its own dispatcher hop.</para>
/// </summary>
internal sealed class LocationBindingsController
{
    private readonly FfiLocationBindingsModel _model;
    private readonly SemaphoreSlim _gate = new(1, 1);

    /// <summary>The live agent seam, or the throwing <see cref="UnreachableLocationControlChannel"/>
    /// before a session has installed one. Throwing (not no-op'ing) is what keeps a row
    /// <c>PendingBind</c> instead of confirming a push that reached nothing.</summary>
    private ILocationControlChannel _channel = new UnreachableLocationControlChannel();

    /// <summary>Per-path sync mode as last reported by the agent. Mode is <i>agent</i> truth,
    /// orthogonal to whether a binding is confirmed — a row the agent has never seen renders
    /// the <c>always</c> default until it does.</summary>
    private Dictionary<string, string> _modes = new();

    /// <summary>The rendered union AS LAST SIGNALLED, so a pass that changes nothing raises
    /// nothing. Linux keeps the same guard (<c>AgentUi::last_rendered</c>) for the same
    /// reason: a renderer rebuilds its rows from scratch, and a no-change rebuild destroys
    /// live widgets under the user's cursor. It matters more here than it did before —
    /// background reconciles now run whenever the agent is reachable, not only when the
    /// Folders page happens to be open.</summary>
    private string? _lastSignalled;

    /// <summary>The rendered union changed. Raised on an arbitrary thread; a renderer hops to
    /// its own UI thread before reading <see cref="Rendered"/>.</summary>
    internal event Action? Changed;

    /// <summary>Raise <see cref="Changed"/> iff the rendered union (rows + their modes)
    /// actually differs from the last one signalled.</summary>
    private void SignalIfChanged()
    {
        var signature = string.Join(
            "",
            _model.Rendered().Select(r =>
                $"{r.@path}\u001e{r.@folder}\u001e{ModeFor(r.@path)}\u001e{r.@accessRevoked}\u001e{r.@deletesHeld}\u001e{r.@deletesSkippedUnreadable}"));
        if (signature == _lastSignalled)
        {
            return;
        }

        _lastSignalled = signature;
        Changed?.Invoke();
    }

    internal LocationBindingsController(FfiLocationBindingsModel? model = null)
    {
        _model = model ?? new FfiLocationBindingsModel();
    }

    /// <summary>The shared model itself, for the surfaces that must move it directly.</summary>
    internal FfiLocationBindingsModel Model => _model;

    /// <summary>The union view every renderer reads — confirmed rows plus optimistic
    /// pending-binds, minus pending-unbinds. The twin of linux's
    /// <c>sync_agent::current_locations()</c>.</summary>
    internal IReadOnlyList<FfiBindingRow> Rendered() => _model.Rendered();

    /// <summary>The agent's last-reported mode for <paramref name="path"/>, or — for a
    /// row the agent has not reported yet (an optimistic pending bind) — the mode the
    /// agent WILL give it: <see cref="InMemoryLocationControlChannel.FreshBindingMode"/>,
    /// the windows fresh-binding default (on-demand). Mode is agent truth; the fallback
    /// only has to match what the next <c>ListLocations</c> reply will say, so the row
    /// does not flicker from one mode to the other between the bind and the reconcile.</summary>
    internal string ModeFor(string path) =>
        _modes.TryGetValue(path, out var mode) ? mode : InMemoryLocationControlChannel.FreshBindingMode;

    /// <summary>
    /// Fold a fresh <c>list_engine_holds()</c> roster onto the model (delete-propagation.md
    /// § A wholesale-vanished folder is infrastructure failure — the mass-delete floor's
    /// confirm affordance). Called from the app's existing ~10s agent-status poll tick, NOT
    /// from <see cref="ReconcileAsync"/>: the roster arrives on that separate cadence, and
    /// folding it into a reconcile pass (which also runs on user mutations and reachability
    /// edges) would blank the surface between polls, the same trap tui/linux's own legs
    /// documented. Signals iff the rendered union actually changed — a page holding a row
    /// open under the user's cursor is not rebuilt for a same-value repeat poll.
    /// </summary>
    internal void FoldEngineHolds(IReadOnlyList<FfiEngineHold> holds)
    {
        _model.FoldEngineHolds(holds.ToArray());
        SignalIfChanged();
    }

    /// <summary>
    /// Watch the agent's binding <b>park</b> (file-sync.md § Multi-writer shared sets →
    /// <i>Revocation</i>): read the agent's rows and mirror their <c>access_revoked</c> flag
    /// onto the model (the shared <c>LocationBindingsModel::fold_parks</c>). Called from the
    /// same ~10s status-poll tick as <see cref="FoldEngineHolds"/>, for the same reason: the
    /// agent derives the park itself when the nest refuses a demoted writer's upload, so no
    /// gesture and no reachability edge re-drives <see cref="ReconcileAsync"/> when it
    /// changes — without this the demoted writer's row never learns its binding stopped
    /// syncing. A failed read is skipped, never folded as an empty list (that would clear a
    /// park the user is being told about). Pushes nothing. The twin of macOS's
    /// <c>SyncAgentHealthModel.onLocationsTick</c> → <c>LocationsModel.foldParks</c>.
    /// </summary>
    internal async Task PollParksAsync()
    {
        FfiAgentLocation[] agentRows;
        try
        {
            agentRows = (await Volatile.Read(ref _channel).ListLocationsAsync()).ToArray();
        }
        catch (Exception)
        {
            return;
        }

        _model.FoldParks(agentRows);
        SignalIfChanged();
    }

    /// <summary>
    /// Apply a folder's held deletes (<c>folder-location-apply-deletes-button</c>). The agent
    /// re-derives what is actually still missing at click time and deletes exactly that SET —
    /// never the count this UI last rendered, which a confirm racing a remount would otherwise
    /// stale-delete against. Never optimistic: the row keeps its held count until the reply
    /// lands, then repaints from <c>remaining_held</c> (0 retracts the affordance; non-zero,
    /// e.g. a re-engaged floor, keeps it up) — clearing before the reply would tell the user
    /// their files were gone before anything was actually recorded.
    /// </summary>
    internal async Task<FfiHeldDeletesApplied> ApplyHeldDeletesAsync(string folder)
    {
        var reply = await Volatile.Read(ref _channel).ApplyHeldDeletesAsync(folder);
        _model.SetEngineHold(folder, reply.@remainingHeld);
        SignalIfChanged();
        return reply;
    }

    /// <summary>
    /// The sync-agent session just installed: adopt its control channel and immediately
    /// reconcile, which is what pushes every binding made during start-up. Fire-and-forget by
    /// contract — the caller is the session-install path and must not block on the agent.
    /// </summary>
    internal void AttachChannel(ILocationControlChannel channel)
    {
        SetChannel(channel);
        _ = ReconcileAsync();
    }

    /// <summary>
    /// Adopt a channel WITHOUT driving a reconcile — for a controller whose owner drives the
    /// first pass itself (the Folders page's injected render fixture, the unit tests). The
    /// session-install path wants <see cref="AttachChannel"/>: there, the reconcile IS the
    /// point.
    /// </summary>
    internal void SetChannel(ILocationControlChannel channel) =>
        Interlocked.Exchange(ref _channel, channel);

    /// <summary>
    /// The session went away (sign-out, account switch, nest re-point) without this login
    /// ending: fall back to the throwing channel so pushes stay pending rather than
    /// confirming against a dead provisioner. The model is kept — the rows are the user's,
    /// not the agent's.
    /// </summary>
    internal void DetachChannel() =>
        Interlocked.Exchange(ref _channel, new UnreachableLocationControlChannel());

    /// <summary>
    /// Reconcile the optimistic model against the agent's <c>ListLocations</c> truth and
    /// push whatever the model asks for. Returns <c>true</c> when at least one push failed —
    /// the caller decides whether that is reportable (a user gesture: yes; a background edge:
    /// no, since an agent that is simply not running is the normal opt-in case).
    ///
    /// <para>Each push is guarded individually: a throw leaves that row unconfirmed, which is
    /// exactly the model's re-push state, so one rejected binding cannot stop the others from
    /// landing.</para>
    /// </summary>
    internal async Task<bool> ReconcileAsync()
    {
        await _gate.WaitAsync();
        try
        {
            var channel = Volatile.Read(ref _channel);
            FfiAgentLocation[] agentRows;
            try
            {
                agentRows = (await channel.ListLocationsAsync()).ToArray();
            }
            catch (Exception)
            {
                // Unreachable — a pending bind stays visible and the next edge re-drives us.
                SignalIfChanged();
                return true;
            }

            _modes = agentRows
                .GroupBy(f => f.@path)
                .ToDictionary(g => g.Key, g => g.Last().@mode);

            var actions = _model.Reconcile(agentRows);
            var pushFailed = false;

            foreach (var bind in actions.@toBind)
            {
                try
                {
                    await channel.BindLocationAsync(bind.@path, bind.@folder, bind.@folderId);
                    _model.ConfirmBind(bind.@path);
                }
                catch (Exception)
                {
                    pushFailed = true;
                }
            }

            foreach (var path in actions.@toUnbind)
            {
                try
                {
                    await channel.UnbindLocationAsync(path);
                    _model.ConfirmUnbind(path);
                }
                catch (Exception)
                {
                    pushFailed = true;
                }
            }

            SignalIfChanged();
            return pushFailed;
        }
        finally
        {
            _gate.Release();
        }
    }

    /// <summary>
    /// Optimistic add: record the binding (it renders immediately, before the bind push) and
    /// reconcile. No content-key push precedes it: the agent resolves the newly-bound set's
    /// keys from the account's custody itself (on-demand-files.md § Shared sets on a
    /// capability host → <i>One mechanism</i>). A blank path or set is a no-op —
    /// the set is contextual on the Folders page and the model holds only <i>bindings</i>,
    /// so there is no "add unbound" state. <paramref name="folderId"/> — the set's
    /// <c>FolderRef</c> wire string — is the binding's key and is required; the caller
    /// refuses a row that yields none (<c>LocationsViewModel.AddLocationAsync</c>).
    /// Returns <c>true</c> when the push failed.
    /// </summary>
    internal async Task<bool> AddAsync(string? path, string? folder, string folderId)
    {
        if (string.IsNullOrWhiteSpace(path) || string.IsNullOrWhiteSpace(folder))
        {
            return false;
        }

        _model.Add(path, folder.Trim(), folderId);
        SignalIfChanged();
        return await ReconcileAsync();
    }

    /// <summary>
    /// Optimistic remove keyed by <b>path</b> — this UI's row key. <c>RemoveBySet</c> would
    /// also unbind a sibling folder bound to the same set, which the user never touched.
    /// </summary>
    internal Task<(string[] Removed, bool PushFailed)> RemoveByPathAsync(string path) =>
        RemoveAsync(_model.RemoveByPath(path));

    /// <summary>
    /// Remove every binding of <paramref name="folder"/> — linux's set-keyed row key, kept so
    /// the shared e2e action layer stays uniform across apps.
    /// </summary>
    internal Task<(string[] Removed, bool PushFailed)> RemoveBySetAsync(string folder) =>
        RemoveAsync(_model.RemoveBySet(folder));

    /// <summary>Shared tail of the two removes: an empty selection is nothing to push (and
    /// the caller's cue that no such binding existed), otherwise render then reconcile.</summary>
    private async Task<(string[] Removed, bool PushFailed)> RemoveAsync(string[] removed)
    {
        if (removed.Length == 0)
        {
            return (removed, false);
        }

        SignalIfChanged();
        return (removed, await ReconcileAsync());
    }

    /// <summary>
    /// Toggle a folder's sync mode (<c>folder-location-mode-toggle</c>). Orthogonal to the
    /// binding, so it does not touch the model; the reconcile afterwards re-reads the agent's
    /// mode for the row. Returns <c>true</c> when the mode push failed.
    /// </summary>
    internal async Task<bool> SetModeAsync(string path, string mode)
    {
        var ok = true;
        try
        {
            await Volatile.Read(ref _channel).SetLocationSyncModeAsync(path, mode);
        }
        catch (Exception)
        {
            ok = false;
        }

        await ReconcileAsync();
        return !ok;
    }
}
