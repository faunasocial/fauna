using FaunaApp.Core.ViewModels;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// family-safety.md § "A knob value a client cannot parse renders fail-closed,
/// on every app": within a major version this client may be OLDER than its
/// nest, so a stored policy value can be one this build cannot name. It must
/// render — and, because save writes the editor's state back, persist — as the
/// strictest option, never "allow" (the linux <c>FAIL_CLOSED_INDEX</c> /
/// android <c>normalize*Wire</c> rule; windows was the last client to adopt it).
/// <para>
/// <see cref="FamilyViewModel.NormalizeUnknownSenderWire"/> /
/// <see cref="FamilyViewModel.NormalizeFeedSourcesWire"/> /
/// <see cref="FamilyViewModel.NormalizeContentFloorWire"/> no longer hardcode the
/// recognized-value set or the fail-closed answer — they reverse-look-up the
/// shared <c>fauna_core::format</c> catalog/label FFI exports
/// (<c>FaunaFfiMethods.UnknownSenderLabel</c>/<c>Options</c>,
/// <c>FeedSourcesLabel</c>/<c>Options</c>,
/// <c>ContentFloorLabel</c>/<c>Options</c>). These tests therefore exercise the
/// real FFI-backed fail-closed behavior end to end, not a local re-implementation
/// of it — a regression here means either the windows reverse-lookup broke or the
/// shared Rust fail-closed rule itself changed.
/// </para>
/// </summary>
public class FamilyPolicyFailClosedTests
{
    [Theory]
    [InlineData("allow", "allow")]
    [InlineData("hold", "hold")]
    [InlineData("reject", "reject")]
    [InlineData("quarantine_v2", "hold")]
    [InlineData("", "hold")]
    [InlineData("ALLOW", "hold")] // wire values are exact; case variants are not ours
    // "block" is a `feed_sources` value, not one of THIS knob's catalog — must
    // fail closed to "hold", never surface as if it were a recognized
    // unknown_sender_mail value (the converse of the merged-catalog trap below).
    [InlineData("block", "hold")]
    public void UnknownSenderMail_NormalizesUnrecognizedToHold(string wire, string expected)
        => Assert.Equal(expected, FamilyViewModel.NormalizeUnknownSenderWire(wire));

    [Theory]
    [InlineData("allow", "allow")]
    [InlineData("block", "block")]
    [InlineData("curated_v2", "block")]
    [InlineData("", "block")]
    // The windows merged-ValueLabel trap, pinned: "reject" is a value of the
    // OTHER knob (unknown_sender_mail); feed_sources must never render it as a
    // recognized value of its own catalog — it fails closed to "block" like any
    // other value outside the allow/block set (fauna-core's
    // a_feed_sources_value_never_renders_the_other_knobs_label pins the same
    // rule shared-Rust-side).
    [InlineData("reject", "block")]
    // A content-floor value ("collapse") is likewise outside this knob's catalog.
    [InlineData("collapse", "block")]
    [InlineData("inherit", "block")]
    public void FeedSources_NormalizesUnrecognizedToBlock(string wire, string expected)
        => Assert.Equal(expected, FamilyViewModel.NormalizeFeedSourcesWire(wire));

    /// <summary>
    /// family-safety.md § Content policy: each per-category floor is
    /// <c>inherit</c> | <c>collapse</c> | <c>block</c>, and <b>a floor value the
    /// client cannot parse renders fail-closed (<c>block</c>), never
    /// <c>inherit</c></b>. `inherit` is this knob's PERMISSIVE option (the ward's
    /// own preferences decide, no guardian rule at all), so a newer nest's floor
    /// value degrading to it would show the guardian a weaker policy than the one
    /// actually enforced — and, because <c>SavePolicyAsync</c> writes the editor's
    /// state back, the next save would persist that downgrade of the ward's
    /// protection for real.
    /// </summary>
    [Theory]
    [InlineData("inherit", "inherit")]
    [InlineData("collapse", "collapse")]
    [InlineData("block", "block")]
    // A value a NEWER nest could store that this build cannot name.
    [InlineData("quarantine", "block")]
    [InlineData("shadow_v2", "block")]
    [InlineData("", "block")]
    [InlineData("INHERIT", "block")] // wire values are exact; case variants are not ours
    public void ContentFloor_NormalizesUnrecognizedToBlock(string wire, string expected)
        => Assert.Equal(expected, FamilyViewModel.NormalizeContentFloorWire(wire));

    /// <summary>
    /// The merged-label-map trap, pinned for the content knob (same shape as the
    /// two <c>[InlineData]</c> cross-knob rows above, which pin the bug windows
    /// shipped once when one ValueLabel map served every knob). The content-floor
    /// value set is <b>disjoint</b> from the reach knobs' — <c>allow</c>/<c>hold</c>/
    /// <c>reject</c> are not content floors, and <c>inherit</c>/<c>collapse</c> are
    /// not reach values — so each must fail closed to its OWN knob's strict option
    /// rather than leak across as if recognized. fauna-core pins the same rule
    /// shared-Rust-side (<c>content_floor_label_key</c> has its own map,
    /// deliberately not reusing the reach-knob maps).
    /// <para><c>block</c> is the one genuinely shared spelling (it is both a
    /// <c>feed_sources</c> value and a content floor) — and it is the fail-closed
    /// answer on both knobs, so the overlap is harmless by construction.</para>
    /// </summary>
    [Theory]
    [InlineData("allow")]   // feed_sources / unknown_sender_mail
    [InlineData("hold")]    // unknown_sender_mail / unknown_peer_dm
    [InlineData("reject")]  // unknown_sender_mail
    public void ContentFloor_NeverAdoptsAReachKnobsValue(string reachWire)
        => Assert.Equal("block", FamilyViewModel.NormalizeContentFloorWire(reachWire));

    [Theory]
    [InlineData("inherit")]
    [InlineData("collapse")]
    public void UnknownSenderMail_NeverAdoptsAContentFloorValue(string contentWire)
        => Assert.Equal("hold", FamilyViewModel.NormalizeUnknownSenderWire(contentWire));

    /// <summary>
    /// <c>unknown_peer_dm</c> (family-safety.md § The bridge-DM gate) is the ONE
    /// knob whose wire type is <c>Option&lt;String&gt;</c> — an ABSENT value is NOT
    /// unparseable, it is the knob sitting at its <c>allow</c> DEFAULT (the nest
    /// omits a knob at its default; pinned shared-Rust-side by
    /// <c>an_absent_unknown_peer_dm_renders_its_allow_default_not_the_fail_closed_value</c>).
    /// A PRESENT-but-unrecognized value still fails closed to <c>hold</c>, this
    /// knob's strict option, exactly like its siblings above — including a value
    /// belonging to one of the other reach/content knobs, which must never leak in
    /// as if recognized (the merged-label-map trap the two theories above already
    /// pin for feed_sources/unknown_sender_mail).
    /// </summary>
    [Theory]
    [InlineData(null, "allow")]
    [InlineData("allow", "allow")]
    [InlineData("hold", "hold")]
    [InlineData("quarantine_v2", "hold")]
    [InlineData("", "hold")] // present-but-empty is NOT absent
    [InlineData("ALLOW", "hold")] // wire values are exact; case variants are not ours
    [InlineData("reject", "hold")]     // unknown_sender_mail's value
    [InlineData("block", "hold")]      // feed_sources / content-floor value
    [InlineData("inherit", "hold")]    // content-floor value
    [InlineData("collapse", "hold")]   // content-floor value
    public void UnknownPeerDm_NormalizesAbsentToAllowAndUnrecognizedToHold(string? wire, string expected)
        => Assert.Equal(expected, FamilyViewModel.NormalizeUnknownPeerDmWire(wire));
}
