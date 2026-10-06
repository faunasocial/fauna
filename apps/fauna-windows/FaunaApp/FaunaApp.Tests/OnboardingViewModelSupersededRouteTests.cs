using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_onboarding_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The launch arm for a <b>succeeded</b> identity (identity-succession.md
/// § Propagation → *Own device fleet*): the refusal lands the wizard on the import
/// page with the claim-free reason, set atomically with the step through the shared
/// <c>begin_import_identity_with_reason</c>, then upgrades it to name the successor
/// only once the chain walk VERIFIES one — or, when this device holds that verified
/// successor's key, adopts it (the lost-reply state; <c>identity-succession.md</c>
/// § Implementation status today). The windows twin of apple's
/// <c>SupersededLaunchRoute</c>. The resolver is injected so the claim-free window
/// is deterministic here — the e2e can only pin the stable, verified half.
///
/// <para>Joins <c>StringsGlobal</c>: the adoption writes the process-global
/// <see cref="SuccessionHandoff"/>, which <c>RecoveryKitViewModelTests</c> also
/// clears.</para>
/// </summary>
[Collection("StringsGlobal")]
public class OnboardingViewModelSupersededRouteTests : IDisposable
{
    private sealed class FakeOnboardingObserver : OnboardingObserver
    {
        public void OnChanged() { }
    }

    private const string Secret = "1111111111111111111111111111111111111111111111111111111111111111";
    private const string Claimed = "2222222222222222222222222222222222222222222222222222222222222222";
    private const string Verified = "3333333333333333333333333333333333333333333333333333333333333333";
    private const string Refused = "4444444444444444444444444444444444444444444444444444444444444444";

    public OnboardingViewModelSupersededRouteTests() => SuccessionHandoff.ClearOnCredentialWipe();
    public void Dispose() => SuccessionHandoff.ClearOnCredentialWipe();

    private static OnboardingViewModel NewVm(FakeAccountRegistry? registry = null) =>
        new(new FakeOnboardingObserver(), registry ?? new FakeAccountRegistry());

    [Fact]
    public async Task Refusal_LandsOnImport_WithClaimFreeReason_ThenNamesTheVerifiedSuccessor()
    {
        var vm = NewVm();
        var gate = new TaskCompletionSource<string?>();
        var route = vm.RouteSupersededRefusalAsync(
            Claimed, Secret, "https://nest.example", resolve: (_, _) => gate.Task);

        Assert.Equal(OnboardingStep.IdentityImport, vm.CurrentStep);
        // Before verification the message must NOT name any successor — the claim
        // is the nest's word, not proof.
        Assert.Equal(Strings.Get("onboarding/launch/identity_superseded"), vm.ErrorMessage);
        Assert.DoesNotContain(Claimed, vm.ErrorMessage);

        gate.SetResult(Verified);
        await route;
        Assert.Equal(
            Strings.Format("onboarding/launch/identity_superseded_verified", Verified),
            vm.ErrorMessage);
    }

    [Fact]
    public async Task UnverifiedWalk_LeavesTheClaimFreeReasonStanding()
    {
        var vm = NewVm();
        await vm.RouteSupersededRefusalAsync(
            Claimed, Secret, "https://nest.example",
            resolve: (_, _) => Task.FromResult<string?>(null));

        Assert.Equal(OnboardingStep.IdentityImport, vm.CurrentStep);
        Assert.Equal(Strings.Get("onboarding/launch/identity_superseded"), vm.ErrorMessage);
    }

    [Fact]
    public async Task NoSessionMaterial_SkipsTheWalk_ClaimFreeReasonIsFinal()
    {
        var vm = NewVm();
        var walked = false;
        await vm.RouteSupersededRefusalAsync(
            Claimed, secretHex: null, nestUrl: null,
            resolve: (_, _) => { walked = true; return Task.FromResult<string?>(Verified); });

        Assert.False(walked);
        Assert.Equal(OnboardingStep.IdentityImport, vm.CurrentStep);
        Assert.Equal(Strings.Get("onboarding/launch/identity_superseded"), vm.ErrorMessage);
    }

    [Fact]
    public async Task WalkLandingAfterTheUserLeftImport_DoesNotDragThemBack()
    {
        var vm = NewVm();
        var gate = new TaskCompletionSource<string?>();
        var route = vm.RouteSupersededRefusalAsync(
            Claimed, Secret, "https://nest.example", resolve: (_, _) => gate.Task);

        vm.BackCommand.Execute(null);
        var stepAfterLeaving = vm.CurrentStep;
        Assert.NotEqual(OnboardingStep.IdentityImport, stepAfterLeaving);

        gate.SetResult(Verified);
        await route;
        Assert.Equal(stepAfterLeaving, vm.CurrentStep);
    }

    // ── A held verified successor is adopted (the lost-reply relaunch) ────────

    /// <summary>
    /// The chain verified the successor AND this device holds its key: the shared
    /// decision records the link, the adoption owes the kit and the group sweep to
    /// the successor, and only then does the app switch to it.
    /// </summary>
    [Fact]
    public async Task HeldVerifiedSuccessor_IsAdopted_OwingKitAndSweep_ThenSwitchedTo()
    {
        var registry = new FakeAccountRegistry(active: Refused);
        registry.HeldSuccessors.Add(Verified);
        var vm = NewVm(registry);
        var switched = new List<string>();
        var owedAtSwitch = false;

        await vm.RouteSupersededRefusalAsync(
            Claimed, Secret, "https://nest.example",
            adopt: s =>
            {
                switched.Add(s);
                owedAtSwitch = SuccessionHandoff.KitOwed
                    && SuccessionHandoff.SweepOwedTo == Verified;
                return Task.CompletedTask;
            },
            resolve: (_, _) => Task.FromResult<string?>(Verified));

        Assert.Equal(new[] { (Refused, Verified) }, registry.Adoptions);
        Assert.Equal(new[] { Verified }, switched);
        Assert.True(owedAtSwitch, "the obligations must be recorded BEFORE the switch");
        Assert.Equal(Verified, SuccessionHandoff.SuccessorActorIdHex);
        Assert.Equal(Refused, SuccessionHandoff.PredecessorActorIdHex);
    }

    /// <summary>A verified successor this device does NOT hold: the import screen
    /// stays the answer, naming it — nothing adopted, nothing owed.</summary>
    [Fact]
    public async Task VerifiedSuccessorNotHeld_StaysOnImport_NothingOwed()
    {
        var registry = new FakeAccountRegistry(active: Refused);
        var vm = NewVm(registry);
        var switched = false;

        await vm.RouteSupersededRefusalAsync(
            Claimed, Secret, "https://nest.example",
            adopt: _ => { switched = true; return Task.CompletedTask; },
            resolve: (_, _) => Task.FromResult<string?>(Verified));

        Assert.False(switched);
        Assert.Empty(registry.Adoptions);
        Assert.False(SuccessionHandoff.KitOwed);
        Assert.Null(SuccessionHandoff.SweepOwedTo);
        Assert.Equal(
            Strings.Format("onboarding/launch/identity_superseded_verified", Verified),
            vm.ErrorMessage);
    }

    /// <summary>Never adopt what the chain did not verify: an unverified walk asks
    /// the registry nothing, even when the device holds the CLAIMED successor.</summary>
    [Fact]
    public async Task UnverifiedClaim_IsNeverAdopted_EvenWhenHeld()
    {
        var registry = new FakeAccountRegistry(active: Refused);
        registry.HeldSuccessors.Add(Claimed);
        var vm = NewVm(registry);
        var switched = false;

        await vm.RouteSupersededRefusalAsync(
            Claimed, Secret, "https://nest.example",
            adopt: _ => { switched = true; return Task.CompletedTask; },
            resolve: (_, _) => Task.FromResult<string?>(null));

        Assert.False(switched);
        Assert.Empty(registry.Adoptions);
        Assert.Equal(OnboardingStep.IdentityImport, vm.CurrentStep);
    }

    /// <summary>A walk landing after the user left the import page adopts nothing
    /// either — the same "don't drag them back" rule as the message upgrade.</summary>
    [Fact]
    public async Task HeldSuccessor_WalkLandingAfterTheUserLeft_AdoptsNothing()
    {
        var registry = new FakeAccountRegistry(active: Refused);
        registry.HeldSuccessors.Add(Verified);
        var vm = NewVm(registry);
        var gate = new TaskCompletionSource<string?>();
        var switched = false;
        var route = vm.RouteSupersededRefusalAsync(
            Claimed, Secret, "https://nest.example",
            adopt: _ => { switched = true; return Task.CompletedTask; },
            resolve: (_, _) => gate.Task);

        vm.BackCommand.Execute(null);
        gate.SetResult(Verified);
        await route;

        Assert.False(switched);
        Assert.Empty(registry.Adoptions);
    }
}
