using System;
using System.IO;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Windows' (OS login, account) single-instance guard
/// (<c>docs/goal/architecture/apps/account-scoping.md</c> § Concurrent
/// instances), over the real shared lock — no mocks, the same cross-language
/// conformance basis as <see cref="AccountStateDirTests"/>.
///
/// <para><b>Why the acquire path is ONE test.</b>
/// <see cref="SessionInstance.Become"/> and <see cref="SessionInstance.LaunchBinding"/>
/// read and write <b>process-global</b> Rust state (the held-lock holder and the
/// launch-binding cell) that outlives any single test. Sibling <c>[Fact]</c>s would
/// let a binding set by one race the holder read by another — green in isolation,
/// red in a full run. That is exactly the shape of the <c>MutedKeywordsCache</c>
/// race this suite hit on 2026-07-22, and the fix is the same: one owner for the
/// global, not retries. The probe-only tests below touch no global and stay
/// separate.</para>
///
/// <para>Every test points at a fresh temp base through the base-passed
/// <c>*Under</c> overloads, so nothing here can perturb the real
/// <c>%LocalAppData%\Fauna</c> — including the installed product's own lock files.</para>
/// </summary>
[Collection("SessionInstanceGlobal")]
public class SessionInstanceTests : IDisposable
{
    private static string RandomActor()
        => Convert.ToHexString(System.Security.Cryptography.RandomNumberGenerator.GetBytes(32))
            .ToLowerInvariant();

    private readonly string _base;

    /// <summary>
    /// The store container the holder declares itself under (the shared-root
    /// serving lock). Inside the temp base, so no test writes a lock file into the
    /// real per-user store root.
    /// </summary>
    private readonly string _store;

    public SessionInstanceTests()
    {
        _base = Path.Combine(Path.GetTempPath(), "fauna-instlock-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_base);
        _store = Path.Combine(_base, "store");
        Directory.CreateDirectory(_store);
    }

    public void Dispose()
    {
        try { Directory.Delete(_base, recursive: true); } catch { }
    }

    /// <summary>
    /// The whole acquire seam in order: a plain launch has no binding; a fresh
    /// acquire, then reuse; a cross-account switch swaps (releasing the outgoing
    /// account); a chooser pick binds the process; and bound-or-refuse then outranks
    /// a free lock. This is the contract every launch and every switch rides.
    /// </summary>
    [Fact]
    public void TheAcquireSeamCoversNoBinding_Acquire_Reuse_Swap_Pick_AndBoundOrRefuse()
    {
        var actorA = RandomActor();
        var actorB = RandomActor();

        // A plain launch carries no binding until the chooser sets one.
        Assert.Null(SessionInstance.LaunchBinding);

        var acquired = SessionInstance.BecomeUnder(_base, actorA, _store);
        Assert.Equal(SessionInstanceStatus.Acquired, acquired.Status);
        Assert.True(acquired.MayProceed);
        Assert.True(SessionInstance.IsServedUnder(_base, actorA));

        // A same-account session rebuild REUSES: re-acquiring would make this
        // process refuse itself.
        Assert.Equal(SessionInstanceStatus.Reused, SessionInstance.BecomeUnder(_base, actorA, _store).Status);

        // A cross-account switch swaps by replacement — the holder releases the
        // outgoing account's lock, so A is free for another instance afterwards.
        Assert.Equal(SessionInstanceStatus.Acquired, SessionInstance.BecomeUnder(_base, actorB, _store).Status);
        Assert.False(SessionInstance.IsServedUnder(_base, actorA));
        Assert.True(SessionInstance.IsServedUnder(_base, actorB));

        // The chooser's pick binds this process, normalized — so the binding, the
        // lock key and the bound-or-refuse compare cannot disagree on spelling.
        SessionInstance.BindLaunchTo("  " + actorB.ToUpperInvariant() + "  ");
        Assert.Equal(actorB, SessionInstance.LaunchBinding);

        // Bound-or-refuse now outranks everything, including a free lock: a bound
        // process never falls back onto the account the session resolved.
        var mismatch = SessionInstance.BecomeUnder(_base, actorA, _store);
        Assert.Equal(SessionInstanceStatus.RefusedBoundMismatch, mismatch.Status);
        Assert.False(mismatch.MayProceed);
        Assert.Contains(actorB, mismatch.Reason);

        // Reuse precedes the base check — a same-account rebuild never needs a
        // resolvable base.
        Assert.Equal(SessionInstanceStatus.Reused, SessionInstance.BecomeUnder(null, actorB, _store).Status);

        // An unresolvable state base DEGRADES OPEN, never refuses — a filesystem
        // hiccup must not become a client that cannot launch.
        var actorC = RandomActor();
        SessionInstance.BindLaunchTo(actorC);
        var degraded = SessionInstance.BecomeUnder(null, actorC, _store);
        Assert.Equal(SessionInstanceStatus.Degraded, degraded.Status);
        Assert.True(degraded.MayProceed);
    }

    /// <summary>
    /// A live holder is what the served probe reports. The holder's shared lock
    /// is taken directly over the FFI (a second handle conflicts with the
    /// exclusive probe even in-process), so this pins the probe without
    /// spawning a second app.
    /// </summary>
    [Fact]
    public void AnAccountAlreadyServedByAnotherHolderReadsAsServed()
    {
        var actor = RandomActor();
        Assert.False(SessionInstance.IsServedUnder(_base, actor));

        using var competitor = uniffi.fauna_ffi.FaunaFfiMethods.AcquireAccountInstanceLockShared(_base, actor, _store);
        Assert.NotNull(competitor);
        Assert.True(competitor!.IsHeld());

        Assert.True(SessionInstance.IsServedUnder(_base, actor));
        Assert.DoesNotContain(actor, SessionInstance.NotCurrentlyServedUnder(_base, new[] { actor }));
    }

    /// <summary>
    /// The chooser's list keeps registry order and drops only the served accounts,
    /// so the human sees the accounts they added in the order they added them.
    /// </summary>
    [Fact]
    public void NotCurrentlyServed_KeepsOrderAndDropsOnlyTheServedOnes()
    {
        var free1 = RandomActor();
        var served = RandomActor();
        var free2 = RandomActor();

        using var holder = uniffi.fauna_ffi.FaunaFfiMethods.AcquireAccountInstanceLockShared(_base, served, _store);
        Assert.NotNull(holder);

        Assert.Equal(
            new[] { free1, free2 },
            SessionInstance.NotCurrentlyServedUnder(_base, new[] { free1, served, free2 }));
    }

    /// <summary>
    /// An unresolvable base offers NOTHING rather than everything: with no lock
    /// files to probe there is no way to tell free from served, and offering a
    /// served account would hand the user a pick that can only fail. The chooser's
    /// other two exits still work, so nobody is stranded.
    /// </summary>
    [Fact]
    public void AnUnresolvableBaseOffersNothingAndReportsNothingServed()
    {
        var actor = RandomActor();
        Assert.Empty(SessionInstance.NotCurrentlyServedUnder(null, new[] { actor }));
        Assert.False(SessionInstance.IsServedUnder(null, actor));
    }

    [Theory]
    [InlineData(SessionInstanceStatus.Acquired, true)]
    [InlineData(SessionInstanceStatus.Reused, true)]
    [InlineData(SessionInstanceStatus.Degraded, true)]
    [InlineData(SessionInstanceStatus.RefusedAlreadyServed, false)]
    [InlineData(SessionInstanceStatus.RefusedBoundMismatch, false)]
    public void OnlyTheTwoRefusalsAreTerminal(SessionInstanceStatus status, bool mayProceed)
        => Assert.Equal(mayProceed, new SessionInstanceResult(status, string.Empty).MayProceed);

    /// <summary>A registry with no accounts of its own — the sign-out gate below asks
    /// about the on-disk scope this test creates directly, the same shape
    /// <c>sole_instance.rs</c>'s own "a malformed index still asks about what is on
    /// disk" test uses, so no secret backend plumbing is needed.</summary>
    private sealed class EmptyBackend : ISecretBackend
    {
        public string? Get(string resource, string user) => null;
        public void Set(string resource, string user, string value) { }
        public void Delete(string resource, string user) { }
    }

    private static uniffi.fauna_ffi.FfiAccountRegistry NewEmptyRegistry() =>
        new uniffi.fauna_ffi.FfiAccountRegistry(new LogicalSecretStore(new EmptyBackend()));

    /// <summary>
    /// windows' leg of <c>account-scoping.md</c> § Concurrent instances → <i>An
    /// erase refuses while a sibling serves the account</i> — the case a wrong probe breaks: signing out here must proceed
    /// exactly as it does today when nobody else is serving this account.
    /// <see cref="SessionInstance.BindLaunchTo"/> pins this process's own actor
    /// first so a binding left over by another test in this serialized collection
    /// cannot spuriously refuse the acquire below.
    /// </summary>
    [Fact]
    public void SignOutBlockedProceedsForALoneInstance()
    {
        var actor = RandomActor();
        Directory.CreateDirectory(Path.Combine(_base, actor));
        SessionInstance.BindLaunchTo(actor);
        Assert.Equal(SessionInstanceStatus.Acquired, SessionInstance.BecomeUnder(_base, actor, _store).Status);

        using var registry = NewEmptyRegistry();
        Assert.Null(uniffi.fauna_ffi.FaunaFfiMethods.SignOutBlocked(registry, _base, _store, null));
    }

    /// <summary>
    /// ⚠ The defect this gate exists for: a sibling window is still serving the
    /// account, so sign-out must refuse entirely. windows serves through the
    /// process-global holder (<see cref="SessionInstance.BecomeUnder"/>), so the
    /// sibling is simulated the way apple's own raw-lock route serves for real: an
    /// independent SHARED lock over the same account, taken directly over the FFI
    /// (mirrors <see cref="AnAccountAlreadyServedByAnotherHolderReadsAsServed"/>
    /// above, and <c>erase_guard.rs</c>'s own
    /// <c>a_sibling_window_refuses_the_sign_out_with_the_shared_line</c>).
    /// </summary>
    [Fact]
    public void SignOutBlockedRefusesWhileASiblingWindowServesTheAccount()
    {
        var actor = RandomActor();
        Directory.CreateDirectory(Path.Combine(_base, actor));
        SessionInstance.BindLaunchTo(actor);
        Assert.Equal(SessionInstanceStatus.Acquired, SessionInstance.BecomeUnder(_base, actor, _store).Status);

        using var registry = NewEmptyRegistry();
        using var sibling = uniffi.fauna_ffi.FaunaFfiMethods.AcquireAccountInstanceLockShared(_base, actor, _store);
        Assert.NotNull(sibling);

        var blocked = uniffi.fauna_ffi.FaunaFfiMethods.SignOutBlocked(registry, _base, _store, null);
        Assert.NotNull(blocked);
        Assert.Equal(new[] { actor }, blocked!.accounts);
    }
}

/// <summary>
/// Serialized: these tests drive process-global Rust state (the held-lock holder
/// and the launch-binding cell), which xUnit's default per-class parallelism would
/// otherwise interleave. Same convention as <c>AccountStateDirGlobal</c> /
/// <c>ActorScopedStaticsGlobal</c>.
/// </summary>
[CollectionDefinition("SessionInstanceGlobal", DisableParallelization = true)]
public class SessionInstanceCollection { }
