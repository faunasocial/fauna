using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Threading.Tasks;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// In-memory <see cref="ILocationControlChannel"/> — the app-side E2E backend for the
/// Folders page's nested local-folder binding (the production-side twin of the unit
/// tests' <c>FakeLocationControlChannel</c>). The cross-app e2e seeds it via the
/// <c>sync_inject_locations</c> TestAgent command (<see cref="FromJson"/>) and swaps the page
/// VM onto it, so the page renders and dispatches over a deterministic fake instead of a
/// live agent: the real add flow opens a native OS folder picker the drivers can't drive.
/// Only wired when the TestAgent is active; production never uses it.
/// (file-sync.md § On-Demand Files; ui.yaml § folders.)
///
/// <para><b>A pure render fixture.</b> The page pairs it with a throwaway
/// <c>FfiLocationBindingsModel</c>, so nothing here reaches the session model that
/// <c>AppDataSnapshot.GetSyncForState</c> reports — which is what stops an injected folder
/// list from letting a test assert a real engine is serving on a box with no agent. The
/// same line linux draws between <c>sync_add_location</c> and <c>sync_inject_locations</c>.</para>
/// </summary>
internal sealed class InMemoryLocationControlChannel : ILocationControlChannel
{
    private sealed record Location(string Path, string Folder, string FolderId, string Mode);

    private readonly List<Location> _locations = new();

    /// <summary>Seed one bound folder (path → folder + mode). A blank mode defaults to
    /// <c>always</c>. A blank folder is skipped: the binding model holds only bindings,
    /// and an unbound folder is not a renderable one.
    ///
    /// <para>A binding is keyed by its set's <c>FolderRef</c>, so every seeded row carries
    /// one: <paramref name="folderId"/> when the fixture names it, else a stable synthetic
    /// <c>local:</c> ref per distinct folder name — this fake reaches no agent and no
    /// nest, so the ref only has to be well-formed and consistent.</para></summary>
    public void Seed(string path, string? folder, string? mode, string? folderId = null)
    {
        if (string.IsNullOrWhiteSpace(folder)) return;
        _locations.Add(new Location(
            path,
            folder,
            string.IsNullOrWhiteSpace(folderId) ? SyntheticRef(folder) : folderId,
            string.IsNullOrWhiteSpace(mode) ? FreshBindingMode : mode));
    }

    /// <summary>The mode a fresh windows binding starts in — <c>"on-demand"</c>, the
    /// agent's windows default (<c>LocationMode::fresh_binding_default</c> in
    /// <c>bins/fauna-sync-agent/src/config.rs</c>; user ruling 2026-09-26,
    /// on-demand-files.md § On-Demand Files → <em>The choice is the user's</em>). This
    /// fake stands in for that agent under e2e, so a seeded row lacking <c>mode</c> and a
    /// <see cref="BindLocationAsync"/> both start where the real agent would.</summary>
    public const string FreshBindingMode = "on-demand";

    private string SyntheticRef(string folder)
    {
        var known = _locations.FirstOrDefault(l => l.Folder == folder);
        if (known is not null) return known.FolderId;
        return $"local:{_locations.Select(l => l.Folder).Distinct().Count() + 1}";
    }

    /// <summary>Build a seeded fake from a <c>sync_inject_locations</c> command's
    /// <c>folders</c> array (<c>[{path, folder, mode?, folder_id?}]</c> — the shape
    /// actions/sync_locations.py sends). Entries with no <c>path</c> or no
    /// <c>folder</c> are skipped.</summary>
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
                fake.Seed(path, Str(f, "folder"), Str(f, "mode"), Str(f, "folder_id"));
            }
        }
        return fake;
    }

    private static string? Str(JsonElement obj, string key) =>
        obj.TryGetProperty(key, out var v) && v.ValueKind == JsonValueKind.String
            ? v.GetString()
            : null;

    public Task BindLocationAsync(string path, string folder, string folderId)
    {
        _locations.RemoveAll(f => f.Path == path);
        _locations.Add(new Location(path, folder, folderId, FreshBindingMode));
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
            .Select(f => new FfiAgentLocation(f.Path, f.Folder, f.FolderId, f.Mode, false))
            .ToList();
        return Task.FromResult(list);
    }

    /// <summary>A pure render fixture never holds anything (no real engine underneath) —
    /// nothing applied, nothing remaining, no floor engaged.</summary>
    public Task<FfiHeldDeletesApplied> ApplyHeldDeletesAsync(string folder) =>
        Task.FromResult(new FfiHeldDeletesApplied(0, 0, false));
}
