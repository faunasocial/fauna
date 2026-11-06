using System;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// Session-start publish of the subscriber encapsulation-key registry entry —
/// the windows leg of tui <c>spawn_publish_subscriber_encaps_key</c> / the other
/// apps' post-auth publish (<c>post-quantum.md</c> § surface B).
/// <para>
/// Pure trigger glue over the shared, idempotent
/// <c>subscriptions_publish_encaps_key</c>. Best-effort with NO user-facing
/// surface on any outcome: only logged. Invisible plumbing — no UI, no ui.yaml IDs.
/// </para>
/// </summary>
internal static class SubscriberEncapsKeyPublish
{
    /// <summary>
    /// Publish the entry once for a freshly connected session. Call from the
    /// universal post-auth hook, beside <see cref="MailEpochSchedule.StartAsync"/>.
    /// Never throws.
    /// </summary>
    internal static async Task PublishAsync(INestRpcClient rpc)
    {
        try
        {
            await rpc.SubscriptionPublishEncapsKeyAsync();
            ShellLog.Info("SubscriberEncapsKeyPublish", "subscriber encapsulation key published");
        }
        catch (Exception ex)
        {
            ShellLog.Warn("SubscriberEncapsKeyPublish",
                $"encapsulation-key publish failed, best-effort: {ex.GetType().Name}: {ex.Message}");
        }
    }
}
