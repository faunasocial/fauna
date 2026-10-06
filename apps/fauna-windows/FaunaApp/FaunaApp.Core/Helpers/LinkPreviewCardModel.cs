using System.Collections.Generic;
using System.Linq;
using uniffi.fauna_core;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// The <c>link-preview-card</c> fields a feed post (<c>FeedPostItem</c>) OR a conversation
/// DM bubble (<c>DmMessageBubble</c>) paints, derived once from a document's folded
/// <c>LinkPreview</c> blocks (render-model.md § D4). Single-sourced so the feed and the
/// bubble paint the SAME card from one derivation (priority #2/#4) instead of two copies
/// that can drift.
///
/// <para><b>A document carries a LIST of these, not one.</b> The producer's bare-URL rule
/// emits a <c>LinkPreview</c> after **each** standalone bare-URL paragraph, so a body with
/// two bare URLs yields two cards. <see cref="All"/> returns every one, in body order —
/// windows previously painted only the first (a <c>FirstOrDefault</c> twin), the lone-outlier
/// gap recorded in render-model.md § Implementation status; the other six apps all iterate
/// the full list.</para>
///
/// Only a <c>Resolved</c> block yields a card — the shared
/// <c>render_document_resolved_link_previews</c> face pre-filters, so neither this model nor
/// any call site re-derives a <c>PreviewState.Resolved</c> match (the per-call-site
/// re-derivation web and apple deleted). <c>Resolving</c>/<c>Failed</c>/absent simply
/// contribute no card, and the producer keeps the inline body link, which already shows the
/// URL — no skeleton, matching the shipped web reference.
///
/// og:image reveal gate (user-ratified 2026-06-27): the og:image is a nest-served
/// content-addressed blob but obeys the post/message's D3 remote-content reveal exactly like
/// a body <c>RemoteImage</c>. <see cref="ShowImage"/> stays <c>false</c> and
/// <see cref="ImageHash"/> is WITHHELD (<c>""</c>) until <see cref="ImageRevealed"/>, so the
/// card never fetches the blob before the <c>load-remote-content-button</c> is tapped.
///
/// Public (not internal) because the feed paints these through a XAML <c>ItemsControl</c>
/// whose <c>DataTemplate</c> is <c>x:DataType</c>-bound to this type; every member is a
/// primitive, so nothing UniFFI-internal leaks into the public surface (the CS0053 trap
/// <c>SubscriptionOffer.Status</c> documents).
/// </summary>
public sealed record LinkPreviewCardModel(
    string Url,
    string Title,
    string Description,
    string Domain,
    bool ImageRevealed,
    bool ShowImage,
    string ImageHash)
{
    /// <summary>Every resolved link-preview card in <paramref name="doc"/>, in body order —
    /// empty when the document folds none, or none has resolved yet. Delegates to the shared
    /// <c>RenderDocument::resolved_link_previews</c> (which recurses, unlike the top-level-only
    /// twin this replaced). Domain via the shared <c>fauna_core::format::url_host</c>
    /// (<c>FaunaFfiMethods.UrlHost</c>), not a per-app parse.</summary>
    /// <remarks><c>internal</c>, not <c>public</c>, because the generated
    /// <c>RenderDocument</c> is an <c>internal</c> UniFFI record — a public method taking it
    /// is CS0051 (the accessibility trap <c>SubscriptionOffer.Status</c> documents). The TYPE
    /// stays public so the feed's <c>DataTemplate</c> can <c>x:DataType</c>-bind it; every
    /// painted property is a primitive, so nothing UniFFI-internal leaks.</remarks>
    internal static IReadOnlyList<LinkPreviewCardModel> All(RenderDocument doc)
        => DocumentRenderer.ResolvedLinkPreviews(doc).Select(FromResolved).ToList();

    /// <summary>Project one shared <c>ResolvedLinkPreviewOwned</c> onto the painted card,
    /// applying the D3 og:image withholding.</summary>
    private static LinkPreviewCardModel FromResolved(ResolvedLinkPreviewOwned p)
    {
        var show = !string.IsNullOrEmpty(p.imageHash) && p.revealed;
        return new LinkPreviewCardModel(
            Url: p.url,
            Title: p.title,
            Description: p.description,
            Domain: FaunaFfiMethods.UrlHost(p.url),
            ImageRevealed: p.revealed,
            ShowImage: show,
            // Withhold the fetch-target hash until reveal — the card must never load the
            // blob before the post/message's remote content is revealed (the D3 posture).
            ImageHash: show ? p.imageHash! : "");
    }
}
