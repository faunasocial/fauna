using System.Collections.Generic;
using System.Threading.Tasks;
using FaunaApp.Core.Helpers;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The deployment-seed custody leg's windows glue — box-recovery.md § The
/// plane-era recovery floor, <i>(c) The writes</i>. The shared leg
/// (<c>self_heal_deployment_seed_custody</c>, run at store-ready by the
/// account-runtime seat and at every connect by <c>App.StartMainAppAsync</c>)
/// already does the work AND chooses the warning it owes
/// (<c>fauna_client_config::custody_leg_warning</c>, tested there), so the
/// windows glue only passes that <c>LocalizedText</c> on — and answers the
/// shared "not attempted" warning when the leg could not even be reached. The
/// types are UniFFI-<c>internal</c> so this rides <c>FaunaApp.Core</c>'s
/// <c>[InternalsVisibleTo]</c>.
/// </summary>
public class DeploymentSeedCustodyTests
{
    // transport.md: one authenticated WebSocket per actor — the leg rides the
    // session's own client and builds no connection of its own; the shared
    // warning comes back untouched.
    [Fact]
    public async Task SelfHealAsync_RidesTheSessionClientAndPassesTheSharedWarningOn()
    {
        var warning = new uniffi.fauna_core.LocalizedText(
            "launch.recovery_custody_mismatch", new Dictionary<string, string>());
        var rpc = new MockNestRpcClient { NextDeploymentSeedWarning = warning };

        var answered = await DeploymentSeedCustody.SelfHealAsync(rpc);

        Assert.Equal(warning, answered);
        Assert.Equal(new[] { "SelfHealDeploymentSeedCustody" }, rpc.Calls);
    }

    // A leg the glue could not reach is an unconfirmed custody, never a silent
    // drop — and its wording is the shared Rust one, not a key picked here.
    [Fact]
    public async Task SelfHealAsync_AThrownCallAnswersTheSharedUnattemptedWarning()
    {
        var rpc = new MockNestRpcClient { DeploymentSeedCustodyThrows = true };

        var answered = await DeploymentSeedCustody.SelfHealAsync(rpc);

        // LocalizedText is a record over a Dictionary: record equality compares the
        // args by reference, so compare the key and the args' contents.
        var expected = FaunaFfiMethods.DeploymentSeedCustodyUnattemptedWarning();
        Assert.NotNull(answered);
        Assert.Equal(expected.key, answered!.key);
        Assert.Equal(expected.args, answered.args);
    }

    [Fact]
    public async Task SelfHealAsync_NothingOwedIsSilent()
    {
        var answered = await DeploymentSeedCustody.SelfHealAsync(new MockNestRpcClient());

        Assert.Null(answered);
    }
}
