namespace FaunaApp.Core.Services;

/// <summary>
/// What app startup should do once it knows whether another instance is already
/// running. See <see cref="SingleInstanceGate"/>.
/// </summary>
public enum SingleInstanceDecision
{
    /// <summary>Become (or remain) the primary instance and start the UI.</summary>
    StartNormally,

    /// <summary>
    /// Another instance is already running — surface its window and exit THIS
    /// process. Never kill the running instance (a resident/tray app may hold
    /// unsaved compose state — apps/windows.md § App Lifecycle, principle 1).
    /// </summary>
    RedirectAndExit,
}

/// <summary>
/// Pure single-instance startup decision (apps/windows.md § App Lifecycle).
/// The Win32 mutex + window-activation mechanics live in the FaunaApp
/// presentation layer; this is the platform-agnostic policy it consults, so it
/// is unit-testable in FaunaApp.Tests (no WinUI types).
///
/// <para>The Windows twin of linux's NON_UNIQUE-in-e2e gate
/// (<c>apps/fauna-linux/src/main.rs</c>): production enforces a single instance
/// (a second launch activates the primary and bows out), but under the E2E
/// bridge the guard is disabled so the harness can run concurrent app processes
/// against different nests.</para>
/// </summary>
public static class SingleInstanceGate
{
    /// <summary>
    /// Decide what a launching process should do.
    /// </summary>
    /// <param name="isE2E">
    /// True when running under the E2E bridge (<c>FAUNA_E2E_BRIDGE</c> set). When
    /// true the guard is disabled — every launch starts normally as its own
    /// process, even if another is already running.
    /// </param>
    /// <param name="anotherInstanceRunning">
    /// True when another instance already holds the single-instance mutex (i.e.
    /// this process did not acquire it).
    /// </param>
    public static SingleInstanceDecision Decide(bool isE2E, bool anotherInstanceRunning)
    {
        if (isE2E)
        {
            return SingleInstanceDecision.StartNormally;
        }

        return anotherInstanceRunning
            ? SingleInstanceDecision.RedirectAndExit
            : SingleInstanceDecision.StartNormally;
    }
}
