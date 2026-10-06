using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Auto-start at sign-in — the shape-A residency policy (apps/windows.md
/// § App Lifecycle → Auto-start at sign-in). The registry mechanics are
/// windows-glue; the decision (<see cref="AutoStartGate"/>) and the Run-key
/// command line (<see cref="AutoStartService.BuildRunValue"/>) are pure and
/// pinned here.
/// </summary>
public class AutoStartTests
{
    // Default-on: a user who never chose gets auto-start registered at the
    // post-auth hook (works out-of-the-box — sync + badges live at every
    // sign-in with no manual step).
    [Fact]
    public void Gate_NoChoice_Production_Registers()
        => Assert.True(AutoStartGate.ShouldRegister(isE2E: false, userChoice: null));

    // An explicit opt-out is never overridden (the client UI is the one
    // configuration surface).
    [Fact]
    public void Gate_ExplicitOptOut_Production_DoesNotRegister()
        => Assert.False(AutoStartGate.ShouldRegister(isE2E: false, userChoice: false));

    // An explicit opt-in re-registers (self-heals a stale exe path after an
    // install move/upgrade).
    [Fact]
    public void Gate_ExplicitOptIn_Production_Registers()
        => Assert.True(AutoStartGate.ShouldRegister(isE2E: false, userChoice: true));

    // The Settings toggle shows the CHOICE, tri-state, default ON — never the Run
    // key's existence (which an e2e run never writes and a fresh install has not
    // written yet): a never-chosen user reads ON, an explicit opt-out reads OFF.
    [Theory]
    [InlineData(null, true)]
    [InlineData(true, true)]
    [InlineData(false, false)]
    public void ChoiceIsOn_IsTheTriState_DefaultingOn(bool? choice, bool expected)
        => Assert.Equal(expected, AutoStartGate.ChoiceIsOn(choice));

    // Under the E2E bridge the hook must never write the machine's real
    // HKCU Run key, whatever the choice says.
    [Theory]
    [InlineData(null)]
    [InlineData(true)]
    [InlineData(false)]
    public void Gate_E2E_NeverRegisters(bool? choice)
        => Assert.False(AutoStartGate.ShouldRegister(isE2E: true, userChoice: choice));

    // The Run-key value quotes the exe (paths contain spaces) and carries
    // --autostart so the sign-in launch is tray-resident (hidden), not a
    // window over a fresh desktop.
    [Fact]
    public void RunValue_QuotesExeAndPassesAutostartFlag()
        => Assert.Equal(
            "\"C:\\Program Files\\Fauna\\app\\FaunaApp.exe\" --autostart",
            AutoStartService.BuildRunValue("C:\\Program Files\\Fauna\\app\\FaunaApp.exe"));
}
