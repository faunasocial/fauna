using System;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// What an identity succession hands across its own account switch
/// (<c>identity-succession.md</c> § The RecoveryKey → <i>At succession</i>) — the
/// windows twin of apple's <c>SuccessionHandoffTests.swift</c>.
///
/// <para><b>Serialized:</b> <see cref="SuccessionHandoff"/> is process-global
/// static state by design (its whole contract is outliving the objects a switch
/// rebuilds), so this class clears it around every test. It joins
/// <c>StringsGlobal</c> — not a collection of its own — because
/// <c>RecoveryKitViewModelTests</c> touches the same statics AND registers a
/// localizer, and xUnit parallelizes by class: two separate collections would let
/// that sibling's <c>ClearOnCredentialWipe</c> land between this class's
/// <c>Record</c> and its assertion.</para>
/// </summary>
[Collection("StringsGlobal")]
public class SuccessionHandoffTests : IDisposable
{
    public SuccessionHandoffTests() => SuccessionHandoff.ClearOnCredentialWipe();
    public void Dispose() => SuccessionHandoff.ClearOnCredentialWipe();

    private const string Predecessor = "11111111111111111111111111111111"
        + "11111111111111111111111111111111";
    private const string Successor = "22222222222222222222222222222222"
        + "22222222222222222222222222222222";

    [Fact]
    public void NothingIsOwed_BeforeAnyCeremony()
    {
        Assert.False(SuccessionHandoff.KitOwed);
        Assert.Null(SuccessionHandoff.SuccessorActorIdHex);
        Assert.Null(SuccessionHandoff.SweepStateJson);
        Assert.False(SuccessionHandoff.ClaimOwedKit(Successor));
    }

    /// <summary>
    /// A relaunch adoption (<c>succession-propagation.md</c> § Propagation → <i>Own
    /// device fleet</i>, the relaunch-adoption clause) owes the ceremony's closing
    /// obligations minus what only the ceremony had: the kit AND the group sweep,
    /// both bound to the successor, with no sweep report and no stamp carried.
    /// </summary>
    [Fact]
    public void RelaunchAdoption_OwesTheKitAndTheSweep_ToTheSuccessorOnly()
    {
        SuccessionHandoff.RecordRelaunchAdoption(Predecessor, Successor);

        Assert.True(SuccessionHandoff.KitOwed);
        Assert.Equal(Successor, SuccessionHandoff.SuccessorActorIdHex);
        Assert.Equal(Predecessor, SuccessionHandoff.PredecessorActorIdHex);
        Assert.Equal(Successor, SuccessionHandoff.SweepOwedTo);
        Assert.Null(SuccessionHandoff.SucceededAtUnix);
        Assert.Null(SuccessionHandoff.Sweep);
        Assert.Null(SuccessionHandoff.SweepStateJson);

        Assert.False(SuccessionHandoff.ClaimOwedSweep(Predecessor));
        Assert.True(SuccessionHandoff.ClaimOwedSweep(Successor));
        Assert.False(SuccessionHandoff.ClaimOwedSweep(Successor));
    }

    /// <summary>The owed sweep is its own binding: discharging the kit first (a
    /// busy view model deferred the sweep) must not leave the sweep owed to nobody.</summary>
    [Fact]
    public void RelaunchAdoption_KitClaimDoesNotSpendTheSweep()
    {
        SuccessionHandoff.RecordRelaunchAdoption(Predecessor, Successor);

        Assert.True(SuccessionHandoff.ClaimOwedKit(Successor));

        Assert.Equal(Successor, SuccessionHandoff.SweepOwedTo);
        Assert.True(SuccessionHandoff.ClaimOwedSweep(Successor));
    }

    [Fact]
    public void OwedSweep_RearmsForTheSuccessor_AndTheWipeClearsIt()
    {
        SuccessionHandoff.RecordRelaunchAdoption(Predecessor, Successor);
        Assert.True(SuccessionHandoff.ClaimOwedSweep(Successor));

        SuccessionHandoff.RearmOwedSweep(Successor);
        Assert.Equal(Successor, SuccessionHandoff.SweepOwedTo);

        SuccessionHandoff.ClearOnCredentialWipe();
        Assert.Null(SuccessionHandoff.SweepOwedTo);
        Assert.False(SuccessionHandoff.KitOwed);
    }

    /// <summary>A ceremony's own landing runs its sweep before its switch, so it owes none.</summary>
    [Fact]
    public void ALandedCeremony_OwesNoSweep()
    {
        SuccessionHandoff.Record(
            MockNestRpcClient.MakeLandedSuccession(newActorIdHex: Successor), Predecessor);

        Assert.Null(SuccessionHandoff.SweepOwedTo);
    }

    /// <summary>The successor owes itself a kit on BOTH arms of <c>persisted</c> —
    /// the account moved either way, so it is kitless and escrowless either way.</summary>
    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public void RecordOwesAKit_OnBothPersistArms(bool persisted)
    {
        var landed = MockNestRpcClient.MakeLandedSuccession(
            newActorIdHex: Successor, persisted: persisted, sweepStateJson: "{\"kind\":\"ran\"}");

        SuccessionHandoff.Record(landed, Predecessor);

        Assert.True(SuccessionHandoff.KitOwed);
        Assert.Equal(Successor, SuccessionHandoff.SuccessorActorIdHex);
        Assert.Equal(Predecessor, SuccessionHandoff.PredecessorActorIdHex);
        Assert.Equal("{\"kind\":\"ran\"}", SuccessionHandoff.SweepStateJson);
        Assert.Equal(1_700_000_000L, SuccessionHandoff.SucceededAtUnix);
    }

    /// <summary>
    /// The guard that makes the obligation survivable: the ceremony runs from a
    /// still-live Account page that outlives the teardown, so an unbound claim
    /// would be taken by the OUTGOING session — which then mints against a nest
    /// that has just revoked its bearers, fails, and leaves the flag spent.
    /// </summary>
    [Fact]
    public void TheDepartingIdentityCannotClaimTheOwedKit()
    {
        SuccessionHandoff.Record(
            MockNestRpcClient.MakeLandedSuccession(newActorIdHex: Successor), Predecessor);

        Assert.False(SuccessionHandoff.ClaimOwedKit(Predecessor));
        Assert.False(SuccessionHandoff.ClaimOwedKit(null));
        Assert.False(SuccessionHandoff.ClaimOwedKit(""));
        // Still owed — a refused claim must not consume the obligation.
        Assert.True(SuccessionHandoff.KitOwed);
        Assert.True(SuccessionHandoff.ClaimOwedKit(Successor));
    }

    /// <summary>Claimed once: a second visit to the section must not mint a second
    /// kit, which would register one nobody was shown.</summary>
    [Fact]
    public void TheOwedKitIsClaimedExactlyOnce()
    {
        SuccessionHandoff.Record(
            MockNestRpcClient.MakeLandedSuccession(newActorIdHex: Successor), Predecessor);

        Assert.True(SuccessionHandoff.ClaimOwedKit(Successor));
        Assert.False(SuccessionHandoff.ClaimOwedKit(Successor));
        Assert.False(SuccessionHandoff.KitOwed);
    }

    /// <summary>
    /// The sweep report outlives the claim: it is rendered AFTER the switch, and
    /// the two hand-offs are separate facts about the same ceremony. Discharging
    /// the kit must not take the sweep with it.
    /// </summary>
    [Fact]
    public void TheSweepReportSurvivesTheKitClaim()
    {
        SuccessionHandoff.Record(
            MockNestRpcClient.MakeLandedSuccession(
                newActorIdHex: Successor, sweepStateJson: "{\"kind\":\"no_engine\"}"),
            Predecessor);

        Assert.True(SuccessionHandoff.ClaimOwedKit(Successor));

        Assert.Equal("{\"kind\":\"no_engine\"}", SuccessionHandoff.SweepStateJson);
        Assert.Equal(1_700_000_000L, SuccessionHandoff.SucceededAtUnix);
    }

    /// <summary>Null <c>succeededAt</c> is the reconcile arm's real, honest answer —
    /// never a placeholder to be filled in later.</summary>
    [Fact]
    public void TheReconcileArmsMissingStampIsCarriedAsNull()
    {
        SuccessionHandoff.Record(
            MockNestRpcClient.MakeLandedSuccession(newActorIdHex: Successor, succeededAt: null),
            Predecessor);

        Assert.Null(SuccessionHandoff.SucceededAtUnix);
        Assert.True(SuccessionHandoff.KitOwed);
    }

    /// <summary>The credential wipe (sign-out / factory reset) is the ONE clear
    /// point: it destroys every identity on the box, so there is no successor left
    /// to owe a kit to.</summary>
    [Fact]
    public void TheCredentialWipeClearsEverything()
    {
        SuccessionHandoff.Record(
            MockNestRpcClient.MakeLandedSuccession(newActorIdHex: Successor), Predecessor);

        SuccessionHandoff.ClearOnCredentialWipe();

        Assert.False(SuccessionHandoff.KitOwed);
        Assert.Null(SuccessionHandoff.PredecessorActorIdHex);
        Assert.Null(SuccessionHandoff.SuccessorActorIdHex);
        Assert.Null(SuccessionHandoff.SucceededAtUnix);
        Assert.Null(SuccessionHandoff.SweepStateJson);
    }
}
