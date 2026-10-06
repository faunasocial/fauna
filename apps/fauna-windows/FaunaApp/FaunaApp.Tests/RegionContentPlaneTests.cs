using System;
using System.Collections.Generic;
using System.IO;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The windows leg of the region content plane (<c>region-blocking.md</c> § The
/// content plane → <i>How an app obtains its region's policy</i>, <i>The blocked
/// render and the transparency surface</i>): the render-arm ordering the region
/// placeholder takes (ahead of every family/muted arm, a region <c>block</c> never
/// revealable), the settings-section paint of the shared plane's view, the
/// placeholder's frame, and — over the real native <c>FfiRegionPlane</c> — the
/// leaf's answer reaching the view the settings surface paints.
///
/// <para>The shared decisions themselves (verify, fold, compose, placeholder_for)
/// are pinned in <c>libs/fauna-ffi/src/region.rs</c> and
/// <c>libs/fauna-client-region</c>; the journey is
/// <c>tests/e2e-unified/tests/test_region_content_policy.py</c>.</para>
///
/// <para>Serializes with the other <c>Strings.Initialize</c>-mutating classes.</para>
/// </summary>
[Collection("StringsGlobal")]
public class RegionContentPlaneTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        // The real templates (i18n/strings/en.yaml § region), so the named
        // substitutions are exercised, not just the key fallback.
        private static readonly Dictionary<string, string> Map = new()
        {
            ["region/blocked_notice"] = "Not shown in {region} — blocked under the policy of {authority}",
            ["region/collapsed_notice"] = "Hidden in {region} under the policy of {authority} — select to show",
            ["region/declared"] = "Your region: {region}",
            ["region/none_declared"] = "No region is declared on this device",
            ["region/source_system_region"] = "From your system region setting — change it in your system settings",
            ["region/no_policy"] = "No regional content policy is in force",
            ["region/policy_authority"] = "{region}: {authority}",
            ["region/policy_version"] = "Version {sequence}, issued {issued}",
            ["region/inert_notice"] = "Uses a policy format this app does not understand (version {version}) — nothing is blocked under it",
            ["region/malformed_notice"] = "This policy could not be read — nothing is blocked under it",
            ["region/last_checked"] = "Last checked {time}",
            ["region/stale_warning"] = "Could not check for policy updates recently — the policies above stay in force",
        };
        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public RegionContentPlaneTests() => Strings.Initialize(new FakeLocalizer());

    private static string Time(long secs) => $"@{secs}s";

    // ── The render arm ─────────────────────────────────────────────────────

    [Fact]
    public void A_region_block_wins_over_the_family_block_and_the_muted_collapse()
    {
        Assert.Equal(SocialRenderArm.RegionWithheld,
            SocialRenderGate.Decide("block", contentRevealed: false, muted: true, regionVerb: "block"));
    }

    [Fact]
    public void A_region_block_is_never_revealable()
    {
        Assert.Equal(SocialRenderArm.RegionWithheld,
            SocialRenderGate.Decide("block", contentRevealed: true, muted: false, regionVerb: "block"));
    }

    [Fact]
    public void A_region_collapse_is_withheld_until_revealed_then_renders_live()
    {
        Assert.Equal(SocialRenderArm.RegionWithheld,
            SocialRenderGate.Decide("collapse", contentRevealed: false, muted: false, regionVerb: "collapse"));
        // The reveal is the shared content-policy set: once tapped, the composed
        // `collapse` falls through the family collapse arm, which it also lifted.
        Assert.Equal(SocialRenderArm.Live,
            SocialRenderGate.Decide("collapse", contentRevealed: true, muted: false, regionVerb: "collapse"));
    }

    [Fact]
    public void The_tombstones_still_outrank_the_region()
    {
        Assert.Equal(SocialRenderArm.Deleted,
            SocialRenderGate.Decide("block", false, false, deleted: true, regionVerb: "block"));
        Assert.Equal(SocialRenderArm.LegalTakedown,
            SocialRenderGate.Decide("block", false, false, legalTakedownRef: "ref", regionVerb: "block"));
    }

    [Fact]
    public void No_region_verb_leaves_the_family_arms_exactly_as_before()
    {
        Assert.Equal(SocialRenderArm.ContentBlocked, SocialRenderGate.Decide("block", false, false));
        Assert.Equal(SocialRenderArm.Muted, SocialRenderGate.Decide("collapse", false, true));
        Assert.Equal(SocialRenderArm.ContentCollapsed, SocialRenderGate.Decide("collapse", false, false));
    }

    // ── The placeholder's frame ────────────────────────────────────────────

    [Fact]
    public void The_placeholder_frame_names_the_region_and_its_authority_in_the_verbs_words()
    {
        var block = new RegionPlaceholderModel("block", "XZ", "Synthetic Test Authority", "reason");
        var collapse = block with { Verb = "collapse" };
        Assert.True(block.IsBlock);
        Assert.False(collapse.IsBlock);
        Assert.Equal("Not shown in XZ — blocked under the policy of Synthetic Test Authority", block.NoticeText);
        Assert.Equal("Hidden in XZ under the policy of Synthetic Test Authority — select to show", collapse.NoticeText);
    }

    // ── The settings section ───────────────────────────────────────────────

    [Fact]
    public void An_undeclared_device_says_so_and_nothing_else()
    {
        var model = RegionSettingsModel.From(
            new FfiRegionView(null, Array.Empty<FfiRegionPolicyRow>(), null, false), Time);
        Assert.Equal("No region is declared on this device", model.DeclaredText);
        Assert.Null(model.SourceText);
        Assert.Empty(model.Policies);
        Assert.Null(model.LastCheckedText);
        Assert.Null(model.StaleWarningText);
    }

    [Fact]
    public void The_section_paints_each_policy_its_notices_and_the_staleness()
    {
        var view = new FfiRegionView(
            new FfiDeclaredRegion("XZ", FfiRegionSource.SystemRegion, "region.source_system_region"),
            new[]
            {
                new FfiRegionPolicyRow("XZ", "Synthetic Test Authority", 1, 100, "applied", null),
                new FfiRegionPolicyRow("XZ", "Synthetic Test Authority", 2, 200, "inert", 99),
                new FfiRegionPolicyRow("XZ", "Synthetic Test Authority", 3, 300, "malformed", null),
            },
            400,
            true);
        var model = RegionSettingsModel.From(view, Time);

        Assert.Equal("Your region: XZ", model.DeclaredText);
        Assert.Equal("From your system region setting — change it in your system settings", model.SourceText);
        Assert.Null(model.NoPolicyText);
        Assert.Equal(3, model.Policies.Count);
        Assert.Equal("XZ: Synthetic Test Authority", model.Policies[0].AuthorityText);
        Assert.Equal("Version 1, issued @100s", model.Policies[0].VersionText);
        Assert.Null(model.Policies[0].NoticeText);
        Assert.Equal(
            "Uses a policy format this app does not understand (version 99) — nothing is blocked under it",
            model.Policies[1].NoticeText);
        Assert.Equal("This policy could not be read — nothing is blocked under it", model.Policies[2].NoticeText);
        Assert.Equal("Last checked @400s", model.LastCheckedText);
        Assert.NotNull(model.StaleWarningText);
    }

    [Fact]
    public void A_declared_region_with_no_policy_says_none_is_in_force()
    {
        var view = new FfiRegionView(
            new FfiDeclaredRegion("NO", FfiRegionSource.SystemRegion, "region.source_system_region"),
            Array.Empty<FfiRegionPolicyRow>(), null, false);
        Assert.Equal("No regional content policy is in force", RegionSettingsModel.From(view, Time).NoPolicyText);
    }

    // ── The leaf's answer through the real shared plane ───────────────────

    private static string TempConfigDir()
    {
        var dir = Path.Combine(Path.GetTempPath(), "fauna-win-region-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(dir);
        return dir;
    }

    [Fact]
    public void The_windows_region_setting_reaches_the_view_as_a_system_region_declaration()
    {
        var dir = TempConfigDir();
        try
        {
            using var plane = FfiRegionPlane.Open("NO", FfiRegionSource.SystemRegion, dir);
            var model = RegionSettingsModel.From(plane.View(), Time);
            Assert.Equal("Your region: NO", model.DeclaredText);
            Assert.Equal("From your system region setting — change it in your system settings", model.SourceText);
            Assert.Equal("No regional content policy is in force", model.NoPolicyText);
        }
        finally { Directory.Delete(dir, recursive: true); }
    }

    [Fact]
    public void An_unreadable_setting_declares_nothing()
    {
        // RegionLeaf hands null when the setting cannot be read; the plane then
        // declares nothing, and the settings surface says so.
        var dir = TempConfigDir();
        try
        {
            using var plane = FfiRegionPlane.Open(null, FfiRegionSource.SystemRegion, dir);
            Assert.Null(plane.View().@declared);
            Assert.Equal("No region is declared on this device",
                RegionSettingsModel.From(plane.View(), Time).DeclaredText);
        }
        finally { Directory.Delete(dir, recursive: true); }
    }
}
