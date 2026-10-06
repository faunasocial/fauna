using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The ONE ordering decision both social render surfaces make — the feed
/// post-card (<c>FeedPostItem.RenderArm</c>) and the conversation bubble
/// (<c>DmMessageBubble.Bind</c>) — family-safety.md § Content policy.
///
/// <para>These are the DM arms in particular: <c>DmMessageView</c> and
/// <c>DmMessageBubble</c> live in the WinUI app assembly, which this test project
/// deliberately does not reference (it compiles <c>FaunaApp.Core</c> only), so the
/// bubble's ordering is testable precisely BECAUSE the decision was lifted into
/// <see cref="SocialRenderGate"/> instead of living inside <c>Bind</c>'s
/// early-return chain. The bubble now only paints the arm it is handed.</para>
///
/// <para><b>The ordering — not the individual arms — is what these pin.</b> Each
/// arm on its own is obvious; what a reviewer cannot verify by reading, and what
/// silently regresses, is that <c>block</c> is resolved AHEAD of the muted arm and
/// AHEAD of every reveal set. Reordering the two <c>if</c>s in
/// <see cref="SocialRenderGate.Decide"/> compiles, renders plausibly, and quietly
/// makes a guardian-blocked message revealable — these are the only thing that
/// catches it.</para>
///
/// <para>Pure: no <see cref="ContentPolicyCache"/> mutation, no FFI, so no xUnit
/// collection is needed (unlike <see cref="ContentPolicyCacheTests"/>). The
/// labels → verdict half is pinned there and in
/// <see cref="ContentPolicyRenderTests"/>.</para>
/// </summary>
public class SocialRenderGateTests
{
    // ── The four content-policy verdicts (both surfaces) ──────────────────

    /// <summary>A <c>block</c> paints the policy-naming notice in place of the
    /// body (family-safety.md § Content policy — never a silent disappearance).</summary>
    [Fact]
    public void Block_TakesTheBlockedArm()
    {
        Assert.Equal(
            SocialRenderArm.ContentBlocked,
            SocialRenderGate.Decide("block", contentRevealed: false, muted: false));
    }

    /// <summary>A <c>collapse</c> sits behind the session-local reveal...</summary>
    [Fact]
    public void Collapse_TakesTheCollapsedArm()
    {
        Assert.Equal(
            SocialRenderArm.ContentCollapsed,
            SocialRenderGate.Decide("collapse", contentRevealed: false, muted: false));
    }

    /// <summary>...and once revealed this session, renders the full body.</summary>
    [Fact]
    public void Collapse_OnceRevealed_RendersLive()
    {
        Assert.Equal(
            SocialRenderArm.Live,
            SocialRenderGate.Decide("collapse", contentRevealed: true, muted: false));
    }

    /// <summary><c>show</c> and <c>badge</c> are IDENTICAL to this gate: both
    /// render normally (the badge is painted beside the body either way). Only
    /// <c>collapse</c> and <c>block</c> change the render.</summary>
    [Fact]
    public void ShowAndBadge_BothRenderLive()
    {
        Assert.Equal(SocialRenderArm.Live,
            SocialRenderGate.Decide("show", contentRevealed: false, muted: false));
        Assert.Equal(SocialRenderArm.Live,
            SocialRenderGate.Decide("badge", contentRevealed: false, muted: false));
    }

    // ── THE ORDERING CLAIM (family-safety.md § Content policy, linux ref leg) ──

    /// <summary>
    /// <b>A blocked item is never revealable.</b> Even with the content reveal set
    /// — the "show anyway" the collapse arm offers — already marked for this item,
    /// the verdict stays <see cref="SocialRenderArm.ContentBlocked"/>: the block
    /// check sits ahead of every reveal consultation, so a <c>block</c> paints no
    /// reveal button and no reveal can reach it.
    /// </summary>
    [Fact]
    public void Block_IsNotRevealable_EvenWhenTheRevealSetSaysRevealed()
    {
        Assert.Equal(
            SocialRenderArm.ContentBlocked,
            SocialRenderGate.Decide("block", contentRevealed: true, muted: false));
    }

    /// <summary>
    /// <b>A block outranks the muted arm.</b> An item that is BOTH muted and
    /// blocked must render the blocked notice, not the muted placeholder — the
    /// muted placeholder carries a reveal button, so taking that arm would hand
    /// the viewer a way past the guardian's floor. This is the exact regression a
    /// reordering of <see cref="SocialRenderGate.Decide"/>'s two <c>if</c>s
    /// introduces, and it is invisible to a reader of the bubble/card markup.
    /// </summary>
    [Fact]
    public void Block_OutranksTheMutedArm()
    {
        Assert.Equal(
            SocialRenderArm.ContentBlocked,
            SocialRenderGate.Decide("block", contentRevealed: false, muted: true));
    }

    /// <summary>The muted arm still wins over a content <c>collapse</c> (both are
    /// revealable collapses; the muted placeholder is the more specific
    /// explanation) — the linux leg's order, so windows can't drift.</summary>
    [Fact]
    public void Muted_OutranksTheContentCollapse()
    {
        Assert.Equal(
            SocialRenderArm.Muted,
            SocialRenderGate.Decide("collapse", contentRevealed: false, muted: true));
    }

    // ── DM-only arms: the two tombstones outrank the content policy ────────

    /// <summary>
    /// <b>Legal takedown still outranks a content-policy block.</b> A takedown is a
    /// legal obligation on the nest (moderation.md § Categories &amp; enforcement
    /// item 1) — its tombstone names the legal reference, which a family-policy
    /// notice would wrongly replace. Asserted on an item that is takedown AND
    /// blocked AND muted at once.
    /// </summary>
    [Fact]
    public void LegalTakedown_OutranksContentBlock()
    {
        Assert.Equal(
            SocialRenderArm.LegalTakedown,
            SocialRenderGate.Decide(
                "block", contentRevealed: false, muted: true, deleted: false,
                legalTakedownRef: "EU-DSA-2024/12345"));
    }

    /// <summary>An EMPTY takedown reference is not a takedown (the snapshot field
    /// is an empty-string-vs-null wire hazard) — the content policy then decides.</summary>
    [Fact]
    public void EmptyLegalTakedownRef_IsNotATakedown()
    {
        Assert.Equal(
            SocialRenderArm.ContentBlocked,
            SocialRenderGate.Decide(
                "block", contentRevealed: false, muted: false, deleted: false,
                legalTakedownRef: ""));
    }

    /// <summary>The cooperative delete tombstone outranks everything — a deleted
    /// message has no body to withhold in the first place.</summary>
    [Fact]
    public void Deleted_OutranksEverything()
    {
        Assert.Equal(
            SocialRenderArm.Deleted,
            SocialRenderGate.Decide(
                "block", contentRevealed: false, muted: true, deleted: true,
                legalTakedownRef: "EU-DSA-2024/12345"));
    }

    /// <summary>The default of every optional input is "nothing withholds it".</summary>
    [Fact]
    public void NothingWithholding_RendersLive()
    {
        Assert.Equal(
            SocialRenderArm.Live,
            SocialRenderGate.Decide("show", contentRevealed: false, muted: false));
    }
}
