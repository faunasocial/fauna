using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit tests for the local-folder binding VM (file-sync.md § On-Demand
/// Files; ui.yaml § folders — binding is nested under a contextual set), over a
/// <see cref="FakeLocationControlChannel"/> implementing the same
/// <see cref="ILocationControlChannel"/> seam the real
/// <c>AgentLocationControlChannel</c> does — no live agent, no <c>\\.\pipe\fauna-sync</c>,
/// no FlaUI (which flakes on win-arm64, so this VM is the windows deterministic gate).
///
/// <para><b>These drive the REAL shared state machine.</b> The VM holds an
/// <c>FfiLocationBindingsModel</c> — <c>fauna_client_sync::agent::LocationBindingsModel</c>
/// over UniFFI — and the test assembly loads the native FFI, so nothing about the
/// optimistic/union semantics is mocked here. That is deliberate: the semantics are
/// already unit-pinned in Rust, so re-asserting them against a C# double would only pin
/// the double. What these pin is the half Rust cannot see — that the VM <i>drives</i> the
/// model correctly: reconcile at attach, after every mutation, and on the reachable edge;
/// confirm only on a successful push; render from the model rather than the agent's list.</para>
///
/// <para>The four faces below marked (i)–(iv) mirror macOS's
/// <c>LocationsModelReconcileTests</c> one-for-one (priority #3: the same faces pinned
/// the same way on every app), which is what the finding asked for.</para>
/// </summary>
public class LocationsViewModelTests
{
    private static LocationsViewModel Vm(FakeLocationControlChannel fake) => new(fake);

    [Fact]
    public async Task Load_AdoptsAgentRowsWithPathFolderAndMode()
    {
        var fake = new FakeLocationControlChannel();
        fake.Seed(@"C:\Users\alice\Docs", "documents", "on-demand");
        var vm = Vm(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        var row = Assert.Single(vm.Locations);
        Assert.Equal(@"C:\Users\alice\Docs", row.Path);
        Assert.Equal("documents", row.Folder);
        Assert.Equal("on-demand", row.Mode);
        Assert.True(row.IsOnDemand);
    }

    [Fact]
    public async Task AddLocation_BindsViaRefVariant_AndRenders()
    {
        var fake = new FakeLocationControlChannel();
        var vm = Vm(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.AddLocationAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");

        Assert.Contains(@"bind:C:\Users\alice\Docs:documents:ref-documents", fake.Calls);
        var row = Assert.Single(vm.Locations);
        Assert.Equal("documents", row.Folder);
        Assert.Null(vm.ErrorMessage);
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("   ")]
    public async Task AddLocation_BlankPath_IsNoOp(string? path)
    {
        var fake = new FakeLocationControlChannel();
        var vm = Vm(fake);

        await vm.AddLocationAsync(path, "documents", "ref-documents");

        Assert.Empty(fake.Calls);
        Assert.Empty(vm.Locations);
    }

    /// <summary>The set is contextual on the folders page (ui.yaml retired the free-text
    /// <c>folder-location-fileset-input</c>), and the shared model holds only <i>bindings</i> —
    /// so a blank set is not "add unbound", it is nothing to do.</summary>
    [Fact]
    public async Task AddLocation_BlankFolder_IsNoOp()
    {
        var fake = new FakeLocationControlChannel();
        var vm = Vm(fake);

        await vm.AddLocationAsync(@"C:\Users\alice\Docs", "  ", "ref-x");

        Assert.Empty(fake.Calls);
        Assert.Empty(vm.Locations);
    }

    /// <summary>A row that yields no <c>FolderRef</c> is refused on the error line — fail
    /// closed — and nothing is bound or rendered: the binding is keyed by the ref alone,
    /// and the name-keyed bind it used to fall back to is retired.</summary>
    [Fact]
    public async Task AddLocation_WithNoFolderRef_IsRefused()
    {
        var fake = new FakeLocationControlChannel();
        var vm = Vm(fake);

        await vm.AddLocationAsync(@"C:\Users\alice\Docs", "documents", null);

        Assert.Empty(fake.Calls);
        Assert.Empty(vm.Locations);
        Assert.NotNull(vm.ErrorMessage);
    }

    [Fact]
    public async Task SetMode_DispatchesSetLocationSyncMode_AndRereadsTheMode()
    {
        var fake = new FakeLocationControlChannel();
        fake.Seed(@"C:\Users\alice\Docs", "documents", "always");
        var vm = Vm(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.SetModeAsync(@"C:\Users\alice\Docs", "on-demand");

        Assert.Contains(@"mode:C:\Users\alice\Docs:on-demand", fake.Calls);
        Assert.True(Assert.Single(vm.Locations).IsOnDemand);
    }

    /// <summary>Why the VM removes by PATH, not by set: two folders may be bound to the
    /// same folder, and the per-row <c>folder-location-remove-button</c> means one row.
    /// A set-keyed remove would silently unbind the sibling the user never touched.</summary>
    [Fact]
    public async Task Remove_TakesOnlyThatPath_LeavingASiblingOnTheSameSet()
    {
        var fake = new FakeLocationControlChannel();
        fake.Seed(@"C:\Users\alice\Docs", "shared", "always");
        fake.Seed(@"C:\Users\alice\More", "shared", "always");
        var vm = Vm(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.RemoveLocationAsync(@"C:\Users\alice\Docs");

        Assert.Contains(@"unbind:C:\Users\alice\Docs", fake.Calls);
        Assert.DoesNotContain(@"unbind:C:\Users\alice\More", fake.Calls);
        Assert.Equal(@"C:\Users\alice\More", Assert.Single(vm.Locations).Path);
    }

    /// <summary>(i) The race face: on the first post-upgrade launch the agent is not up when
    /// the VM attaches, so the at-attach reconcile finds it unreachable. The binding must
    /// still be pushed once the agent comes up — which is what
    /// <see cref="LocationsViewModel.OnAgentReachableAsync"/> exists for. Before the A4
    /// fix, reconcile fired only at attach and the row sat unpushed forever.</summary>
    [Fact]
    public async Task AgentDownAtAttach_ThenReachableEdge_PushesThePendingBind()
    {
        var fake = new FakeLocationControlChannel { Reachable = false };
        var vm = Vm(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.AddLocationAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");
        Assert.Single(vm.Locations);            // rendered optimistically...
        Assert.Empty(fake.Bound);             // ...but nothing reached the agent

        fake.Reachable = true;
        await vm.OnAgentReachableAsync();

        Assert.Equal(@"C:\Users\alice\Docs", Assert.Single(fake.Bound).Path);
        Assert.Single(vm.Locations);
    }

    /// <summary>(ii) Face (b) of the A4 union fix: a row whose bind is REJECTED (the agent is
    /// up and answers "no") must stay rendered and re-push on the next reconcile — never
    /// vanish because the agent's shorter list was adopted as truth.</summary>
    [Fact]
    public async Task RejectedBind_StaysRenderedAndRepushes()
    {
        var fake = new FakeLocationControlChannel();
        fake.RejectFolders.Add("documents");
        var vm = Vm(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.AddLocationAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");

        Assert.Single(vm.Locations);                 // survived the failed push
        Assert.NotNull(vm.ErrorMessage);           // and said so
        var attempts = fake.Calls.Count(c => c.StartsWith("bind:"));

        await vm.OnAgentReachableAsync();          // next reconcile re-pushes it
        Assert.True(fake.Calls.Count(c => c.StartsWith("bind:")) > attempts);
        Assert.Single(vm.Locations);
    }

    /// <summary>(iii) Face (a): a binding made while the agent is down lives only in the
    /// model, so an intervening reconcile against the agent's (empty) truth must not drop
    /// it.</summary>
    [Fact]
    public async Task BindingMadeWhileAgentDown_SurvivesAReconcileAgainstAnEmptyAgent()
    {
        var fake = new FakeLocationControlChannel { Reachable = false };
        var vm = Vm(fake);
        await vm.AddLocationAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");

        fake.Reachable = true;                     // agent up, but knows nothing
        fake.FailBinds = true;                     // and still won't take the push
        await vm.OnAgentReachableAsync();

        Assert.Single(vm.Locations);
    }

    /// <summary>(iv) The agent is the authority on a park: <c>accessRevoked</c> mirrors onto
    /// the row in BOTH directions, so a re-bind that cleared it agent-side clears the
    /// rendering too (file-sync.md § Multi-writer shared sets — D4).</summary>
    [Fact]
    public async Task AccessRevoked_MirrorsFromTheAgentBothWays()
    {
        var fake = new FakeLocationControlChannel();
        fake.Seed(@"C:\Users\alice\Docs", "documents", "always");
        fake.RevokedFolders.Add("documents");
        var vm = Vm(fake);

        await vm.LoadCommand.ExecuteAsync(null);
        Assert.True(Assert.Single(vm.Locations).AccessRevoked);

        fake.RevokedFolders.Clear();
        await vm.OnAgentReachableAsync();
        Assert.False(Assert.Single(vm.Locations).AccessRevoked);
    }

    /// <summary>A folder bound from another control surface (fauna-tui, another session) is
    /// adopted as confirmed rather than fought over.</summary>
    [Fact]
    public async Task AgentRowFromAnotherSurface_IsAdopted()
    {
        var fake = new FakeLocationControlChannel();
        var vm = Vm(fake);
        await vm.LoadCommand.ExecuteAsync(null);
        Assert.Empty(vm.Locations);

        fake.Seed(@"C:\Users\alice\FromTui", "tui-set", "always");
        await vm.OnAgentReachableAsync();

        Assert.Equal(@"C:\Users\alice\FromTui", Assert.Single(vm.Locations).Path);
        Assert.DoesNotContain(fake.Calls, c => c.StartsWith("bind:"));  // adopted, not re-pushed
    }

    /// <summary>An agent that simply is not running is the NORMAL case — on-demand sync is
    /// opt-in — so the at-attach reconcile must not raise an error banner. Only a user
    /// gesture that fails does.</summary>
    [Fact]
    public async Task UnreachableAtAttach_IsNotAnError()
    {
        var fake = new FakeLocationControlChannel { Reachable = false };
        var vm = Vm(fake);

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Null(vm.ErrorMessage);
        Assert.Empty(vm.Locations);
    }

    [Fact]
    public async Task AddLocation_HelperUnreachable_SurfacesError()
    {
        var fake = new FakeLocationControlChannel { Reachable = false };
        var vm = Vm(fake);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.AddLocationAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");

        Assert.NotNull(vm.ErrorMessage);   // unreachable helper → user-visible, not silent
        Assert.Single(vm.Locations);         // but the optimistic row still renders
    }
}

/// <summary>
/// In-memory <see cref="ILocationControlChannel"/> for the VM unit tests — the C# peer of
/// macOS's <c>FakeLocationControlChannel</c>, modelling the agent's bound-folder table
/// (path → folder + mode) and recording every dispatched call in order.
///
/// <para>Unlike the best-effort <c>Try*</c> pipe fake it replaces, these <b>throw</b> when
/// the agent will not take a call — the seam's real contract, and the one the optimistic
/// model needs in order to leave a failed push pending.</para>
/// </summary>
internal sealed class FakeLocationControlChannel : ILocationControlChannel
{
    internal sealed record Location(string Path, string Folder, string FolderId, string Mode);

    /// <summary>The ref a seeded row (and a test's bind) carries by convention: one set
    /// per name. A binding is keyed by its <c>FolderRef</c> since the name-keyed bind was
    /// retired, so every row needs one.</summary>
    public static string RefFor(string folder) => $"ref-{folder}";

    private readonly List<Location> _locations = new();

    /// <summary>Every call the VM made, in order (<c>bind:path:set:ref</c>,
    /// <c>unbind:path</c>, <c>mode:path:mode</c>, <c>list</c>) — lets a test assert both
    /// what was pushed and what was deliberately NOT re-pushed.</summary>
    public List<string> Calls { get; } = new();

    /// <summary>When false the agent is "not running": every call throws, which is what the
    /// real seam does on an unreachable socket.</summary>
    public bool Reachable { get; set; } = true;

    /// <summary>Folders whose bind is rejected even while reachable (the agent answers
    /// "no") — face (ii)'s rejected push.</summary>
    public HashSet<string> RejectFolders { get; } = new();

    /// <summary>Reject every bind regardless of set.</summary>
    public bool FailBinds { get; set; }

    /// <summary>Folders the agent reports as access-revoked (parked) — face (iv).</summary>
    public HashSet<string> RevokedFolders { get; } = new();

    /// <summary>The bindings the agent actually holds — what a test asserts really landed,
    /// as opposed to what the VM merely rendered.</summary>
    public IReadOnlyList<Location> Bound => _locations;

    public void Seed(string path, string folder, string mode) =>
        _locations.Add(new Location(path, folder, RefFor(folder), mode));

    public Task BindLocationAsync(string path, string folder, string folderId)
    {
        Calls.Add($"bind:{path}:{folder}:{folderId}");
        if (!Reachable) throw new InvalidOperationException("agent unreachable");
        if (FailBinds || RejectFolders.Contains(folder))
            throw new InvalidOperationException("bind rejected");

        _locations.RemoveAll(f => f.Path == path);
        // The agent's windows fresh-binding default (user ruling 2026-09-26).
        _locations.Add(new Location(path, folder, folderId, InMemoryLocationControlChannel.FreshBindingMode));
        return Task.CompletedTask;
    }

    public Task UnbindLocationAsync(string path)
    {
        Calls.Add($"unbind:{path}");
        if (!Reachable) throw new InvalidOperationException("agent unreachable");
        _locations.RemoveAll(f => f.Path == path);
        return Task.CompletedTask;
    }

    public Task SetLocationSyncModeAsync(string path, string mode)
    {
        Calls.Add($"mode:{path}:{mode}");
        if (!Reachable) throw new InvalidOperationException("agent unreachable");
        var i = _locations.FindIndex(f => f.Path == path);
        if (i >= 0) _locations[i] = _locations[i] with { Mode = mode };
        return Task.CompletedTask;
    }

    /// Widening <see cref="FfiAgentLocation"/> touches exactly this one factory instead of every hand-rolled positional call site.
    private static FfiAgentLocation AgentLocation(
        string path, string folder, string folderId, string mode, bool accessRevoked) =>
        new(path, folder, folderId, mode, accessRevoked);

    public Task<IReadOnlyList<FfiAgentLocation>> ListLocationsAsync()
    {
        Calls.Add("list");
        if (!Reachable) throw new InvalidOperationException("agent unreachable");
        IReadOnlyList<FfiAgentLocation> list = _locations
            .Select(f => AgentLocation(
                f.Path, f.Folder, f.FolderId, f.Mode, RevokedFolders.Contains(f.Folder)))
            .ToList();
        return Task.FromResult(list);
    }

    /// <summary>What the agent actually re-derives as still held for a folder at apply
    /// time — deliberately NOT what the model last rendered, so a test can assert the
    /// caller sends no count of its own (the SET-not-count contract).</summary>
    public ulong RemainingHeldAfterApply { get; set; }

    /// Widening <see cref="FfiHeldDeletesApplied"/> touches exactly this one factory instead of every hand-rolled positional call site.
    private static FfiHeldDeletesApplied HeldDeletesApplied(ulong applied, ulong remainingHeld, bool stillEngaged) =>
        new(applied, remainingHeld, stillEngaged);

    public Task<FfiHeldDeletesApplied> ApplyHeldDeletesAsync(string folder)
    {
        Calls.Add($"apply-held-deletes:{folder}");
        if (!Reachable) throw new InvalidOperationException("agent unreachable");
        var applied = RemainingHeldAfterApply == 0 ? 1ul : 0ul;
        return Task.FromResult(HeldDeletesApplied(applied, RemainingHeldAfterApply, RemainingHeldAfterApply > 0));
    }
}
