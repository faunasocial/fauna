namespace FaunaApp.Core.Services;

/// <summary>
/// Which arm a social item (feed post-card / conversation bubble) renders. Exactly
/// one arm wins; the order they are checked in is the load-bearing part, not the
/// individual arms — see <see cref="SocialRenderGate.Decide"/>.
/// </summary>
public enum SocialRenderArm
{
    /// <summary>Cooperative delete tombstone (<c>dm-message-deleted</c>) — DM only.</summary>
    Deleted,

    /// <summary>Legal-takedown tombstone (moderation.md § Categories &amp; enforcement
    /// item 1) — DM only; the feed's takedown arm lives on the quoted-post embed.</summary>
    LegalTakedown,

    /// <summary>The region content policy withholds the item (region-blocking.md
    /// § The blocked render): the <c>region-blocked-notice</c> placeholder — frame,
    /// authority, the authority's reason verbatim — in place of the body; a
    /// <c>collapse</c> adds the <c>region-collapsed-reveal-button</c>, a
    /// <c>block</c> has no reveal.</summary>
    RegionWithheld,

    /// <summary>Content-policy <c>block</c>: the policy-naming
    /// <c>content-policy-blocked-notice</c> in place of the body, with NO reveal.</summary>
    ContentBlocked,

    /// <summary>Muted-keyword collapse behind its own session-local reveal.</summary>
    Muted,

    /// <summary>Content-policy <c>collapse</c> behind a session-local reveal.</summary>
    ContentCollapsed,

    /// <summary>Nothing withholds the item — paint the full body.</summary>
    Live,
}

/// <summary>
/// The ONE ordering decision both social render surfaces make — the feed
/// post-card (<c>FeedPostItem</c>) and the conversation bubble
/// (<c>DmMessageBubble.Bind</c>). Pure: no cache reads, no FFI, no UI types, so
/// the ordering is unit-testable rather than only observable through a running
/// WinUI tree.
///
/// <para>Sharing one function is the point (priority #2/#4, and the same reason
/// <see cref="ContentPolicyCache"/> is a single static): the two surfaces can
/// never drift on how a floor is enforced. It is the windows twin of linux's
/// twin early-return chains (<c>views/feed/post_list.rs::build_post_card</c> and
/// <c>views/conversations/message_bubble.rs</c>), web's <c>PostCard.svelte</c> /
/// <c>conversations/+page.svelte</c> arms, and android's
/// <c>FeedScreen</c>/<c>ConversationDetailScreen</c> arms.</para>
/// </summary>
public static class SocialRenderGate
{
    /// <summary>
    /// Resolve the render arm for one item.
    ///
    /// <para><b>The ordering is the claim</b> (family-safety.md § Content policy,
    /// the linux reference leg): a <c>block</c> is evaluated <b>ahead of the muted
    /// arm and ahead of the reveal set</b>, so an item that is both blocked and
    /// muted (or blocked and already revealed) can never be revealed past the
    /// guardian's block — a <c>block</c> has no reveal affordance at all. A
    /// <c>collapse</c> sits <i>after</i> the muted arm because both are revealable
    /// collapses, and the muted placeholder is the more specific explanation.</para>
    /// </summary>
    /// <param name="contentVerdict">The shared render verdict for the item's
    /// labels — <c>"show" | "badge" | "collapse" | "block"</c>, from
    /// <see cref="ContentPolicyCache.VerdictFor"/>. <c>"badge"</c> and
    /// <c>"show"</c> are identical to this gate (both render normally; the badge
    /// is painted beside the body either way).</param>
    /// <param name="contentRevealed">Whether the viewer tapped "show anyway" on
    /// this item's content-policy collapse this session
    /// (<see cref="ContentPolicyCache.IsRevealed"/>). Deliberately consulted ONLY
    /// for the <c>collapse</c> arm.</param>
    /// <param name="muted">Whether the muted-keyword collapse applies, i.e. the
    /// item matched the muted list AND its own (separate) session reveal has not
    /// been tapped — that reveal is already folded in by the caller
    /// (<c>MutedKeywordsCache.IsMuted</c> / <c>FeedPostItem.Revealed</c>).</param>
    /// <param name="deleted">DM only: the cooperative delete tombstone.</param>
    /// <param name="legalTakedownRef">DM only: the nest withheld the envelope
    /// under a legal obligation. Outranks the content policy — a takedown is a
    /// legal obligation on the nest, not a family preference.</param>
    /// <param name="regionVerb">The region placeholder's verb when the REGION drove
    /// the composed verdict (<c>"block"</c> / <c>"collapse"</c>,
    /// <c>RegionRenderDecision.Placeholder</c>), else <c>null</c>. Painted AHEAD of
    /// the family and muted arms — same verb, better attributed (the linux
    /// reference leg's <c>region_withheld</c>, checked first); a region
    /// <c>collapse</c> shares the family reveal set, so once revealed the composed
    /// <c>"collapse"</c> verdict falls through to the family collapse arm, which
    /// the same reveal has already lifted.</param>
    public static SocialRenderArm Decide(
        string contentVerdict,
        bool contentRevealed,
        bool muted,
        bool deleted = false,
        string? legalTakedownRef = null,
        string? regionVerb = null)
    {
        if (deleted) return SocialRenderArm.Deleted;
        if (!string.IsNullOrEmpty(legalTakedownRef)) return SocialRenderArm.LegalTakedown;
        if (regionVerb == "block" || (regionVerb == "collapse" && !contentRevealed))
            return SocialRenderArm.RegionWithheld;
        // ⚠ Do NOT move this below the muted arm, and do NOT gate it on
        // contentRevealed: either change silently makes a blocked item revealable.
        if (contentVerdict == "block") return SocialRenderArm.ContentBlocked;
        if (muted) return SocialRenderArm.Muted;
        if (contentVerdict == "collapse" && !contentRevealed) return SocialRenderArm.ContentCollapsed;
        return SocialRenderArm.Live;
    }
}
