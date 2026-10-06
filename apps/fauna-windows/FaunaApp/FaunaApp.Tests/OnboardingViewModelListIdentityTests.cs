using System.Text.Json;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_onboarding_machine;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The invariant this file pins: <b>a collection getter the view binds
/// <c>OneWay</c> returns the SAME instance while its content is unchanged.</b>
///
/// <para>Why it needs pinning, and why it is not a micro-optimisation.
/// <c>VpsConfigView</c> (like <c>DnsConfigView</c>) subscribes to the VM's
/// <c>PropertyChanged</c> and calls <c>Bindings.Update()</c> on <b>every observer
/// tick</b> — that is the windows pattern for repainting an <c>x:Bind</c> page, and
/// it is correct. What it means for the VM is that every <c>OneWay</c> source is
/// re-read many times a second. A getter that materialises a fresh
/// <c>List</c> per read therefore hands <c>ItemsSource</c> a NEW OBJECT every tick,
/// and WinUI compares by reference: the control tears down and rebuilds every row,
/// continuously, forever.</para>
///
/// <para>What that costs is not frames, it is the <b>automation surface</b>. With
/// the rows being rebuilt underneath it, UIA cannot answer questions about them:
/// every cross-process call against the app pays a ~2 s provider timeout. The e2e
/// bridge's own per-phase trace caught it exactly —
/// <c>scroll-into-view vps-server-type-radio[0]: threw in 1998ms … 2000ms … 2012ms</c>,
/// over and over — and then <c>Application.GetMainWindow</c>, which EVERY element
/// route re-resolves before it searches, timed out outright and failed the run.
/// A page can look perfectly fine to a human and be undrivable.</para>
///
/// <para>The same class had already been paid for once, one page over: the
/// credentials form's <c>Fields</c> DP fired on every tick because
/// <c>VisibleDnsFields</c> was a fresh array per read, and the rebuild destroyed the
/// hosted-auth button mid-<c>await</c>. Two instances is a pattern, so this pins the
/// property rather than the page.</para>
///
/// <para>These are ~2 s to run and need no WinUI, no UIA and no e2e — the cost split
/// that matters, since the only other way to notice is an ~18 min journey run
/// failing somewhere unrelated.</para>
/// </summary>
public class OnboardingViewModelListIdentityTests
{
    private sealed class FakeOnboardingObserver : OnboardingObserver
    {
        public void OnChanged() { }
    }

    private static OnboardingViewModel NewVm()
        => new(new FakeOnboardingObserver(), new FakeAccountRegistry());

    /// <summary>Seed the VPS catalog the way the cross-app e2e does — through the
    /// FFI-reachable test setter, so this test and the journey drive one state
    /// path. Three plans, all ≥2 GB so the mail-mode RAM gate admits them.</summary>
    private static void SeedCatalog(OnboardingViewModel vm, string selected = "cax21")
        => vm.CallMachineMethod("set_vps_state_for_test", JsonSerializer.Serialize(new
        {
            provider_id = "hetzner",
            server_types = new[]
            {
                new { id = "cax11", vcpu = 2, mem_gb = 4.0, disk_gb = 40, price_monthly_cents = 399UL, currency = "EUR" },
                new { id = "cax21", vcpu = 4, mem_gb = 8.0, disk_gb = 80, price_monthly_cents = 599UL, currency = "EUR" },
                new { id = "cax31", vcpu = 8, mem_gb = 16.0, disk_gb = 160, price_monthly_cents = 999UL, currency = "EUR" },
            },
            selected_server_type_id = selected,
        }));

    [Fact]
    public void VpsServerTypes_ReturnsTheSameInstance_WhenTheCatalogHasNotChanged()
    {
        var vm = NewVm();
        SeedCatalog(vm);

        var first = vm.VpsServerTypes;
        Assert.NotEmpty(first);

        // Twenty reads stands in for twenty observer ticks: the view re-reads this
        // source on each one, and any of them handing back a new object is the
        // defect.
        for (var i = 0; i < 20; i++)
            Assert.Same(first, vm.VpsServerTypes);
    }

    [Fact]
    public void VpsServerTypes_ReturnsANewInstance_WhenTheCATALOGActuallyChanges()
    {
        var vm = NewVm();
        SeedCatalog(vm);
        var first = vm.VpsServerTypes;

        // A different catalog must still produce a real rebuild — identity stability
        // may never become staleness. `record` structural equality is what tells the
        // two apart, so a changed FIELD (not just a changed id) has to count.
        vm.CallMachineMethod("set_vps_state_for_test", JsonSerializer.Serialize(new
        {
            provider_id = "hetzner",
            server_types = new[]
            {
                new { id = "cax11", vcpu = 2, mem_gb = 4.0, disk_gb = 40, price_monthly_cents = 399UL, currency = "EUR" },
                new { id = "cax21", vcpu = 4, mem_gb = 8.0, disk_gb = 80, price_monthly_cents = 777UL, currency = "EUR" },
                new { id = "cax31", vcpu = 8, mem_gb = 16.0, disk_gb = 160, price_monthly_cents = 999UL, currency = "EUR" },
            },
            selected_server_type_id = "cax21",
        }));

        Assert.NotSame(first, vm.VpsServerTypes);
    }

    /// <summary>The mail-mode toggle RAM-gates the list, so flipping it is a genuine
    /// content change in one direction and a genuine restoration in the other. Both
    /// must be observed — a cache keyed on something coarser than the shown set would
    /// pass the "unchanged" test above and still paint the wrong plans.</summary>
    [Fact]
    public void VpsServerTypes_TracksTheMailModeRamGate_InBothDirections()
    {
        var vm = NewVm();
        vm.CallMachineMethod("set_vps_state_for_test", JsonSerializer.Serialize(new
        {
            provider_id = "hetzner",
            server_types = new[]
            {
                new { id = "tiny", vcpu = 1, mem_gb = 1.0, disk_gb = 20, price_monthly_cents = 199UL, currency = "EUR" },
                new { id = "cax21", vcpu = 4, mem_gb = 8.0, disk_gb = 80, price_monthly_cents = 599UL, currency = "EUR" },
            },
            selected_server_type_id = (string?)null,
        }));

        vm.SetProvisionMailMode(false);
        var withTiny = vm.VpsServerTypes;
        Assert.Contains(withTiny, s => s.Id == "tiny");

        vm.SetProvisionMailMode(true);
        var gated = vm.VpsServerTypes;
        Assert.NotSame(withTiny, gated);
        Assert.DoesNotContain(gated, s => s.Id == "tiny");
        Assert.Same(gated, vm.VpsServerTypes);   // and stable again at the new content

        vm.SetProvisionMailMode(false);
        Assert.Contains(vm.VpsServerTypes, s => s.Id == "tiny");
    }

    /// <summary>Indices are the e2e's addressing scheme
    /// (<c>vps-server-type-radio[i]</c>), and they are assigned when the list is
    /// built. A cache that survived a content change would freeze them against the
    /// wrong rows — so assert the numbering, not just the identity.</summary>
    [Fact]
    public void VpsServerTypes_RenumbersFromZero_OverTheShownSet()
    {
        var vm = NewVm();
        SeedCatalog(vm);

        var shown = vm.VpsServerTypes;
        for (var i = 0; i < shown.Count; i++)
        {
            Assert.Equal(i, shown[i].Index);
            Assert.Equal($"vps-server-type-radio[{i}]", shown[i].RadioItemId);
        }
    }

    [Fact]
    public void VpsLocations_ReturnsTheSameInstance_WhenTheProviderReturnedTheSameRegions()
    {
        var vm = NewVm();
        SeedCatalog(vm);

        var first = vm.VpsLocations;
        for (var i = 0; i < 20; i++)
            Assert.Same(first, vm.VpsLocations);
    }
}
