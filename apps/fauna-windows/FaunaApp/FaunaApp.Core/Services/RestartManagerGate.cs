namespace FaunaApp.Core.Services;

/// <summary>
/// How the Restart-Manager window proc should treat an incoming window message.
/// See <see cref="RestartManagerGate"/>.
/// </summary>
public enum ShutdownClassification
{
    /// <summary>Not a session-end message — pass it to the default window proc.</summary>
    Ignore,

    /// <summary>
    /// An OS / Restart-Manager session-end request (<c>WM_QUERYENDSESSION</c> or
    /// <c>WM_ENDSESSION</c>): persist unsaved compose state and close gracefully so
    /// an MSI install/upgrade can close-and-relaunch the app without force-killing
    /// it (apps/windows.md § App Lifecycle, principle 2).
    /// </summary>
    ShutdownRequest,
}

/// <summary>
/// Pure Restart-Manager cooperative-shutdown policy (apps/windows.md
/// § App Lifecycle, "Cooperative shutdown for installers"). The Win32 mechanism —
/// <c>RegisterApplicationRestart</c> + a persistent hidden top-level window that
/// receives the OS session-end messages — lives in the FaunaApp presentation layer
/// (<c>RestartManagerService</c>); this is the platform-agnostic decision it
/// consults, so it is unit-testable in FaunaApp.Tests (no WinUI / Win32 types).
///
/// <para>Restart registration is gated off under the E2E bridge for the same reason
/// single-instance is (<see cref="SingleInstanceGate"/>): the harness spawns and
/// kills app instances directly, and OS-restart registration would have Windows
/// relaunch test instances.</para>
/// </summary>
public static class RestartManagerGate
{
    /// <summary>
    /// <c>WM_QUERYENDSESSION</c> — sent to each top-level window when the session is
    /// ending (logoff / shutdown / a Restart-Manager <c>RmShutdown</c>).
    /// </summary>
    public const uint WmQueryEndSession = 0x0011;

    /// <summary>
    /// <c>WM_ENDSESSION</c> — follows <c>WM_QUERYENDSESSION</c> once every window has
    /// agreed; <c>wParam != 0</c> means the session is really ending.
    /// </summary>
    public const uint WmEndSession = 0x0016;

    /// <summary>
    /// Whether the app should register for OS restart at startup. False under the
    /// E2E bridge (<c>FAUNA_E2E_BRIDGE</c> set); true in production.
    /// </summary>
    public static bool ShouldRegisterRestart(bool isE2E) => !isE2E;

    /// <summary>
    /// Classify a window message as a session-end shutdown request or something to
    /// ignore (pass through to the default window proc).
    /// </summary>
    public static ShutdownClassification Classify(uint msg) =>
        msg is WmQueryEndSession or WmEndSession
            ? ShutdownClassification.ShutdownRequest
            : ShutdownClassification.Ignore;
}
