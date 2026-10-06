using System;
using System.Threading;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The windows twin of linux's <c>actor_scope.rs</c> tests and apple's
/// <c>ActorScopeTests.swift</c>: pin that the Core half of the in-memory
/// actor-scoped drop actually drops, so the isolation contract in
/// <c>account-scoping.md</c> § The scoping taxonomy holds across an actor change.
///
/// <para><b>The headline case is the accumulation</b>, because that is windows'
/// specific shape of this defect. Apple's <c>DnsAutoRenewCadence</c> was
/// <i>latched</i>, so a teardown that skipped the drop left exactly ONE stale
/// cadence and the pin there is <c>armCount</c>. Windows' auto-renew cadence was a
/// bare <c>static async Task</c> with <c>while (true) { await Task.Delay(6h); … }</c>
/// spawned at every transition into <c>Online</c> — no token, no latch — so it
/// accumulated one live loop per login, each holding its own
/// <c>FfiNestClient</c> and issuing TLS certs against a nest that no longer knew
/// that client. The witness is therefore
/// <see cref="ActorScope.LiveBackgroundLoops"/>: a count, because a count is the
/// only thing that can tell "one loop was retired" from "N loops are still
/// running".</para>
///
/// <para>Scope is deliberately the Core half only. The two WinUI-owned surfaces
/// (<c>ConversationsManagerHost</c>, <c>AppDataSnapshot</c>) live in the app project
/// this test project cannot reference, and are dropped by
/// <c>App.DropActorScopedState()</c>, reached through the e2e actor-switch journey
/// rather than from here. The cadence itself has no e2e pin either and cannot get
/// one — it is skipped entirely under the E2E bridge (<c>App.xaml.cs</c>, the
/// <c>E2eEnv.Bridge</c> guard at its spawn site), so tier_1 is the whole story for
/// it.</para>
///
/// <para>Every wait below is on a cancellation that has already happened, bounded
/// by a generous ceiling — never a settle-sleep (<c>testing.md</c> convention 14).
/// The fake loops never touch the network.</para>
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class ActorScopeTests : IDisposable
{
    /// A generous ceiling for "a cancelled loop notices and exits". Cancellation is
    /// immediate; this is sized far above any non-pathological scheduling delay so a
    /// loaded machine cannot make the test lie in either direction.
    private static readonly TimeSpan LoopExitBudget = TimeSpan.FromSeconds(30);

    public ActorScopeTests() => ActorScope.DropActorScopedState();
    public void Dispose() => ActorScope.DropActorScopedState();

    /// <summary>
    /// A stand-in for <c>RunAutoRenewCadenceAsync</c>: leases the actor scope, then
    /// waits on a delay it can only leave by cancellation — the same shape, minus
    /// the FFI client and the 6 h period. Returns the loop's task so a test can
    /// await its exit instead of sleeping.
    /// </summary>
    private static Task FakeActorScopedLoop()
    {
        return Task.Run(async () =>
        {
            using var lease = ActorScope.BeginBackgroundLoop();
            try
            {
                while (!lease.Token.IsCancellationRequested)
                {
                    await Task.Delay(TimeSpan.FromHours(6), lease.Token);
                }
            }
            catch (OperationCanceledException)
            {
                // The actor changed — the only way out, exactly as the real cadence.
            }
        });
    }

    /// <summary>
    /// The headline: N logins must leave ONE live loop, not N. This is the defect
    /// verbatim — before the seam, the second login's cadence joined the first
    /// instead of replacing it, and nothing ever ended either.
    ///
    /// <para>Mutation check: remove the <c>outgoing.Cancel()</c> from
    /// <c>ActorScope.DropActorScopedState</c> and the first await times out; remove
    /// the <c>using</c> from the loop (or the decrement from
    /// <c>BackgroundLoopLease.Dispose</c>) and the counts go wrong instead.</para>
    /// </summary>
    [Fact]
    public async Task ADropRetiresTheOutgoingLoopSoRepeatedLoginsCannotAccumulate()
    {
        var first = FakeActorScopedLoop();
        await WaitForLiveLoops(1);

        // Login two, the way every teardown path reaches it.
        ActorScope.DropActorScopedState();
        await first.WaitAsync(LoopExitBudget);
        Assert.Equal(0, ActorScope.LiveBackgroundLoops);

        var second = FakeActorScopedLoop();
        await WaitForLiveLoops(1);
        Assert.Equal(1, ActorScope.LiveBackgroundLoops);

        // ...and a third, to show the property is per-actor-change and not a
        // one-shot: the count must not creep.
        ActorScope.DropActorScopedState();
        await second.WaitAsync(LoopExitBudget);
        var third = FakeActorScopedLoop();
        await WaitForLiveLoops(1);
        Assert.Equal(1, ActorScope.LiveBackgroundLoops);

        ActorScope.DropActorScopedState();
        await third.WaitAsync(LoopExitBudget);
        Assert.Equal(0, ActorScope.LiveBackgroundLoops);
    }

    /// <summary>
    /// One drop retires the whole outgoing GENERATION, not just the newest loop.
    /// This is why the seam is a shared token source rather than a handle the
    /// spawner remembers: the pre-fix app could genuinely have several cadences
    /// live at once (one per login, since nothing ended them), and a fix that only
    /// stopped "the current one" would have left the older ones running forever.
    /// </summary>
    [Fact]
    public async Task OneDropRetiresEveryLoopOfTheOutgoingGeneration()
    {
        var loops = new[] { FakeActorScopedLoop(), FakeActorScopedLoop(), FakeActorScopedLoop() };
        await WaitForLiveLoops(3);

        ActorScope.DropActorScopedState();

        await Task.WhenAll(loops).WaitAsync(LoopExitBudget);
        Assert.Equal(0, ActorScope.LiveBackgroundLoops);
    }

    /// <summary>
    /// The drop must not poison the seam: a lease taken AFTER it belongs to the
    /// incoming actor and is not already cancelled. This is the counterpart of
    /// apple's latch case — there the danger was a drop being skipped, here it
    /// would be a drop that cancelled every future loop too, which reads identically
    /// in production (the incoming account's cadence never runs) and would be found
    /// only 6 h later, or never.
    /// </summary>
    [Fact]
    public void AFreshLeaseAfterTheDropBelongsToTheIncomingActor()
    {
        using var outgoing = ActorScope.BeginBackgroundLoop();
        Assert.False(outgoing.Token.IsCancellationRequested);

        ActorScope.DropActorScopedState();
        Assert.True(outgoing.Token.IsCancellationRequested,
            "the outgoing actor's loop was left running against a signed-out identity");

        using var incoming = ActorScope.BeginBackgroundLoop();
        Assert.False(incoming.Token.IsCancellationRequested,
            "the drop cancelled the INCOMING actor's loop too — its cadence would never run");
    }

    /// <summary>
    /// Every surface on the canonical list is actually dropped. This is the test
    /// that catches the drift the whole remediation exists to prevent: the two
    /// content caches were on none of the five old hand-lists, Guardian Notify was
    /// missing from factory-reset, and each site read perfectly correct alone.
    ///
    /// <para>Mutation check (run 2026-08-24, one line at a time): deleting
    /// <c>ContentPolicyCache.Reset()</c>, <c>MutedKeywordsCache.Reset()</c>,
    /// <c>GuardianNotifyCache.Reset()</c> or <c>ScreenTimeCache.Reset()</c> from
    /// <c>ActorScope.DropActorScopedState</c> turns this case red — the two cache lines also redden
    /// <see cref="TheDropIsIdempotentAndSafeWhenNothingIsArmed"/>, which reads the
    /// same statics. <c>CriticalAlertsHost</c> and <c>AtprotoSettingsMachineHost</c> are the
    /// two that carry no readable post-state here (an empty alert registry and a
    /// null memo are their own construction defaults, so asserting them would be
    /// vacuous) — they were already dropped on all three production paths before
    /// this change, and the e2e journey covers them.</para>
    /// </summary>
    [Fact]
    public void TheDropClearsEverySurfaceOnTheCanonicalList()
    {
        // The outgoing account: a spam threshold low enough to collapse, reveals on
        // both surfaces, a muted word, and a pending Guardian Notify count.
        ContentPolicyCache.SetOwnThresholds(((ushort)10, (ushort)10));
        ContentPolicyCache.Reveal("post-1");
        MutedKeywordsCache.SetKeywords(new[] { new uniffi.fauna_core.MutedKeyword("lottery", -1000) });
        MutedKeywordsCache.Reveal("msg-1");
        GuardianNotifyCache.SetEnabled(true);
        GuardianNotifyCache.Record("post-1", new[] { "violence" });
        ScreenTimeCache.SetWardScreenTime(
            FfiScreenTimePolicyFixture.Make(dailyMinutes: 120),
            guardianHandle: "guardian1", usageTodayMinutes: 45);

        // Each assertion below is paired with its pre-drop opposite, so none can
        // pass vacuously on a default-constructed cache. Guardian Notify is the one
        // exception and deliberately has NO pre-drop check: its only observer,
        // `TryTakeDue`, DRAINS what it reports and then bars the next call until the
        // min report interval elapses — so a pre-drop drain would leave the
        // post-drop assertion passing for the interval's sake whether the drop
        // cleared anything or not. (Measured: with a pre-drop drain in place,
        // deleting `GuardianNotifyCache.Reset()` from the drop left all five cases
        // green.) Its pre-drop opposite — Record → TryTakeDue reports the count — is
        // pinned by `GuardianNotifyCacheTests.Record_ThenTryTakeDue_...`, so this
        // test only has to show the post-drop side.
        Assert.Equal("collapse", ContentPolicyCache.VerdictFor(SpamLabel(900)));
        Assert.True(ContentPolicyCache.IsRevealed("post-1"));
        Assert.True(MutedKeywordsCache.IsRevealed("msg-1"));
        Assert.NotEmpty(MutedKeywordsCache.Words);
        Assert.Equal(45u, ScreenTimeCache.UsedTodayMinutes());

        ActorScope.DropActorScopedState();

        // The guardian floor and own thresholds are gone, so the same label reads
        // "show" — the fail-open default, which is exactly why re-hydrating this on
        // every actor change is load-bearing rather than tidy.
        Assert.Equal("show", ContentPolicyCache.VerdictFor(SpamLabel(900)));
        Assert.False(ContentPolicyCache.IsRevealed("post-1"),
            "the outgoing account's post reveals survived into the incoming session");
        Assert.Empty(MutedKeywordsCache.Words);
        Assert.False(MutedKeywordsCache.IsRevealed("msg-1"),
            "the outgoing account's message reveals survived into the incoming session");
        // Both halves: the pending batch AND the enabled flag. A survivor of either
        // would under-report to the INCOMING ward's guardian, which is
        // indistinguishable from "nothing happened".
        Assert.Null(GuardianNotifyCache.TryTakeDue());
        GuardianNotifyCache.Record("post-2", new[] { "violence" });
        Assert.Null(GuardianNotifyCache.TryTakeDue());
        // A survivor here would lock the INCOMING ward under the outgoing one's
        // guardian/budget — the same under/over-report class of bug as the
        // Guardian Notify pair above, just on the other pillar.
        Assert.Null(ScreenTimeCache.UsedTodayMinutes());
        Assert.Null(ScreenTimeCache.LockMessage);
    }

    /// <summary>
    /// Safe to call repeatedly and from a partially-built session — several of the
    /// five teardown sites can run before any login completed (the test agent's
    /// <c>reset</c> arm on a fresh install, a factory reset from onboarding).
    /// </summary>
    [Fact]
    public void TheDropIsIdempotentAndSafeWhenNothingIsArmed()
    {
        ActorScope.DropActorScopedState();
        ActorScope.DropActorScopedState();

        Assert.Equal(0, ActorScope.LiveBackgroundLoops);
        Assert.Empty(MutedKeywordsCache.Words);
    }

    /// A deadline poll on the loop-count, never a settle-sleep: a freshly spawned
    /// <c>Task.Run</c> has not necessarily reached its lease yet.
    private static async Task WaitForLiveLoops(int expected)
    {
        var deadline = DateTime.UtcNow + LoopExitBudget;
        while (ActorScope.LiveBackgroundLoops != expected && DateTime.UtcNow < deadline)
        {
            await Task.Yield();
        }
        Assert.Equal(expected, ActorScope.LiveBackgroundLoops);
    }

    /// One nest-applied spam label at `permille`, the shape
    /// <c>ContentPolicyCacheTests.Labels</c> builds.
    private static uniffi.fauna_core.ContentLabelEntry[] SpamLabel(ushort permille) =>
        new[] { new uniffi.fauna_core.ContentLabelEntry("spam", permille) };
}

/// <summary>
/// Serializes every class that mutates one of the process-global statics
/// <see cref="ActorScope.DropActorScopedState"/> drops — <c>ContentPolicyCache</c>,
/// <c>MutedKeywordsCache</c>, <c>GuardianNotifyCache</c>, <c>ScreenTimeCache</c> —
/// plus the loop-lease counter itself.
///
/// <para>It replaced the three narrower collections (<c>ContentPolicyGlobal</c>,
/// <c>MutedKeywordsGlobal</c>, <c>GuardianNotifyGlobal</c>) on 2026-08-24, and had
/// to: xUnit parallelizes by class and allows a class only ONE collection, so once
/// a single canonical drop touches all three statics, any class calling it races
/// every class in all three former collections. Without the merge one class's drop
/// lands mid-assertion in another and the failure looks like a product bug — cache
/// empty in a full run, green in isolation.</para>
///
/// <para>Add any future class that calls <c>ActorScope.DropActorScopedState()</c>,
/// or that mutates one of those caches directly, to this collection.</para>
/// </summary>
[CollectionDefinition("ActorScopedStaticsGlobal", DisableParallelization = true)]
public class ActorScopedStaticsGlobalCollection { }
