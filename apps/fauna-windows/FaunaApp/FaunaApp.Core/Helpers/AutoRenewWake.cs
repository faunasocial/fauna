// testing.md § Cross-app e2e conventions, point 15 — the automation surface is
// compiled out of release artifacts. This whole file is the windows leg of the
// certificate auto-renew tick's e2e poke: a Release build compiles NONE of it, so
// the shipped cadence (App.RunAutoRenewCadenceAsync) waits on its clock alone.
#if DEBUG || FAUNA_E2E_AGENT
using System;
using System.Threading;
using System.Threading.Tasks;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// What ends the certificate auto-renew tick's wait early: windows' leg of
/// <c>fauna_e2e_agent::AUTO_RENEW_WAKE</c> (<c>auto_renew_wake</c>, convention 14's
/// <c>run_now</c>). With it the tick witness
/// (<c>test_admin_dns_cert_auto_renew_tick.py</c>) drives the production loop's own
/// next pass instead of waiting out its six-hour clock. tui's
/// <c>fauna_core::loop_wake::LoopWake</c> is the reference shape. android's
/// <c>AutoRenewWake</c> and apple's <c>CadenceWake</c> are its twins, and this
/// class carries android's name.
///
/// <para><b>One per loop.</b> <c>App.RunAutoRenewCadenceAsync</c> mints a fresh
/// wake (<see cref="Mint"/>) each time a cadence starts, bound to that loop's
/// actor-scope token, and retires it (<see cref="Retire"/>) when the loop exits.
/// So a wake while no loop runs (before sign-in, after the actor changed, or when
/// the loop could not build its machine) is refused rather than acked into nothing
/// (convention 11), and a departed loop's handle is fired by nothing.</para>
///
/// <para>A wake that lands while no wait is pending (the loop mid-pass) is kept and
/// ends the very next wait, so a test never loses its poke to a busy loop.
/// Cancelling the loop's token still ends the wait at once, by throwing, exactly
/// as the bare <c>Task.Delay</c> it replaces does.</para>
/// </summary>
internal sealed class AutoRenewWake
{
    private static readonly object CurrentGate = new();
    private static AutoRenewWake? _current;

    private readonly CancellationToken _loopGone;
    private readonly object _gate = new();
    private bool _pending;
    private TaskCompletionSource? _waiter;

    private AutoRenewWake(CancellationToken loopGone) => _loopGone = loopGone;

    /// <summary>
    /// Mint the calling loop's wake and make it the one <see cref="Fire"/> reaches.
    /// <paramref name="loopGone"/> is the loop's actor-scope token: once it is
    /// cancelled the wake is refused, even before the loop's own
    /// <see cref="Retire"/> runs.
    /// </summary>
    internal static AutoRenewWake Mint(CancellationToken loopGone)
    {
        var wake = new AutoRenewWake(loopGone);
        lock (CurrentGate)
        {
            _current = wake;
        }
        return wake;
    }

    /// <summary>
    /// The loop that owns <paramref name="wake"/> has exited. This clears it only if
    /// it is still the current one, so a departed loop exiting late cannot clear
    /// its successor's wake.
    /// </summary>
    internal static void Retire(AutoRenewWake wake)
    {
        lock (CurrentGate)
        {
            if (ReferenceEquals(_current, wake)) _current = null;
        }
    }

    /// <summary>
    /// <c>auto_renew_wake</c>: end the live loop's current wait, so its own body
    /// runs the next pass now. The caller's barrier is that pass's
    /// <c>auto-renew pass:</c> log line, never this return. Returns the refusal
    /// sentence, or <c>null</c> once the wake landed.
    /// </summary>
    internal static string? Fire()
    {
        AutoRenewWake? now;
        lock (CurrentGate)
        {
            now = _current;
        }
        if (now is null || now._loopGone.IsCancellationRequested)
        {
            return "no authenticated session, so no auto-renew tick to wake";
        }
        now.Wake();
        return null;
    }

    /// <summary>
    /// Wait <paramref name="period"/>, or until a wake ends the wait first.
    /// Throws <see cref="OperationCanceledException"/> when
    /// <paramref name="cancel"/> fires, as <c>Task.Delay</c> does.
    /// </summary>
    internal async Task WaitAsync(TimeSpan period, CancellationToken cancel)
    {
        // A departed loop leaves now, even with a wake pending.
        cancel.ThrowIfCancellationRequested();
        TaskCompletionSource woken;
        lock (_gate)
        {
            if (_pending)
            {
                _pending = false;
                return;
            }
            woken = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            _waiter = woken;
        }
        // The clock is cancelled when the wake wins, so a woken wait leaves no
        // six-hour timer behind it.
        using var clockGone = CancellationTokenSource.CreateLinkedTokenSource(cancel);
        var clock = Task.Delay(period, clockGone.Token);
        try
        {
            var first = await Task.WhenAny(clock, woken.Task).ConfigureAwait(false);
            if (first == clock)
            {
                // The period elapsed, or the loop's token was cancelled (this
                // throws).
                await clock.ConfigureAwait(false);
                return;
            }
            cancel.ThrowIfCancellationRequested();
        }
        finally
        {
            clockGone.Cancel();
            lock (_gate)
            {
                if (ReferenceEquals(_waiter, woken)) _waiter = null;
            }
        }
    }

    private void Wake()
    {
        TaskCompletionSource? waiter;
        lock (_gate)
        {
            waiter = _waiter;
            _waiter = null;
            if (waiter is null) _pending = true;
        }
        waiter?.TrySetResult();
    }

    /// <summary>Test seam: forget the current loop, so a pin starts from "none running".</summary>
    internal static void ClearForTest()
    {
        lock (CurrentGate)
        {
            _current = null;
        }
    }
}
#endif
