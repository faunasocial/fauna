using System.Collections.Generic;
using System.Linq;
using uniffi.fauna_core;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// The <c>doc-remote-image</c> fields a feed post (<c>FeedPostItem</c>) OR a conversation
/// DM bubble (<c>DmMessageBubble</c>) paints, derived once from a document's folded
/// <c>RemoteImage</c> blocks (render-model.md § D3 + § Implementation status; apps/tui.md
/// § Rendering). Single-sourced so the feed and the bubble paint the SAME element from one
/// derivation (priority #2/#4) instead of two copies that can drift — the
/// <see cref="Helpers.LinkPreviewCardModel"/> shape, applied to the fifth lifted
/// embed-projection.
///
/// <para><b>A document carries a LIST of these, not one.</b> A body can carry several remote
/// images, and one reveal frees them all. <see cref="All"/> returns every one, in body order,
/// via the shared <c>RenderDocument::remote_images</c> face (recurses, unlike a hand-rolled
/// top-level-only walk).</para>
///
/// The element registers in EVERY state under the same id — blocked, revealed-but-loading,
/// painted — so "has it painted?" is a question about the element's content, not its
/// existence (ui.yaml <c>doc-remote-image</c>, indexed, ID user-approved 2026-07-31).
/// <see cref="Revealed"/> gates the fetch: the url is requested only after the post's/
/// message's <c>load-remote-content-button</c> (the shared D3 reveal set), never before.
///
/// Public (not internal) because the feed paints these through a XAML <c>ItemsControl</c>
/// whose <c>DataTemplate</c> is <c>x:DataType</c>-bound to this type; every member is a
/// primitive, so nothing UniFFI-internal leaks into the public surface (the CS0053 trap
/// <c>SubscriptionOffer.Status</c> documents).
/// </summary>
public sealed record RemoteImageCardModel(
    string Url,
    string Alt,
    bool Revealed)
{
    /// <summary>Every <c>doc-remote-image</c> in <paramref name="doc"/>, in body order —
    /// empty when the document folds none. Delegates to
    /// <see cref="DocumentRenderer.RemoteImages"/> (the shared
    /// <c>RenderDocument::remote_images</c> face).</summary>
    /// <remarks><c>internal</c>, not <c>public</c>, because the generated
    /// <c>RenderDocument</c> is an <c>internal</c> UniFFI record — a public method taking it
    /// is CS0051 (the accessibility trap <c>SubscriptionOffer.Status</c> documents). The TYPE
    /// stays public so the feed's <c>DataTemplate</c> can <c>x:DataType</c>-bind it; every
    /// painted property is a primitive, so nothing UniFFI-internal leaks.</remarks>
    internal static IReadOnlyList<RemoteImageCardModel> All(RenderDocument doc)
        => DocumentRenderer.RemoteImages(doc).Select(FromRef).ToList();

    /// <summary>Project one shared <c>RemoteImageRefOwned</c> onto the painted card.</summary>
    private static RemoteImageCardModel FromRef(RemoteImageRefOwned r)
        => new(Url: r.url, Alt: r.alt, Revealed: r.revealed);
}
