using System.IO;
using System.Text.Json;

namespace FaunaApp.Core.Services;

/// <summary>
/// Persisted desktop app-behaviour preferences, stored as a small JSON file under
/// the data dir (<c>%LocalAppData%\Fauna\app-settings.json</c>). Read by BOTH the
/// Settings → General "Close to tray" toggle (<c>SettingsViewModel</c>) and the
/// <c>TrayIconService</c> window-close handler, so it lives in <c>FaunaApp.Core</c>
/// where both the presentation layer and any future shared consumer reach it.
///
/// <para>A file store, NOT <c>ApplicationData.LocalSettings</c> — the latter is
/// unreliable for an unpackaged WinUI app (<c>App.xaml.cs</c> guards every access
/// to it with try/catch). Reads/writes are best-effort and never throw: a corrupt
/// or missing file reads as the default.</para>
///
/// <para>The "Close to tray" default is ON (the 2026-07-16 shape-A residency
/// ratification — a window close must not silently stop file sync / badges /
/// toasts; the deliberate stop is the tray Quit). An explicitly persisted
/// <c>false</c> from before the flip stays honoured. The linux default flip is
/// entrusted (advisory, host-gated) — apps/windows.md § App Lifecycle.</para>
/// </summary>
public sealed class AppSettingsStore
{
    private readonly string _path;

    /// <param name="dataDir">
    /// The directory the settings file lives in. Defaults to
    /// <see cref="BackupPaths.DataDir"/> (<c>%LocalAppData%\Fauna</c>) in production;
    /// tests inject a temp dir.
    /// </param>
    public AppSettingsStore(string? dataDir = null)
        => _path = Path.Combine(dataDir ?? BackupPaths.DataDir, "app-settings.json");

    /// <summary>
    /// When true (the default), closing the main window hides it to the system
    /// tray — the process stays resident so file sync, shell-ext badges, and
    /// toasts keep working; when false (an explicit opt-out), closing the window
    /// quits the process. apps/windows.md § App Lifecycle.
    /// </summary>
    public bool CloseToTray
    {
        get => Read().CloseToTray;
        set => Write(Read() with { CloseToTray = value });
    }

    /// <summary>
    /// Tri-state auto-start-at-sign-in choice (apps/windows.md § App Lifecycle
    /// → Auto-start at sign-in): <c>null</c> = never explicitly chosen (the
    /// post-auth hook registers by default), <c>true</c>/<c>false</c> = the
    /// user's explicit toggle. Consulted by <see cref="AutoStartGate"/>; an
    /// explicit opt-out is never overridden.
    /// </summary>
    public bool? AutoStartChoice
    {
        get => Read().AutoStartChoice;
        set => Write(Read() with { AutoStartChoice = value });
    }

    private AppSettingsData Read()
    {
        try
        {
            if (!File.Exists(_path)) return new AppSettingsData();
            return JsonSerializer.Deserialize<AppSettingsData>(File.ReadAllText(_path))
                   ?? new AppSettingsData();
        }
        catch
        {
            return new AppSettingsData();
        }
    }

    private void Write(AppSettingsData data)
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(_path)!);
            File.WriteAllText(_path, JsonSerializer.Serialize(data));
        }
        catch
        {
            // Best-effort: a failed write must never crash the app or the toggle.
        }
    }

    private sealed record AppSettingsData
    {
        // Default ON (shape A residency): a persisted
        // explicit false keeps it (JSON present wins over the init default).
        public bool CloseToTray { get; init; } = true;

        // null = absent from JSON = no explicit choice yet.
        public bool? AutoStartChoice { get; init; }
    }
}
