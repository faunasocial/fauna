using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_core;
using uniffi.fauna_feed;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The TWO social render surfaces enforcing the content policy end to end
/// (family-safety.md § Content policy, claim: "it binds at the two render surfaces
/// — the feed read model and the conversations render"): real labels → the real
/// shared <c>ContentRenderVerdict</c> compose (the native <c>fauna_ffi</c> dll
/// loads in the test host) → the shared <see cref="SocialRenderGate"/> arm → the
/// gates the XAML paints.
///
/// <para>The feed half drives the REAL <see cref="FeedPostItem"/> the
/// <c>post-card</c> DataTemplate binds. The DM half composes the two calls
/// <c>ConversationsPage.ToMessageView</c> + <c>DmMessageBubble.Bind</c> make, since
/// both live in the WinUI app assembly this project deliberately does not
/// reference — see <see cref="DmArm"/>.</para>
///
/// <para>Guardian floors trigger at a hard-coded 500‰
/// (<c>GUARDIAN_FLOOR_TRIGGER_PERMILLE</c>) independent of the viewer's own
/// slider, so every fixture below uses 900‰ ("well above") / 100‰ ("well below")
/// and never sits on that boundary.</para>
///
/// <para>The pure ORDERING claims (block ahead of muted, block ahead of every
/// reveal, takedown ahead of block) are pinned in
/// <see cref="SocialRenderGateTests"/> — the feed cannot express a muted post in a
/// unit test (<c>FeedPostItem.IsMuted</c> is a live <c>FfiFeedManager</c> query and
/// is <c>false</c> for a manager-less item), so the ordering is pinned once on the
/// shared gate both surfaces delegate to rather than duplicated per surface.</para>
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class ContentPolicyRenderTests : IDisposable
{
    // Process-lifetime static state (see ContentPolicyCacheTests) — reset around
    // each test, and serialized against every other class that touches it by the
    // shared collection above.
    public ContentPolicyRenderTests() => ContentPolicyCache.Reset();
    public void Dispose() => ContentPolicyCache.Reset();

    /// <summary>A guardian who blocks the <c>nsfw</c> category and inherits the
    /// rest — the supervised-ward shape.</summary>
    private static void GuardianBlocksNsfw() =>
        ContentPolicyCache.SetGuardianPolicy(
            FfiContentPolicyFixture.Make(nsfw: "block"));

    /// <summary>The every-user half (moderation.md § Categories &amp; enforcement
    /// item 1): the viewer's OWN spam/phishing thresholds, no guardian involved.
    /// An own threshold only ever COLLAPSES — it can never block.</summary>
    private static void ViewerCollapsesSpamAbove(ushort permille) =>
        ContentPolicyCache.SetOwnThresholds((permille, permille));

    private static ContentLabelEntry[] Labels(params (string Category, ushort Permille)[] entries)
    {
        var result = new ContentLabelEntry[entries.Length];
        for (var i = 0; i < entries.Length; i++)
        {
            result[i] = new ContentLabelEntry(entries[i].Category, entries[i].Permille);
        }
        return result;
    }

    // ── Feed post-card (FeedViewModel.FeedPostItem + FeedPage.xaml gates) ──

    /// <summary>
    /// A guardian <c>block</c> floor over the post's labels paints the
    /// <c>content-policy-blocked-notice</c> IN PLACE OF the body: the normal-content
    /// gate is off and the blocked gate is on. The notice naming the policy — rather
    /// than the post vanishing — is the ward-transparency claim (family-safety.md
    /// § Content policy).
    /// </summary>
    [Fact]
    public void Feed_Block_HidesBodyAndShowsTheNotice()
    {
        GuardianBlocksNsfw();

        var item = new FeedPostItem(LabeledPost("p1", ("nsfw", 900)));

        Assert.Equal("block", item.ContentVerdict);
        Assert.True(item.ShowContentBlockedNotice);
        Assert.False(item.ShowNormalContent);
        Assert.False(item.ShowContentCollapse);
        Assert.False(item.ShowMutedCollapse);
    }

    /// <summary>
    /// <b>The sharpest arm: a blocked post has NO reveal path.</b> The collapse
    /// gate (the only gate that paints a reveal button) is off, and driving the
    /// reveal anyway — both through the card's own <c>RevealContent()</c> and
    /// through the cache directly, as a stale session entry for this id would —
    /// leaves the post blocked with its body still withheld.
    /// </summary>
    [Fact]
    public void Feed_Block_IsNotRevealable()
    {
        GuardianBlocksNsfw();

        var item = new FeedPostItem(LabeledPost("p1", ("nsfw", 900)));

        // No reveal affordance is painted for a block...
        Assert.False(item.ShowContentCollapse);

        // ...and forcing the reveal by both available routes changes nothing.
        item.RevealContent();
        ContentPolicyCache.Reveal("p1");

        Assert.True(item.ContentRevealed); // the session set really did record it
        Assert.True(item.ShowContentBlockedNotice);
        Assert.False(item.ShowNormalContent);
    }

    /// <summary>
    /// A <c>collapse</c> (here the every-user own-threshold half — no guardian at
    /// all) withholds the body behind a one-tap reveal, and the reveal un-collapses
    /// it for the session.
    /// </summary>
    [Fact]
    public void Feed_Collapse_HidesBodyBehindReveal_ThenRevealShowsIt()
    {
        ViewerCollapsesSpamAbove(500);

        var item = new FeedPostItem(LabeledPost("p1", ("spam", 900)));

        Assert.Equal("collapse", item.ContentVerdict);
        Assert.True(item.ShowContentCollapse);
        Assert.False(item.ShowNormalContent);
        Assert.False(item.ShowContentBlockedNotice);

        item.RevealContent();

        Assert.True(item.ShowNormalContent);
        Assert.False(item.ShowContentCollapse);
    }

    /// <summary>
    /// The reveal is keyed on the CACHE by post id, not on a per-instance flag, so
    /// it survives the observer-tick rebuild that constructs a brand-new
    /// <see cref="FeedPostItem"/> for the same post. A per-instance bool (the shape
    /// the muted arm gets away with, because <c>Revealed</c> is excluded from
    /// <c>ContentEquals</c> so its instance usually survives) would silently snap
    /// the post back to collapsed here.
    /// </summary>
    [Fact]
    public void Feed_CollapseReveal_SurvivesAnObserverTickRebuild()
    {
        ViewerCollapsesSpamAbove(500);

        var first = new FeedPostItem(LabeledPost("p1", ("spam", 900)));
        first.RevealContent();

        var rebuilt = new FeedPostItem(LabeledPost("p1", ("spam", 900)));

        Assert.True(rebuilt.ShowNormalContent);
        Assert.False(rebuilt.ShowContentCollapse);
    }

    /// <summary>
    /// <c>badge</c> and <c>show</c> render normally — identical to the render gate.
    /// A label below the viewer's threshold badges; an unlabeled post shows; an
    /// unsupervised viewer with no thresholds at all shows even a 900‰ label.
    /// </summary>
    [Fact]
    public void Feed_BadgeAndShow_RenderNormally()
    {
        ViewerCollapsesSpamAbove(500);

        var belowThreshold = new FeedPostItem(LabeledPost("p1", ("spam", 100)));
        Assert.Equal("badge", belowThreshold.ContentVerdict);
        Assert.True(belowThreshold.ShowNormalContent);
        Assert.False(belowThreshold.ShowContentCollapse);
        Assert.False(belowThreshold.ShowContentBlockedNotice);
        // The badge itself still paints beside the body — the two are independent.
        Assert.True(belowThreshold.HasContentLabel);

        var unlabeled = new FeedPostItem(LabeledPost("p2"));
        Assert.Equal("show", unlabeled.ContentVerdict);
        Assert.True(unlabeled.ShowNormalContent);

        // No policy hydrated at all (the unsupervised, pre-preload posture): the
        // snapshot short-circuits to "show" and the card renders in full.
        ContentPolicyCache.Reset();
        var noPolicy = new FeedPostItem(LabeledPost("p3", ("nsfw", 900)));
        Assert.Equal("show", noPolicy.ContentVerdict);
        Assert.True(noPolicy.ShowNormalContent);
    }

    /// <summary>
    /// The verdict is part of <c>ContentEquals</c>, so a policy that hydrates AFTER
    /// the feed's first paint makes the affected row compare unequal and rebuild on
    /// the next observer tick — i.e. the floor actually reaches already-rendered
    /// posts. Without this the reconcile would keep the un-enforced row forever.
    /// </summary>
    [Fact]
    public void Feed_LatePolicyHydration_MakesTheRowRebuild()
    {
        var beforeHydration = new FeedPostItem(LabeledPost("p1", ("nsfw", 900)));
        Assert.True(beforeHydration.ShowNormalContent);

        GuardianBlocksNsfw();
        var afterHydration = new FeedPostItem(LabeledPost("p1", ("nsfw", 900)));

        Assert.False(beforeHydration.ContentEquals(afterHydration));
        Assert.True(afterHydration.ShowContentBlockedNotice);
    }

    // ── Conversation bubble (ConversationsPage.ToMessageView + DmMessageBubble) ──

    /// <summary>
    /// The exact two-step composition the DM surface performs:
    /// <c>ToMessageView</c> projects <c>MessageSnapshot.labels</c> +
    /// <c>messageId</c> through <see cref="ContentPolicyCache"/> onto
    /// <c>DmMessageView.{ContentVerdict, ContentRevealed}</c>, and <c>Bind</c>
    /// hands those to <see cref="SocialRenderGate.Decide"/> together with the
    /// message's deleted / legal-takedown / muted state. Reproduced here because
    /// both types live in the WinUI app assembly this project does not reference.
    /// </summary>
    private static SocialRenderArm DmArm(
        ContentLabelEntry[] labels,
        string messageId,
        bool muted = false,
        bool deleted = false,
        string? legalTakedownRef = null) =>
        SocialRenderGate.Decide(
            ContentPolicyCache.VerdictFor(labels),
            ContentPolicyCache.IsRevealed(messageId),
            muted,
            deleted,
            legalTakedownRef);

    /// <summary>A guardian <c>block</c> over a message's post-decrypt labels
    /// collapses the bubble to <c>content-policy-blocked-notice</c>.</summary>
    [Fact]
    public void Dm_Block_CollapsesToTheNotice()
    {
        GuardianBlocksNsfw();

        Assert.Equal(
            SocialRenderArm.ContentBlocked,
            DmArm(Labels(("nsfw", 900)), "msg-1"));
    }

    /// <summary>...and no reveal reaches it, even with this message id already in
    /// the session reveal set.</summary>
    [Fact]
    public void Dm_Block_IsNotRevealable()
    {
        GuardianBlocksNsfw();
        ContentPolicyCache.Reveal("msg-1");

        Assert.Equal(
            SocialRenderArm.ContentBlocked,
            DmArm(Labels(("nsfw", 900)), "msg-1"));
    }

    /// <summary>A <c>collapse</c> sits behind the reveal; revealing this message id
    /// renders it live (and only THAT message — the set is per item).</summary>
    [Fact]
    public void Dm_Collapse_HidesBehindReveal_ThenRevealShowsIt()
    {
        ViewerCollapsesSpamAbove(500);

        Assert.Equal(SocialRenderArm.ContentCollapsed, DmArm(Labels(("spam", 900)), "msg-1"));

        ContentPolicyCache.Reveal("msg-1");

        Assert.Equal(SocialRenderArm.Live, DmArm(Labels(("spam", 900)), "msg-1"));
        Assert.Equal(SocialRenderArm.ContentCollapsed, DmArm(Labels(("spam", 900)), "msg-2"));
    }

    /// <summary><c>badge</c>/<c>show</c> render the bubble normally.</summary>
    [Fact]
    public void Dm_BadgeAndShow_RenderNormally()
    {
        ViewerCollapsesSpamAbove(500);

        Assert.Equal(SocialRenderArm.Live, DmArm(Labels(("spam", 100)), "msg-1"));
        Assert.Equal(SocialRenderArm.Live, DmArm(Labels(), "msg-2"));
    }

    /// <summary>
    /// <b>The ordering claim on the live DM composition:</b> a message that is BOTH
    /// guardian-blocked AND muted renders the blocked notice, never the muted
    /// placeholder — the muted arm carries a reveal button, so taking it would hand
    /// the viewer a way past the guardian's floor.
    /// </summary>
    [Fact]
    public void Dm_Block_OutranksTheMutedArm()
    {
        GuardianBlocksNsfw();

        Assert.Equal(
            SocialRenderArm.ContentBlocked,
            DmArm(Labels(("nsfw", 900)), "msg-1", muted: true));
    }

    /// <summary>
    /// <b>...and a legal takedown still outranks the content-policy block</b>
    /// (moderation.md § Categories &amp; enforcement item 1): the tombstone naming
    /// the legal reference must not be replaced by a family-policy notice.
    /// </summary>
    [Fact]
    public void Dm_LegalTakedown_OutranksContentBlock()
    {
        GuardianBlocksNsfw();

        Assert.Equal(
            SocialRenderArm.LegalTakedown,
            DmArm(Labels(("nsfw", 900)), "msg-1", muted: true,
                legalTakedownRef: "EU-DSA-2024/12345"));
    }

    // ── fixtures ──────────────────────────────────────────────────────────

    /// <summary>Mint a <see cref="PostSummary"/> with the given id + content labels;
    /// every other field is an inert default (a one-paragraph body document so the
    /// card has something to withhold).</summary>
    private static PostSummary LabeledPost(
        string postId, params (string Category, ushort Permille)[] labels) =>
        PostSummaryFixture.Make(
            postId: postId, body: "the body",
            document: new RenderDocument(new RenderBlock[]
            {
                new RenderBlock.Paragraph(new Inline[] { new Inline.Text("the body") }),
            }),
            labels: Labels(labels));
}
