using System;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// The critical-alert sweep call sites (<c>critical-alerts.md</c>
/// § Mechanism → <i>Who runs the detector</i> / <i>How often the detector
/// runs</i>) — windows' twin of tui's
/// <c>critical_alerts::spawn_session_start_sweep</c> /
/// <c>run_alert_sweep_loop</c> caller. Pure trigger glue over the shared,
/// best-effort <c>FaunaFfiMethods.RunCriticalAlertSweep</c> /
/// <c>RunCriticalAlertSweepLoop</c> (<c>libs/fauna-ffi/src/critical_alerts.rs</c>
/// — the same free functions android already calls), which themselves call
/// <c>fauna_client_alert_sweep::run_session_start_sweep</c> /
/// <c>run_alert_sweep_loop</c> against the process-wide
/// <see cref="Services.CriticalAlertsHost"/> registry.
/// <para>
/// Same shape as <see cref="MailEpochSchedule"/>: never throws, safe to fire
/// unconditionally from a post-auth hook without awaiting sign-in on it.
/// </para>
/// </summary>
internal static class CriticalAlertsSweep
{
    private static readonly object Gate = new();

    // The (actor, nest) the live loop sweeps for, and the token of the run that
    // owns it. Null when no loop is live: none started yet, the identity was torn
    // down (ForgetLoop), or the loop itself exited.
    private static string? _loopKey;
    private static object? _loopToken;

    /// <summary>
    /// The one post-auth entry point, the split tui, linux and web follow: a
    /// first or identity-changing sign-in starts the re-sweep LOOP
    /// (<see cref="RunLoopAsync"/>); a same-identity re-establish runs ONE pass
    /// (<see cref="RunAsync"/>), because the loop is still running for that
    /// identity and starting another would stack a concurrent sweeper per re-auth.
    /// Called from both post-auth sites — production's
    /// <c>App.StartMainAppAsync</c> and the e2e <c>session</c> seam, which never
    /// reaches it — so the e2e build runs the same loop the product does, which is
    /// what the <c>alert_sweep_wake</c> seam wakes. Never throws.
    /// <para>"Identity" is the actor AND the nest: a re-point at another nest
    /// needs a loop sweeping that nest. The previous nest's loop is retired at
    /// the next identity teardown, like any other.</para>
    /// <para>Both ride the session's own connection (<paramref name="rpc"/>), never a
    /// one-shot connection of their own (transport.md: one WebSocket per actor).
    /// <paramref name="nestUrl"/> only keys the identity.</para>
    /// </summary>
    internal static void StartForIdentity(INestRpcClient rpc, string nestUrl, string actorIdHex)
    {
        if (ClaimLoop(actorIdHex + "@" + nestUrl) is { } token)
            _ = RunLoopAsync(rpc, token);
        else
            _ = RunAsync(rpc);
    }

    /// <summary>
    /// The split's decision: a token when <paramref name="identityKey"/> has no live
    /// loop (the caller starts one, owning the token), <c>null</c> when it does (the
    /// caller runs one pass).
    /// </summary>
    internal static object? ClaimLoop(string identityKey)
    {
        lock (Gate)
        {
            if (_loopKey == identityKey) return null;
            var token = new object();
            _loopKey = identityKey;
            _loopToken = token;
            return token;
        }
    }

    /// <summary>
    /// The loop <paramref name="token"/> owns has ended (a failed connect, or a
    /// teardown), so it no longer covers its identity and the next sign-in must
    /// start one. Only if it is still the recorded loop: a departed identity's loop
    /// ending late must not clear its successor's record.
    /// </summary>
    internal static void ReleaseLoop(object token)
    {
        lock (Gate)
        {
            if (!ReferenceEquals(_loopToken, token)) return;
            _loopKey = null;
            _loopToken = null;
        }
    }

    /// <summary>
    /// The identity is gone (sign-out, account switch, factory reset): the next
    /// sign-in starts a fresh loop. Called from <c>ActorScope.DropActorScopedState</c>
    /// right beside the registry's <c>ClearAll</c>, whose teardown-epoch bump is what
    /// actually stops the old loop, so the two can never disagree.
    /// </summary>
    internal static void ForgetLoop()
    {
        lock (Gate)
        {
            _loopKey = null;
            _loopToken = null;
        }
    }

    /// <summary>
    /// Run the sweep once — the same-identity re-establish arm of
    /// <see cref="StartForIdentity"/>. Never throws.
    /// </summary>
    private static async Task RunAsync(INestRpcClient rpc)
    {
        try
        {
            await rpc.RunCriticalAlertSweepAsync();
            ShellLog.Info("CriticalAlertsSweep", "session-start critical-alert sweep dispatched");
        }
        catch (Exception ex)
        {
            ShellLog.Warn("CriticalAlertsSweep",
                $"session-start sweep failed, best-effort: {ex.GetType().Name}: {ex.Message}");
        }
    }

    /// <summary>
    /// Run the session-start sweep, then repeat it every
    /// <c>RE_SWEEP_INTERVAL_SECS</c> (6 h) for as long as the identity lives
    /// — never returns under normal operation; it stops on the first wake
    /// after <see cref="Services.CriticalAlertsHost"/>'s registry sees
    /// <c>clear_all</c> (sign-out / account switch / factory reset). Started only
    /// by <see cref="StartForIdentity"/>. Never throws.
    /// </summary>
    private static async Task RunLoopAsync(INestRpcClient rpc, object token)
    {
        try
        {
            await rpc.RunCriticalAlertSweepLoopAsync();
            ShellLog.Info("CriticalAlertsSweep", "critical-alert sweep loop stopped (identity teardown)");
        }
        catch (Exception ex)
        {
            ShellLog.Warn("CriticalAlertsSweep",
                $"critical-alert sweep loop failed, best-effort: {ex.GetType().Name}: {ex.Message}");
        }
        finally
        {
            ReleaseLoop(token);
        }
    }
}
