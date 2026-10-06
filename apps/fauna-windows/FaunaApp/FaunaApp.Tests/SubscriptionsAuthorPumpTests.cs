using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Unit tests for <see cref="SubscriptionsAuthorPump"/> — the author-side
/// reconciliation loop that makes an encrypted-mode follow frictionless
/// (`monetization.md` § The unifying model, grant path 2: "auto_approve … no
/// creator action").
/// <para>The pump is <b>scheduling only</b>: the tick body and the cadence are
/// shared policy (`monetization.md` § Pillar 1 → Where the logic lives), so
/// these tests pin what this class actually owns — that the shared tick runs on
/// <em>every</em> iteration, that a reported fault never ends the loop, that
/// cancellation exits cleanly, and that the cadence is read from shared Rust
/// rather than restated. Resume-before-drain ordering is guaranteed inside
/// <c>reconcile_once</c> and pinned by its own Rust tests — deliberately not
/// re-proved here, because a C# test could only re-assert it by re-deriving the
/// order this seam exists to remove.</para>
/// </summary>
public class SubscriptionsAuthorPumpTests
{
    /// <summary>A delay seam that never elapses on its own: each awaited tick is
    /// released by the test (or the token), so poll iterations are deterministic.</summary>
    private sealed class ManualDelay
    {
        private readonly Queue<Func<Task>> _onTick = new();
        public int Ticks { get; private set; }

        /// <summary>Enqueue an action to run when the pump next awaits the poll
        /// delay; the delay completes immediately after it runs. With the queue
        /// empty the delay blocks until the token cancels (the pump then exits).</summary>
        public void OnNextTick(Func<Task> action) => _onTick.Enqueue(action);

        public async Task Wait(TimeSpan _, CancellationToken ct)
        {
            Ticks++;
            if (_onTick.Count > 0)
            {
                await _onTick.Dequeue()();
                ct.ThrowIfCancellationRequested();
                return;
            }
            await Task.Delay(Timeout.InfiniteTimeSpan, ct);
        }
    }

    private static (MockNestRpcClient rpc, ManualDelay delay, SubscriptionsAuthorPump pump) Build()
    {
        var rpc = new MockNestRpcClient();
        var delay = new ManualDelay();
        var pump = new SubscriptionsAuthorPump(rpc, delay: delay.Wait);
        return (rpc, delay, pump);
    }

    private static int Count(MockNestRpcClient rpc, string call) =>
        rpc.Calls.Count(c => c == call);

    [Fact]
    public async Task Run_TicksTheSharedReconcilePass_OnConnect()
    {
        var (rpc, _, pump) = Build();
        using var cts = new CancellationTokenSource();
        cts.CancelAfter(TimeSpan.FromSeconds(5)); // safety net; exits at the first (empty) tick
        var run = pump.RunAsync(cts.Token);
        cts.Cancel();
        await run;

        // ONE shared call, not a hand-sequenced resume/drain pair — the whole
        // tick body is `SubscriptionsAuthor::reconcile_once`.
        Assert.Equal(
            new[] { "SubscriptionsReconcileOnce" },
            rpc.Calls.Where(c => c.StartsWith("Subscriptions")).ToArray());
    }

    [Fact]
    public async Task Run_PollBackstopRunsTheWholeTick_NotJustTheDrainHalf()
    {
        var (rpc, delay, pump) = Build();
        using var cts = new CancellationTokenSource();
        delay.OnNextTick(() => Task.CompletedTask);                              // tick 1 → second pass
        delay.OnNextTick(() => { cts.Cancel(); return Task.CompletedTask; });    // tick 2 → exit
        await pump.RunAsync(cts.Token);

        Assert.Equal(2, Count(rpc, "SubscriptionsReconcileOnce"));
    }

    [Fact]
    public async Task Run_RemovalStagedAfterStartup_IsResumedByALaterTick()
    {
        // THE regression this pump shape exists to prevent. A subscriber removal
        // is staged in `fauna.state.subscriptions` *before* its upload (crash-safety), and that
        // staging can appear mid-session: a transient nest rejection, a
        // disconnect mid-upload, or a peer device's staging merged in by config
        // sync. Until it is resumed, the roster the drain mints over still
        // contains the removed subscriber — so they keep read access to the
        // author's encrypted-mode content. Hoisting the resume half above the
        // loop (what windows did until 2026-08-02) heals it only at the next
        // connect; `monetization.md` § Pillar 1 → Where the logic lives,
        // property (2): "both halves run on every tick".
        var (rpc, delay, pump) = Build();
        using var cts = new CancellationTokenSource();
        // Tick 1: a removal is staged NOW, after the pump already started.
        delay.OnNextTick(() => { rpc.NextResumeRemovalsCount = 1; return Task.CompletedTask; });
        delay.OnNextTick(() => { cts.Cancel(); return Task.CompletedTask; });
        await pump.RunAsync(cts.Token);

        Assert.Equal(2, Count(rpc, "SubscriptionsReconcileOnce"));
        // The pass AFTER the staging saw it — healed without a reconnect.
        Assert.Equal(0u, rpc.ReconcilePasses[0].resumed);
        Assert.Equal(1u, rpc.ReconcilePasses[1].resumed);
    }

    [Fact]
    public async Task Run_ReportedHalfFailures_AreBestEffort_AndTheLoopKeepsTicking()
    {
        // Neither half can abort the other or the loop: the shared tick reports
        // faults in the pass instead of throwing, and the pump only logs them.
        var (rpc, delay, pump) = Build();
        rpc.NextResumeRemovalsError = "config sync unavailable";
        rpc.NextDrainAutoApprovalsError = "nest unreachable";
        using var cts = new CancellationTokenSource();
        // Tick 1: heal both faults → the next pass is clean. Tick 2: exit.
        delay.OnNextTick(() =>
        {
            rpc.NextResumeRemovalsError = null;
            rpc.NextDrainAutoApprovalsError = null;
            return Task.CompletedTask;
        });
        delay.OnNextTick(() => { cts.Cancel(); return Task.CompletedTask; });
        await pump.RunAsync(cts.Token); // must not throw

        Assert.Equal(2, Count(rpc, "SubscriptionsReconcileOnce"));
        Assert.Equal("config sync unavailable", rpc.ReconcilePasses[0].resumeError);
        Assert.Null(rpc.ReconcilePasses[1].resumeError);
        Assert.Null(rpc.ReconcilePasses[1].drainError);
    }

    [Fact]
    public async Task Run_SeamThrow_DoesNotKillTheLoop()
    {
        // The surrounding seam CAN throw (no connection yet, a torn-down
        // client) even though the tick itself does not.
        var (rpc, delay, pump) = Build();
        rpc.NextError = "not connected";
        using var cts = new CancellationTokenSource();
        delay.OnNextTick(() => { rpc.NextError = null; return Task.CompletedTask; });
        delay.OnNextTick(() => { cts.Cancel(); return Task.CompletedTask; });
        await pump.RunAsync(cts.Token); // must not throw

        Assert.Equal(2, Count(rpc, "SubscriptionsReconcileOnce"));
    }

    [Fact]
    public async Task Run_CancellationDuringPollDelay_ExitsCleanly()
    {
        var (_, delay, pump) = Build();
        using var cts = new CancellationTokenSource();
        var run = pump.RunAsync(cts.Token);
        // Give the pump a moment to reach the (blocking) empty-queue delay, then cancel.
        await Task.Delay(50);
        cts.Cancel();
        await run; // completes without OperationCanceledException escaping

        Assert.True(delay.Ticks >= 1, "the pump reached the poll backstop before exiting");
    }

    [Fact]
    public void DefaultPollInterval_ReadsTheSharedCadence_NotALocalLiteral()
    {
        // The cadence is shared policy: the 30 s default PLUS the
        // FAUNA_SUBS_POLL_SECS e2e override, which a hard-coded local constant
        // could not honour (the deviation this leg retires). Asserted against
        // the shared value itself rather than against 30, so an override in the
        // environment does not make this test lie.
        Assert.Equal(
            TimeSpan.FromSeconds(FaunaFfiMethods.SubscriptionsAuthorPollSecs()),
            SubscriptionsAuthorPump.DefaultPollInterval);
    }
}
