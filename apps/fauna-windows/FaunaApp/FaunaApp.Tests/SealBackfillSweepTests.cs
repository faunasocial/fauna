using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.Helpers;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// The S8 session-start seal backfill. The D1-then-D3-skip-member sequencing itself now lives once in
/// the shared <c>fauna_client_folders::seal_backfill</c> sweep (own coverage
/// there); this pins only the glue's call + best-effort-logging contract
/// against the shared <see cref="MockNestRpcClient"/> fixture — never a real
/// nest.
/// </summary>
public class SealBackfillSweepTests
{
    private static FfiSealBackfillSweepReport QuietReport() =>
        new(fields: null, fieldsError: null, tags: new FfiTagSealBackfillReport(0, 0, 0),
            setsSwept: 2, memberSetsSkipped: 1, setFailures: 0, rosterError: null);

    private static FfiSealBackfillSweepReport NoteworthyReport() =>
        new(fields: null, fieldsError: "nest unreachable", tags: new FfiTagSealBackfillReport(0, 0, 0),
            setsSwept: 0, memberSetsSkipped: 0, setFailures: 0, rosterError: null);

    [Fact]
    public async Task RunAsync_calls_the_shared_sweep_exactly_once()
    {
        var mock = new MockNestRpcClient { NextSealBackfillSweepReport = QuietReport() };

        await SealBackfillSweep.RunAsync(mock);

        Assert.Equal(1, mock.RunSealBackfillSweepCalls);
    }

    [Fact]
    public async Task RunAsync_never_throws_on_a_noteworthy_report()
    {
        var mock = new MockNestRpcClient { NextSealBackfillSweepReport = NoteworthyReport() };

        // A fields_error / roster_error / failure count is best-effort by the
        // shared sweep's own contract (it never fails) — the glue only logs.
        await SealBackfillSweep.RunAsync(mock);
    }

    [Fact]
    public async Task RunAsync_never_throws_when_the_connection_faults()
    {
        var mock = new MockNestRpcClient
        {
            NextSealBackfillSweepReport = QuietReport(),
            NextError = "nest unreachable",
        };

        // Best-effort per the S8 rule: a faulted sweep must never surface into
        // the post-auth hook that fires it.
        await SealBackfillSweep.RunAsync(mock);
    }
}
