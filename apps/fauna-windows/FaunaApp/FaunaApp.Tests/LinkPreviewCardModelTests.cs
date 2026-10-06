using System.Linq;
using FaunaApp.Core.Helpers;
using uniffi.fauna_core;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The shared <c>LinkPreviewCardModel.All(document)</c> derivation that BOTH the feed
/// post card (<c>FeedPostItem</c>) and the conversations DM bubble
/// (<c>DmMessageBubble</c>) paint from — one source for the <c>link-preview-card</c>
/// (render-model.md § D4), so the feed and the bubble can't drift (priority #2/#4).
///
/// <para>It returns a LIST: the producer folds a <c>LinkPreview</c> after EACH standalone
/// bare-URL paragraph, so a two-URL body paints two cards. windows painted only the first
/// until 2026-08-02 (a <c>FirstOrDefault</c> twin — the lone-outlier gap in render-model.md
/// § Implementation status).</para>
///
/// Only a <c>Resolved</c> folded block yields a card; <c>Resolving</c>/<c>Failed</c>/absent
/// contribute none (the kept inline body link shows the URL — no skeleton, matching the web
/// reference). The og:image is reveal-gated (user-ratified 2026-06-27): blocked-by-default
/// like any <c>RemoteImage</c>, its blob hash is WITHHELD until revealed so the card never
/// fetches the blob before the post/message's <c>load-remote-content-button</c> is tapped.
///
/// The derivation delegates to the shared <c>render_document_resolved_link_previews</c>, so
/// the state filter and the tree walk are shared Rust, not per-app C#; the native
/// <c>fauna_ffi</c> dll loads in the test host
/// (<c>reference_windows_dotnet_test_loads_native_ffi</c>) so these exercise the real shared
/// projection and the real <c>fauna_core::format::url_host</c>.
/// </summary>
public class LinkPreviewCardModelTests
{
    [Fact]
    public void Resolved_DerivesCardTextFromDocument()
    {
        var card = Assert.Single(LinkPreviewCardModel.All(MakeDoc(
            new PreviewState.Resolved("My Title", "A short description", "imghash", revealed: false))));

        Assert.Equal("https://example.com/article", card.Url);
        Assert.Equal("My Title", card.Title);
        Assert.Equal("A short description", card.Description);
        // Domain via the shared fauna_core::format::url_host (FaunaFfiMethods.UrlHost),
        // not a per-app parse — render-model.md § D4 (priority #1/#2).
        Assert.Equal("example.com", card.Domain);
    }

    [Fact]
    public void ResolvedRevealed_ExposesOgImageHashAndShows()
    {
        // Once the post/message's remote content is revealed, the og:image paints and its
        // hash (the /api/v1/blob/<hash> fetch target) is exposed for the card's loader.
        var card = Assert.Single(LinkPreviewCardModel.All(MakeDoc(
            new PreviewState.Resolved("My Title", "A short description", "imghash", revealed: true))));

        Assert.True(card.ImageRevealed);
        Assert.True(card.ShowImage);
        Assert.Equal("imghash", card.ImageHash);
    }

    [Fact]
    public void ResolvedBlocked_HidesOgImageAndWithholdsHash()
    {
        // Blocked-by-default (D4 reveal gate, user-ratified 2026-06-27): the text card
        // paints, but the og:image is hidden and its hash is NOT exposed until reveal —
        // the card must never fetch the blob before reveal (privacy posture, the D3 twin).
        var card = Assert.Single(LinkPreviewCardModel.All(MakeDoc(
            new PreviewState.Resolved("My Title", "A short description", "imghash", revealed: false))));

        Assert.False(card.ImageRevealed);
        Assert.False(card.ShowImage);
        Assert.Equal("", card.ImageHash);
    }

    [Fact]
    public void ResolvedNoImage_NeverShowsOgImage()
    {
        // A Resolved preview with no og:image (image_hash null) → no image even when revealed.
        var card = Assert.Single(LinkPreviewCardModel.All(MakeDoc(
            new PreviewState.Resolved("My Title", "A short description", null, revealed: true))));

        Assert.False(card.ShowImage);
        Assert.Equal("", card.ImageHash);
    }

    [Fact]
    public void Resolving_RendersNoCardYet()
    {
        // A Resolving block paints no card — the kept inline link shows the URL (no
        // skeleton, matching the web reference). The trigger flips it to Resolved/Failed.
        Assert.Empty(LinkPreviewCardModel.All(MakeDoc(new PreviewState.Resolving())));
    }

    [Fact]
    public void Failed_RendersNoCard()
    {
        // Failed → fall back to the plain inline link, no card (render-model.md § D4).
        Assert.Empty(LinkPreviewCardModel.All(MakeDoc(new PreviewState.Failed())));
    }

    [Fact]
    public void NoLinkPreviewBlock_YieldsNoCards()
    {
        Assert.Empty(LinkPreviewCardModel.All(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("just a body, no preview") }),
        })));
    }

    // ── The two properties the shared face buys us, both invisible to every test above ──
    // Each of these FAILS against the FirstOrDefault/top-level-only twin this delegation
    // replaced, which is the point: a lift whose payoff no test can see is unproven
    // (render-model.md § Implementation status — the same property apple pinned with
    // DocumentEmbedExtractorTests and mutation-proved).

    [Fact]
    public void TwoBareUrls_YieldACardEach_InBodyOrder()
    {
        // The producer emits a LinkPreview after EACH standalone bare-URL paragraph, so a
        // two-URL body carries two previews. The old FirstOrDefault twin returned ONE, which
        // silently dropped the second card on both the feed post and the DM bubble.
        var cards = LinkPreviewCardModel.All(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("two links follow") }),
            new RenderBlock.LinkPreview("https://first.example/a",
                new PreviewState.Resolved("First", "desc one", null, revealed: false)),
            new RenderBlock.LinkPreview("https://second.example/b",
                new PreviewState.Resolved("Second", "desc two", null, revealed: false)),
        }));

        Assert.Equal(2, cards.Count);
        Assert.Equal(new[] { "First", "Second" }, cards.Select(c => c.Title));
        Assert.Equal(new[] { "first.example", "second.example" }, cards.Select(c => c.Domain));
    }

    [Fact]
    public void PreviewNestedInABlockQuote_IsStillFound()
    {
        // The shared projection RECURSES; the hand-rolled twin scanned doc.blocks top level
        // only, so a preview inside a block quote (or list item) painted no card at all.
        var cards = LinkPreviewCardModel.All(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("quoting someone") }),
            new RenderBlock.BlockQuote(new RenderBlock[]
            {
                new RenderBlock.LinkPreview("https://nested.example/deep",
                    new PreviewState.Resolved("Nested", "inside a quote", null, revealed: false)),
            }),
        }));

        var card = Assert.Single(cards);
        Assert.Equal("Nested", card.Title);
        Assert.Equal("nested.example", card.Domain);
    }

    // The producer keeps the inline link in the paragraph and adds the LinkPreview as an
    // additional top-level block (render.rs bare-URL rule); the derivation reads the block.
    private static RenderDocument MakeDoc(PreviewState state) =>
        new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("check this out") }),
            new RenderBlock.LinkPreview("https://example.com/article", state),
        });
}
