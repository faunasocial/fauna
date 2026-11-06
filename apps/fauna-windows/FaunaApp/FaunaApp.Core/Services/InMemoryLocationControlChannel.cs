using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Threading.Tasks;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

#if DEBUG || FAUNA_E2E_AGENT
/// <summary>
/// In-memory <see cref="ILocationControlChannel"/> — the app-side E2E backend for the
/// Folders page's nested local-folder binding (the production-side twin of the unit
/// tests' <c>FakeLocationControlChannel</c>). The cross-app e2e seeds it via the
/// <c>sync_inject_locations</c> TestAgent command (<see cref="FromJson"/>) and swaps the page
/// VM onto it, so the page renders and dispatches over a deterministic fake instead of a
/// live agent: the real add flow opens a native OS folder picker the drivers can't drive.
/// Only wired when the TestAgent is active; production never uses it, so the whole
/// class compiles out of a Release build with the agent (convention 15).
/// (on-demand-files.md § On-Demand Files; ui.yaml § folders.)
///
/// <para><b>A pure render fixture.</b> The page pairs it with a throwaway
/// <c>FfiLocationBindingsModel</c>, so nothing here reaches the session model that
/// <c>AppDataSnapshot.GetSyncForState</c> reports — which is what stops an injected folder
/// list from letting a test assert a real engine is serving on a box with no agent. The
/// same line linux draws between <c>sync_add_location</c> and <c>sync_inject_locations</c>.</para>
/// </summary>
internal sealed class InMemoryLocationControlChannel : ILocationControlChannel
{
    /// <summary>One bound folder. <c>OnDeviceBytes</c> of <c>TotalBytes</c> are on this
    /// device, held by <c>OnDeviceFiles</c> files; <c>FreeSpace</c> is the latest
    /// <i>Free up space</i> sweep (<c>null</c> until one has run) — the readings the real
    /// agent reports on <c>ListLocations</c>.</summary>
    private sealed record Location(
        string Path,
        string Folder,
        string FolderId,
        string Mode,
        ulong TotalBytes = 0,
        ulong OnDeviceBytes = 0,
        ulong OnDeviceFiles = 0,
        FfiFreeSpaceRun? FreeSpace = null);

    private readonly List<Location> _locations = new();

    /// <summary>Seed one bound folder (path → folder + mode). A blank mode defaults to
    /// <c>always</c>. A blank folder is skipped: the binding model holds only bindings,
    /// and an unbound folder is not a renderable one.
    ///
    /// <para>A binding is keyed by its set's <c>FolderRef</c>, so every seeded row carries
    /// one: <paramref name="folderId"/> when the fixture names it, else a stable synthetic
    /// <c>local:</c> ref per distinct folder name — this fake reaches no agent and no
    /// nest, so the ref only has to be well-formed and consistent.</para>
    ///
    /// <para><paramref name="onDeviceBytes"/> of <paramref name="totalBytes"/>, held by
    /// <paramref name="onDeviceFiles"/> files (one when only bytes are given), are on this
    /// device — what a <see cref="FreeLocationSpaceAsync"/> press frees.</para></summary>
    public void Seed(
        string path,
        string? folder,
        string? mode,
        string? folderId = null,
        ulong totalBytes = 0,
        ulong onDeviceBytes = 0,
        ulong? onDeviceFiles = null)
    {
        if (string.IsNullOrWhiteSpace(folder)) return;
        _locations.Add(new Location(
            path,
            folder,
            string.IsNullOrWhiteSpace(folderId) ? SyntheticRef(folder) : folderId,
            string.IsNullOrWhiteSpace(mode) ? LocationBindingsController.FreshBindingMode : mode,
            Math.Max(totalBytes, onDeviceBytes),
            onDeviceBytes,
            onDeviceFiles ?? (onDeviceBytes > 0 ? 1ul : 0ul)));
    }

    private string SyntheticRef(string folder)
    {
        var known = _locations.FirstOrDefault(l => l.Folder == folder);
        if (known is not null) return known.FolderId;
        return $"local:{_locations.Select(l => l.Folder).Distinct().Count() + 1}";
    }

    /// <summary>Build a seeded fake from a <c>sync_inject_locations</c> command's
    /// <c>folders</c> array (<c>[{path, folder, mode?, folder_id?, total_bytes?,
    /// on_device_bytes?, on_device_files?}]</c> — the shape actions/sync_locations.py
    /// sends). Entries with no <c>path</c> or no <c>folder</c> are skipped.</summary>
    public static InMemoryLocationControlChannel FromJson(JsonElement folders)
    {
        var fake = new InMemoryLocationControlChannel();
        if (folders.ValueKind == JsonValueKind.Array)
        {
            foreach (var f in folders.EnumerateArray())
            {
                if (f.ValueKind != JsonValueKind.Object) continue;
                var path = Str(f, "path");
                if (string.IsNullOrEmpty(path)) continue;
                fake.Seed(
                    path,
                    Str(f, "folder"),
                    Str(f, "mode"),
                    Str(f, "folder_id"),
                    Num(f, "total_bytes") ?? 0,
                    Num(f, "on_device_bytes") ?? 0,
                    Num(f, "on_device_files"));
            }
        }
        return fake;
    }

    private static string? Str(JsonElement obj, string key) =>
        obj.TryGetProperty(key, out var v) && v.ValueKind == JsonValueKind.String
            ? v.GetString()
            : null;

    private static ulong? Num(JsonElement obj, string key) =>
        obj.TryGetProperty(key, out var v) && v.ValueKind == JsonValueKind.Number
            && v.TryGetUInt64(out var n)
            ? n
            : null;

    public Task BindLocationAsync(string path, string folder, string folderId)
    {
        _locations.RemoveAll(f => f.Path == path);
        _locations.Add(new Location(path, folder, folderId, LocationBindingsController.FreshBindingMode));
        return Task.CompletedTask;
    }

    public Task UnbindLocationAsync(string path)
    {
        _locations.RemoveAll(f => f.Path == path);
        return Task.CompletedTask;
    }

    public Task SetLocationSyncModeAsync(string path, string mode)
    {
        var i = _locations.FindIndex(f => f.Path == path);
        if (i >= 0) _locations[i] = _locations[i] with { Mode = mode };
        return Task.CompletedTask;
    }

    public Task<IReadOnlyList<FfiAgentLocation>> ListLocationsAsync()
    {
        IReadOnlyList<FfiAgentLocation> list = _locations
            .Select(f => new FfiAgentLocation(
                f.Path,
                f.Folder,
                f.FolderId,
                f.Mode,
                false,
                totalBytes: f.TotalBytes,
                // The agent reports the bytes here for an on-demand binding alone: a
                // resident one keeps every file.
                onDeviceBytes: f.Mode == "on-demand" ? f.OnDeviceBytes : null,
                freeSpace: f.FreeSpace))
            .ToList();
        return Task.FromResult(list);
    }

    /// <summary>A pure render fixture never holds anything (no real engine underneath) —
    /// nothing applied, nothing remaining, no floor engaged.</summary>
    public Task<FfiHeldDeletesApplied> ApplyHeldDeletesAsync(string folder) =>
        Task.FromResult(new FfiHeldDeletesApplied(0, 0, false));

    /// <summary>The agent's sweep, settled at once: every seeded byte on this device is
    /// freed (this fixture holds no unrecorded change and no pin) and the outcome is what
    /// the next <see cref="ListLocationsAsync"/> reports. Refused, as the agent refuses it,
    /// for a path that is no bound folder and for a resident one.</summary>
    public Task FreeLocationSpaceAsync(string path)
    {
        var i = _locations.FindIndex(f => f.Path == path);
        if (i < 0)
            return Task.FromException(new InvalidOperationException($"{path} is not a bound location's root"));
        var location = _locations[i];
        if (location.Mode != "on-demand")
            return Task.FromException(new InvalidOperationException(
                $"{path} keeps every file on this device (always-resident): there is nothing to free"));
        _locations[i] = location with
        {
            OnDeviceBytes = 0,
            OnDeviceFiles = 0,
            FreeSpace = new FfiFreeSpaceRun(false, location.OnDeviceFiles, location.OnDeviceBytes, 0, 0),
        };
        return Task.CompletedTask;
    }

    /// <summary>A pure render fixture has no group set and no engine underneath, so there
    /// is never a review row to read: nothing moved.</summary>
    public Task<bool> RefreshSetConflictsAsync(FfiLocationBindingsModel model) =>
        Task.FromResult(false);

    /// <summary>Refused, as the agent refuses a row it does not hold: this fixture holds
    /// none.</summary>
    public Task RestoreRetainedVersionAsync(string folderId, long conflictId) =>
        Task.FromException(new InvalidOperationException(
            $"{folderId} has no review row {conflictId}"));
}
#endif
