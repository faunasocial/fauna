using System;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// The session-start S8 seal backfill call site (<c>file-sync.md</c> § Sealed
/// names &amp; paths → Implementation status today; apps row 228) — windows'
/// twin of apple's <c>FaunaClient.runSealBackfill</c> / android's
/// <c>MailEnableGlueVM.runSealBackfill</c>. The D1-then-D3-skip-member
/// sequencing lives once in the shared <c>fauna_client_folders::seal_backfill</c>
/// sweep (<see cref="INestRpcClient.RunSealBackfillSweepAsync"/>, which every
/// UniFFI app now calls instead of hand-rolling the loop); this glue only logs.
/// <para>
/// Same shape as <see cref="CriticalAlertsSweep"/>: never throws, safe to fire
/// unconditionally from the post-auth hook without awaiting sign-in on it.
/// </para>
/// </summary>
internal static class SealBackfillSweep
{
    /// <summary>
    /// Run the shared sweep once, then log — noteworthy (any error or failure)
    /// as a warning with the failure fields, quiet convergence as info. Call
    /// from the universal post-auth hook (<c>App.StartMainAppAsync</c>), after
    /// the connection is up. Never throws into its caller; the sweep itself is
    /// best-effort by contract (only a genuinely unreachable connection can
    /// throw here, and that is caught too).
    /// </summary>
    internal static async Task RunAsync(INestRpcClient rpc)
    {
        uniffi.fauna_ffi.FfiSealBackfillSweepReport report;
        try
        {
            report = await rpc.RunSealBackfillSweepAsync().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("SealBackfillSweep",
                $"sweep failed, best-effort: {ex.GetType().Name}: {ex.Message}");
            return;
        }

        var noteworthy = report.fieldsError is not null
            || report.rosterError is not null
            || report.setFailures > 0
            || report.tags.stampFailures > 0
            || (report.fields is { } fields
                && (fields.names > 0 || fields.updateFailures > 0 || fields.identityMismatch > 0));

        if (noteworthy)
        {
            ShellLog.Warn("SealBackfillSweep",
                $"sweep: fieldsError={report.fieldsError} rosterError={report.rosterError} " +
                $"setFailures={report.setFailures} tagStampFailures={report.tags.stampFailures}");
        }
        else
        {
            ShellLog.Info("SealBackfillSweep",
                $"sweep: ok (setsSwept={report.setsSwept}, memberSetsSkipped={report.memberSetsSkipped})");
        }
    }
}
