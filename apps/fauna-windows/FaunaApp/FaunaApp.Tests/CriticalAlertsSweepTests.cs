using System.Threading.Tasks;
using FaunaApp.Core.Helpers;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Pins <see cref="CriticalAlertsSweep"/>'s glue (<c>critical-alerts.md</c>
/// § Mechanism → <i>Who runs the detector</i>): the post-auth hook hands the sweep
/// to the shared entry with the nest's URL, on the session's own client, and never
/// throws. The split itself — loop, one pass, or a re-established loop on a rebuilt
/// client — is decided in shared Rust and pinned there
/// (<c>fauna_client_alert_sweep</c>'s <c>LoopClaim</c> tests).
/// </summary>
public class CriticalAlertsSweepTests
{
    [Fact]
    public async Task The_hook_hands_the_sweep_to_the_shared_entry_with_the_nest_url()
    {
        var rpc = new MockNestRpcClient();
        rpc.CriticalAlertSweepEnds.SetResult();

        await CriticalAlertsSweep.RunAsync(rpc, "https://nest.example");

        Assert.Contains("StartCriticalAlertSweep(https://nest.example)", rpc.Calls);
    }

    [Fact]
    public async Task A_sweep_that_fails_to_start_never_throws()
    {
        var rpc = new MockNestRpcClient { CriticalAlertSweepThrows = true };

        await CriticalAlertsSweep.RunAsync(rpc, "https://nest.example");

        Assert.Contains("StartCriticalAlertSweep(https://nest.example)", rpc.Calls);
    }
}
