using System;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The login-scoped folder-binding control plane (<see cref="LocationBindingsController"/>),
/// over the same <see cref="FakeLocationControlChannel"/> the VM tests use.
///
/// <para><b>What these pin that <c>LocationsViewModelTests</c> cannot.</b> Those tests
/// always hand the VM a channel at construction, so they can only ever exercise a control
/// plane that already has an agent. The bug this class exists for is the opposite case: on
/// windows the agent session installs ~10 s AFTER login (two nest round-trips inside
/// <c>SyncAgentSession.CreateAsync</c>, each timing out at ~5 s when the nest connection is
/// still down), and every binding surface used to be gated on that session. A folder bound
/// in that window hit a null session, took a refusal path, and was <b>dropped</b> — one-shot,
/// never retried. A real user who bound a folder just after signing in silently lost it.</para>
///
/// <para>Note what is deliberately absent below: any wall-clock wait. The faces are
/// "recorded before a channel existed" and "pushed on the attach edge", both of which are
/// ordering, not timing — so a slower nest widens no window and these give the same verdict
/// on a loaded machine (testing.md § conventions point 14).</para>
/// </summary>
public class LocationBindingsControllerTests
{
    /// Widening <see cref="FfiEngineHold"/> touches exactly this one factory instead of every hand-rolled positional call site.
    private static FfiEngineHold EngineHold(string folder, ulong deletesHeld, ulong deletesSkippedUnreadable = 0) =>
        new(folder, FakeLocationControlChannel.RefFor(folder), deletesHeld, deletesSkippedUnreadable);


    /// <summary>THE regression face. Bind with no agent at all, then let the session install:
    /// the row must be rendered the whole time and pushed on attach. Before the control plane
    /// was split off the session, there was nothing to record it into.</summary>
    [Fact]
    public async Task BindBeforeTheSessionInstalls_IsRenderedImmediately_AndPushedOnAttach()
    {
        var controller = new LocationBindingsController();

        // t≈1s: the user binds a folder. No session, no channel — the agent does not exist.
        await controller.AddAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");

        var row = Assert.Single(controller.Rendered());
        Assert.Equal(@"C:\Users\alice\Docs", row.@path);
        Assert.Equal("documents", row.@folder);

        // t≈10s: SyncAgentSession.CreateAsync finally returns and its channel is installed.
        var fake = new FakeLocationControlChannel();
        controller.AttachChannel(fake);
        await controller.ReconcileAsync();

        var bound = Assert.Single(fake.Bound);
        Assert.Equal(@"C:\Users\alice\Docs", bound.Path);
        Assert.Equal("documents", bound.Folder);
        Assert.Single(controller.Rendered());
    }

    /// <summary>The attach itself drives the reconcile — a caller that only attaches (as
    /// <c>App.StartHydrationSession</c> does) must not have to remember to push.</summary>
    [Fact]
    public async Task AttachChannel_DrivesTheReconcileByItself()
    {
        var controller = new LocationBindingsController();
        await controller.AddAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");

        var fake = new FakeLocationControlChannel();
        controller.AttachChannel(fake);

        // AttachChannel is fire-and-forget by contract (the session-install path must not
        // block on the agent), so settle it the same way the production caller does — by
        // driving one more reconcile, which serializes behind the first on the same gate.
        await controller.ReconcileAsync();

        Assert.Equal(@"C:\Users\alice\Docs", Assert.Single(fake.Bound).Path);
    }

    /// <summary>The reachable edge is the controller's, not a page's: with no renderer
    /// subscribed at all, a pending bind still lands. This is the half that made the A4
    /// fix ineffective on windows — the edge was routed
    /// to <c>FoldersPage</c>, whose notifier is by contract a no-op while the page is
    /// closed, which is nearly always.</summary>
    [Fact]
    public async Task ReachableEdge_PushesWithNoRendererAttached()
    {
        var fake = new FakeLocationControlChannel { Reachable = false };
        var controller = new LocationBindingsController();
        controller.SetChannel(fake);

        await controller.AddAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");
        Assert.Empty(fake.Bound);              // agent down: nothing landed
        Assert.Single(controller.Rendered());  // but the row survives

        fake.Reachable = true;
        await controller.ReconcileAsync();     // the agent-reachable edge

        Assert.Equal(@"C:\Users\alice\Docs", Assert.Single(fake.Bound).Path);
    }

    /// <summary>A sign-out drops the channel but not the rows, and the throwing fallback
    /// keeps them <c>PendingBind</c> rather than confirming a push that reached nothing.</summary>
    [Fact]
    public async Task DetachChannel_KeepsTheRowsPendingRatherThanConfirmingThem()
    {
        var fake = new FakeLocationControlChannel();
        var controller = new LocationBindingsController();
        controller.SetChannel(fake);
        await controller.AddAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");
        Assert.Single(fake.Bound);

        controller.DetachChannel();
        var pushFailed = await controller.ReconcileAsync();

        Assert.True(pushFailed);                       // unreachable, as designed
        Assert.Single(controller.Rendered());          // the user's row is still the user's

        // Re-attach a FRESH agent that knows nothing (an agent config reset): the union
        // semantics re-push rather than adopting the empty list as truth.
        var replacement = new FakeLocationControlChannel();
        controller.SetChannel(replacement);
        await controller.ReconcileAsync();

        Assert.Equal(@"C:\Users\alice\Docs", Assert.Single(replacement.Bound).Path);
    }

    /// <summary>Removal by path and by set both report what they actually removed, so the e2e
    /// command can refuse loudly on "no such binding" instead of reporting a silent success
    /// (testing.md § conventions point 11).</summary>
    [Fact]
    public async Task Remove_ReportsWhatItRemoved_ForBothKeys()
    {
        var fake = new FakeLocationControlChannel();
        var controller = new LocationBindingsController();
        controller.SetChannel(fake);
        await controller.AddAsync(@"C:\Users\alice\Docs", "shared", "ref-shared");
        await controller.AddAsync(@"C:\Users\alice\More", "shared", "ref-shared");

        var (byPath, _) = await controller.RemoveByPathAsync(@"C:\Users\alice\Docs");
        Assert.Equal(@"C:\Users\alice\Docs", Assert.Single(byPath));
        Assert.Equal(@"C:\Users\alice\More", Assert.Single(controller.Rendered()).@path);

        var (bySet, _) = await controller.RemoveBySetAsync("shared");
        Assert.Equal(@"C:\Users\alice\More", Assert.Single(bySet));
        Assert.Empty(controller.Rendered());

        var (none, _) = await controller.RemoveBySetAsync("never-bound");
        Assert.Empty(none);
    }

    /// <summary>Mode is agent truth, so an unseen row renders the windows fresh-binding
    /// default — on-demand, what the agent's <c>AddLocation</c> gives a new path (user
    /// ruling 2026-09-26) — rather than inventing one, and the agent's report moves it.</summary>
    [Fact]
    public async Task ModeFor_DefaultsToOnDemand_UntilTheAgentReportsOtherwise()
    {
        var fake = new FakeLocationControlChannel();
        var controller = new LocationBindingsController();
        controller.SetChannel(fake);

        await controller.AddAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");
        Assert.Equal("on-demand", controller.ModeFor(@"C:\Users\alice\Docs"));
        Assert.Equal(InMemoryLocationControlChannel.FreshBindingMode, controller.ModeFor(@"C:\Users\alice\Docs"));

        await controller.SetModeAsync(@"C:\Users\alice\Docs", "always");
        Assert.Equal("always", controller.ModeFor(@"C:\Users\alice\Docs"));
        Assert.Contains(@"mode:C:\Users\alice\Docs:always", fake.Calls);
    }

    /// <summary>The e2e in-memory channel is the agent's stand-in, so a fresh bind and
    /// a seeded row lacking <c>mode</c> both start on-demand, exactly as the agent's
    /// windows <c>AddLocation</c> does — the half of the default the windows e2e witness
    /// (<c>test_folder_location_mode_toggle.py</c>) then reads through the page.</summary>
    [Fact]
    public async Task InMemoryChannel_StartsAFreshBindingOnDemand()
    {
        var fake = new InMemoryLocationControlChannel();
        await fake.BindLocationAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");
        fake.Seed(@"C:\Users\alice\More", "more", mode: null);
        fake.Seed(@"C:\Users\alice\Pinned", "pinned", "always");

        var rows = (await fake.ListLocationsAsync()).ToDictionary(r => r.@path, r => r.@mode);
        Assert.Equal("on-demand", rows[@"C:\Users\alice\Docs"]);
        Assert.Equal("on-demand", rows[@"C:\Users\alice\More"]);
        Assert.Equal("always", rows[@"C:\Users\alice\Pinned"]);
    }

    /// <summary>A blank path or set is nothing to bind — the set is contextual on the
    /// Folders page and the shared model holds only bindings, so there is no "add unbound".</summary>
    [Theory]
    [InlineData(null, "documents")]
    [InlineData("", "documents")]
    [InlineData("   ", "documents")]
    [InlineData(@"C:\Users\alice\Docs", "  ")]
    public async Task Add_BlankPathOrSet_TouchesNothing(string? path, string? folder)
    {
        var fake = new FakeLocationControlChannel();
        var controller = new LocationBindingsController();
        controller.SetChannel(fake);

        await controller.AddAsync(path, folder, FakeLocationControlChannel.RefFor(folder));

        Assert.Empty(fake.Calls);
        Assert.Empty(controller.Rendered());
    }

    // ── The park watch (file-sync.md § Multi-writer shared sets → *Revocation*): the agent
    // derives the park itself, so no gesture or reachability edge re-drives a reconcile when
    // it changes — only the status-poll tick's PollParksAsync carries it to the row. ──

    /// <summary>THE regression face: the park lands agent-side after the last reconcile,
    /// and nothing but the poll tick brings it to the row. Without the watch the demoted
    /// writer's row never learns its binding is parked, so it never shows the warning.</summary>
    [Fact]
    public async Task PollParks_MirrorsAParkThatLandedAfterTheLastReconcile_AndSignals()
    {
        var fake = new FakeLocationControlChannel();
        var controller = new LocationBindingsController();
        controller.SetChannel(fake);
        await controller.AddAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");
        Assert.False(Assert.Single(controller.Rendered()).@accessRevoked);
        var signalCount = 0;
        controller.Changed += () => signalCount++;

        fake.RevokedFolders.Add("documents");
        await controller.PollParksAsync();

        Assert.True(Assert.Single(controller.Rendered()).@accessRevoked);
        Assert.Equal(1, signalCount);

        // A re-bind cleared it agent-side: the watch mirrors that direction too.
        fake.RevokedFolders.Clear();
        await controller.PollParksAsync();
        Assert.False(Assert.Single(controller.Rendered()).@accessRevoked);
    }

    /// <summary>A failed read is skipped, never folded as an empty list: a transient
    /// agent hiccup must not clear a park the user is being told about.</summary>
    [Fact]
    public async Task PollParks_AnUnreachableAgent_KeepsTheLastKnownPark()
    {
        var fake = new FakeLocationControlChannel();
        var controller = new LocationBindingsController();
        controller.SetChannel(fake);
        await controller.AddAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");
        fake.RevokedFolders.Add("documents");
        await controller.PollParksAsync();
        Assert.True(Assert.Single(controller.Rendered()).@accessRevoked);

        fake.Reachable = false;
        await controller.PollParksAsync();

        Assert.True(Assert.Single(controller.Rendered()).@accessRevoked);
    }

    /// <summary>The watch pushes nothing: it mirrors one flag, it is not a reconcile.</summary>
    [Fact]
    public async Task PollParks_PushesNothing()
    {
        var fake = new FakeLocationControlChannel();
        var controller = new LocationBindingsController();
        controller.SetChannel(fake);
        await controller.AddAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");
        fake.Calls.Clear();

        await controller.PollParksAsync();

        Assert.Equal(new[] { "list" }, fake.Calls);
    }

    // ── The mass-delete floor's confirm affordance (delete-propagation.md § A
    // wholesale-vanished folder is infrastructure failure) — folded onto the model from the app's ~10s status-poll tick, never a
    // reconcile pass. ──

    [Fact]
    public async Task FoldEngineHolds_SetsDeletesHeldOnTheMatchingRenderedRow()
    {
        var controller = new LocationBindingsController();
        await controller.AddAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");
        var fake = new FakeLocationControlChannel();
        controller.AttachChannel(fake);
        await controller.ReconcileAsync();
        Assert.Equal(0ul, Assert.Single(controller.Rendered()).@deletesHeld);

        controller.FoldEngineHolds(new[] { EngineHold("documents", 3) });

        Assert.Equal(3ul, Assert.Single(controller.Rendered()).@deletesHeld);
    }

    [Fact]
    public void FoldEngineHolds_SignalsChanged_OnlyWhenTheHeldCountActuallyDiffers()
    {
        var controller = new LocationBindingsController();
        var fake = new FakeLocationControlChannel();
        controller.AttachChannel(fake);
        var signalCount = 0;
        controller.Changed += () => signalCount++;

        controller.FoldEngineHolds(new[] { EngineHold("documents", 3) });
        // No matching row yet (nothing bound to "documents") — the fold changes
        // nothing rendered, so this must not signal.
        Assert.Equal(0, signalCount);
    }

    /// <summary>Twin of <see cref="FoldEngineHolds_SetsDeletesHeldOnTheMatchingRenderedRow"/>
    /// for the unreadable-path count (delete-propagation.md § Unreadable is not absent):
    /// the FFI's `deletes_skipped_unreadable` reaches this app's rendered row through the
    /// same fold as the hold.</summary>
    [Fact]
    public async Task FoldEngineHolds_SetsDeletesSkippedUnreadableOnTheMatchingRenderedRow()
    {
        var controller = new LocationBindingsController();
        await controller.AddAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");
        var fake = new FakeLocationControlChannel();
        controller.AttachChannel(fake);
        await controller.ReconcileAsync();
        Assert.Equal(0ul, Assert.Single(controller.Rendered()).@deletesSkippedUnreadable);

        controller.FoldEngineHolds(new[] { EngineHold("documents", 0, 8) });

        Assert.Equal(8ul, Assert.Single(controller.Rendered()).@deletesSkippedUnreadable);
    }

    /// <summary>Regression face for the rendered-union signature: a row's unreadable count
    /// can change while its held count stays put (they are independent conditions), so the
    /// signature that gates <see cref="LocationBindingsController.Changed"/> must include
    /// both fields — a signature keyed on <c>deletesHeld</c> alone would silently drop this
    /// edge and leave the app never painting `folder-location-unreadable`.</summary>
    [Fact]
    public async Task FoldEngineHolds_SignalsChanged_WhenOnlyTheUnreadableCountDiffers()
    {
        var controller = new LocationBindingsController();
        await controller.AddAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");
        var fake = new FakeLocationControlChannel();
        controller.AttachChannel(fake);
        await controller.ReconcileAsync();
        var signalCount = 0;
        controller.Changed += () => signalCount++;

        controller.FoldEngineHolds(new[] { EngineHold("documents", 0, 8) });

        Assert.Equal(1, signalCount);
    }

    /// <summary>THE regression face for the "send the SET, never the rendered count"
    /// rule: the reply's own `remainingHeld` — not anything the caller passed in — is
    /// what lands on the model. A leg that instead sent its rendered number would turn
    /// a safety property into a race.</summary>
    [Fact]
    public async Task ApplyHeldDeletes_SetsEngineHoldFromTheReplysRemainingHeld_NeverACallerCount()
    {
        var controller = new LocationBindingsController();
        await controller.AddAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");
        var fake = new FakeLocationControlChannel { RemainingHeldAfterApply = 1 };
        controller.AttachChannel(fake);
        await controller.ReconcileAsync();
        controller.FoldEngineHolds(new[] { EngineHold("documents", 5) });
        Assert.Equal(5ul, Assert.Single(controller.Rendered()).@deletesHeld);

        var reply = await controller.ApplyHeldDeletesAsync("documents");

        Assert.Equal(1ul, reply.@remainingHeld);
        Assert.Contains("apply-held-deletes:documents", fake.Calls);
        // The rendered row now reflects the AGENT's re-derived remainder (1), never
        // the 5 this test folded earlier or any other caller-supplied number.
        Assert.Equal(1ul, Assert.Single(controller.Rendered()).@deletesHeld);
    }

    /// <summary>0 retracts the affordance entirely — the only thing that ever does,
    /// since the hold is derived per reconcile pass and never stored.</summary>
    [Fact]
    public async Task ApplyHeldDeletes_ZeroRemaining_ClearsTheHeldCount()
    {
        var controller = new LocationBindingsController();
        await controller.AddAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");
        var fake = new FakeLocationControlChannel { RemainingHeldAfterApply = 0 };
        controller.AttachChannel(fake);
        await controller.ReconcileAsync();
        controller.FoldEngineHolds(new[] { EngineHold("documents", 5) });

        await controller.ApplyHeldDeletesAsync("documents");

        Assert.Equal(0ul, Assert.Single(controller.Rendered()).@deletesHeld);
    }

    [Fact]
    public async Task ApplyHeldDeletes_Unreachable_Throws()
    {
        var controller = new LocationBindingsController();
        var fake = new FakeLocationControlChannel { Reachable = false };
        controller.AttachChannel(fake);

        await Assert.ThrowsAsync<InvalidOperationException>(
            () => controller.ApplyHeldDeletesAsync("documents"));
    }

    /// <summary>A bind is just the recorded row plus the bind push — no content-key push
    /// precedes it: the agent resolves the newly-bound set's keys from the account's custody
    /// itself (on-demand-files.md § Shared sets on a capability host → <i>One
    /// mechanism</i>).</summary>
    [Fact]
    public async Task AddAsync_RecordsTheRowAndPushesTheBind_WithNoKeyPushFirst()
    {
        var fake = new FakeLocationControlChannel();
        var controller = new LocationBindingsController();
        controller.SetChannel(fake);

        await controller.AddAsync(@"C:\Users\alice\Docs", "documents", "ref-documents");

        Assert.Equal(new[] { "list", @"bind:C:\Users\alice\Docs:documents:ref-documents" }, fake.Calls);
        Assert.Equal(@"C:\Users\alice\Docs", Assert.Single(fake.Bound).Path);
    }
}
