using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Every account-runtime stop this process has started and not yet seen finish, and
/// the barrier an erase waits behind (<c>account-scoping.md</c> § Erasure follows
/// scope, the ⚠ <i>An OPEN store is an unerasable store</i> note: a host must be able
/// to WAIT for the stop under a named bounded budget, and erase anyway, loudly, if the
/// budget lapses).
///
/// <para><b>Why a set, not the latest task.</b> A teardown starts its stop
/// fire-and-forget (<c>App.DisposeNestClients</c>: the switch, the e2e
/// <c>reset</c>/<c>logout</c> arms, a nest re-point), so several can be running at
/// once — a switch to the admin and straight back leaves the admin's stop running
/// behind the user's. This used to be one field each dispose overwrote, and the
/// remove-account erase never read even that: removing the admin right after the
/// switch back erased under its still-open store, which windows refuses
/// (<c>os error 32</c>), and the scope survived. The barrier holds while ANY stop is in
/// flight, including one started while it waits — the rule the shared
/// <c>StopQueue</c> applies on linux and tui (<c>fauna_client_account_runtime</c>).</para>
///
/// <para><b>Why a finished teardown is not a closed store.</b> The shared stop returns
/// when its own budget lapses (<c>ACCOUNT_RUNTIME_STOP_BUDGET</c>, 5 s) and goes on
/// closing the store in the background, in the shared set of stores still closing
/// (<c>fauna_client_account_runtime::closing</c>). Counting the teardown alone let an
/// erase run 3 ms after such a lapse, over the open store (a full-file
/// <c>--app windows</c> run, 2026-11-04). So once every tracked stop has returned, the
/// barrier also waits for that set to drain (<c>account_runtime_stores_closed</c>), on
/// the same budget.</para>
///
/// <para>One instance per process (<see cref="Process"/>), because the account
/// runtime it tracks is a process global; tests build their own.</para>
/// </summary>
internal sealed class AccountRuntimeStops
{
    /// <summary>
    /// How long an erase waits for the stops in flight and the stores they leave closing
    /// before erasing anyway. Named and generous (e2e convention 14): a healthy stop
    /// resolves in well under a second, and the longest legitimate one is a teardown's
    /// draft leave-flush (≤5 s), the shared stop's own budget
    /// (<c>ACCOUNT_RUNTIME_STOP_BUDGET</c>, 5 s) and, when that lapses, the store's close
    /// (<c>CLOSING_STORES_BUDGET</c>, 15 s) — so only a wedged teardown spends this, and
    /// spending it is still better than hanging a sign-out or a remove forever. Erasing
    /// anyway on a lapse fails loudly in the log (the survivor line), never silently.
    /// </summary>
    internal static readonly TimeSpan StopBudget = TimeSpan.FromSeconds(30);

    /// <summary>This process's stops, over the shared set of stores still closing.</summary>
    internal static AccountRuntimeStops Process { get; } =
        new(() => FaunaFfiMethods.AccountRuntimeStoresClosed());

    private readonly object _gate = new();
    private readonly HashSet<Task> _inFlight = new();

    // The shared wait for the stores a lapsed stop left closing — `true` once none is
    // left, `false` on its own lapse. `null` in the tests that pin the tracked set alone.
    private readonly Func<Task<bool>>? _storesClosed;

    /// <param name="storesClosed">The wait for the stores still closing — the process's is
    /// the shared <c>account_runtime_stores_closed</c>; a test passes its own.</param>
    internal AccountRuntimeStops(Func<Task<bool>>? storesClosed = null)
    {
        _storesClosed = storesClosed;
    }

    /// <summary>
    /// Count <paramref name="stop"/> as in flight until it finishes — however it finishes:
    /// a stop that faulted has still let go of whatever it was going to let go of.
    /// </summary>
    internal void Track(Task stop)
    {
        lock (_gate) _inFlight.Add(stop);
        _ = stop.ContinueWith(
            done =>
            {
                lock (_gate) _inFlight.Remove(done);
                if (done.Exception is { } ex)
                {
                    ShellLog.Warn("AccountRuntimeStops",
                        $"[account-runtime] a stop failed: {ex.GetBaseException().Message}");
                }
            },
            CancellationToken.None,
            TaskContinuationOptions.ExecuteSynchronously,
            TaskScheduler.Default);
    }

    /// <summary>The barrier at <see cref="StopBudget"/>.</summary>
    internal Task<bool> BarrierAsync(string erase) => BarrierAsync(erase, StopBudget);

    /// <summary>
    /// Wait until no stop is in flight and no store is still closing, or until
    /// <paramref name="budget"/> lapses. Returns <c>true</c> when nothing is left — the
    /// scope is erasable — and <c>false</c> on a lapse, which it (or the shared wait)
    /// reports itself. Never throws; the caller erases either way.
    /// </summary>
    /// <param name="erase">The erase about to run, for the log lines.</param>
    internal async Task<bool> BarrierAsync(string erase, TimeSpan budget)
    {
        var clock = Stopwatch.StartNew();
        var waitedFor = new HashSet<Task>();
        while (true)
        {
            Task[] pending;
            lock (_gate) pending = _inFlight.Where(t => !t.IsCompleted).ToArray();
            if (pending.Length == 0)
            {
                // Every teardown has returned; the stores a lapsed one left closing are
                // the last thing between the erase and a closed store.
                if (!await StoresClosedAsync(erase, budget, clock).ConfigureAwait(false))
                {
                    return false;
                }
                // A stop started while that wait ran holds the erase back too.
                lock (_gate) pending = _inFlight.Where(t => !t.IsCompleted).ToArray();
                if (pending.Length == 0) break;
                continue;
            }
            waitedFor.UnionWith(pending);

            // Over each stop's finishing, never its outcome: a faulted stop has finished
            // (Track reports the fault), and a faulted WhenAll here would go unobserved.
            var all = Task.WhenAll(pending.Select(stop => stop.ContinueWith(
                static _ => { },
                CancellationToken.None,
                TaskContinuationOptions.ExecuteSynchronously,
                TaskScheduler.Default)));
            if (!await WithinAsync(all, budget - clock.Elapsed).ConfigureAwait(false))
            {
                ShellLog.Warn("AccountRuntimeStops",
                    $"[account-runtime] {pending.Length} stop(s) still running after "
                    + $"{budget.TotalSeconds:F0}s — the {erase} erases anyway; a still-open "
                    + "store fails its remove_dir_all (os error 32)");
                return false;
            }
        }
        if (waitedFor.Count > 0)
        {
            ShellLog.Info("AccountRuntimeStops",
                $"[account-runtime] the {erase} waited {clock.ElapsedMilliseconds} ms for "
                + $"{waitedFor.Count} stop(s) to finish; the scope is erasable");
        }
        return true;
    }

    /// <summary>The shared wait for the stores still closing, inside what is left of the
    /// barrier's budget. The shared wait logs its own wait and its own lapse.</summary>
    private async Task<bool> StoresClosedAsync(string erase, TimeSpan budget, Stopwatch clock)
    {
        if (_storesClosed is null) return true;
        Task<bool> closed;
        try
        {
            closed = _storesClosed();
        }
        catch (Exception ex)
        {
            ShellLog.Warn("AccountRuntimeStops",
                $"[account-runtime] could not ask whether the stores are closed ({ex.Message}) "
                + $"— the {erase} erases anyway");
            return false;
        }
        if (!await WithinAsync(closed, budget - clock.Elapsed).ConfigureAwait(false))
        {
            ShellLog.Warn("AccountRuntimeStops",
                $"[account-runtime] a store was still closing after {budget.TotalSeconds:F0}s "
                + $"— the {erase} erases anyway; a still-open store fails its remove_dir_all "
                + "(os error 32)");
            return false;
        }
        try
        {
            return await closed.ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("AccountRuntimeStops",
                $"[account-runtime] the wait for the stores failed ({ex.Message}) — the "
                + $"{erase} erases anyway");
            return false;
        }
    }

    /// <summary><c>true</c> when <paramref name="task"/> finished within
    /// <paramref name="left"/> (checked at once when nothing is left).</summary>
    private static async Task<bool> WithinAsync(Task task, TimeSpan left)
    {
        if (left > TimeSpan.Zero && !task.IsCompleted)
        {
            using var lapse = new CancellationTokenSource();
            await Task.WhenAny(task, Task.Delay(left, lapse.Token)).ConfigureAwait(false);
            lapse.Cancel();
        }
        return task.IsCompleted;
    }
}
