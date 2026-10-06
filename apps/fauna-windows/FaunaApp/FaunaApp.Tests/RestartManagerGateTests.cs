using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit check for the Restart-Manager cooperative-shutdown policy
/// (apps/windows.md § App Lifecycle, "Cooperative shutdown for installers").
/// The Win32 mechanism — RegisterApplicationRestart + a hidden top-level window
/// that receives the OS session-end messages — lives in the FaunaApp presentation
/// layer (RestartManagerService, manual/integration verified); this is the pure
/// decision it consults: whether to register for restart (gated off under the E2E
/// bridge, like single-instance) and whether a given window message is an OS
/// shutdown request. Pinning the WM_QUERYENDSESSION / WM_ENDSESSION constants here
/// guards the one thing the untestable Win32 glue can't: a typo'd message id.
/// </summary>
public class RestartManagerGateTests
{
    [Theory]
    // Production: register so an MSI/RM upgrade can close-and-relaunch us.
    [InlineData(false, true)]
    // Under the E2E bridge: do NOT register — the harness spawns/kills app
    // instances directly, and OS-restart registration would have Windows relaunch
    // test instances. Mirrors the single-instance E2E gate.
    [InlineData(true, false)]
    public void ShouldRegisterRestart_GatedOffUnderE2E(bool isE2E, bool expected)
        => Assert.Equal(expected, RestartManagerGate.ShouldRegisterRestart(isE2E));

    [Theory]
    // The two OS session-end messages a top-level window receives when the system
    // (or the Restart Manager) wants the app to close.
    [InlineData(0x0011, ShutdownClassification.ShutdownRequest)] // WM_QUERYENDSESSION
    [InlineData(0x0016, ShutdownClassification.ShutdownRequest)] // WM_ENDSESSION
    // Everything else is passed through to the default window proc untouched.
    [InlineData(0x0010, ShutdownClassification.Ignore)]          // WM_CLOSE (user clicks X)
    [InlineData(0x0002, ShutdownClassification.Ignore)]          // WM_DESTROY
    [InlineData(0x8000, ShutdownClassification.Ignore)]          // WM_APP (tray callback)
    public void Classify_OnlySessionEndIsAShutdownRequest(uint msg, ShutdownClassification expected)
        => Assert.Equal(expected, RestartManagerGate.Classify(msg));

    [Fact]
    public void Constants_MatchWin32SessionEndMessages()
    {
        // The presentation-layer WndProc keys off these; if a refactor drifts them
        // from the real Win32 values the cooperative shutdown silently stops firing
        // (the installer's taskkill is then the only — force-kill — fallback).
        Assert.Equal(0x0011u, RestartManagerGate.WmQueryEndSession);
        Assert.Equal(0x0016u, RestartManagerGate.WmEndSession);
    }
}
