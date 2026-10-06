using System;
using System.Threading.Tasks;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Process-lifetime holder for the current login's <see cref="FfiUsageHeartbeat"/>
/// (family-safety.md § Screen time, Slice E) — the windows twin of linux
/// <c>screen_lock.rs</c> / web <c>screenTime.svelte.ts</c> / android
/// <c>ScreenTimeStore</c> / apple <c>ScreenTimeStore</c>. Holds the heartbeat
/// handle for the session and exposes the one <see cref="LockMessage"/> the
/// global <c>screen-time-lock</c> overlay renders, so the gate and its
/// wording can never drift apart.
///
/// <para><b>Client-enforced by construction.</b> The nest cannot see when a
/// child's device is in use and deliberately does not gate on it, so this
/// cache IS the enforcement. <b>Every decision is shared Rust</b> — this class
/// holds no policy logic at all: whether to lock, and what the lock says, are
/// one call to <see cref="FaunaFfiMethods.ScreenLockMessage"/>, which folds
/// the policy, the device's local clock and the day's cross-device total.</para>
///
/// <para>Sibling of <see cref="GuardianNotifyCache"/>/<see
/// cref="ContentPolicyCache"/>: seeded from the SAME <c>fauna.family.status</c>
/// read (<c>MainPage.CheckFamilyStatusAsync</c>), so a guardian's policy edit
/// takes effect on the ward's next read rather than needing a restart.</para>
/// </summary>
public static class ScreenTimeCache
{
    /// One-minute production tick, matching the shared engine's own accrual-
    /// step requirement of a caller (a slower tick would silently under-count
    /// — <c>MAX_ACCRUAL_STEP_SECS</c> is two minutes). Mirrors linux
    /// <c>LOCK_TICK_SECS</c> / android <c>TICK_INTERVAL_MS</c>, both 60s.
    public const int TickIntervalSecs = 60;

    private static readonly object _gate = new();
    private static FfiUsageHeartbeat _heartbeat = new();
    private static FfiScreenTimePolicy? _policy;
    private static string? _guardianHandle;
    private static string? _lockMessage;

    /// Test-only clock skew in seconds (testing.md § convention 14's fake
    /// clock), the windows twin of android <c>testClockSkewSecs</c> / linux
    /// <c>advance_test_clock</c>. Stays 0 in production; only the debug-only
    /// <c>TestAgent</c>'s <c>screen_time_heartbeat</c> command writes it via
    /// <see cref="AdvanceTestClockAndTickAsync"/>.
    private static long _testClockSkewSecs;

    /// Whether the app window currently has focus — set from
    /// <c>App.xaml.cs</c>'s <c>Window.Activated</c>. "active" for the
    /// heartbeat is foregrounded AND not locked (lock-screen time is not
    /// use — crediting it would inflate the guardian's readout with minutes
    /// the child never spent).
    private static bool _windowActive = true;

    /// The current lock verdict, resolved to display text, or <c>null</c> for
    /// "not locked" — the single decision point the global overlay renders
    /// from (mirrors <see cref="ContentPolicyCache.Current"/>'s snapshot shape).
    public static string? LockMessage { get { lock (_gate) return _lockMessage; } }

    /// Must be called under <see cref="_gate"/>.
    private static long NowSecsLocked() => DateTimeOffset.UtcNow.ToUnixTimeSeconds() + _testClockSkewSecs;

    /// <summary>
    /// Record the ward's own screen-time policy, guardian, and the day's
    /// cross-device usage total, from <c>fauna.family.status</c>. Called on
    /// every status read — the SAME <c>MainPage.CheckFamilyStatusAsync</c>
    /// read that seeds <see cref="ContentPolicyCache"/> and <see
    /// cref="GuardianNotifyCache"/> — so a guardian's policy edit takes
    /// effect on the ward's next read rather than needing a restart. <c>null</c>
    /// <paramref name="guardianHandle"/> (unsupervised) clears the lock.
    ///
    /// <para><paramref name="usageTodayMinutes"/> seeds the heartbeat so the
    /// very first paint can already evaluate the budget, instead of leaving
    /// an over-budget ward unlocked until the first heartbeat round-trip
    /// completes.</para>
    /// </summary>
    internal static void SetWardScreenTime(FfiScreenTimePolicy? policy, string? guardianHandle, uint? usageTodayMinutes)
    {
        lock (_gate)
        {
            _policy = policy;
            _guardianHandle = guardianHandle;
            _heartbeat.SetPolicy(policy);
            _heartbeat.SeedTotal(usageTodayMinutes);
            RecomputeLocked();
        }
    }

    /// The ward's own usage figure for their read-only summary — the same
    /// number the guardian sees (§ Screen time transparency rule). <c>null</c>
    /// when no total has been heard yet.
    public static uint? UsedTodayMinutes()
    {
        lock (_gate) return _heartbeat.UsedTodayMinutes(NowSecsLocked());
    }

    /// Re-evaluate the lock verdict and publish it. The single decision
    /// point: both the overlay's visibility and its text come from here. Runs
    /// EVERY tick regardless of whether a budget is set — the window half is
    /// pure client-local clock and needs no accounting at all (§ Screen time
    /// build order: window first). Must be called under <see cref="_gate"/>.
    private static void RecomputeLocked()
    {
        if (_guardianHandle is not { } guardian)
        {
            _lockMessage = null;
            return;
        }
        var now = DateTime.Now;
        var nowLocalMinutes = (ushort)(now.Hour * 60 + now.Minute);
        var used = _heartbeat.UsedTodayMinutes(NowSecsLocked());
        var text = FaunaFfiMethods.ScreenLockMessage(_policy, nowLocalMinutes, used, guardian);
        _lockMessage = text is null ? null : Strings.Resolve(text);
    }

    /// <summary>
    /// The one-minute production tick. Re-evaluates the lock (the window half
    /// needs nothing else) and, while a daily budget is set, drives the
    /// heartbeat one step — sending <c>fauna.family.usage_report</c> when the
    /// shared engine says a report is due. <paramref name="rpc"/> is the live
    /// connection, matching <see cref="GuardianNotifyCache.CheckNowAsync"/>'s
    /// shape.
    /// </summary>
    internal static async Task TickAsync(INestRpcClient rpc)
    {
        bool accounting;
        bool activeArg;
        lock (_gate)
        {
            accounting = _heartbeat.IsAccounting();
            activeArg = _windowActive && _lockMessage is null;
        }
        if (!accounting)
        {
            lock (_gate) RecomputeLocked();
            return;
        }
        await HeartbeatStepAsync(activeArg, rpc);
    }

    /// <summary>
    /// Drive the heartbeat one step and, if the shared engine says a report is
    /// due, send <c>fauna.family.usage_report</c> and land the reply.
    /// <paramref name="active"/> is whether the app is being used right now:
    /// foregrounded AND the lock is not showing. A failure re-credits the
    /// minutes rather than forgiving them (the delta is defined against the
    /// last <b>successful</b> report).
    /// </summary>
    private static async Task HeartbeatStepAsync(bool active, INestRpcClient rpc)
    {
        uint? due;
        lock (_gate)
        {
            var now = NowSecsLocked();
            _heartbeat.SetActive(active, now);
            due = _heartbeat.TakeDue(now);
        }
        if (due is not { } minutes)
        {
            lock (_gate) RecomputeLocked();
            return;
        }

        var offset = DeviceOffset.UtcOffsetMinutes();
        try
        {
            var reply = await rpc.FamilyUsageReportAsync(minutes, offset);
            lock (_gate) _heartbeat.ReportSucceeded(reply.@day, reply.@dayTotalMinutes, NowSecsLocked());
        }
        catch
        {
            // Best-effort, same posture as GuardianNotifyCache.CheckNowAsync:
            // re-credit rather than forgive (the delta is defined against the
            // last SUCCESSFUL report).
            lock (_gate) _heartbeat.ReportFailed();
        }
        lock (_gate) RecomputeLocked();
    }

    /// <summary>
    /// Test-only: advance the clock by <paramref name="minutes"/> of
    /// foreground use, in the accrual steps a real caller would tick in, then
    /// run one production heartbeat step — the windows twin of android
    /// <c>advanceTestClockAndTick</c> / linux <c>advance_test_clock</c> +
    /// <c>flush_usage_report(true)</c> / web <c>advanceTestClock</c> +
    /// <c>tickUsageHeartbeat</c>. <c>active = true</c> throughout: the poke
    /// asserts what a ward actively using the app accrues. Driven only by the
    /// debug-only <c>TestAgent</c>'s <c>screen_time_heartbeat</c> command
    /// (testing.md § convention 14's fake clock + <c>run_now</c> poke;
    /// § convention 15 keeps this whole path out of release artifacts via
    /// <c>TestAgent</c>'s own build-type gate).
    /// </summary>
    internal static async Task AdvanceTestClockAndTickAsync(int minutes, INestRpcClient rpc)
    {
        const long stepSecs = 120; // fauna_core::screen_time::MAX_ACCRUAL_STEP_SECS
        var remaining = Math.Max(0L, (long)minutes * 60);
        // Prime the engine's reference point before advancing — it accrues
        // from the GAP between calls, so with no prior call the first step
        // would credit nothing and the poke would silently deliver less use
        // than it was asked for.
        lock (_gate) _heartbeat.SetActive(true, NowSecsLocked());
        while (remaining > 0)
        {
            var bump = Math.Min(remaining, stepSecs);
            lock (_gate)
            {
                _testClockSkewSecs += bump;
                remaining -= bump;
                _heartbeat.SetActive(true, NowSecsLocked());
            }
        }
        await HeartbeatStepAsync(active: true, rpc);
    }

    /// Set from <c>App.xaml.cs</c>'s <c>Window.Activated</c> — whether the app
    /// window currently has focus. Production-only signal for <see
    /// cref="TickAsync"/>'s "active" argument; the test poke above always
    /// passes <c>true</c> directly, so this has no effect on the two tier_3
    /// screen-time journeys.
    public static void SetWindowActive(bool active)
    {
        lock (_gate) _windowActive = active;
    }

    /// Drop this ward's screen-time state on an account switch or sign-out —
    /// without this a new account would inherit the previous ward's lock,
    /// naming a guardian the user does not have (linux
    /// <c>clear_for_identity_change</c>).
    public static void Reset()
    {
        lock (_gate)
        {
            _heartbeat = new FfiUsageHeartbeat();
            _policy = null;
            _guardianHandle = null;
            _lockMessage = null;
            _testClockSkewSecs = 0;
            _windowActive = true;
        }
    }
}
