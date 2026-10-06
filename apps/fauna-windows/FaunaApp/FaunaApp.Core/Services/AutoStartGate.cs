namespace FaunaApp.Core.Services;

/// <summary>
/// Pure auto-start registration decision (apps/windows.md § App Lifecycle →
/// Auto-start at sign-in — the shape-A residency ratification, 2026-07-16).
/// The registry mechanics live in <see cref="AutoStartService"/>; this is the
/// platform-agnostic policy it consults, so it is unit-testable in
/// FaunaApp.Tests (no WinUI/registry types). Mirrors the
/// <see cref="SingleInstanceGate"/> / <c>RestartManagerGate</c> pattern.
/// </summary>
public static class AutoStartGate
{
    /// <summary>
    /// Whether the universal post-auth hook should (re-)register the app's
    /// per-user Run-key entry for auto-start at Windows sign-in.
    /// </summary>
    /// <param name="isE2E">
    /// True when running under the E2E bridge (<c>FAUNA_E2E_BRIDGE</c> set).
    /// When true, never register — a harness login must not write the dev
    /// machine's real <c>HKCU\…\Run</c> key.
    /// </param>
    /// <param name="userChoice">
    /// The persisted tri-state choice (<c>AppSettingsStore.AutoStartChoice</c>):
    /// <c>null</c> = the user never chose → register by default (works
    /// out-of-the-box: sync + badges live at every sign-in with no manual
    /// step); <c>false</c> = an explicit opt-out → never re-register (the
    /// client UI is the one configuration surface, and an explicit choice is
    /// never overridden); <c>true</c> = an explicit opt-in → register (also
    /// self-heals a stale exe path after an install move/upgrade).
    /// </param>
    public static bool ShouldRegister(bool isE2E, bool? userChoice)
    {
        if (isE2E)
        {
            return false;
        }

        return ChoiceIsOn(userChoice);
    }

    /// <summary>
    /// What the Settings toggle SHOWS: the user's tri-state choice, defaulting ON — never
    /// the registration's existence (apps/windows.md § App Entry → Auto-start). The Run
    /// key can be absent for reasons that are not the user's choice (an e2e run never
    /// writes it, a fresh install has not reached its first login hook, Task Manager can
    /// disable the entry) and present for reasons that are not either, so reading it back
    /// made an explicit OFF read as ON again after a relaunch.
    /// </summary>
    /// <param name="userChoice">The persisted tri-state choice; <c>null</c> = never chose.</param>
    public static bool ChoiceIsOn(bool? userChoice) => userChoice != false;
}
