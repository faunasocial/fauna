using System.Linq;
using uniffi.fauna_conversations;

namespace FaunaApp.Core.Services;

/// <summary>
/// <c>dm-message-withheld-badge</c> — the author's own conversation message the nest
/// withholds, matched to their queue row (takedown-and-block.md § Legal takedown →
/// <i>App render</i>, the fourth surface; conversations.md § Element IDs → <i>The author's
/// own withheld message</i>). Projected from the shared <c>MessageSnapshot.withheld</c>
/// (<c>WithheldOwnMessage</c>, refreshed by <c>ConversationsManager.RefreshWithheldMessages</c>
/// when a thread opens): every word and the ground are shared Rust's; this record only
/// resolves them. A PUBLIC record so the bubble's public view carries no UniFFI-internal
/// type (the <see cref="RegionPlaceholderModel"/> reason).
/// </summary>
/// <param name="ContentId">The matched queue row's <c>content_id</c> — what the tap's
/// appeal names (the Moderation page opens that row's own appeal).</param>
/// <param name="Text">The shared lines resolved NESTED (the ground line's argument is
/// itself a key) and joined side by side with <c>" · "</c> — the ground, the "decided by"
/// suffix when another nest decided, then the appeal entry.</param>
/// <param name="CarriesBlockNotice">Whether the badge already says what the beside-body
/// admin-block notice would (a <c>Blocked</c> ground — Rust
/// <c>WithheldOwnMessage::carries_block_notice</c>, read off the ground since the UniFFI
/// record has no methods).</param>
public sealed record WithheldBadgeView(string ContentId, string Text, bool CarriesBlockNotice)
{
    /// <summary>The badge for one message, or <c>null</c> when the nest withholds
    /// nothing of it.</summary>
    internal static WithheldBadgeView? From(WithheldOwnMessage? withheld) =>
        withheld is null
            ? null
            : new WithheldBadgeView(
                withheld.contentId,
                string.Join(" · ", withheld.lines.Select(Strings.ResolveNested)),
                withheld.ground == WithheldGround.Blocked);

    /// <summary>The admin-block notice to paint BESIDE the author's kept body, or
    /// <c>null</c>: none when the copy carries no block, and none when the badge already
    /// carries it — the bubble says it once (takedown-and-block.md § Admin block of a
    /// reported item, decision 4, beside the fourth surface).</summary>
    public static string? BesideBlockNotice(string? adminBlockNotice, WithheldBadgeView? badge) =>
        badge is { CarriesBlockNotice: true } ? null : adminBlockNotice;
}
