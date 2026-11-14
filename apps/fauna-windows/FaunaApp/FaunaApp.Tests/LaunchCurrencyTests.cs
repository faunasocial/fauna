using FaunaApp.Core.Services;
using uniffi.fauna_launch_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// <c>onboarding.md</c> § App-launch routing → <i>A verdict renders only while its own
/// launch is still the current one</i> — windows' ownership test
/// (<see cref="LaunchCurrency"/>), the twin of linux's <c>launch_currency</c> unit test.
/// Each test drives its own instance, so the process-wide <see cref="LaunchCurrency.Shared"/>
/// the app reads is never touched.
/// </summary>
public class LaunchCurrencyTests
{
    /// <summary>A machine is only ever compared by reference here; no call reaches it.</summary>
    private sealed class FakeMachine : LaunchMachineFakeBase { }

    [Fact]
    public void TheHeldLaunchRoutesOnceAndAReplacedOneIsDropped()
    {
        var currency = new LaunchCurrency();
        var stale = new FakeMachine();
        var newer = new FakeMachine();

        currency.Begin(stale);
        currency.Begin(newer);

        Assert.False(currency.Deliver(stale));
        Assert.True(currency.Deliver(newer));
        // The verdict settled the launch, so nothing is in flight any more.
        Assert.False(currency.Deliver(newer));
        Assert.Equal((1L, 2L), (currency.Routed, currency.Superseded));
    }

    [Fact]
    public void ARetiredLaunchIsDroppedAndRetiringNothingIsANoOp()
    {
        var currency = new LaunchCurrency();
        var retired = new FakeMachine();

        currency.Begin(retired);
        Assert.True(currency.RetireInFlight());
        Assert.False(currency.Deliver(retired));

        // A settled launch holds nothing in flight, so a later supersession retires
        // nothing: the session's own machine keeps serving it.
        var settled = new FakeMachine();
        currency.Begin(settled);
        Assert.True(currency.Deliver(settled));
        Assert.False(currency.RetireInFlight());
        Assert.Equal((1L, 1L), (currency.Routed, currency.Superseded));
    }

    [Fact]
    public void ARetryOnTheHeldMachineIsItsOwnDrive()
    {
        // Retry and Trust re-send on the machine already on screen: each re-send
        // begins again, and a supersession while it is in flight drops it.
        var currency = new LaunchCurrency();
        var machine = new FakeMachine();

        currency.Begin(machine);
        Assert.True(currency.Deliver(machine));
        currency.Begin(machine);
        currency.RetireInFlight();
        Assert.False(currency.Deliver(machine));
        Assert.Equal((1L, 1L), (currency.Routed, currency.Superseded));
    }

    [Fact]
    public void TheObservableIsTheTwoCounters()
    {
        var currency = new LaunchCurrency();
        var machine = new FakeMachine();
        currency.Begin(machine);
        currency.Deliver(machine);

        var observable = currency.Observable;

        Assert.Equal(2, observable.Count);
        Assert.Equal(1L, observable["routed"]);
        Assert.Equal(0L, observable["superseded"]);
    }
}
