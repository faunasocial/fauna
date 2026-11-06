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
/// The guardian-side content policy (family-safety.md § App surface → Content
/// rules): the rules are the shared rule editor's (<c>family-policy-rules-section</c>,
/// over the ward's stored <c>FfiContentPolicy</c>, saved by its own button through
/// <c>FfiFamilyClient.SaveContentRules</c>); the policy form keeps only the Guardian
/// Notify knob and sends the content pillar ABSENT. This class pins the flows around
/// that split — load (the stored policy reaches the editor's seed), the two saves
/// (the payload actually put on the wire), and the ward's read-only summary (a
/// guardian rule's reason rides its line).
/// <para>
/// The editor's own paint and edits are <see cref="RuleEditorModelTests"/>'.
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

    /// <summary>An ABSENT <c>content_policy</c> — a guardian who never authored one —
    /// seeds the guardian editor with nothing (<c>FfiRuleEditor.ForGuardian(null)</c>:
    /// no rules), and an absent <c>content_notify</c> is off. Neither is a reason to
    /// invent a rule.</summary>
    [Fact]
    public async Task Load_WithNoContentPolicyOnTheWard_SeedsTheRuleEditorWithNothing()
    {
        var vm = new FamilyViewModel(RpcWith(Policy(content: null, notify: null)));

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Null(vm.SelectedWardContentPolicy);
        Assert.NotNull(vm.SelectedWardRulesKey);
        Assert.Empty(FfiRuleEditor.ForGuardian(vm.SelectedWardContentPolicy).View().rows);
        Assert.False(vm.PolicyContentNotify);
    }

    /// <summary>The stored policy reaches the editor's seed as the nest sent it —
    /// its document's rules show as rules there (shared Rust,
    /// <c>RuleEditor::load_guardian</c>) — and the Notify knob reaches the form.</summary>
    [Fact]
    public async Task Load_CarriesTheStoredPolicyToTheRuleEditorAndTheNotifyKnobToTheForm()
    {
        var vm = new FamilyViewModel(RpcWith(Policy(
            FfiContentPolicyFixture.Make(spam: uniffi.fauna_client_policy.RuleVerdict.Block),
            notify: true)));

        await vm.LoadCommand.ExecuteAsync(null);

        var stored = Assert.IsType<FfiContentPolicy>(vm.SelectedWardContentPolicy);
        Assert.NotNull(stored.document);
        Assert.Single(FfiRuleEditor.ForGuardian(stored).View().rows);
        Assert.True(vm.PolicyContentNotify);
    }

    /// <summary>The editor is reopened only when what the nest stored changed: the
    /// key moves with the stored document, and stays put on a re-read of the same
    /// policy (so a re-read never throws away the guardian's unsaved edits).</summary>
    [Fact]
    public void RulesKey_MovesWithTheStoredDocumentAndNotOtherwise()
    {
        var ward = Enumerable.Repeat((byte)7, 32).ToArray();
        var empty = FfiContentPolicyFixture.Make();
        var withDoc = empty with { document = new byte[] { 1, 2, 3 } };

        Assert.Equal(FamilyViewModel.RulesKey(ward, empty), FamilyViewModel.RulesKey(ward, FfiContentPolicyFixture.Make()));
        Assert.NotEqual(FamilyViewModel.RulesKey(ward, empty), FamilyViewModel.RulesKey(ward, withDoc));
        Assert.NotEqual(FamilyViewModel.RulesKey(ward, null), FamilyViewModel.RulesKey(new byte[32], null));
    }

    // ── Save ─────────────────────────────────────────────────────────────

    /// <summary>The payload the policy form's <c>fauna.family.policy.update</c>
    /// actually receives: the content pillar ABSENT — "leave unchanged", since the
    /// rules are the rule editor's own save and this form must never overwrite them
    /// (tui's, web's, android's and apple's twin) — beside a present
    /// <c>content_notify</c> AND a present <c>screen_time</c> (replace semantics: an
    /// untouched screen-time editor sends an all-empty (all-<c>null</c>-field)
    /// <c>FfiScreenTimePolicy</c>, never absent — "present but empty" is what makes an
    /// all-empty save able to CLEAR a previously-set limit, family-safety.md § Screen
    /// time).</summary>
    [Fact]
    public async Task Save_SendsTheContentPillarAbsentWithNotifyAndScreenTimePresent()
    {
        var rpc = RpcWith(Policy(FfiContentPolicyFixture.Make(spam: uniffi.fauna_client_policy.RuleVerdict.Block)));
        var vm = new FamilyViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        vm.PolicyContentNotify = true;
        await vm.SavePolicyAsync();

        Assert.Contains("FamilyPolicyUpdate", rpc.Calls);
        var sent = rpc.LastFamilyPolicyUpdate!.Value.Policy;
        Assert.Null(sent.contentPolicy);
        Assert.Equal(true, sent.contentNotify);
        var screenTime = Assert.IsType<FfiScreenTimePolicy>(sent.screenTime);
        Assert.Null(screenTime.windowStart);
        Assert.Null(screenTime.windowEnd);
        Assert.Null(screenTime.dailyMinutes);
        // The guardian never touched unknown_peer_dm this session — null leaves
        // it unchanged server-side (Option<String> leave-unchanged contract,
        // family-safety.md § Policy-update compatibility), never overwriting
        // whatever a sibling client (or an earlier save on this one) set.
        Assert.Null(sent.unknownPeerDm);
    }

    /// <summary>The rule editor's save writes the editor's document for the selected
    /// ward through <c>SaveContentRules</c>, re-reads the page, and answers the
    /// editor's status line with <c>rule_editor.saved</c>.</summary>
    [Fact]
    public async Task SaveContentRules_WritesTheDocumentForTheSelectedWardAndAnswersSaved()
    {
        var rpc = RpcWith(Policy(content: null));
        var vm = new FamilyViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        rpc.NextFamilySaveContentRules = Ward(Policy(content: null));
        var document = new byte[] { 0xa1, 0x00 };

        var status = await vm.SaveContentRulesAsync(document);

        Assert.Equal(Strings.Get("rule_editor/saved"), status);
        var (wardId, sentDocument) = Assert.Single(rpc.FamilyContentRulesWrites);
        Assert.Equal(Enumerable.Repeat((byte)7, 32).ToArray(), wardId);
        Assert.Equal(document, sentDocument);
        Assert.Equal(2, rpc.Calls.Count(c => c == "FamilyStatus"));
        // The rules ride their own write — never the policy form's.
        Assert.DoesNotContain("FamilyPolicyUpdate", rpc.Calls);
    }

    /// <summary>A caller who no longer guards the ward wrote nothing: the status says
    /// nothing, and the re-read is what drops the ward (android's, apple's and web's
    /// twin).</summary>
    [Fact]
    public async Task SaveContentRules_ForAWardNoLongerGuarded_AnswersNothing()
    {
        var rpc = RpcWith(Policy(content: null));
        var vm = new FamilyViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        rpc.NextFamilySaveContentRules = null;

        var status = await vm.SaveContentRulesAsync(new byte[] { 0xa0 });

        Assert.Null(status);
        Assert.Single(rpc.FamilyContentRulesWrites);
    }

    // ── The ward's read-only summary ─────────────────────────────────────

    /// <summary>Ward transparency (family-safety.md § Content policy — the ward's
    /// read-only summary renders the guardian's rules). This asserts EXISTING
    /// wiring rather than new code: <c>FamilyViewModel.LoadAsync</c> renders
    /// <c>FaunaFfiMethods.ReachPolicySummary(status.policy)</c>, and the FFI record
    /// carries the whole policy, so a category rule of the guardian's document
    /// appears in <c>family-policy-summary</c> with no windows-side change. The pin
    /// exists so a future refactor of that line cannot quietly drop the ward's view
    /// of the content rules — the transparency half of the pillar.</summary>
    [Fact]
    public async Task WardSummary_RendersAGuardianCategoryRuleAndTheNotifyLine()
    {
        Strings.Initialize(new FakeLocalizer(new()
        {
            ["family/policy_content_spam_label"] = "Spam",
            ["family/value_block"] = "Block",
            ["family/policy_content_notify_label"] = "Notify me about flagged content",
            ["common/enable"] = "On",
        }));
        var own = Policy(
            FfiContentPolicyFixture.Make(spam: uniffi.fauna_client_policy.RuleVerdict.Block),
            notify: true);
        var vm = new FamilyViewModel(RpcWith(Policy(content: null), ownPolicy: own));

        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Contains("Spam: Block", vm.PolicySummary);
        Assert.Contains("Notify me about flagged content: On", vm.PolicySummary);
        // A category the guardian wrote no rule for is not a line — the summary
        // states rules, not defaults.
        Assert.DoesNotContain("family/policy_content_nsfw_label", vm.PolicySummary);
    }

    /// <summary>The reason is the member's to read (nest-rules.md § The shared rule
    /// editor): a line a rule of the guardian's document makes carries the rule's
    /// reason verbatim — "{label}: {value} — {reason}" — picked off the record as web
    /// and android pick it (the UI language's entry, else <c>default</c>).</summary>
    [Fact]
    public async Task WardSummary_AppendsAGuardianRulesReasonToItsLine()
    {
        Strings.Initialize(new FakeLocalizer(new()));
        var author = FfiRuleEditor.ForGuardian(null);
        var row = author.AddRule(uniffi.fauna_client_policy.RuleKind.Category)!.Value;
        var view = author.View();
        var spam = view.categories.First(c => c.category.Contains("spam"));
        // Setting a value the new rule already holds may answer false; the reason
        // is what this test needs to land.
        author.SetCategory(row, spam.category);
        author.SetVerdict(row, uniffi.fauna_client_policy.RuleVerdict.Block);
        Assert.True(author.SetReason(row, view.reasonDefaultKey, "Not for this account"));
        var own = Policy(new FfiContentPolicy(document: author.DocumentBytes()));
        var vm = new FamilyViewModel(RpcWith(Policy(content: null), ownPolicy: own));

        await vm.LoadCommand.ExecuteAsync(null);

        var lines = (vm.PolicySummary ?? "").Split('\n');
        Assert.Contains(lines, l => l.EndsWith(" — Not for this account", StringComparison.Ordinal));
    }

    [Fact]
    public void SummaryLineText_PicksTheUiLanguageThenTheDefaultReason()
    {
        Strings.Initialize(new FakeLocalizer(new() { ["k/label"] = "Spam", ["k/value"] = "Block" }));
        uniffi.fauna_core.LocalizedText L(string key) => new(key, new Dictionary<string, string>());

        Assert.Equal("Spam: Block",
            FamilyViewModel.SummaryLineText(new(L("k.label"), L("k.value"))));
        Assert.Equal("Spam: Block — fallback",
            FamilyViewModel.SummaryLineText(new(L("k.label"), L("k.value"),
                new Dictionary<string, string> { ["default"] = "fallback", ["xx"] = "other" })));
        Assert.Equal("Spam: Block — own",
            FamilyViewModel.SummaryLineText(new(L("k.label"), L("k.value"),
                new Dictionary<string, string> { ["default"] = "fallback", [RegionPlaneHost.UiLang] = "own" })));
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
