using FaunaApp.Core.Helpers;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Pins <see cref="CriticalAlertsSweep.StartForIdentity"/>'s loop/one-shot split
/// (<c>critical-alerts.md</c> § Mechanism → <i>How often the detector runs</i>):
/// a first or identity-changing sign-in claims the re-sweep LOOP, a same-identity
/// re-establish does not (it gets one pass — a second loop would stack a sweeper
/// per re-auth), and both a teardown and the loop's own exit hand the next sign-in
/// a fresh loop. Decision only — no FFI, no network.
/// <para>In <c>ActorScopedStaticsGlobal</c> because <c>ActorScopeTests</c>' drop
/// clears the same record (<c>ActorScope.DropActorScopedState</c> →
/// <see cref="CriticalAlertsSweep.ForgetLoop"/>).</para>
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class CriticalAlertsSweepTests
{
    [Fact]
    public void A_first_sign_in_claims_the_loop_and_a_same_identity_re_auth_does_not()
    {
        CriticalAlertsSweep.ForgetLoop();
        Assert.NotNull(CriticalAlertsSweep.ClaimLoop("a@nest1"));
        Assert.Null(CriticalAlertsSweep.ClaimLoop("a@nest1"));
    }

    [Fact]
    public void Another_identity_or_nest_claims_a_loop_of_its_own()
    {
        CriticalAlertsSweep.ForgetLoop();
        Assert.NotNull(CriticalAlertsSweep.ClaimLoop("a@nest1"));
        Assert.NotNull(CriticalAlertsSweep.ClaimLoop("b@nest1"));
        Assert.NotNull(CriticalAlertsSweep.ClaimLoop("b@nest2"));
    }

    [Fact]
    public void A_teardown_hands_the_next_sign_in_a_fresh_loop()
    {
        CriticalAlertsSweep.ForgetLoop();
        Assert.NotNull(CriticalAlertsSweep.ClaimLoop("a@nest1"));
        CriticalAlertsSweep.ForgetLoop();
        Assert.NotNull(CriticalAlertsSweep.ClaimLoop("a@nest1"));
    }

    /// <summary>
    /// The drop's two halves (<c>critical-alerts.md</c> § Mechanism → <i>Lifetime</i>:
    /// "session" means identity). An identity teardown forgets the loop; a
    /// same-identity client rebuild (a re-point, a re-establish) keeps it — the
    /// identity lives on, and so do its alerts and the loop re-checking them.
    /// </summary>
    [Fact]
    public void Only_an_identity_teardown_forgets_the_loop()
    {
        CriticalAlertsSweep.ForgetLoop();
        Assert.NotNull(CriticalAlertsSweep.ClaimLoop("a@nest1"));

        Core.Services.ActorScope.DropActorScopedState(identityEnds: false);
        Assert.Null(CriticalAlertsSweep.ClaimLoop("a@nest1"));

        Core.Services.ActorScope.DropActorScopedState(identityEnds: true);
        Assert.NotNull(CriticalAlertsSweep.ClaimLoop("a@nest1"));
    }

    [Fact]
    public void A_loop_that_ended_hands_the_next_sign_in_a_fresh_loop_but_a_late_one_does_not()
    {
        CriticalAlertsSweep.ForgetLoop();
        var departed = CriticalAlertsSweep.ClaimLoop("a@nest1")!;
        var current = CriticalAlertsSweep.ClaimLoop("b@nest1")!;

        // The departed identity's loop ending late must not clear its successor's.
        CriticalAlertsSweep.ReleaseLoop(departed);
        Assert.Null(CriticalAlertsSweep.ClaimLoop("b@nest1"));

        // The live loop ending does.
        CriticalAlertsSweep.ReleaseLoop(current);
        Assert.NotNull(CriticalAlertsSweep.ClaimLoop("b@nest1"));
    }
}
