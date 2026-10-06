using System.IO;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Deterministic unit check for the persisted desktop app-behaviour preferences
/// (apps/windows.md § App Lifecycle). <see cref="AppSettingsStore"/> persists
/// the "Close to tray" bool (default ON since the 2026-07-16 shape-A residency
/// ratification — a window close must not silently stop sync/badges/toasts) and
/// the tri-state auto-start choice to a JSON file under the data dir — read by
/// the Settings → General toggles (VM), the TrayIconService close handler, and
/// the post-auth auto-start hook. A file store, NOT ApplicationData.LocalSettings
/// (unreliable for an unpackaged WinUI app).
/// </summary>
public class AppSettingsStoreTests
{
    private static string FreshDir()
    {
        var dir = Path.Combine(Path.GetTempPath(), "fauna-appsettingstest-" + System.Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(dir);
        return dir;
    }

    // Shape-A residency default: with no persisted choice the app stays
    // tray-resident on window close.
    [Fact]
    public void CloseToTray_DefaultsOn_WhenNoFile()
    {
        var dir = FreshDir();
        try { Assert.True(new AppSettingsStore(dir).CloseToTray); }
        finally { Directory.Delete(dir, recursive: true); }
    }

    // An explicit pre-flip (or post-flip) opt-out persists and is honoured.
    [Fact]
    public void CloseToTray_ExplicitOffPersistsAcrossInstances()
    {
        var dir = FreshDir();
        try
        {
            new AppSettingsStore(dir).CloseToTray = false;
            // A fresh store over the same dir reads the persisted value (next launch).
            Assert.False(new AppSettingsStore(dir).CloseToTray);
        }
        finally { Directory.Delete(dir, recursive: true); }
    }

    [Fact]
    public void CloseToTray_CanBeToggledBackOn()
    {
        var dir = FreshDir();
        try
        {
            var store = new AppSettingsStore(dir);
            store.CloseToTray = false;
            store.CloseToTray = true;
            Assert.True(new AppSettingsStore(dir).CloseToTray);
        }
        finally { Directory.Delete(dir, recursive: true); }
    }

    [Fact]
    public void CloseToTray_CorruptFile_DefaultsOnNotThrows()
    {
        var dir = FreshDir();
        try
        {
            File.WriteAllText(Path.Combine(dir, "app-settings.json"), "not json");
            Assert.True(new AppSettingsStore(dir).CloseToTray);
        }
        finally { Directory.Delete(dir, recursive: true); }
    }

    // A file that never mentioned CloseToTray (e.g. only future fields)
    // reads the new default, not false — the flip applies to users who never chose.
    [Fact]
    public void CloseToTray_FieldAbsentFromJson_ReadsNewDefaultOn()
    {
        var dir = FreshDir();
        try
        {
            File.WriteAllText(Path.Combine(dir, "app-settings.json"), "{}");
            Assert.True(new AppSettingsStore(dir).CloseToTray);
        }
        finally { Directory.Delete(dir, recursive: true); }
    }

    // Tri-state auto-start choice: absent = null (no explicit choice — the
    // post-auth hook registers by default per AutoStartGate).
    [Fact]
    public void AutoStartChoice_DefaultsNull_WhenNoFile()
    {
        var dir = FreshDir();
        try { Assert.Null(new AppSettingsStore(dir).AutoStartChoice); }
        finally { Directory.Delete(dir, recursive: true); }
    }

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public void AutoStartChoice_ExplicitChoicePersistsAcrossInstances(bool choice)
    {
        var dir = FreshDir();
        try
        {
            new AppSettingsStore(dir).AutoStartChoice = choice;
            Assert.Equal(choice, new AppSettingsStore(dir).AutoStartChoice);
        }
        finally { Directory.Delete(dir, recursive: true); }
    }

    // The two preferences share one file: writing one must not clobber the other.
    [Fact]
    public void WritingOnePreference_PreservesTheOther()
    {
        var dir = FreshDir();
        try
        {
            var store = new AppSettingsStore(dir);
            store.CloseToTray = false;
            store.AutoStartChoice = false;
            Assert.False(new AppSettingsStore(dir).CloseToTray);
            Assert.False(new AppSettingsStore(dir).AutoStartChoice);
        }
        finally { Directory.Delete(dir, recursive: true); }
    }
}
