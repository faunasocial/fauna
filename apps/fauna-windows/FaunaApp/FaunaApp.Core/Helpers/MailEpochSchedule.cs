using System;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// Content-sealing epoch-schedule refresh glue — the windows leg of linux
/// <c>FaunaClient::refresh_mail_epoch_schedule</c> / tui
/// <c>spawn_refresh_mail_epoch_schedule</c> /
/// android <c>KidsExcisedApi.startMailEpochRefresh</c> (the shared UniFFI trigger) / web
/// <c>refreshMailEpochSchedule</c>
/// (<c>encryption-at-rest.md</c> § Capability tiering → Content-sealing epochs).
/// <para>
/// Pure start glue over the shared trigger
/// (<c>fauna_client_mail_settings::rpc_glue::refresh_epoch_schedule_on_every_connect</c>),
/// which runs the <b>idempotent</b> <c>MailSettingsMachine::refresh_epoch_schedule</c>
/// at start and after every reconnect of the client, and refreshes the
/// submission token. It is a no-op when mail isn't enabled — so it is always
/// safe to start unconditionally at the post-auth hook, exactly like
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
    /// Start the shared connect-time refresh trigger for this client, once per
    /// client (<see cref="INestRpcClient.StartMailEpochRefreshOnEveryConnectAsync"/>
    /// is idempotent). Call from the <b>universal</b> post-auth hook
    /// (<c>App.StartMainAppAsync</c>'s single "transition into Online" site) and
    /// from the e2e <c>session</c> door, which builds its own client. Rides the
    /// session's own connection (<paramref name="rpc"/>), never a one-shot
    /// connection of its own (transport.md: one WebSocket per actor). Never
    /// throws — the connect behind it can fail, and the call is fire-and-forget.
    /// </summary>
    internal static async Task StartAsync(INestRpcClient rpc)
    {
        try
        {
            await rpc.StartMailEpochRefreshOnEveryConnectAsync();
            ShellLog.Info("MailEpochSchedule", "mail connect-time refresh trigger started");
        }
        catch (Exception ex)
        {
            ShellLog.Warn("MailEpochSchedule",
                $"mail refresh trigger start failed, best-effort: {ex.GetType().Name}: {ex.Message}");
        }
    }
}
