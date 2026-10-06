using System;
using System.Threading.Tasks;

namespace FaunaApp.Core.Services;

/// <summary>
/// The ordering every windows path that erases account-scoped stores must run
/// in (<c>account-scoping.md</c> § Concurrent instances — the agent bullet;
/// <c>sync-agent.md</c> § Control plane split → <i>The UnprovisionCapability
/// reply is a receipt</i>): await the sync agent's un-provision reply BEFORE
/// the erase, never a spawned fire-and-forget beside it, because the reply is
/// now the agent's own-mount-down receipt — an open handle under a still-
/// mounted store fails the erase outright on windows (os error 32), stranding
/// the signed-out account's data.
/// </summary>
internal static class ErasePrecondition
{
    /// <summary>
    /// Awaits <paramref name="unprovision"/> when <paramref name="shouldUnprovision"/>
    /// is true, then always runs <paramref name="erase"/> — <paramref name="unprovision"/>
    /// is best-effort by contract (it never throws), so an unreachable or refusing
    /// agent still lets the erase proceed (degrade open, the posture every other
    /// probe in this erase takes).
    /// </summary>
    internal static async Task AwaitUnprovisionThenErase(
        bool shouldUnprovision, Func<Task> unprovision, Action erase)
    {
        if (shouldUnprovision)
        {
            await unprovision();
        }
        erase();
    }

    /// <summary>
    /// The same ordering for a path that must tear its clients down
    /// SYNCHRONOUSLY, before the erase it hands back: <paramref name="unprovision"/>
    /// is STARTED first, then <paramref name="tearDown"/> runs, and the returned
    /// step awaits the un-provision's reply before <paramref name="erase"/>.
    ///
    /// <para><b>Why start before the teardown, not merely await before the
    /// erase.</b> The teardown (<c>DisposeNestClients</c>) drops the sync-agent
    /// session, and an un-provision that runs after it finds no session to tear
    /// down and sends the agent nothing — so the agent keeps its account runtime,
    /// and with it this account's <c>account-store.db</c>, open through the
    /// erase. Starting it first is safe because the un-provision claims the
    /// session synchronously, before its first await. The account switch learned
    /// this ordering first (<c>TearDownAndRelaunchAsync</c>'s step 4 ⚠ note); the
    /// e2e <c>reset</c>/<c>logout</c> arms had it backwards until this existed.</para>
    /// </summary>
    internal static Func<Task> BeginUnprovisionThenTearDown(
        bool shouldUnprovision, Func<Task> unprovision, Action tearDown, Action erase)
    {
        var unprovisioning = shouldUnprovision ? unprovision() : Task.CompletedTask;
        tearDown();
        return async () =>
        {
            await unprovisioning;
            erase();
        };
    }
}
