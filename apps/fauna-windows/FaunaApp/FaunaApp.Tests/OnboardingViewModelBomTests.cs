using System.Collections.Generic;
using System.Text.Json;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_onboarding_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Windows leg of the §6 top-region price summary — the "Bill of Materials"
/// pre-commit recap (<c>docs/goal/behavior/onboarding.md</c> § 6, the
/// <c>provisioning-price-bom</c> region).
///
/// WHICH template each line uses is shared-Rust's decision and is pinned there
/// (<c>fauna-onboarding-machine</c>'s <c>bom_domain_line_uses_bom_line_*</c> /
/// <c>bom_vps_line_uses_bom_line_recurring</c>). What these pin is the
/// **windows glue** the view actually binds: that the VM resolves the line
/// through <see cref="Strings.ResolveNested"/> — NOT plain <c>Resolve</c>,
/// since <c>{label}</c> is itself an i18n key and a plain resolve would paint
/// the raw key inside the rendered sentence — and that each line's Visibility
/// getter follows its own <c>Option</c> being present, so the domain line is
/// absent off the buy-a-new-domain path.
///
/// Constructs a real <see cref="OnboardingMachine"/> (the native fauna_ffi dll
/// loads in the test host) and seeds it through the same FFI-reachable
/// <c>call_machine_method</c> setters the cross-app e2e uses, so this test and
/// <c>test_provisioning_progress.py</c>/<c>test_bundled_provider.py</c> drive
/// one state path.
///
/// <para>
/// Serializes with the other <c>Strings.Initialize</c>-mutating test classes
/// under xUnit's default parallel-by-class runner — see
/// <c>StringsGlobalCollection</c>.
/// </para>
/// </summary>
[Collection("StringsGlobal")]
public class OnboardingViewModelBomTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private readonly Dictionary<string, string> _map;
        public FakeLocalizer(Dictionary<string, string> map) => _map = map;
        public string Get(string key) => _map.TryGetValue(key, out var v) ? v : key;
    }

    private sealed class FakeOnboardingObserver : OnboardingObserver
    {
        public void OnChanged() { }
    }

    // The three real en-US templates + the two step labels the {label} arg
    // resolves to (i18n/strings/en.yaml → Strings/en-US/Resources.resw).
    private static readonly Dictionary<string, string> Templates = new()
    {
        ["onboarding/nest_provisioning/bom_line"] = "{label}: {price}",
        ["onboarding/nest_provisioning/bom_line_recurring"] = "{label}: {price}/month",
        ["onboarding/provision/step/domain"] = "Domain",
        ["onboarding/provision/step/server"] = "Server",
    };

    private static OnboardingViewModel NewVm()
    {
        Strings.Initialize(new FakeLocalizer(Templates));
        return new OnboardingViewModel(
            new FakeOnboardingObserver(), new FakeAccountRegistry());
    }

    private static void SeedBuyableDomain(OnboardingViewModel vm, ulong priceCents, string currency)
        => vm.CallMachineMethod("set_dns_availability_for_test", JsonSerializer.Serialize(new
        {
            provider_id = "cloudflare",
            buy_domain = true,
            price_cents = priceCents,
            currency,
        }));

    private static void SeedSelectedServerType(OnboardingViewModel vm, ulong priceCents, string currency)
        => vm.CallMachineMethod("set_vps_state_for_test", JsonSerializer.Serialize(new
        {
            provider_id = "hetzner",
            server_types = new[]
            {
                new
                {
                    id = "cax21",
                    vcpu = 4,
                    mem_gb = 8.0,
                    disk_gb = 80,
                    price_monthly_cents = priceCents,
                    currency,
                },
            },
            selected_server_type_id = "cax21",
        }));

    [Fact]
    public void BothLines_AreAbsent_OnAFreshMachine()
    {
        var vm = NewVm();

        Assert.False(vm.ProvisioningBomDomainLineVisible);
        Assert.Equal("", vm.ProvisioningBomDomainLineText);
        Assert.False(vm.ProvisioningBomVpsLineVisible);
        Assert.Equal("", vm.ProvisioningBomVpsLineText);
    }

    /// <summary>The {label} arg is an i18n key, so a plain Resolve would leave
    /// "onboarding.provision.step.domain" sitting in the sentence. This is the
    /// whole reason the getter uses ResolveNested.</summary>
    [Fact]
    public void DomainLine_ResolvesTheLabelKey_NotJustTheTemplate()
    {
        var vm = NewVm();
        SeedBuyableDomain(vm, 1099, "USD");

        var text = vm.ProvisioningBomDomainLineText;
        Assert.True(vm.ProvisioningBomDomainLineVisible);
        Assert.StartsWith("Domain: ", text);
        Assert.Contains("10.99", text);
        Assert.DoesNotContain("onboarding.provision.step.domain", text);
    }

    [Fact]
    public void VpsLine_RendersTheRecurringTemplate_WithTheResolvedLabel()
    {
        var vm = NewVm();
        SeedSelectedServerType(vm, 599, "EUR");

        var text = vm.ProvisioningBomVpsLineText;
        Assert.True(vm.ProvisioningBomVpsLineVisible);
        Assert.StartsWith("Server: ", text);
        Assert.Contains("5.99", text);
        Assert.EndsWith("/month", text);
        Assert.DoesNotContain("onboarding.provision.step.server", text);
    }

    /// <summary>The domain line is the OPTIONAL one — onboarding.md § 6: shown
    /// only when the wizard is buying a new domain. Seeding a server type alone
    /// must not conjure it.</summary>
    [Fact]
    public void DomainLine_StaysAbsent_WhenOnlyTheVpsIsChosen()
    {
        var vm = NewVm();
        SeedSelectedServerType(vm, 599, "EUR");

        Assert.False(vm.ProvisioningBomDomainLineVisible);
        Assert.Equal("", vm.ProvisioningBomDomainLineText);
        Assert.True(vm.ProvisioningBomVpsLineVisible);
    }

    [Fact]
    public void BothLines_RenderTogether_OnTheBuyDomainPath()
    {
        var vm = NewVm();
        SeedBuyableDomain(vm, 1099, "USD");
        SeedSelectedServerType(vm, 599, "EUR");

        Assert.True(vm.ProvisioningBomDomainLineVisible);
        Assert.True(vm.ProvisioningBomVpsLineVisible);
        Assert.Contains("10.99", vm.ProvisioningBomDomainLineText);
        Assert.Contains("5.99", vm.ProvisioningBomVpsLineText);
    }
}
