using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The process-lifetime <see cref="uniffi.fauna_ffi.FfiUsageHeartbeat"/>
/// holder (family-safety.md § Screen time, Slice E) — the windows twin of
/// linux <c>screen_lock.rs</c> / android <c>ScreenTimeStore</c>. Sibling of
/// <c>GuardianNotifyCacheTests</c>/<c>ContentPolicyCacheTests</c>; serialized
/// the same way since xUnit parallelizes by class and this is process-global
/// static state.
///
/// <para>The shared engine's own rules (the accrual clamp, the failed-report
/// re-credit, the day-bucket math) are proven exhaustively at tier_1 in
/// <c>fauna_core::screen_time::tests</c> — these tests exercise the WIRING
/// only: that this cache really drives the heartbeat, really calls
/// <c>fauna.family.usage_report</c>, and really resets on identity
/// change.</para>
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class ScreenTimeCacheTests : IDisposable
{
    public ScreenTimeCacheTests() => ScreenTimeCache.Reset();
    public void Dispose() => ScreenTimeCache.Reset();

    [Fact]
    public void LockMessage_DefaultsToNull()
    {
        Assert.Null(ScreenTimeCache.LockMessage);
    }

    [Fact]
    public void SetWardScreenTime_NoGuardian_NeverLocks()
    {
        // A daily budget of 0 (a deliberate full lock) would lock a SUPERVISED
        // ward outright — proving this stays unlocked isolates the guardian
        // check, not a "budget too generous to trigger" false negative.
        ScreenTimeCache.SetWardScreenTime(
            FfiScreenTimePolicyFixture.Make(dailyMinutes: 0),
            guardianHandle: null,
            usageTodayMinutes: 0);

        Assert.Null(ScreenTimeCache.LockMessage);
    }

    [Fact]
    public void SetWardScreenTime_GuardianButNoPolicy_UnsupervisedEquivalentDefault_NeverLocks()
    {
        // null policy is NOT "no data yet" — it is the shared engine's own
        // unsupervised-equivalent default (ScreenTimePolicy::default()), which
        // never locks (family-safety.md § Screen time).
        ScreenTimeCache.SetWardScreenTime(policy: null, guardianHandle: "guardian1", usageTodayMinutes: null);

        Assert.Null(ScreenTimeCache.LockMessage);
    }

    [Fact]
    public void UsedTodayMinutes_DefaultsToNull_NoTotalHeardYet()
    {
        Assert.Null(ScreenTimeCache.UsedTodayMinutes());
    }

    [Fact]
    public void SetWardScreenTime_SeedsTheHeartbeatTotal()
    {
        ScreenTimeCache.SetWardScreenTime(
            FfiScreenTimePolicyFixture.Make(dailyMinutes: 120),
            guardianHandle: "guardian1",
            usageTodayMinutes: 45);

        Assert.Equal(45u, ScreenTimeCache.UsedTodayMinutes());
    }

    [Fact]
    public async Task AdvanceTestClockAndTickAsync_WithBudgetSet_ReportsTheRequestedMinutes()
    {
        ScreenTimeCache.SetWardScreenTime(
            FfiScreenTimePolicyFixture.Make(dailyMinutes: 120),
            guardianHandle: "guardian1",
            usageTodayMinutes: 0);
        var rpc = new MockNestRpcClient
        {
            NextFamilyUsageReport = FfiFamilyUsageReportFixture.Make(day: 20000, dayTotalMinutes: 15),
        };

        await ScreenTimeCache.AdvanceTestClockAndTickAsync(15, rpc);

        Assert.NotNull(rpc.LastFamilyUsageReport);
        Assert.Equal(15u, rpc.LastFamilyUsageReport!.Value.Minutes);
        // The reply's day_total_minutes lands via ReportSucceeded, so the ward's
        // own readout reflects the NEST's total, not a locally-summed guess.
        Assert.Equal(15u, ScreenTimeCache.UsedTodayMinutes());
    }

    [Fact]
    public async Task AdvanceTestClockAndTickAsync_NoBudgetSet_NeverCallsTheRpc()
    {
        // No accounting without a declared policy (family-safety.md § Screen
        // time) — a poke with nothing to accrue against must not call the RPC
        // at all, matching the shared engine's own take_due() = None.
        ScreenTimeCache.SetWardScreenTime(policy: null, guardianHandle: "guardian1", usageTodayMinutes: null);
        var rpc = new MockNestRpcClient();

        await ScreenTimeCache.AdvanceTestClockAndTickAsync(15, rpc);

        Assert.Null(rpc.LastFamilyUsageReport);
    }

    [Fact]
    public async Task AdvanceTestClockAndTickAsync_RpcThrows_ReCreditsRatherThanForgives()
    {
        ScreenTimeCache.SetWardScreenTime(
            FfiScreenTimePolicyFixture.Make(dailyMinutes: 120),
            guardianHandle: "guardian1",
            usageTodayMinutes: 0);
        var rpc = new MockNestRpcClient { NextError = "offline" };

        // Best-effort: the failure must not propagate out of the poke (a test
        // asserting the JOURNEY, not this one RPC, must be able to retry).
        await ScreenTimeCache.AdvanceTestClockAndTickAsync(15, rpc);

        // A failed report re-credits its minutes (never forgiven) — the next
        // successful report should therefore still see the FULL delta pending,
        // provable by seeding a fresh cache-visible total once one lands.
        rpc.NextError = null;
        rpc.NextFamilyUsageReport = FfiFamilyUsageReportFixture.Make(day: 20000, dayTotalMinutes: 15);
        await ScreenTimeCache.AdvanceTestClockAndTickAsync(0, rpc);

        Assert.Equal(15u, rpc.LastFamilyUsageReport!.Value.Minutes);
    }

    [Fact]
    public void Reset_DropsGuardianAndPolicyAndUsage()
    {
        ScreenTimeCache.SetWardScreenTime(
            FfiScreenTimePolicyFixture.Make(dailyMinutes: 120),
            guardianHandle: "guardian1",
            usageTodayMinutes: 45);
        Assert.Equal(45u, ScreenTimeCache.UsedTodayMinutes());

        ScreenTimeCache.Reset();

        Assert.Null(ScreenTimeCache.LockMessage);
        Assert.Null(ScreenTimeCache.UsedTodayMinutes());
    }
}
