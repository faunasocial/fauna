using FaunaApp.Core.Logs;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Push notifications on windows — the <c>ws-device</c> transport (<c>common.md</c>
/// § Push Notifications → <i>Transports</i>): this app subscribes the install's row, the
/// per-user sync agent posts the toast while the app is closed, and an open app owns its
/// own banners and ignores the frame.
///
/// <para>Everything stateful is the shared <c>fauna_client_push::registration</c> machine
/// behind <see cref="IFfiPushRegistration"/> (the install intent bit, the which-actor
/// record, the leave-shape drops — <c>common.md</c> § Push Notifications →
/// <i>Registration</i>); this class only names windows' inputs to it, the same two tui names
/// (<c>apps/fauna-tui/src/push.rs</c>):</para>
/// <list type="bullet">
/// <item><b>the device id</b> — <c>RegistrySessionAccount.DeviceId</c>, i.e.
/// <c>FfiAccountRegistry.DeviceIdForActor</c>, the same id the sync agent's capability is
/// minted from, so the id the row is keyed under, the id this app's connection announces
/// and the id the agent announces are one value by construction;</item>
/// <item><b>the intent store</b> — one file in the install-scoped data dir
/// (<see cref="BackupPaths.DataDir"/>), never an account scope
/// (<c>account-scoping.md</c> class 2), so no sign-out erase touches it.</item>
/// </list>
/// </summary>
internal static class PushSession
{
    /// <summary>The intent file's name under the install-scoped data dir.</summary>
    internal const string IntentFileName = "push-intent.cbor";

    /// <summary>How long a leave gesture waits on its drop before going on: the drop is
    /// best-effort, never a gate, and a leave completes offline (<c>common.md</c>
    /// § Registration).</summary>
    internal static readonly TimeSpan LeaveBound = TimeSpan.FromSeconds(3);

    /// <summary>The install-scoped intent file — what the Settings toggle renders.</summary>
    internal static string IntentPath => Path.Combine(BackupPaths.DataDir, IntentFileName);

    /// <summary>This install's subscription: a <c>ws-device</c> row is its own device id
    /// (the shared machine fills it in), so nothing else is named.</summary>
    internal static FfiPushEndpoint WsDevice() => new("ws-device", "", null, null);

    /// <summary>
    /// At sign-in (launch, switch-in): announce this device on the session's connection,
    /// and re-arm the row when this install opted in — never opting it in. Best-effort: a
    /// failure is logged and the session proceeds.
    /// </summary>
    internal static async Task OnSessionStartAsync(INestRpcClient rpc, string? actorId, string? deviceId)
    {
        if (string.IsNullOrEmpty(actorId) || string.IsNullOrEmpty(deviceId))
        {
            ShellLog.Warn("Push", "no device id for this account; not announcing");
            return;
        }
        try
        {
            var registration = await rpc.BuildPushRegistrationAsync(IntentPath, actorId, deviceId)
                .ConfigureAwait(false);
            await registration.AnnouncePresence().ConfigureAwait(false);
            await registration.Rearm(WsDevice()).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("Push", $"session start: {ex.Message}");
        }
    }

    /// <summary>
    /// A leave gesture (switch-out, sign-out): drop the leaving actor's row, issued by the
    /// leaving session on its own client. Bounded by <see cref="LeaveBound"/> and
    /// best-effort — the caller proceeds whatever happens.
    /// </summary>
    internal static async Task DropActorRowAsync(INestRpcClient? rpc, string? actorId, string? deviceId)
    {
        if (rpc is null || string.IsNullOrEmpty(actorId) || string.IsNullOrEmpty(deviceId)) return;
        var drop = DropAsync(rpc, actorId, deviceId);
        if (await Task.WhenAny(drop, Task.Delay(LeaveBound)).ConfigureAwait(false) != drop)
        {
            ShellLog.Warn("Push", "dropping the leaving account's row did not finish in time; leaving anyway");
        }
    }

    private static async Task DropAsync(INestRpcClient rpc, string actorId, string deviceId)
    {
        try
        {
            var registration = await rpc.BuildPushRegistrationAsync(IntentPath, actorId, deviceId)
                .ConfigureAwait(false);
            await registration.DropActorRow().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("Push", $"dropping the leaving account's row failed: {ex.Message}");
        }
    }
}
