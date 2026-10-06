using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using uniffi.fauna_provisioning;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Cross-language conformance for the shared provisioning step-row display
/// (<c>fauna_provisioning::progress::{status_glyph,step_label,substep_label}</c>,
/// per <c>docs/goal/behavior/value-formatting.md</c> § Provisioning step display).
/// These call the REAL UniFFI exports (<c>FaunaFfiMethods.Provisioning*</c>; the
/// native <c>fauna_ffi</c> dll loads in the test host — memory
/// <c>reference_windows_dotnet_test_loads_native_ffi</c>), mirroring the Rust unit
/// tests in <c>progress.rs</c>. Locks windows to the canonical glyph set and the
/// <c>onboarding.provision.step.*</c> / <c>substep.*</c> i18n keys (priority #2/#4)
/// so the step-row never re-derives them inline — and to the <c>{cause}</c>
/// substitution for <c>status_retrying</c> through the windows <see cref="Strings"/>
/// pipeline.
/// </summary>
[Collection("StringsGlobal")]
public class ProvisioningDisplayFfiTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        // Slash-keyed, {name} placeholders intact (Strings.Resolve substitutes by
        // name) — mirrors the generated windows resw for the provision step/substep
        // strings (en.yaml § onboarding.provision).
        private static readonly Dictionary<string, string> Map = new()
        {
            ["onboarding/provision/step/domain"] = "Domain",
            ["onboarding/provision/step/server"] = "Server",
            ["onboarding/provision/step/dns"] = "DNS",
            ["onboarding/provision/step/online"] = "Online",
            ["onboarding/provision/substep/server_creating"] = "Creating server",
            ["onboarding/provision/substep/status_retrying"] = "Retrying after error: {cause}",
        };

        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public ProvisioningDisplayFfiTests() => Strings.Initialize(new FakeLocalizer());

    [Fact]
    public void StatusGlyph_IsCanonicalSet()
    {
        // The unified set — linux/apple's `…/—/✗` (windows/web's prior `⟳/−/✕`
        // converge onto it). Pure FFI, no i18n.
        Assert.Equal("○", FaunaFfiMethods.ProvisioningStatusGlyph(StepStatus.Pending));
        Assert.Equal("…", FaunaFfiMethods.ProvisioningStatusGlyph(StepStatus.Running));
        Assert.Equal("—", FaunaFfiMethods.ProvisioningStatusGlyph(StepStatus.Skipped));
        Assert.Equal("✓", FaunaFfiMethods.ProvisioningStatusGlyph(StepStatus.Succeeded));
        Assert.Equal("✗", FaunaFfiMethods.ProvisioningStatusGlyph(StepStatus.Failed));
    }

    [Fact]
    public void StepLabel_MapsCanonicalProvisionKeys_AndResolves()
    {
        // The canonical key family is `onboarding.provision.step.*` (not the
        // duplicate `onboarding.nest_provisioning.step.*`).
        Assert.Equal("onboarding.provision.step.domain", FaunaFfiMethods.ProvisioningStepLabel(ProvisionStep.Domain).@key);
        Assert.Equal("onboarding.provision.step.online", FaunaFfiMethods.ProvisioningStepLabel(ProvisionStep.Online).@key);
        Assert.Empty(FaunaFfiMethods.ProvisioningStepLabel(ProvisionStep.Server).@args);

        // Through the windows resolver (dotted→slashed key lookup).
        Assert.Equal("Domain", ValueFormat.ProvisioningStepLabel(ProvisionStep.Domain));
        Assert.Equal("DNS", ValueFormat.ProvisioningStepLabel(ProvisionStep.Dns));
    }

    [Fact]
    public void SubstepLabel_MapsKey_AndResolves()
    {
        var lt = FaunaFfiMethods.ProvisioningSubstepLabel(SubstepKey.ServerCreating, null);
        Assert.Equal("onboarding.provision.substep.server_creating", lt.@key);
        Assert.Empty(lt.@args);
        Assert.Equal("Creating server", ValueFormat.ProvisioningSubstepLabel(SubstepKey.ServerCreating, null));
    }

    [Fact]
    public void SubstepLabel_Retrying_CarriesAndSubstitutesCause()
    {
        var lt = FaunaFfiMethods.ProvisioningSubstepLabel(SubstepKey.StatusRetrying, "boom");
        Assert.Equal("onboarding.provision.substep.status_retrying", lt.@key);
        Assert.Equal("boom", lt.@args["cause"]);
        // The {cause} placeholder is substituted by the windows resolver — the user
        // sees the real cause, not a literal "{cause}" (the bug this lift fixes).
        Assert.Equal("Retrying after error: boom", ValueFormat.ProvisioningSubstepLabel(SubstepKey.StatusRetrying, "boom"));
    }
}
