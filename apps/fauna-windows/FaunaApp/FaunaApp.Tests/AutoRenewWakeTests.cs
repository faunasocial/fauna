#if DEBUG || FAUNA_E2E_AGENT
using System;
using System.Threading;
using System.Threading.Tasks;
using FaunaApp.Core.Helpers;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The tier_1 pin for windows' <c>auto_renew_wake</c> leg
/// (<see cref="AutoRenewWake"/>), the twin of android's <c>AutoRenewWakeTest</c> and
/// apple's <c>CadenceWakeTests</c>. It covers what
/// <c>test_admin_dns_cert_auto_renew_tick.py</c> pokes, proven without an app or a
/// nest:
/// <list type="bullet">
/// <item>the wake ends the loop's six-hour wait;</item>
/// <item>a wake that lands mid-pass is kept for the next wait;</item>
/// <item>a wake with no loop running (never minted, the loop's actor gone, or the
/// loop retired) is refused, never acked (convention 11);</item>
/// <item>the actor change still ends the wait at once.</item>
/// </list>
///
/// <para>Every wait here is on an event that has already happened or that the test
/// itself fires, bounded by a generous ceiling. None is a settle-sleep
/// (convention 14).</para>
/// </summary>
[Collection("AutoRenewWakeStatic")]
public class AutoRenewWakeTests : IDisposable
{
    private static readonly TimeSpan SixHours = TimeSpan.FromHours(6);

    /// A ceiling for "the wait ended", far above any scheduling delay, so a loaded
    /// machine cannot make the test lie in either direction.
    private static readonly TimeSpan EndBudget = TimeSpan.FromSeconds(30);

    public AutoRenewWakeTests() => AutoRenewWake.ClearForTest();
    public void Dispose() => AutoRenewWake.ClearForTest();

    [Fact]
    public void AWakeWithNoTickRunningIsRefused()
    {
        var refusal = AutoRenewWake.Fire();
        Assert.NotNull(refusal);
        Assert.Contains("no auto-renew tick", refusal);
    }

    [Fact]
    public async Task AWakeEndsTheSixHourWait()
    {
        using var loop = new CancellationTokenSource();
        var wake = AutoRenewWake.Mint(loop.Token);
        var waiting = wake.WaitAsync(SixHours, loop.Token);
        Assert.False(waiting.IsCompleted);

        Assert.Null(AutoRenewWake.Fire());

        await waiting.WaitAsync(EndBudget);
    }

    [Fact]
    public async Task AWakeThatLandsMidPassEndsTheNextWait()
    {
        using var loop = new CancellationTokenSource();
        var wake = AutoRenewWake.Mint(loop.Token);

        // No wait is pending: the loop is mid-pass. The wake is kept.
        Assert.Null(AutoRenewWake.Fire());

        await wake.WaitAsync(SixHours, loop.Token).WaitAsync(EndBudget);

        // It was consumed: the wait after it runs on the clock again.
        var next = wake.WaitAsync(SixHours, loop.Token);
        Assert.False(next.IsCompleted);
        loop.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => next.WaitAsync(EndBudget));
    }

    [Fact]
    public async Task TheActorChangeEndsTheWaitAndRefusesTheWake()
    {
        using var loop = new CancellationTokenSource();
        var wake = AutoRenewWake.Mint(loop.Token);
        var waiting = wake.WaitAsync(SixHours, loop.Token);

        loop.Cancel();

        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => waiting.WaitAsync(EndBudget));
        // The loop has not run its Retire yet; its cancelled token alone refuses.
        Assert.NotNull(AutoRenewWake.Fire());
    }

    [Fact]
    public void ARetiredLoopsWakeIsRefused()
    {
        using var loop = new CancellationTokenSource();
        var wake = AutoRenewWake.Mint(loop.Token);
        AutoRenewWake.Retire(wake);
        Assert.NotNull(AutoRenewWake.Fire());
    }

    [Fact]
    public async Task ADepartedLoopRetiringLateLeavesItsSuccessorsWake()
    {
        using var first = new CancellationTokenSource();
        var departed = AutoRenewWake.Mint(first.Token);
        using var second = new CancellationTokenSource();
        var successor = AutoRenewWake.Mint(second.Token);
        first.Cancel();

        AutoRenewWake.Retire(departed);

        var waiting = successor.WaitAsync(SixHours, second.Token);
        Assert.Null(AutoRenewWake.Fire());
        await waiting.WaitAsync(EndBudget);
    }
}
#endif
