using System;
using System.Threading;

namespace FaunaApp.Core.Services;

/// <summary>
/// The <b>Core-owned half</b> of windows' in-memory actor-scoped state — every
/// process-lifetime cache, memoized machine and background loop that belongs to
/// ONE signed-in account, dropped from one place so no teardown site has to
/// remember a list of its own.
///
/// <para><c>account-scoping.md</c> § The scoping taxonomy binds this: the
/// switch/sign-out isolation contract forbids account B rendering or modifying
/// account A's local state, and its in-memory corollary makes a live cache, a
/// memoized machine or a running timer account-scoped by class 1/4 exactly as its
/// on-disk twin would be. Windows never exits the process on an actor change —
/// <c>SwitchAccountHandler</c> tears down in place and rebuilds a
/// <c>LaunchMachine</c>, while sign-out and factory-reset re-root
/// <c>OnboardingPage</c> in the same process — so nothing here is dropped for us
/// by a shell teardown.</para>
///
/// <para>── Why one function rather than a list per teardown site ─────────────</para>
///
/// <para>Before this type the drop was <b>hand-listed at five sites</b> in
/// <c>App.xaml.cs</c> (<c>SwitchAccountHandler</c>, <c>SignOutHandler</c>,
/// <c>FactoryResetReonboardHandler</c>, <c>DisposeNestClients</c> and the test
/// agent's <c>reset</c>/<c>logout</c> arms) and the five had already drifted apart,
/// which is the failure mode rather than an accident of maintenance: the two
/// content caches were on <b>none</b> of them, the conversations manager on the
/// switch only, and Guardian Notify + the e2e snapshot on all but factory-reset.
/// Web solved the same problem with a registration registry
/// (<c>lib/actorScope.ts</c>), which works there because importing a module runs
/// its registration as a side effect; linux ruled the opposite shape for a language
/// without module-init side effects (<c>apps/fauna-linux/src/actor_scope.rs</c>) and
/// apple adopted it (<c>FaunaKit/Core/ActorScope.swift</c>): <b>one explicit,
/// statically greppable function that every teardown site calls</b>. A registry in
/// C# would only relocate the hand-list — into <c>static</c> constructors that run
/// lazily, on first touch — and add a <i>silent</i> failure mode when one is
/// forgotten. Adding Core-owned actor-scoped state means adding one line to
/// <see cref="DropActorScopedState"/> and nowhere else.
///
/// <para>── The two halves, and why ───────────────────────────────────────────</para>
///
/// <para>The WinUI assembly references this one, never the reverse
/// (<c>FaunaApp.csproj</c>), so the app-owned surfaces that live up there —
/// <c>ConversationsManagerHost</c>, <c>AppDataSnapshot</c> — cannot be reached from
/// here. They are dropped by <c>App.DropActorScopedState()</c>, which calls this
/// first and is the single entry point the five teardown sites use. That is apple's
/// split verbatim (FaunaKit's <c>resetSharedState()</c> under each target's
/// <c>dropActorScopedState()</c>), for the same reason: the half that can be shared
/// is dropped where it cannot drift.</para>
///
/// <para>── Why windows needs a cancellation seam that apple did not ──────────</para>
///
/// <para>Dropping state is only half of an actor change; the other half is retiring
/// the <b>background loops</b> that write it. Apple's cadences are objects owning
/// their loop as a <c>Task</c>, so <c>stop()</c> cancels the captured work outright.
/// Windows' auto-renew cadence was a bare <c>static async Task</c> with
/// <c>while (true) { await Task.Delay(6h); … }</c>, no token and no guard, spawned
/// at every transition into <c>Online</c> — so it <b>accumulated one live loop per
/// login</b>, each still issuing TLS certs every 6 h through its own
/// <c>FfiNestClient</c> against a nest that no longer knows that client. (Apple's
/// twin was merely <i>latched</i>: one stale cadence survived, and the next login's
/// <c>start</c> silently no-oped. Windows' shape is the worse of the two.) A drop
/// cannot stop a loop that holds no token, so the seam comes first: every
/// actor-scoped loop takes a <see cref="BeginBackgroundLoop"/> lease and observes
/// its <see cref="BackgroundLoopLease.Token"/>, and the drop cancels the whole
/// outgoing generation at once — however many leases it handed out.</para>
///
/// <para>Site-specific work — disposing the nest/RPC clients, unprovisioning the
/// sync agent, nilling session fields, rebuilding crypto, navigation, credential
/// wipes and the e2e-only <c>ClearForTest</c> arms — stays at its own site: this
/// type drops <i>state</i>, exactly as linux's <c>reset_actor_scoped_state</c>
/// does.</para>
/// </summary>
public static class ActorScope
{
    private static readonly object _gate = new();

    /// Cancelled (and replaced) once per actor change. Every lease handed out
    /// since the last drop shares it, which is what makes one drop retire a whole
    /// generation of loops rather than the one the caller happened to remember.
    private static CancellationTokenSource _generation = new();

    private static int _liveBackgroundLoops;

    /// <summary>
    /// How many actor-scoped background loops are alive right now — leases taken
    /// and not yet disposed.
    ///
    /// <para>This is the witness the accumulation defect needs: it is not enough
    /// to know a token was cancelled, because the pre-fix loop held no token and
    /// so survived every teardown. <c>ActorScopeTests</c> asserts on this.</para>
    /// </summary>
    public static int LiveBackgroundLoops => Volatile.Read(ref _liveBackgroundLoops);

    /// <summary>
    /// A lease on the current actor's background-loop generation. Its
    /// <see cref="Token"/> is cancelled by the next
    /// <see cref="DropActorScopedState"/>; disposing it (a <c>using</c> at the top
    /// of the loop) is what takes the loop off <see cref="LiveBackgroundLoops"/>.
    /// </summary>
    public sealed class BackgroundLoopLease : IDisposable
    {
        private int _released;

        internal BackgroundLoopLease(CancellationToken token) => Token = token;

        /// <summary>Cancelled when the actor this loop was started for goes away.</summary>
        public CancellationToken Token { get; }

        /// <summary>Idempotent — a loop that exits through both its cancellation
        /// path and a <c>finally</c> must not double-decrement.</summary>
        public void Dispose()
        {
            if (Interlocked.Exchange(ref _released, 1) != 0) return;
            Interlocked.Decrement(ref _liveBackgroundLoops);
        }
    }

    /// <summary>
    /// Take a lease for a background loop that belongs to the signed-in account.
    /// Call it at the top of the loop's own method, <c>using</c>, and await
    /// everything that can wait on <see cref="BackgroundLoopLease.Token"/>.
    /// </summary>
    public static BackgroundLoopLease BeginBackgroundLoop()
    {
        CancellationToken token;
        lock (_gate)
        {
            // Read the generation under the lock so a loop starting concurrently
            // with a drop lands wholly on one side of it: either it takes the
            // outgoing generation's token (already cancelled by the time the drop
            // returns, so the loop retires on its first await) or the incoming
            // one's. What it must never get is a token from a source the drop has
            // already disposed.
            token = _generation.Token;
        }
        Interlocked.Increment(ref _liveBackgroundLoops);
        return new BackgroundLoopLease(token);
    }

    /// <summary>
    /// Drop every piece of Core-owned in-memory actor-scoped state, then retire the
    /// outgoing actor's background loops. <b>The</b> canonical list — every teardown
    /// path reaches it (through <c>App.DropActorScopedState()</c>, which adds the
    /// WinUI-owned half) and hand-lists nothing of its own.
    ///
    /// <para>Ordering is part of the contract, and it is linux's: state is dropped
    /// <b>before</b> the generation is retired, so a loop that wakes between the two
    /// finds its own token still current and merely operates on already-cleared
    /// state, rather than reading a half-dropped mix.</para>
    ///
    /// <para>Safe to call repeatedly and from a partially-built session — several
    /// teardown sites run before any login completed.</para>
    ///
    /// <para><paramref name="identityEnds"/> is <c>false</c> only for a client
    /// rebuild that keeps the SAME identity (a nest re-point or re-establish of the
    /// signed-in actor): everything bound to the dying client still drops, but the
    /// identity's standing alerts and the loop re-checking them survive, because
    /// the identity does (<c>critical-alerts.md</c> § Mechanism → <i>Lifetime</i>:
    /// "session" means identity, and a failed re-check never clears — dropping them
    /// here took a standing alarm down whenever the next check could not read).</para>
    /// </summary>
    public static void DropActorScopedState(bool identityEnds = true)
    {
        if (identityEnds)
        {
            // critical-alerts.md § Mechanism → Lifetime: an alert names a condition
            // on the outgoing account's nest; left standing it accuses the INCOMING
            // account with the outgoing one's finding.
            CriticalAlertsHost.Instance.ClearAll();
            // …and the teardown-epoch bump above is what stops that account's
            // re-sweep loop, so forget it in the same breath: the next sign-in
            // starts a loop of its own rather than taking the same-identity
            // one-shot arm.
            Helpers.CriticalAlertsSweep.ForgetLoop();
        }
        // Memoized against the outgoing secret AND the RPC client it was built
        // with — either alone is enough to make it wrong after a teardown.
        AtprotoSettingsMachineHost.Instance.Reset();
        // family-safety.md § Guardian Notify: the pending count batch AND the
        // per-item dedup set. Carrying the dedup set across an actor change makes
        // the incoming ward's first enforcement on a reused item id silently
        // UNcounted — an under-report indistinguishable from "nothing happened".
        GuardianNotifyCache.Reset();
        // The guardian per-category floor, the viewer's own spam/phishing
        // thresholds and this session's reveal set. Its reads fail OPEN by design,
        // which is exactly what makes re-hydrating the floor on every actor change
        // load-bearing (web and linux both recorded this).
        ContentPolicyCache.Reset();
        // The region plane is the DEVICE's (a region is a fact about the device, not
        // the account — region-blocking.md § How an app obtains its region's
        // policy): only its refresh clock and the outgoing session's nest go, so
        // the next login asks the relay at once.
        RegionPlaneHost.ClearSession();
        // The screen-time heartbeat + lock state (family-safety.md § Screen
        // time) — without this a new account inherits the previous ward's
        // lock, naming a guardian the user does not have.
        ScreenTimeCache.Reset();
        // The muted-word list and its reveal set — the conversation-bubble twin of
        // the line above.
        MutedKeywordsCache.Reset();

        // Retire the outgoing actor's loops LAST, per the ordering note above.
        CancellationTokenSource outgoing;
        lock (_gate)
        {
            outgoing = _generation;
            _generation = new CancellationTokenSource();
        }
        // Cancel OUTSIDE the lock: `Cancel()` may run a waiter's continuation
        // inline on this thread, and a loop that reacted by taking a new lease
        // would deadlock against a lock still held here.
        try
        {
            outgoing.Cancel();
        }
        catch (AggregateException ex)
        {
            // A cancellation callback of someone else's is not this drop's problem
            // — but swallowing it silently would hide a real fault, so say so.
            Logs.ShellLog.Warn("ActorScope", $"loop cancellation threw: {ex.Message}");
        }
        outgoing.Dispose();
    }
}
