using System;
using System.Threading;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_onboarding_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// <see cref="OnboardingViewModel.SignOutResidue"/> — the state behind
/// <c>identity_choice</c>'s <c>sign-out-residue</c> view
/// (<c>docs/goal/architecture/apps/account-scoping.md</c> § Erasure follows
/// scope → <i>the residue surface</i>).
///
/// Pins what makes it a surface of its own rather than the sticky
/// <c>error-message</c> fallback it replaced: (1) it is state the wizard's
/// machine does not own, so no observer tick wipes it and it is never the
/// wizard's error; (2) setting it raises <c>PropertyChanged</c> synchronously;
/// (3) Remove Again paints exactly what the re-sweep left — nothing, when the
/// device is clean; (4) presses are serialized, so a press that lands while a
/// re-sweep is running runs over what that one left instead of being dropped.
/// </summary>
public class OnboardingViewModelSignOutResidueTests
{
    private sealed class FakeOnboardingObserver : OnboardingObserver
    {
        public void OnChanged() { }
    }

    private sealed class FakeResidue : ISignOutResidueSurface
    {
        public FakeResidue(string line) => Line = line;

        public string Line { get; }
        public Func<ISignOutResidueSurface?> OnRetry { get; init; } = () => null;
        public int Retries;

        public ISignOutResidueSurface? Retry()
        {
            Interlocked.Increment(ref Retries);
            return OnRetry();
        }
    }

    private const string Line =
        "Signed out, but 1 item(s) of your data could not be removed from this device";

    private static OnboardingViewModel NewVm() =>
        new(new FakeOnboardingObserver(), new FakeAccountRegistry());

    [Fact]
    public void FreshWizard_PaintsNoResidueView()
    {
        var vm = NewVm();

        Assert.False(vm.HasSignOutResidue);
        Assert.Null(vm.SignOutResidueMessage);
    }

    [Fact]
    public void AResidue_PaintsItsLine_AndIsNotTheWizardsError()
    {
        var vm = NewVm();

        vm.SignOutResidue = new FakeResidue(Line);

        Assert.True(vm.HasSignOutResidue);
        Assert.Equal(Line, vm.SignOutResidueMessage);
        // The residue rides its own view now: error-message stays the wizard's.
        Assert.False(vm.HasError);
        Assert.Null(vm.ErrorMessage);
    }

    [Fact]
    public void SettingTheResidue_RaisesPropertyChanged_SoTheViewRepaintsImmediately()
    {
        var vm = NewVm();
        var raised = false;
        vm.PropertyChanged += (_, _) => raised = true;

        vm.SignOutResidue = new FakeResidue(Line);

        Assert.True(raised);
    }

    [Fact]
    public async Task RemoveAgain_ClosesTheView_WhenTheResweepLeavesNothing()
    {
        var vm = NewVm();
        var residue = new FakeResidue(Line);
        vm.SignOutResidue = residue;

        await vm.RetrySignOutResidueAsync();

        Assert.Equal(1, residue.Retries);
        Assert.False(vm.HasSignOutResidue);
        Assert.Null(vm.SignOutResidueMessage);
    }

    [Fact]
    public async Task RemoveAgain_PaintsTheNewOutcome_WhenSomethingIsStillLeft()
    {
        var vm = NewVm();
        const string blocked = "Not removed — another Fauna window is using some of this data.";
        vm.SignOutResidue = new FakeResidue(Line) { OnRetry = () => new FakeResidue(blocked) };

        await vm.RetrySignOutResidueAsync();

        Assert.True(vm.HasSignOutResidue);
        Assert.Equal(blocked, vm.SignOutResidueMessage);
    }

    [Fact]
    public async Task RemoveAgain_WithNoResidue_DoesNothing()
    {
        var vm = NewVm();

        await vm.RetrySignOutResidueAsync();

        Assert.False(vm.HasSignOutResidue);
    }

    /// <summary>
    /// A re-sweep that threw has told us nothing about the disk, so the
    /// statement on screen is still the last true one.
    /// </summary>
    [Fact]
    public async Task RemoveAgain_KeepsTheView_WhenTheResweepThrows()
    {
        var vm = NewVm();
        vm.SignOutResidue = new FakeResidue(Line)
        {
            OnRetry = () => throw new InvalidOperationException("the seam failed"),
        };

        await vm.RetrySignOutResidueAsync();

        Assert.True(vm.HasSignOutResidue);
        Assert.Equal(Line, vm.SignOutResidueMessage);
    }

    /// <summary>
    /// The second press arrives while the first re-sweep is still running. It
    /// must not be dropped (the user pressed it after whatever they just fixed)
    /// and must not re-run the residue the first one is already replacing: it
    /// waits, then runs over what the first one left.
    /// </summary>
    [Fact]
    public async Task TwoPresses_AreSerialized_AndTheSecondRunsOverWhatTheFirstLeft()
    {
        var vm = NewVm();
        var firstRunning = new ManualResetEventSlim(false);
        var letFirstFinish = new ManualResetEventSlim(false);
        var second = new FakeResidue("still one left");
        var first = new FakeResidue(Line)
        {
            OnRetry = () =>
            {
                firstRunning.Set();
                letFirstFinish.Wait();
                return second;
            },
        };
        vm.SignOutResidue = first;

        var press1 = vm.RetrySignOutResidueAsync();
        Assert.True(firstRunning.Wait(TimeSpan.FromSeconds(30)), "the first re-sweep never started");
        var press2 = vm.RetrySignOutResidueAsync();
        Assert.False(press2.IsCompleted, "the second press must wait for the first re-sweep");
        letFirstFinish.Set();
        await Task.WhenAll(press1, press2);

        Assert.Equal(1, first.Retries);
        Assert.Equal(1, second.Retries);
        Assert.False(vm.HasSignOutResidue);
    }
}
