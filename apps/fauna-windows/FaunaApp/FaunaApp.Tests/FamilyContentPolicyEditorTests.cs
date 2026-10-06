using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The guardian-side content-policy editor (family-safety.md § Content policy,
/// <c>Status: ratified</c>) — the four per-category floors over the negative
/// canonical categories (<c>nsfw</c>/<c>spam</c>/<c>phishing</c>/<c>commercial</c>,
/// each <c>inherit</c>|<c>collapse</c>|<c>block</c>) plus the Guardian Notify knob,
/// as the <c>FamilyViewModel</c> loads them off a ward and sends them back on
/// <c>fauna.family.policy.update</c>.
/// <para>
/// The per-value fail-closed rule itself lives in
/// <see cref="FamilyPolicyFailClosedTests"/>; this class pins the three flows
/// AROUND it — load (including the absent-policy default), save (the payload
/// actually put on the wire), and the ward's read-only summary.
/// </para>
/// <para>
/// Serializes with the other <c>Strings.Initialize</c>-mutating test classes under
/// xUnit's default parallel-by-class runner — see <c>StringsGlobalCollection</c>.
/// </para>
/// </summary>
[Collection("StringsGlobal")]
public class FamilyContentPolicyEditorTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private readonly Dictionary<string, string> _map;
        public FakeLocalizer(Dictionary<string, string> map) => _map = map;
        public string Get(string key) => _map.TryGetValue(key, out var v) ? v : key;
    }

    private static FfiReachPolicy Policy(FfiContentPolicy? content = null, bool? notify = null, string? unknownPeerDm = null) =>
        new(contactApproval: true, unknownSenderMail: "allow", federationContact: true,
            feedSources: "allow", contentPolicy: content, screenTime: null,
            contentNotify: notify, unknownPeerDm: unknownPeerDm);

    private static FfiFamilyWardInfo Ward(FfiReachPolicy policy) =>
        FfiFamilyWardInfoFixture.Make(actorId: Enumerable.Repeat((byte)7, 32).ToArray(), policy: policy);

    private static MockNestRpcClient RpcWith(FfiReachPolicy wardPolicy, FfiReachPolicy? ownPolicy = null)
    {
        var rpc = new MockNestRpcClient();
        rpc.NextFamilyStatus = FfiFamilyStatusFixture.Make(policy: ownPolicy, wards: [Ward(wardPolicy)]);
        return rpc;
    }

    // ── Load ─────────────────────────────────────────────────────────────

    /// <summary>An ABSENT <c>content_policy</c> — a guardian who
    /// never set a floor — is the all-<c>inherit</c> default (no rule of the
    /// guardian's composes; the ward's own preferences decide), and an absent
    /// <c>content_notify</c> is off. Neither is a reason to invent a floor.</summary>
    [Fact]
    public async Task Load_WithNoContentPolicyOnTheWard_ShowsEveryFloorAsInherit()
    {
        var vm = new FamilyViewModel(RpcWith(Policy(content: null, notify: null)));

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal("inherit", vm.PolicyContentNsfw);
        Assert.Equal("inherit", vm.PolicyContentSpam);
        Assert.Equal("inherit", vm.PolicyContentPhishing);
        Assert.Equal("inherit", vm.PolicyContentCommercial);
        Assert.False(vm.PolicyContentNotify);
    }

    [Fact]
    public async Task Load_CarriesEachStoredFloorAndTheNotifyKnobIntoTheEditor()
    {
        var vm = new FamilyViewModel(RpcWith(Policy(
            FfiContentPolicyFixture.Make(nsfw: "block", spam: "collapse", phishing: "block"),
            notify: true)));

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal("block", vm.PolicyContentNsfw);
        Assert.Equal("collapse", vm.PolicyContentSpam);
        Assert.Equal("block", vm.PolicyContentPhishing);
        Assert.Equal("inherit", vm.PolicyContentCommercial);
        Assert.True(vm.PolicyContentNotify);
    }

    /// <summary>The fail-closed rule at the LOAD boundary (family-safety.md
    /// § Content policy): a floor value stored by a newer nest that this build
    /// cannot name reaches the editor as <c>block</c>, never <c>inherit</c> —
    /// which matters precisely because the very next save writes the editor's
    /// state back, so an `inherit` here would silently downgrade the ward's real
    /// protection.</summary>
    [Fact]
    public async Task Load_WithAFloorThisBuildCannotName_ShowsBlockNotInherit()
    {
        var vm = new FamilyViewModel(RpcWith(Policy(
            FfiContentPolicyFixture.Make(nsfw: "quarantine"))));

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal("block", vm.PolicyContentNsfw);
    }

    // ── Save ─────────────────────────────────────────────────────────────

    /// <summary>The payload <c>fauna.family.policy.update</c> actually receives:
    /// a <b>present</b> <c>content_policy</c> carrying the four editor values plus
    /// a present <c>content_notify</c> AND a present <c>screen_time</c> (replace
    /// semantics — this editor builds all three pillars, so it sends all three,
    /// matching the linux/web/android/apple/tui legs). An untouched screen-time
    /// editor sends an all-empty (all-<c>null</c>-field) <c>FfiScreenTimePolicy</c>,
    /// never absent — "present but empty" is what makes an all-empty save able to
    /// CLEAR a previously-set limit (family-safety.md § Screen time). The one
    /// pillar windows has no UI for stays <c>null</c> = leave unchanged
    /// server-side.</summary>
    [Fact]
    public async Task Save_SendsAPresentContentPolicyAndNotifyFromTheEditor()
    {
        var rpc = RpcWith(Policy(content: null));
        var vm = new FamilyViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        vm.PolicyContentNsfw = "block";
        vm.PolicyContentSpam = "collapse";
        vm.PolicyContentPhishing = "block";
        vm.PolicyContentCommercial = "inherit";
        vm.PolicyContentNotify = true;
        await vm.SavePolicyAsync();

        Assert.Contains("FamilyPolicyUpdate", rpc.Calls);
        var sent = rpc.LastFamilyPolicyUpdate!.Value.Policy;
        var content = Assert.IsType<FfiContentPolicy>(sent.@contentPolicy);
        Assert.Equal("block", content.@nsfw);
        Assert.Equal("collapse", content.@spam);
        Assert.Equal("block", content.@phishing);
        Assert.Equal("inherit", content.@commercial);
        Assert.Equal(true, sent.@contentNotify);
        var screenTime = Assert.IsType<FfiScreenTimePolicy>(sent.@screenTime);
        Assert.Null(screenTime.@windowStart);
        Assert.Null(screenTime.@windowEnd);
        Assert.Null(screenTime.@dailyMinutes);
        // The guardian never touched unknown_peer_dm this session — null leaves
        // it unchanged server-side (Option<String> leave-unchanged contract,
        // family-safety.md § Policy-update compatibility), never overwriting
        // whatever a sibling client (or an earlier save on this one) set.
        Assert.Null(sent.@unknownPeerDm);
    }

    /// <summary>An untouched editor still sends a present, all-<c>inherit</c>
    /// policy — a valid no-op floor, not "leave unchanged". This is what makes
    /// clearing a floor back to `inherit` reachable from the windows UI at all.</summary>
    [Fact]
    public async Task Save_WithAnUntouchedEditor_SendsAPresentAllInheritPolicy()
    {
        var rpc = RpcWith(Policy(content: null));
        var vm = new FamilyViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        await vm.SavePolicyAsync();

        var content = Assert.IsType<FfiContentPolicy>(rpc.LastFamilyPolicyUpdate!.Value.Policy.@contentPolicy);
        Assert.Equal(["inherit", "inherit", "inherit", "inherit"],
            new[] { content.@nsfw, content.@spam, content.@phishing, content.@commercial });
        Assert.Equal(false, rpc.LastFamilyPolicyUpdate!.Value.Policy.@contentNotify);
    }

    // ── The ward's read-only summary ─────────────────────────────────────

    /// <summary>Ward transparency (family-safety.md § Content policy — <i>"the
    /// ward's read-only summary renders the floors"</i>). This asserts EXISTING
    /// wiring rather than new code: <c>FamilyViewModel.LoadAsync</c> renders
    /// <c>FaunaFfiMethods.ReachPolicySummary(status.policy)</c>, and the FFI record
    /// carries the whole policy, so a non-inherit floor appears in
    /// <c>family-policy-summary</c> with no windows-side change. The pin exists so a
    /// future refactor of that line cannot quietly drop the ward's view of the
    /// content rules — the transparency half of the pillar.</summary>
    [Fact]
    public async Task WardSummary_RendersANonInheritFloorAndTheNotifyLine()
    {
        Strings.Initialize(new FakeLocalizer(new()
        {
            ["family/policy_content_spam_label"] = "Spam",
            ["family/value_block"] = "Block",
            ["family/policy_content_notify_label"] = "Notify me about flagged content",
            ["common/enable"] = "On",
        }));
        var own = Policy(
            FfiContentPolicyFixture.Make(spam: "block"),
            notify: true);
        var vm = new FamilyViewModel(RpcWith(Policy(content: null), ownPolicy: own));

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Contains("Spam: Block", vm.PolicySummary);
        Assert.Contains("Notify me about flagged content: On", vm.PolicySummary);
        // An `inherit` category adds no rule over the ward's own preferences, so it
        // is not a line — the summary states rules, not defaults.
        Assert.DoesNotContain("family/policy_content_nsfw_label", vm.PolicySummary);
    }

    // ── Bridge-DM gate (family-safety.md § The bridge-DM gate) ─────────────
    // unknown_peer_dm is the ONE reach knob that is Option<String> on the wire
    // with "absent means leave unchanged" — the three tests below pin that
    // contract around the shared NormalizeUnknownPeerDmWire fail-closed rule,
    // which FamilyPolicyFailClosedTests exercises directly.

    [Fact]
    public async Task Load_WithAnAbsentUnknownPeerDm_ShowsTheAllowDefault()
    {
        var vm = new FamilyViewModel(RpcWith(Policy(unknownPeerDm: null)));

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal("allow", vm.PolicyUnknownPeerDm);
        Assert.False(vm.PolicyUnknownPeerDmEdited);
    }

    [Fact]
    public async Task Load_CarriesTheStoredUnknownPeerDmIntoTheEditor()
    {
        var vm = new FamilyViewModel(RpcWith(Policy(unknownPeerDm: "hold")));

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Equal("hold", vm.PolicyUnknownPeerDm);
    }

    /// <summary>Property 1 of the bridge-DM knob's round trip (the live-wire
    /// proof lives in test_family_bridge_dm_knob_round_trips): a guardian who
    /// has actually touched the select sends the edited value.</summary>
    [Fact]
    public async Task Save_WhenTheGuardianHasTouchedUnknownPeerDm_SendsTheEditedValue()
    {
        var rpc = RpcWith(Policy(unknownPeerDm: null));
        var vm = new FamilyViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        vm.PolicyUnknownPeerDm = "hold";
        vm.PolicyUnknownPeerDmEdited = true;
        await vm.SavePolicyAsync();

        Assert.Equal("hold", rpc.LastFamilyPolicyUpdate!.Value.Policy.@unknownPeerDm);
    }

    /// <summary>Property 2: reloading (a fresh load, OR — critically — a ward
    /// switch, which shares the same LoadPolicyFromWard path) must reset the
    /// touched flag, so a subsequent unrelated save never echoes a stale edit
    /// back onto whichever ward now sits in the editor. Without this reset, a
    /// guardian who touched the select for ward A, then switched to ward B and
    /// saved an unrelated field, would silently rewrite ward B's stored knob.</summary>
    [Fact]
    public async Task Load_ResetsTheUnknownPeerDmTouchedFlagOnEveryWardLoad()
    {
        var rpc = RpcWith(Policy(unknownPeerDm: null));
        var vm = new FamilyViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        vm.PolicyUnknownPeerDmEdited = true;

        // Same ward reloading (an unrelated save's follow-up LoadAsync, or a
        // switch back to this ward) goes through the SAME LoadPolicyFromWard
        // path a switch to a DIFFERENT ward would.
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.False(vm.PolicyUnknownPeerDmEdited);
    }
}
