using System;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// Content-sealing epoch-schedule refresh glue — the windows leg of linux
/// <c>FaunaClient::refresh_mail_epoch_schedule</c> / tui
/// <c>spawn_refresh_mail_epoch_schedule</c> / android
/// <c>MailEnableGlueVM.refreshMailEpochSchedule</c> / web
/// <c>refreshMailEpochSchedule</c>
/// (<c>encryption-at-rest.md</c> § Capability tiering → Content-sealing epochs).
/// <para>
/// Pure trigger glue over the shared, <b>idempotent</b>
/// <c>MailSettingsMachine::refresh_epoch_schedule</c>. It is a no-op when mail
/// isn't enabled — so it is always safe to call unconditionally at
/// the post-auth hook, exactly like
/// <see cref="DeploymentSeedCustody.SelfHealAsync"/> beside which it fires.
/// </para>
/// <para>
/// Best-effort with NO user-facing surface on any outcome, transport failures
/// included: only logged. Invisible plumbing — no UI, no ui.yaml IDs.
/// </para>
/// </summary>
internal static class MailEpochSchedule
{
    /// <summary>
    /// Refresh this client's content-sealing epoch schedule once for a freshly
    /// connected session. Call from the <b>universal</b> post-auth hook
    /// (<c>App.StartMainAppAsync</c>'s single "transition into Online" site), so
    /// every login and returning-user relaunch refreshes exactly once. Rides the
    /// session's own connection (<paramref name="rpc"/>) through the same machine
    /// the mail-settings page builds, never a one-shot connection of its own
    /// (transport.md: one WebSocket per actor). Never throws.
    /// </summary>
    internal static async Task RefreshAsync(INestRpcClient rpc)
    {
        try
        {
            using var machine = await rpc.BuildMailSettingsMachineAsync();
            await machine.RefreshEpochSchedule();
            ShellLog.Info("MailEpochSchedule", "content-sealing epoch schedule refreshed");
        }
        catch (Exception ex)
        {
            ShellLog.Warn("MailEpochSchedule",
                $"epoch-schedule refresh failed, best-effort: {ex.GetType().Name}: {ex.Message}");
        }
    }
}
