using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit check for the single-instance startup decision
/// (apps/windows.md § App Lifecycle). The Win32 mutex + window-activation
/// mechanics live in the FaunaApp presentation layer (manual/integration
/// verified); this is the pure decision the mechanism consults — keyed on the
/// E2E gate (FAUNA_E2E_BRIDGE present) and whether another instance already
/// holds the single-instance mutex. The Windows twin of linux's
/// NON_UNIQUE-in-e2e gate (apps/fauna-linux/src/main.rs:54-67).
/// </summary>
public class SingleInstanceGateTests
{
    [Theory]
    // Production, first instance (no other running): become the primary, start normally.
    [InlineData(false, false, SingleInstanceDecision.StartNormally)]
    // Production, second launch (another instance already running): activate the
    // primary and exit THIS process — never kill the running instance (principle 1).
    [InlineData(false, true, SingleInstanceDecision.RedirectAndExit)]
    // Under the E2E bridge the guard is disabled — every launch is its own process,
    // even if another is already running (the harness runs concurrent app instances).
    [InlineData(true, false, SingleInstanceDecision.StartNormally)]
    [InlineData(true, true, SingleInstanceDecision.StartNormally)]
    public void Decide_FollowsTheGate(bool isE2E, bool anotherInstanceRunning, SingleInstanceDecision expected)
        => Assert.Equal(expected, SingleInstanceGate.Decide(isE2E, anotherInstanceRunning));

    [Fact]
    public void Decide_UnderE2E_NeverRedirects()
    {
        // The E2E bridge launches/relaunches multiple FaunaApp processes against
        // different nests; redirecting would collapse them to one and break tests.
        Assert.Equal(SingleInstanceDecision.StartNormally,
            SingleInstanceGate.Decide(isE2E: true, anotherInstanceRunning: true));
        Assert.Equal(SingleInstanceDecision.StartNormally,
            SingleInstanceGate.Decide(isE2E: true, anotherInstanceRunning: false));
    }

    [Fact]
    public void Decide_SecondProductionLaunch_RedirectsNotKills()
    {
        // Principle 1: the new launch bows out; it must not terminate the primary.
        Assert.Equal(SingleInstanceDecision.RedirectAndExit,
            SingleInstanceGate.Decide(isE2E: false, anotherInstanceRunning: true));
    }
}
