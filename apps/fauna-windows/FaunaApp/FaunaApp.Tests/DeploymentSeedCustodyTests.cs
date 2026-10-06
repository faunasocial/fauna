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
/// already does the work (the BR-2 seed→id refusal, the bounded fetch retry, the
/// account-plane merge), so the windows glue carries NO retry/BR-2 logic: it only
/// maps the <see cref="FfiDeploymentSeedSelfHeal"/> outcome to whether a
/// user-visible "recovery not protected" warning fires (the windows idiom of
/// android's <c>recoveryCustodyOutcomeOf</c>). These pin that pure map
/// (<see cref="DeploymentSeedCustody.MapOutcome"/>) and the thrown-call →
/// <see cref="RecoveryCustodyOutcome.NotProtectedFailed"/> path; the types are
/// UniFFI-<c>internal</c> so this rides <c>FaunaApp.Core</c>'s
/// <c>[InternalsVisibleTo]</c>.
/// </summary>
public class DeploymentSeedCustodyTests
{
    // The plane already holds a live entry — the steady state. Silent.
    [Fact]
    public void AlreadyCustodied_IsSilentOk() =>
        Assert.Equal(RecoveryCustodyOutcome.Ok,
            DeploymentSeedCustody.MapOutcome(new FfiDeploymentSeedSelfHeal.AlreadyCustodied()));

    // Not a roster admin (or the admin check failed) — nothing is owed. Silent.
    [Fact]
    public void NotAdmin_IsSilentOk() =>
        Assert.Equal(RecoveryCustodyOutcome.Ok,
            DeploymentSeedCustody.MapOutcome(new FfiDeploymentSeedSelfHeal.NotAdmin()));

    // The leg fetched the seed and merged it, found it already held, or hit the
    // EXPECTED multi-nest no-op (BR-1). A log, never a user-facing alarm.
    // (The enum is UniFFI-internal, so a public Theory cannot take it as a parameter.)
    [Fact]
    public void CapturedWithoutMismatch_IsSilentOk()
    {
        foreach (var capture in new[]
        {
            FfiDeploymentSeedCapture.Wrote,
            FfiDeploymentSeedCapture.AlreadyHeldSame,
            FfiDeploymentSeedCapture.RefusedDiffering,
        })
        {
            Assert.Equal(RecoveryCustodyOutcome.Ok,
                DeploymentSeedCustody.MapOutcome(new FfiDeploymentSeedSelfHeal.Captured(capture)));
        }
    }

    // RefusedMismatch (BR-2) = the box handed off a seed that does NOT derive to its
    // pinned identity — custodying it would re-instantiate a different identity every
    // TOFU-pinned client rejects. Loud "recovery not protected" warning.
    [Fact]
    public void CapturedRefusedMismatch_WarnsMismatch() =>
        Assert.Equal(RecoveryCustodyOutcome.NotProtectedMismatch,
            DeploymentSeedCustody.MapOutcome(
                new FfiDeploymentSeedSelfHeal.Captured(FfiDeploymentSeedCapture.RefusedMismatch)));

    // Custody owed but unconfirmed this connect (fetch/store failed, or the nest
    // holds no seed): the banner says recovery is not confirmed protected.
    [Fact]
    public void HandoffUnavailable_WarnsNotProtectedFailed() =>
        Assert.Equal(RecoveryCustodyOutcome.NotProtectedFailed,
            DeploymentSeedCustody.MapOutcome(new FfiDeploymentSeedSelfHeal.HandoffUnavailable()));

    [Fact]
    public void NestHoldsNoSeed_WarnsNotProtectedFailed() =>
        Assert.Equal(RecoveryCustodyOutcome.NotProtectedFailed,
            DeploymentSeedCustody.MapOutcome(new FfiDeploymentSeedSelfHeal.NestHoldsNoSeed()));

    // transport.md: one authenticated WebSocket per actor — the leg rides the
    // session's own client and builds no connection of its own.
    [Fact]
    public async Task SelfHealAsync_RidesTheSessionClientAndMapsTheOutcome()
    {
        var rpc = new MockNestRpcClient
        {
            NextDeploymentSeedSelfHeal = new FfiDeploymentSeedSelfHeal.HandoffUnavailable(),
        };

        var outcome = await DeploymentSeedCustody.SelfHealAsync(rpc);

        Assert.Equal(RecoveryCustodyOutcome.NotProtectedFailed, outcome);
        Assert.Equal(new[] { "SelfHealDeploymentSeedCustody" }, rpc.Calls);
    }

    // A thrown leg is an unconfirmed custody, never a silent drop (android's
    // precedent) — the post-auth hook still raises the banner.
    [Fact]
    public async Task SelfHealAsync_AThrownCallIsNotProtectedFailed()
    {
        var rpc = new MockNestRpcClient { NextDeploymentSeedSelfHeal = null };

        var outcome = await DeploymentSeedCustody.SelfHealAsync(rpc);

        Assert.Equal(RecoveryCustodyOutcome.NotProtectedFailed, outcome);
    }

    [Fact]
    public async Task SelfHealAsync_ACustodiedBoxIsSilent()
    {
        var outcome = await DeploymentSeedCustody.SelfHealAsync(new MockNestRpcClient());

        Assert.Equal(RecoveryCustodyOutcome.Ok, outcome);
    }
}
