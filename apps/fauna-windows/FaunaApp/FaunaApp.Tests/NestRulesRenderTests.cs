using System;
using System.Collections.Generic;
using System.IO;
using FaunaApp.Core.Services;
using uniffi.fauna_client_policy;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The windows leg of the member's side of the nest rules (<c>nest-rules.md</c>
/// § The nest source in the engine; § Element IDs): the render arm the nest
/// placeholder takes (right after the region's, ahead of every family/muted arm, a
/// nest <c>block</c> never revealable — never loosenable), the placeholder's frame,
/// the decision's convention-17 verdict side, and — over the real native
/// <c>FfiRegionPlane</c> — the composed <c>render_with_nest</c> face answering no
/// nest placeholder for a nest the book does not know.
///
/// <para>The shared decisions themselves (verify, fold, the nest slice, the tie
/// order, the placeholder) are pinned in <c>libs/fauna-ffi/src/nest_rules.rs</c> and
/// <c>libs/fauna-client-policy</c>; the journey is
/// <c>tests/e2e-unified/tests/test_nest_rules.py</c>.</para>
///
/// <para>Serializes with the other <c>Strings.Initialize</c>-mutating classes.</para>
/// </summary>
[Collection("StringsGlobal")]
public class NestRulesRenderTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        // The real templates (i18n/strings/en.yaml § nest_rules), so the named
        // substitution is exercised, not just the key fallback.
        private static readonly Dictionary<string, string> Map = new()
        {
            ["nest_rules/blocked_notice"] = "Not shown on {nest} — blocked under this nest’s rules",
            ["nest_rules/collapsed_notice"] = "Hidden on {nest} under this nest’s rules — select to show",
            ["nest_rules/rule_line_no_level"] = "{verdict} — {reason}",
            ["rule_editor/verdict/block"] = "Block",
            ["rule_editor/prompt_not_applied"] = "Not applied on this device",
        };
        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public NestRulesRenderTests() => Strings.Initialize(new FakeLocalizer());

    // ── The render arm ─────────────────────────────────────────────────────

    [Fact]
    public void A_nest_block_wins_over_the_family_block_and_the_muted_collapse()
    {
        Assert.Equal(SocialRenderArm.NestWithheld,
            SocialRenderGate.Decide("block", contentRevealed: false, muted: true, nestVerb: "block"));
    }

    [Fact]
    public void A_nest_block_is_never_revealable()
    {
        Assert.Equal(SocialRenderArm.NestWithheld,
            SocialRenderGate.Decide("block", contentRevealed: true, muted: false, nestVerb: "block"));
    }

    [Fact]
    public void A_nest_collapse_is_withheld_until_revealed_then_renders_live()
    {
        Assert.Equal(SocialRenderArm.NestWithheld,
            SocialRenderGate.Decide("collapse", contentRevealed: false, muted: false, nestVerb: "collapse"));
        // The reveal is the shared content-policy set, as the region's is.
        Assert.Equal(SocialRenderArm.Live,
            SocialRenderGate.Decide("collapse", contentRevealed: true, muted: false, nestVerb: "collapse"));
    }

    [Fact]
    public void The_region_and_the_tombstones_still_outrank_the_nest()
    {
        Assert.Equal(SocialRenderArm.RegionWithheld,
            SocialRenderGate.Decide("block", false, false, regionVerb: "block", nestVerb: "block"));
        Assert.Equal(SocialRenderArm.Deleted,
            SocialRenderGate.Decide("block", false, false, deleted: true, nestVerb: "block"));
        Assert.Equal(SocialRenderArm.LegalTakedown,
            SocialRenderGate.Decide("block", false, false, legalTakedownRef: "ref", nestVerb: "block"));
        Assert.Equal(SocialRenderArm.AdminBlocked,
            SocialRenderGate.Decide("block", false, false, adminBlockInPlace: true, nestVerb: "block"));
    }

    // ── The placeholder and the decision ───────────────────────────────────

    [Fact]
    public void The_placeholder_frame_names_the_nest_in_the_verbs_words()
    {
        var block = new NestPlaceholderModel("block", "nest.example", "No gambling promotion.");
        var collapse = block with { Verb = "collapse" };
        Assert.True(block.IsBlock);
        Assert.False(collapse.IsBlock);
        Assert.Equal("Not shown on nest.example — blocked under this nest’s rules", block.NoticeText);
        Assert.Equal("Hidden on nest.example under this nest’s rules — select to show", collapse.NoticeText);
        // The reason is beside the frame, never inside it.
        Assert.DoesNotContain(block.Reason, block.NoticeText);
    }

    [Fact]
    public void The_placeholder_carries_the_ffi_record_field_for_field()
    {
        var model = NestPlaceholderModel.From(new FfiNestPlaceholder("collapse", "nest.example", "Hidden"));
        Assert.Equal(new NestPlaceholderModel("collapse", "nest.example", "Hidden"), model);
        Assert.Null(NestPlaceholderModel.From(null));
    }

    [Fact]
    public void Only_a_nest_block_is_the_convention_17_verdict_side()
    {
        var block = new NestPlaceholderModel("block", "nest.example", "r");
        Assert.True(new RegionRenderDecision("block", null, NestPlaceholder: block).IsNestBlocked);
        Assert.False(new RegionRenderDecision("collapse", null,
            NestPlaceholder: block with { Verb = "collapse" }).IsNestBlocked);
        Assert.False(new RegionRenderDecision("block", null).IsNestBlocked);
    }

    // ── The joining surface's rule line ────────────────────────────────────

    private static NestRuleLine Rule(uniffi.fauna_core.LocalizedText? inert) => new(
        new uniffi.fauna_core.LocalizedText("rule_editor.verdict.block", new Dictionary<string, string>()),
        null,
        "No gambling promotion.",
        inert);

    [Fact]
    public void A_rule_this_device_cannot_apply_carries_its_note_under_its_line_at_joining()
    {
        // nest-rules.md § Acceptance at joining: the not-applied note on a second
        // line of the same item, as tui's joining_rule_line paints it.
        var rule = Rule(new uniffi.fauna_core.LocalizedText(
            "rule_editor.prompt_not_applied", new Dictionary<string, string>()));
        Assert.Equal("Block — No gambling promotion.", NestRulesHost.RuleLine(rule));
        Assert.Equal("Block — No gambling promotion.\nNot applied on this device",
            NestRulesHost.JoiningRuleLine(rule));
    }

    [Fact]
    public void An_applied_rule_reads_at_joining_as_its_rule_line_alone()
    {
        var rule = Rule(null);
        Assert.Equal(NestRulesHost.RuleLine(rule), NestRulesHost.JoiningRuleLine(rule));
    }

    // ── The composed face through the real shared plane ───────────────────

    [Fact]
    public void A_nest_the_book_does_not_know_composes_no_nest_placeholder()
    {
        var dir = Path.Combine(Path.GetTempPath(), "fauna-win-nest-rules-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(dir);
        try
        {
            using var plane = FfiRegionPlane.Open(null, FfiRegionSource.SystemRegion, dir);
            using var book = new FfiNestRulesBook();
            var r = plane.RenderWithNest(
                book, "00", Array.Empty<uniffi.fauna_core.ContentLabelEntry>(), null, null, null,
                null, null, "an ordinary post", Array.Empty<string>(), false, "en");
            Assert.Equal("show", r.@verdict);
            Assert.Null(r.nestPlaceholder);
            Assert.Null(r.guardianPlaceholder);
        }
        finally { Directory.Delete(dir, recursive: true); }
    }
}
