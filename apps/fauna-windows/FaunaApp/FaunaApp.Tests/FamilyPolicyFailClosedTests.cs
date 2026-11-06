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
/// <see cref="FamilyViewModel.NormalizeFeedSourcesWire"/> no longer hardcode the
/// recognized-value set or the fail-closed answer — they reverse-look-up the
/// shared <c>fauna_core::format</c> catalog/label FFI exports
/// (<c>FaunaFfiMethods.UnknownSenderLabel</c>/<c>Options</c>,
/// <c>FeedSourcesLabel</c>/<c>Options</c>). These tests therefore exercise the
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
    [InlineData("inherit", "hold")]
    [InlineData("collapse", "hold")]
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
    // A string that is no value of this knob ("collapse") fails closed too.
    [InlineData("collapse", "block")]
    [InlineData("inherit", "block")]
    public void FeedSources_NormalizesUnrecognizedToBlock(string wire, string expected)
        => Assert.Equal(expected, FamilyViewModel.NormalizeFeedSourcesWire(wire));

    /// <summary>
    /// <c>unknown_peer_dm</c> (family-safety.md § The bridge-DM gate) is the ONE
    /// knob whose wire type is <c>Option&lt;String&gt;</c> — an ABSENT value is NOT
    /// unparseable, it is the knob sitting at its <c>allow</c> DEFAULT (the nest
    /// omits a knob at its default; pinned shared-Rust-side by
    /// <c>an_absent_unknown_peer_dm_renders_its_allow_default_not_the_fail_closed_value</c>).
    /// A PRESENT-but-unrecognized value still fails closed to <c>hold</c>, this
    /// knob's strict option, exactly like its siblings above — including a value
    /// belonging to one of the other reach/content knobs, which must never leak in
    /// as if recognized (the merged-label-map trap the theories above already
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
    [InlineData("block", "hold")]      // feed_sources' value
    [InlineData("inherit", "hold")]    // no value of this knob
    [InlineData("collapse", "hold")]   // no value of this knob

    public void UnknownPeerDm_NormalizesAbsentToAllowAndUnrecognizedToHold(string? wire, string expected)
        => Assert.Equal(expected, FamilyViewModel.NormalizeUnknownPeerDmWire(wire));
}
