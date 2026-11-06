using System;
using System.Collections.ObjectModel;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The local-folder binding VM, nested under each folder on the Folders page
/// (on-demand-files.md § On-Demand Files; ui.yaml § folders — the set is contextual, so there
/// is no free-text set name).
///
/// <para><b>A renderer, not the control plane.</b> The optimistic model, the agent seam and
/// the reconcile all live on the session-scoped <see cref="LocationBindingsController"/>; this
/// VM projects its union view into a bound collection and forwards the page's gestures. That
/// split is load-bearing rather than tidy: a control plane owned by this VM is torn down and
/// rebuilt with the page, so a binding made before the page opened — or an agent-reachable
/// edge that arrived while it was closed — had nothing alive to push it. See the controller's
/// own summary for the user-facing bug that shape caused.</para>
///
/// <para><b>The model is the render source, not the agent</b> (D7-i; sync-agent.md
/// § Implementation status today): a bind made while the agent is down stays visible and
/// re-pushes; a failed push stays <c>PendingBind</c> rather than vanishing; a row bound from
/// another surface (fauna-tui, another session) is adopted; and an agent config reset
/// re-pushes instead of silently erasing the user's bindings.</para>
///
/// <para>⚠ No <c>ConfigureAwait(false)</c> — a WinUI VM that mutates bound state off
/// the UI thread throws a silent <c>COMException</c>
/// (reference_windows_vm_configureawait_comexception). For the same reason
/// <see cref="Render"/> is the page's to call after hopping the controller's
/// <c>Changed</c> signal onto the dispatcher.</para>
/// </summary>
public partial class LocationsViewModel : ViewModelBase
{
    private readonly LocationBindingsController _controller;

    [ObservableProperty] private bool _isLoading;

    /// <summary>The bound folders, one per <c>folder-location-row</c>.</summary>
    public ObservableCollection<LocationRowVm> Locations { get; } = new();

    internal LocationsViewModel(LocationBindingsController controller)
    {
        _controller = controller;
    }

    /// <summary>Stand-alone VM over a bare channel — the unit tests and the e2e
    /// <c>sync_inject_locations</c> render fixture, both of which want a throwaway control
    /// plane of their own rather than the session's.</summary>
    internal LocationsViewModel(ILocationControlChannel channel, FfiLocationBindingsModel? model = null)
        : this(new LocationBindingsController(model))
    {
        // SetChannel, not AttachChannel: the caller drives the first reconcile itself (the
        // page's LoadCommand, a test's explicit Load), and an extra background pass would
        // race it.
        _controller.SetChannel(channel);
    }

    /// <summary>Initial load — the at-attach reconcile.</summary>
    [RelayCommand]
    private Task LoadAsync() => DriveAsync(surfaceErrors: false);

    /// <summary>
    /// Re-drive the reconcile because the agent just became reachable. Kept as a VM entry
    /// point for the tests that pin the reachable-edge faces; in production the controller
    /// is wired to that edge directly, so it fires whether or not this page exists.
    /// </summary>
    public Task OnAgentReachableAsync() => DriveAsync(surfaceErrors: false);

    /// <summary>Project the controller's rendered union into the bound collection; each
    /// row's mode is the shared model's agent-mirrored one. UI thread only.</summary>
    public void Render()
    {
        Locations.Clear();
        foreach (var row in _controller.Rendered())
        {
            var verb = _controller.FreeSpaceVerb(row.@path);
            Locations.Add(new LocationRowVm(
                row.@path,
                row.@folder,
                row.@mode,
                row.@accessRevoked,
                row.@deletesHeld,
                row.@deletesSkippedUnreadable,
                verb is null
                    ? null
                    : new FreeSpaceVerbVm(verb.@running, verb.@buttonLabel, verb.@onDeviceLine, verb.@resultLine)));
        }
    }

    /// <summary>
    /// Run one controller operation and render its outcome. <paramref name="surfaceErrors"/>
    /// is set by the user-gesture callers, for whom a failed push IS reportable — an agent
    /// that is simply not running is the normal case (on-demand sync is opt-in) and must not
    /// raise a banner on its own.
    /// </summary>
    private async Task DriveAsync(bool surfaceErrors, Func<Task<bool>>? operation = null)
    {
        IsLoading = true;
        if (surfaceErrors)
        {
            SetError(null);
        }

        try
        {
            var pushFailed = operation is null
                ? await _controller.ReconcileAsync()
                : await operation();
            Render();
            if (pushFailed && surfaceErrors)
            {
                SetError(Strings.Get("devices/sync_locations/helper_unavailable"));
            }
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// <summary>
    /// Bind a local folder to the folder whose row hosts this control
    /// (<c>folder-location-add-button</c> + <c>folder-location-path-input</c>). A blank path is a
    /// no-op (no folder was chosen). <paramref name="folderId"/> is the set's
    /// <c>FolderRef</c> wire string (<c>FolderRefForRow</c> — the page has it, since the
    /// set is contextual) and the binding's key, since a name is unique only per owner
    /// (on-demand-files.md § Hosting multiple on-demand folders). A row that yields no
    /// ref is <b>refused</b> on the page's error line — fail closed, never bound by
    /// name (the name-keyed bind was retired 2026-09-24).
    ///
    /// <para>The row renders <b>immediately</b>, before the push: that is the optimistic
    /// contract, and it is what keeps a bind made against a downed agent from vanishing.</para>
    /// </summary>
    public Task AddLocationAsync(string? path, string? folder, string? folderId)
    {
        if (string.IsNullOrWhiteSpace(path) || string.IsNullOrWhiteSpace(folder))
        {
            return Task.CompletedTask;
        }

        if (string.IsNullOrWhiteSpace(folderId))
        {
            SetError(Strings.Format(
                "devices/error_bind_location", "the folder's identity could not be resolved"));
            return Task.CompletedTask;
        }

        return DriveAsync(surfaceErrors: true, () => _controller.AddAsync(path, folder, folderId));
    }

    /// <summary>Toggle a folder's sync mode (<c>folder-location-mode-toggle</c>, windows only —
    /// binding + on-demand makes the cfapi placeholder host serve that root).</summary>
    public Task SetModeAsync(string path, string mode) =>
        DriveAsync(surfaceErrors: true, () => _controller.SetModeAsync(path, mode));

    /// <summary>Remove a folder binding (<c>folder-location-remove-button</c>). Keyed by
    /// <b>path</b>, this UI's row key — <c>RemoveBySet</c> would also unbind a sibling
    /// folder bound to the same set, which the user never touched. The row disappears
    /// immediately and the unbind re-pushes until the agent stops listing it.</summary>
    public Task RemoveLocationAsync(string path) =>
        DriveAsync(
            surfaceErrors: true,
            async () => (await _controller.RemoveByPathAsync(path)).PushFailed);

    /// <summary>Apply a folder's held deletes (<c>folder-location-apply-deletes-button</c>;
    /// delete-propagation.md § A wholesale-vanished folder is infrastructure failure).
    /// Never optimistic — the row keeps its held count until the reply lands and repaints
    /// from what the agent actually applied, never a caller-guessed count. Unlike bind/
    /// unbind, the channel verb THROWS rather than returning a pushed/failed bool (an
    /// unreachable agent has nothing to re-derive against), so the catch lives here,
    /// converted to <see cref="DriveAsync"/>'s ordinary pushFailed=true reporting.</summary>
    public Task ApplyHeldDeletesAsync(string folder) =>
        DriveAsync(surfaceErrors: true, async () =>
        {
            try
            {
                await _controller.ApplyHeldDeletesAsync(folder);
                return false;
            }
            catch (Exception)
            {
                return true;
            }
        });

    /// <summary><i>Free up space</i> on the bound on-demand folder at <paramref name="path"/>
    /// (<c>folder-location-free-space-button</c>; on-demand-files.md § The free-up-space
    /// gesture where the OS offers none). Never optimistic: the row repaints from the
    /// agent's <c>ListLocations</c> after the press, and the status tick carries the sweep's
    /// readings as it moves them. A refusal goes to the error line.</summary>
    public Task FreeLocationSpaceAsync(string path) =>
        DriveAsync(surfaceErrors: true, () => _controller.FreeLocationSpaceAsync(path));
}

/// <summary>How a bound row renders its <i>Free up space</i> verb — the shared
/// <c>FreeSpaceVerb</c> as the FFI resolves it: whether a sweep is walking the location
/// (<c>folder-location-free-space-button</c>'s <c>state</c>), the button's label, the
/// standing on-this-device line (<c>folder-location-on-device</c>) and the latest sweep's
/// outcome (<c>folder-location-free-space-result</c>, <c>null</c> until one has run).</summary>
public sealed record FreeSpaceVerbVm(bool Running, string ButtonLabel, string OnDeviceLine, string? ResultLine)
{
    /// <summary>The button's <c>state</c> attr: <c>running</c> or <c>idle</c>.</summary>
    public string State => Running ? "running" : "idle";
}

/// <summary>One bound-folder row (<c>folder-location-row</c>): the local path
/// (<c>folder-location-path</c>), the bound folder name, and the sync mode
/// (<c>folder-location-mode-toggle</c>). Projected from the shared binding model.
/// <see cref="FreeSpace"/> is the row's <i>Free up space</i> verb, <c>null</c> where the shared
/// rule withholds it (a resident row, a root without its placeholder surface).</summary>
public sealed record LocationRowVm(
    string Path,
    string Folder,
    string Mode,
    bool AccessRevoked,
    ulong DeletesHeld,
    ulong DeletesSkippedUnreadable,
    FreeSpaceVerbVm? FreeSpace = null)
{
    /// <summary>True when this folder serves placeholders on demand — drives the
    /// per-row <c>folder-location-mode-toggle</c> on/off state.</summary>
    public bool IsOnDemand => Mode == "on-demand";

    /// <summary>The bound folder name. Every rendered row IS a binding (the model holds
    /// only bindings, and an added-but-unbound agent folder is not a renderable one), so
    /// unlike the pre-cutover row there is no "(unbound)" state to render.</summary>
    public string FolderDisplay => Folder;

    /// <summary>Gates both <c>folder-location-deletes-held</c> and
    /// <c>folder-location-apply-deletes-button</c> — 0 renders NEITHER element (a
    /// standing offer to destroy files over a healthy folder is worse than no
    /// affordance at all; delete-propagation.md's own rule).</summary>
    public bool HasDeletesHeld => DeletesHeld > 0;

    /// <summary>"This folder looks empty. Deletions held: {count}. Reconnect the folder,
    /// or apply them to your nest."</summary>
    public string DeletesHeldText => Strings.Format("folders/deletes_held", DeletesHeld);

    /// <summary>"Apply held deletions ({count})".</summary>
    public string ApplyDeletesText => Strings.Format("folders/apply_deletes", DeletesHeld);

    /// <summary>Gates <c>folder-location-unreadable</c> — 0 renders nothing
    /// (delete-propagation.md § Unreadable is not absent). Independent of
    /// <see cref="HasDeletesHeld"/>: a row can report either, both, or neither.</summary>
    public bool HasUnreadable => DeletesSkippedUnreadable > 0;

    /// <summary>"Fauna couldn't read {count} items in this folder, so it has stopped
    /// syncing them. Check that the drive is connected and that Fauna can open the
    /// folder." No action accompanies this line — there is nothing to confirm.</summary>
    public string UnreadableText => Strings.Format("folders/unreadable", DeletesSkippedUnreadable);
}
