using System.Collections.Generic;
using System.Linq;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The two-same-label injectivity pin (the windows arm of admin.md §
/// 2's "What identifies a user in an admin picker" ruling): the editable,
/// non-unique <c>label</c> must never be the picker option text, or two
/// accounts an admin has labelled the same render two indistinguishable rows
/// (all inbound mail for a domain routed to the wrong account, or the wrong
/// account's site published at the deployment apex). Calls the REAL FFI
/// binding — <c>FaunaApp.Tests.csproj</c> already loads <c>fauna_ffi.dll</c>
/// (unlike android's Robolectric-blocked native code, so no injectable
/// override is needed here).
///
/// <para>[Collection("StringsGlobal")] because <see cref="NotLoadedFallbackLabel_ReturnsTheFullHex_NeverATruncatedPrefix"/>
/// resolves <c>admin/actor_id_fallback_label</c> through the process-global
/// <see cref="Strings"/> — see <c>AdminCustodyHostingViewModelTests</c>. This class installs
/// its own <see cref="FakeLocalizer"/> in its constructor.</para>
/// </summary>
[Collection("StringsGlobal")]
public class AdminActorOptionsTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["admin/actor_id_fallback_label"] = "actor {short}…",
        };
        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public AdminActorOptionsTests() => Strings.Initialize(new FakeLocalizer());

    private static readonly string AlexActor = "11" + new string('1', 62);
    private static readonly string BaoActor = "22" + new string('2', 62);
    private static readonly string BobActor = "33" + new string('3', 62);

    // The property this pin exists to hold: TWO users sharing a display
    // label, BOTH with distinct handles (mirrors apple's
    // `actorLabelStaysInjectiveWhenLabelsCollide` — the shape the judge
    // chose). A handled-vs-handleless pair (the prior shape here) passes
    // vacuously, since the handle-else-hex fallback branches already differ
    // before injectivity is ever exercised.
    [Fact]
    public void Label_TwoUsersSharingALabel_ProducesTwoDistinctOptionStrings()
    {
        var alex = MockNestRpcClient.MakeAdminUser(
            AlexActor, "reach1", label: "e2e-test", handle: "alex99");
        var bao = MockNestRpcClient.MakeAdminUser(
            BaoActor, "reach1", label: "e2e-test", handle: "bao77");

        var first = AdminActorOptions.Label(alex);
        var second = AdminActorOptions.Label(bao);

        Assert.NotEqual(first, second);
        Assert.Equal("alex99", first);
        Assert.Equal("bao77", second);
    }

    [Fact]
    public void Label_HandledUser_ReturnsTheHandle_NeverTheLabel()
    {
        var user = MockNestRpcClient.MakeAdminUser(
            AlexActor, "reach1", label: "some editable nickname", handle: "alex");

        Assert.Equal("alex", AdminActorOptions.Label(user));
    }

    [Fact]
    public void Label_HandlelessUser_FallsBackToTheFullActorHex_NeverTheLabel()
    {
        var user = MockNestRpcClient.MakeAdminUser(
            BobActor, "reach1", label: "some editable nickname", handle: null);

        var option = AdminActorOptions.Label(user);

        Assert.NotEqual("some editable nickname", option);
        Assert.Contains(BobActor, option, System.StringComparison.OrdinalIgnoreCase);
    }

    // The DNS site's own picker builder (AdminDnsPage.BuildActorPicker) is private WinUI
    // code-behind FaunaApp.Tests cannot reach directly (FaunaApp.Tests.csproj references
    // only FaunaApp.Core), so this helper — the one place both AdminDnsPage and
    // AdminWebViewModel build the not-loaded fallback option — is what pins the DNS site
    // against a regression to the old 4-byte-truncated prefix (; mirrors linux's
    // `actor_not_loaded_fallback_label_shows_full_hex_not_truncated_prefix`).
    [Fact]
    public void NotLoadedFallbackLabel_ReturnsTheFullHex_NeverATruncatedPrefix()
    {
        var id = Enumerable.Repeat((byte)0xab, 32).ToArray();

        var label = AdminActorOptions.NotLoadedFallbackLabel(id);

        var fullHex = string.Concat(Enumerable.Repeat("ab", 32));
        Assert.Equal(64, fullHex.Length);
        Assert.Equal($"actor {fullHex}…", label);
        Assert.NotEqual($"actor {string.Concat(Enumerable.Repeat("ab", 4))}…", label);
    }
}
