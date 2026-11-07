using System;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// The critical-alert sweep call site (<c>critical-alerts.md</c>
/// § Mechanism → <i>Who runs the detector</i> / <i>How often the detector
/// runs</i>) — windows' twin of tui's <c>critical_alerts::spawn_session_start_sweep</c>.
/// Pure trigger glue over the shared, best-effort
/// <c>FaunaFfiMethods.StartCriticalAlertSweep</c>
/// (<c>libs/fauna-ffi/src/critical_alerts.rs</c>), which decides the post-auth
/// split itself (<c>fauna_client_alert_sweep::LoopClaim</c>) and runs the arm it
/// picks against the process-wide <see cref="Services.CriticalAlertsHost"/>
/// registry: a first or identity-changing sign-in starts the re-sweep loop; a
/// same-identity re-establish runs one pass when the identity's loop still rides
/// this very client, and otherwise starts a loop on it. Windows disposes the
/// session's client and builds another on every re-establish, and a loop ends with
/// the client it rides, so here that is a new loop each time.
/// <para>
/// Same shape as <see cref="MailEpochSchedule"/>: never throws, safe to fire
/// unconditionally from a post-auth hook without awaiting sign-in on it.
/// </para>
/// </summary>
internal static class CriticalAlertsSweep
{
    /// <summary>
    /// The one post-auth entry point, called from both post-auth sites —
    /// production's <c>App.StartMainAppAsync</c> and the e2e <c>session</c> seam,
    /// which never reaches it — so the e2e build runs the same loop the product
    /// does, which is what the <c>alert_sweep_wake</c> seam wakes.
    /// <para>It rides the session's own connection (<paramref name="rpc"/>), never
    /// a connection of its own (transport.md: one WebSocket per actor).
    /// <paramref name="nestUrl"/> is the nest's literal URL; with the actor it keys
    /// the identity, so a re-point at another nest gets a loop sweeping that nest.
    /// Never throws.</para>
    /// </summary>
    internal static void StartForIdentity(INestRpcClient rpc, string nestUrl) =>
        _ = RunAsync(rpc, nestUrl);

    /// <summary>
    /// The shared sweep, awaited to its end: after one pass, or when the loop ends
    /// (the identity or the connection torn down). Never throws.
    /// </summary>
    internal static async Task RunAsync(INestRpcClient rpc, string nestUrl)
    {
        try
        {
            await rpc.StartCriticalAlertSweepAsync(nestUrl);
            ShellLog.Info("CriticalAlertsSweep",
                "critical-alert sweep ended (one pass, or a loop whose identity or connection was torn down)");
        }
        catch (Exception ex)
        {
            ShellLog.Warn("CriticalAlertsSweep",
                $"critical-alert sweep failed, best-effort: {ex.GetType().Name}: {ex.Message}");
        }
    }
}
