using System;
using System.Collections.Generic;
using System.Linq;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The pure half of windows' launch-collision surface
/// (<c>account-scoping.md</c> § Concurrent instances → "the colliding instance's
/// surface"). The Win32 mutex, the named events and the WinUI page are the
/// presentation layer's; this pins the policy they consult — the C# twin of the
/// linux tests around <c>account_scope::choosable_accounts</c>.
/// </summary>
public class LaunchCollisionGateTests
{
    private const string ActorA = "aa11";
    private const string ActorB = "bb22";
    private const string ActorC = "cc33";

    private static string Label(string? handle, string actorId) => handle ?? $"#{actorId}";

    /// <summary>The plain, ordinary case: nobody serves the would-be account.</summary>
    [Fact]
    public void NoLiveInstance_DoesNotCollide()
    {
        Assert.False(LaunchCollisionGate.CollidesWithALiveInstance(
            launchBinding: null, activeActorId: ActorA, isServed: _ => false));
    }

    [Fact]
    public void AServedActiveAccount_Collides()
    {
        Assert.True(LaunchCollisionGate.CollidesWithALiveInstance(
            launchBinding: null, activeActorId: ActorA, isServed: id => id == ActorA));
    }

    /// <summary>
    /// Claim 2 of the goal doc, and the reason the binding is tested FIRST: a wired
    /// launch that collides is terminally refused, never offered a human chooser.
    /// The probe must not even run for it.
    /// </summary>
    [Fact]
    public void ABoundLaunch_NeverCollides_AndNeverProbes()
    {
        var probed = false;
        var collided = LaunchCollisionGate.CollidesWithALiveInstance(
            launchBinding: ActorB,
            activeActorId: ActorA,
            isServed: _ => { probed = true; return true; });

        Assert.False(collided, "a bound launch is refused at acquire, never offered the chooser");
        Assert.False(probed, "the binding must short-circuit before the probe runs");
    }

    /// <summary>
    /// A fresh install (or an index not yet materialized) has nothing to collide
    /// with and nothing to offer — the ordinary routing applies.
    /// </summary>
    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("   ")]
    public void NoResolvableActiveAccount_DoesNotCollide(string? active)
    {
        Assert.False(LaunchCollisionGate.CollidesWithALiveInstance(
            launchBinding: null, activeActorId: active, isServed: _ => true));
    }

    /// <summary>
    /// Degrade toward the ordinary launch, never toward a chooser: without a probe
    /// the worst case is the terminal refusal at acquire, which is a real answer —
    /// a chooser built on no evidence is not.
    /// </summary>
    [Fact]
    public void AMissingProbe_DoesNotCollide()
    {
        Assert.False(LaunchCollisionGate.CollidesWithALiveInstance(
            launchBinding: null, activeActorId: ActorA, isServed: null!));
    }

    /// <summary>
    /// The served account drops out **because it is served**, not because of an
    /// "exclude the active one" rule — which is what also drops an account a BOUND
    /// sibling holds.
    /// </summary>
    [Fact]
    public void ChoosableAccounts_ExcludeTheServedOnes()
    {
        var entries = new (string, string?)[]
        {
            (ActorA, "ana"),
            (ActorB, "bo"),
            (ActorC, null),
        };

        var offered = LaunchCollisionGate.ChoosableAccounts(
            entries,
            ids => ids.Where(id => id != ActorA).ToArray(),
            Label);

        Assert.Equal(new[] { ActorB, ActorC }, offered.Select(o => o.ActorId));
        Assert.Equal("bo", offered[0].DisplayLabel);
        Assert.Equal(Label(null, ActorC), offered[1].DisplayLabel);
    }

    /// <summary>
    /// Every account already open somewhere → an empty list, which the page renders
    /// as its "all open" explanation. Not an error: the other two exits still work.
    /// </summary>
    [Fact]
    public void ChoosableAccounts_CanBeEmpty()
    {
        var offered = LaunchCollisionGate.ChoosableAccounts(
            new (string, string?)[] { (ActorA, null), (ActorB, null) },
            _ => Array.Empty<string>(),
            Label);

        Assert.Empty(offered);
    }

    /// <summary>
    /// Registry order is the switcher's order; the chooser must not re-sort, or the
    /// two surfaces would disagree about which account is "the second one".
    /// </summary>
    [Fact]
    public void ChoosableAccounts_PreserveRegistryOrder()
    {
        var entries = new (string, string?)[] { (ActorC, "cee"), (ActorA, "ana"), (ActorB, "bo") };

        var offered = LaunchCollisionGate.ChoosableAccounts(
            entries,
            ids => ids.ToArray(),      // nothing served: every row is offerable
            Label);

        Assert.Equal(new[] { ActorC, ActorA, ActorB }, offered.Select(o => o.ActorId));
    }

    /// <summary>
    /// The probe answers "not served" on every failure so a hiccup NARROWS the list
    /// rather than stranding the user — and a probe that returns nothing at all
    /// (the no-resolvable-base case) must yield no rows, never every row.
    /// </summary>
    [Fact]
    public void AnEmptyProbeResult_OffersNothing_NotEverything()
    {
        var offered = LaunchCollisionGate.ChoosableAccounts(
            new (string, string?)[] { (ActorA, "ana"), (ActorB, "bo") },
            _ => null!,
            Label);

        Assert.Empty(offered);
    }

    [Fact]
    public void ChoosableAccounts_WithNoRegistryRows_IsEmpty()
    {
        Assert.Empty(LaunchCollisionGate.ChoosableAccounts(
            Array.Empty<(string, string?)>(), _ => new[] { ActorA }, Label));
    }

    /// <summary>
    /// Actor ids cross the FFI seam as hex and are compared case-insensitively
    /// everywhere else in this app (see <c>SessionInstance.IsServedUnder</c>); the
    /// filter must not silently drop a row over letter case.
    /// </summary>
    [Fact]
    public void ChoosableAccounts_MatchActorIdsCaseInsensitively()
    {
        var offered = LaunchCollisionGate.ChoosableAccounts(
            new (string, string?)[] { ("AA11", "ana") },
            _ => new[] { "aa11" },
            Label);

        Assert.Single(offered);
        Assert.Equal("AA11", offered[0].ActorId);
    }

    // --- focus-existing: the per-(OS login, account) raise channel ----------
    //
    // account-scoping.md § Concurrent instances → "The per-(OS login, account)
    // raise channel" (ratified 2026-07-23): raise best-effort, then degrade
    // honestly — endpoint unowned ⇒ re-probe the LOCK; no longer served ⇒
    // continue as a plain launch; still served ⇒ error-message. The decision is
    // shared Rust (resolve_focus_existing) reached over the real native FFI, so
    // these are conformance tests of the C# seat adapter against it.

    /// <summary>The channel answered — the caller hands the user off and exits.</summary>
    [Fact]
    public void FocusExisting_WhenTheEndpointAnswers_Raises()
    {
        var probed = false;
        var outcome = LaunchCollisionGate.ResolveFocusExisting(
            ActorA,
            tryRaise: id => id == ActorA,
            isServed: _ => { probed = true; return true; });

        Assert.Equal(FfiFocusExistingOutcome.Raised, outcome);
        Assert.False(probed, "a delivered raise needs no lock re-probe — the answer is already known");
    }

    /// <summary>
    /// The sibling died between the collision and the click: nobody owns the
    /// endpoint AND nobody holds the lock, so the honest answer is not an error —
    /// this process may simply go on and open the account itself.
    /// </summary>
    [Fact]
    public void FocusExisting_WhenTheEndpointIsUnownedAndTheLockIsFree_ContinuesAsAPlainLaunch()
    {
        Assert.Equal(
            FfiFocusExistingOutcome.NoLongerServed,
            LaunchCollisionGate.ResolveFocusExisting(
                ActorA, tryRaise: _ => false, isServed: _ => false));
    }

    /// <summary>
    /// The case this whole track exists for: the account IS still served, but its
    /// server owns no reachable endpoint (a tui server, or an instance from before
    /// this leg). Surface it — never exit into nothing, and never claim the
    /// account, which acquire would refuse a moment later anyway.
    /// </summary>
    [Fact]
    public void FocusExisting_WhenTheEndpointIsUnownedButTheAccountIsStillServed_SurfacesTheNoChannelCase()
    {
        Assert.Equal(
            FfiFocusExistingOutcome.StillServedNoChannel,
            LaunchCollisionGate.ResolveFocusExisting(
                ActorA, tryRaise: _ => false, isServed: id => id == ActorA));
    }

    /// <summary>
    /// The re-probe is what separates the two degrades, so it must actually run on
    /// the failed-raise path — and it must be asked about the account the user
    /// clicked for, not some ambient "active" one.
    /// </summary>
    [Fact]
    public void FocusExisting_ReProbesTheClickedAccount_OnAFailedRaise()
    {
        string? probedFor = null;
        LaunchCollisionGate.ResolveFocusExisting(
            ActorB,
            tryRaise: _ => false,
            isServed: id => { probedFor = id; return true; });

        Assert.Equal(ActorB, probedFor);
    }

    /// <summary>
    /// Degrade-open, third edition: a missing collaborator must not throw a
    /// NullReferenceException out of a click handler. No channel and no evidence
    /// the account is free ⇒ the conservative, non-claiming answer.
    /// </summary>
    [Fact]
    public void FocusExisting_WithNoCollaborators_SurfacesRatherThanThrows()
    {
        Assert.Equal(
            FfiFocusExistingOutcome.StillServedNoChannel,
            LaunchCollisionGate.ResolveFocusExisting(ActorA, tryRaise: null!, isServed: null!));
        Assert.Equal(
            FfiFocusExistingOutcome.StillServedNoChannel,
            LaunchCollisionGate.ResolveFocusExisting("  ", tryRaise: _ => true, isServed: _ => true));
    }

    /// <summary>
    /// A hook that throws must not unwind across the FFI callback: a throwing raise
    /// reads as "unowned" (so the lock is still re-probed), a throwing probe as
    /// "still served" — the non-claiming answer.
    /// </summary>
    [Fact]
    public void FocusExisting_WithThrowingHooks_DegradesTowardTheNonClaimingAnswer()
    {
        Assert.Equal(
            FfiFocusExistingOutcome.NoLongerServed,
            LaunchCollisionGate.ResolveFocusExisting(
                ActorA, tryRaise: _ => throw new InvalidOperationException(), isServed: _ => false));
        Assert.Equal(
            FfiFocusExistingOutcome.StillServedNoChannel,
            LaunchCollisionGate.ResolveFocusExisting(
                ActorA, tryRaise: _ => false, isServed: _ => throw new InvalidOperationException()));
    }
}
