using System;
using FaunaApp.Core.Logs;

namespace FaunaApp.Core.Services;

/// <summary>
/// Per-user auto-start registration: the app's own <c>HKCU\…\CurrentVersion\Run</c>
/// entry, written as <c>"&lt;exe&gt;" --autostart</c> so a sign-in launch comes up
/// tray-resident (hidden) instead of opening a window over the desktop
/// (apps/windows.md § App Lifecycle → Auto-start at sign-in).
///
/// <para>Registration is (re-)ensured at the universal post-auth hook
/// (<c>App.StartMainAppAsync</c>) so the first successful login wires the app into
/// every subsequent Windows sign-in — which is what keeps the app-coupled sync
/// agent provisioned and the shell-ext badges live with no manual step. The
/// decision is the pure, unit-tested <see cref="AutoStartGate"/> (E2E-disabled;
/// an explicit user opt-out is never overridden); this class is only the registry
/// mechanics. All operations are best-effort and never throw — auto-start must
/// never disrupt login.</para>
/// </summary>
public static class AutoStartService
{
    private const string RunKeyPath = @"Software\Microsoft\Windows\CurrentVersion\Run";
    private const string RunValueName = "Fauna";

    // Via E2eEnv so the read is compiled out of release builds (convention 15);
    // the production twin returns null, so IsE2E is false exactly as before.
    private static bool IsE2E => E2eEnv.Bridge is not null;

    /// <summary>
    /// The Run-key command line for <paramref name="exePath"/>: quoted, with the
    /// <c>--autostart</c> flag so the sign-in launch is tray-resident (hidden).
    /// Pure — unit-tested in FaunaApp.Tests.
    /// </summary>
    public static string BuildRunValue(string exePath) => $"\"{exePath}\" --autostart";

    // No `IsRegistered()` on purpose: the Settings toggle shows the persisted CHOICE
    // (AutoStartGate.ChoiceIsOn), and reading the Run key back to seed it is the bug
    // that made an explicit OFF read as ON after a relaunch.

    /// <summary>
    /// Write (or refresh) the Run-key entry for the current executable.
    /// Re-writing on every login self-heals a moved/upgraded install's stale path
    /// and upgrades a pre-<c>--autostart</c> entry written by the old toggle.
    /// </summary>
    public static void Register(string? exePath = null)
    {
        // E2E guard on the mechanics too (belt to AutoStartGate's braces): a
        // harness-driven settings-autostart-toggle flip must not write the dev
        // machine's real HKCU Run key.
        if (!OperatingSystem.IsWindows() || IsE2E) return;
        try
        {
            exePath ??= Environment.ProcessPath;
            if (string.IsNullOrEmpty(exePath)) return;
            using var key = Microsoft.Win32.Registry.CurrentUser.OpenSubKey(RunKeyPath, true);
            key?.SetValue(RunValueName, BuildRunValue(exePath));
        }
        catch (Exception ex)
        {
            ShellLog.Debug("AutoStartService", $"register failed: {ex.Message}");
        }
    }

    /// <summary>Delete the Run-key entry (the explicit toggle-off path).</summary>
    public static void Unregister()
    {
        if (!OperatingSystem.IsWindows() || IsE2E) return;
        try
        {
            using var key = Microsoft.Win32.Registry.CurrentUser.OpenSubKey(RunKeyPath, true);
            key?.DeleteValue(RunValueName, false);
        }
        catch (Exception ex)
        {
            ShellLog.Debug("AutoStartService", $"unregister failed: {ex.Message}");
        }
    }

    /// <summary>
    /// The post-auth hook: consult <see cref="AutoStartGate"/> with the persisted
    /// tri-state choice and (re-)register when it says so. Best-effort; never
    /// throws into the login path.
    /// </summary>
    public static void EnsureRegisteredAtLogin(bool? userChoice)
    {
        try
        {
            if (AutoStartGate.ShouldRegister(IsE2E, userChoice))
            {
                Register();
            }
        }
        catch (Exception ex)
        {
            ShellLog.Debug("AutoStartService", $"ensure-registered failed: {ex.Message}");
        }
    }
}
