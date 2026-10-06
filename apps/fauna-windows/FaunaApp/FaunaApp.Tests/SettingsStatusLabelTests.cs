using FaunaApp.Core.Services;
using uniffi.fauna_client_mail_settings;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Cross-language conformance for the mail-settings status indicator, single-sourced
/// in shared Rust (<c>fauna_client_mail_settings::settings_status_label</c> via the
/// crate-level UniFFI export; mail-settings.md § the status indicator). Calls the
/// REAL export (native dll loads in the test host — memory
/// <c>reference_windows_dotnet_test_loads_native_ffi</c>) and resolves the returned
/// <c>LocalizedText</c> through the production <see cref="Strings"/> pipeline with a
/// fake localizer mirroring the windows resw. Locks the <c>Idle</c>-gated-on-<c>enabled</c>
/// mapping and the <c>RotationInProgress</c> <c>{count}</c> substitution that the old
/// per-app switch + <c>RotationText</c> helper hand-rolled (priority #2/#4).
/// </summary>
[Collection("StringsGlobal")]
public class SettingsStatusLabelTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["settings/mail/status_enabled"] = "Enabled",
            ["settings/mail/status_disabled"] = "Disabled",
            ["settings/mail/status_syncing"] = "Syncing…",
            ["settings/mail/status_rotation"] = "{count} remaining",
        };

        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public SettingsStatusLabelTests() => Strings.Initialize(new FakeLocalizer());

    private static string Resolve(SettingsStatus status, bool enabled) =>
        Strings.Resolve(FaunaClientMailSettingsMethods.SettingsStatusLabel(status, enabled));

    [Fact]
    public void Idle_GatesOnEnabled()
    {
        Assert.Equal("Enabled", Resolve(new SettingsStatus.Idle(), enabled: true));
        Assert.Equal("Disabled", Resolve(new SettingsStatus.Idle(), enabled: false));
    }

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public void Syncing_IgnoresEnabled(bool enabled)
    {
        Assert.Equal("Syncing…", Resolve(new SettingsStatus.Syncing(), enabled));
    }

    [Fact]
    public void RotationInProgress_SubstitutesRemainingCount()
    {
        // The remaining-count rides the LocalizedText {count} arg (the RotationText
        // helper used to do the .Replace by hand).
        Assert.Equal("3 remaining", Resolve(new SettingsStatus.RotationInProgress(3UL), enabled: true));
        Assert.Equal("0 remaining", Resolve(new SettingsStatus.RotationInProgress(0UL), enabled: false));
    }
}
