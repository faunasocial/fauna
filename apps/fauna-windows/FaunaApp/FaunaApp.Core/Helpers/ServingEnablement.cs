using System;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// The windows glue of the post-claim serving enablement — the ONE shared step
/// every app's <c>LoggedIn</c> handoff makes with the four intents it read off the
/// onboarding machine (<c>onboarding.md</c> § 3b <i>Mechanism</i>;
/// <c>fauna_client_mail_settings::serving_enablement</c>), over the session's own
/// connection.
///
/// <para>The shared step waits for the account store and the connection and retries
/// a passing failure itself; a step the nest refuses for good ends it, and the step
/// answers the page's <c>error-message</c> text for that refusal
/// (<c>onboarding.md</c> § 3b → <i>The step completes; it is not fired once</i>).
/// <see cref="ApplyAsync"/> hands that text back for the caller to paint on the page
/// the person is on — never only logged. A connect or secret-decode failure, which
/// never reached the shared step, is logged and answers <c>null</c>: it is never
/// thrown into the handoff.</para>
///
/// <para>Lives in <c>FaunaApp.Core</c> so the hand-back is pinned by a unit test over
/// <see cref="INestRpcClient"/>; <c>App.ApplyPostClaimServingEnablementAsync</c> is the
/// WinUI half that paints it. No <c>ConfigureAwait(false)</c>: the caller is the
/// UI-thread launch glue.</para>
/// </summary>
internal static class ServingEnablement
{
    /// <summary>Run the shared step and answer the refusal text to paint on the
    /// current page's <c>error-message</c>, or <c>null</c> when nothing is owed.
    /// Never throws.</summary>
    internal static async Task<string?> ApplyAsync(
        INestRpcClient rpc, string nodeUrl, bool email, bool caldav, bool carddav, bool webdav)
    {
        try
        {
            var refusal = await rpc.ApplyPostClaimServingEnablementAsync(
                nodeUrl, email, caldav, carddav, webdav);
            if (refusal is not null)
            {
                ShellLog.Warn("ServingEnablement",
                    $"post-claim serving enablement: a step was refused for good: {refusal}");
            }
            return refusal;
        }
        catch (Exception ex)
        {
            ShellLog.Error("ServingEnablement",
                $"post-claim serving enablement failed: {ex.GetType().Name}: {ex.Message}");
            return null;
        }
    }
}
