using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Xunit;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Tests;

/// <summary>web-content-hosting.md § Admin apex hosting (Slice 4): the windows
/// <c>admin-web</c> apex-actor picker. Mirrors the tier_3 e2e
/// <c>test_web_authoring.py::test_admin_apex_designate_and_clear</c> at the VM level (the
/// deterministic windows gate). Drives the picker over the faked <see cref="FakeWebClient"/>
/// (defined in <see cref="WebSettingsViewModelTests"/>) + a canned actor list.
///
/// <para>[Collection("StringsGlobal")] because the not-loaded fallback option resolves
/// <c>admin/actor_id_fallback_label</c> through the process-global <see cref="Strings"/> —
/// see <c>AdminCustodyHostingViewModelTests</c>. This class installs its own
/// <see cref="FakeLocalizer"/> in its constructor.</para></summary>
[Collection("StringsGlobal")]
public class AdminWebViewModelTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["admin/web_page/apex_none"] = "None",
            ["admin/actor_id_fallback_label"] = "actor {short}…",
        };
        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public AdminWebViewModelTests() => Strings.Initialize(new FakeLocalizer());

    private static readonly byte[] Alice =
        Enumerable.Range(0, 32).Select(i => (byte)i).ToArray();

    private static Func<Task<IReadOnlyList<ApexActorOption>>> Actors(params ApexActorOption[] a)
        => () => Task.FromResult<IReadOnlyList<ApexActorOption>>(a.ToList());

    [Fact]
    public async Task Apex_designate_then_clear_round_trips()
    {
        var web = new FakeWebClient { ApexActor = null };
        var vm = new AdminWebViewModel(web, Actors(new ApexActorOption(Alice, "alice")), "example.test");

        await vm.LoadAsync();
        Assert.Equal(2, vm.ApexOptions.Count); // [None, alice]
        Assert.Equal(0, vm.ApexSelectedIndex); // none designated

        // Designate alice (index 1) → persists over set/get_apex_actor.
        await vm.SelectApexAsync(1);
        Assert.Equal(Alice, web.ApexSetRequests.Last());
        Assert.Equal(1, vm.ApexSelectedIndex); // re-hydrated selection

        // Clear (index 0 = "None") → reverts to the built-in info page.
        await vm.SelectApexAsync(0);
        Assert.Null(web.ApexSetRequests.Last());
        Assert.Equal(0, vm.ApexSelectedIndex);
    }

    [Fact]
    public async Task Apex_keeps_a_paginated_out_designation_visible()
    {
        var bob = Enumerable.Range(100, 32).Select(i => (byte)i).ToArray();
        // bob is the persisted apex but is NOT in the loaded actor page.
        var web = new FakeWebClient { ApexActor = bob };
        var vm = new AdminWebViewModel(web, Actors(new ApexActorOption(Alice, "alice")), "example.test");

        await vm.LoadAsync();
        Assert.Equal(3, vm.ApexOptions.Count); // [None, alice, <bob trailing>]
        Assert.Equal(2, vm.ApexSelectedIndex); // the trailing entry stays selected

        // admin.md § 2's two-halves rule: the trailing fallback shows the FULL 64-char
        // lowercase actor hex, not the old 8-char/4-byte-truncated prefix ().
        var expectedHex = Convert.ToHexString(bob).ToLowerInvariant();
        Assert.Equal(64, expectedHex.Length);
        Assert.Equal($"actor {expectedHex}…", vm.ApexOptions[2]);
    }
}
